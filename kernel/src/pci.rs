//! PCI (Peripheral Component Interconnect) バススキャン実装
//!
//! PCIデバイスを列挙し、設定空間にアクセスします。
//! MMCONFIG (MCFG経由) を優先し、利用できない場合はレガシーI/Oポートを使用します。

use crate::info;
use crate::paging::KERNEL_VIRTUAL_BASE;
use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU64, Ordering};

/// PCI Configuration Address レジスタ (I/Oポート 0xCF8)
const CONFIG_ADDRESS: u16 = 0xCF8;

/// PCI Configuration Data レジスタ (I/Oポート 0xCFC)
const CONFIG_DATA: u16 = 0xCFC;

/// PCI Status レジスタオフセット
const PCI_STATUS: u16 = 0x06;

/// PCI Status: Capabilities List ビット
const PCI_STATUS_CAP_LIST: u16 = 0x10;

/// PCI Capabilities Pointer レジスタオフセット
const PCI_CAP_POINTER: u16 = 0x34;

/// PCI Capability ID
pub mod capability_id {
    /// MSI (Message Signaled Interrupt)
    pub const MSI: u8 = 0x05;
    /// MSI-X
    pub const MSIX: u8 = 0x11;
}

/// MMCONFIG設定
/// base_address: MCFGテーブルから取得したベースアドレス（0の場合は未設定）
static MMCONFIG_BASE: AtomicU64 = AtomicU64::new(0);
static MMCONFIG_START_BUS: AtomicU64 = AtomicU64::new(0);
static MMCONFIG_END_BUS: AtomicU64 = AtomicU64::new(0);

/// PCIデバイス情報
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision: u8,
    pub header_type: u8,
}

/// BAR（Base Address Register）情報
#[derive(Debug, Clone, Copy)]
pub struct BarInfo {
    /// ベースアドレス（物理アドレス、下位ビットをマスク済み）
    pub base_address: u64,
    /// メモリ空間かどうか（false = I/O空間）
    pub is_memory: bool,
    /// 64ビットアドレスか
    pub is_64bit: bool,
    /// プリフェッチ可能か
    pub prefetchable: bool,
}

impl PciDevice {
    /// デバイス情報を読み込んで新しいPciDeviceを作成
    /// MMCONFIG優先、利用できない場合はレガシーI/Oポートを使用
    fn read(bus: u8, device: u8, function: u8) -> Option<Self> {
        let vendor_id = pci_unified_read_u16(bus, device, function, 0x00);

        // Vendor ID が 0xFFFF の場合、デバイスは存在しない
        if vendor_id == 0xFFFF {
            return None;
        }

        let device_id = pci_unified_read_u16(bus, device, function, 0x02);
        let revision = pci_unified_read_u8(bus, device, function, 0x08);
        let prog_if = pci_unified_read_u8(bus, device, function, 0x09);
        let subclass = pci_unified_read_u8(bus, device, function, 0x0A);
        let class_code = pci_unified_read_u8(bus, device, function, 0x0B);
        let header_type = pci_unified_read_u8(bus, device, function, 0x0E);

        Some(PciDevice {
            bus,
            device,
            function,
            vendor_id,
            device_id,
            class_code,
            subclass,
            prog_if,
            revision,
            header_type,
        })
    }

    /// 指定されたCapability IDを持つCapabilityを検索
    ///
    /// # Arguments
    /// * `cap_id` - 検索するCapability ID（例: `capability_id::MSI`）
    ///
    /// # Returns
    /// Capabilityが見つかった場合はそのオフセット、見つからなければNone
    pub fn find_capability(&self, cap_id: u8) -> Option<u16> {
        // Statusレジスタを読んでCapabilities Listの有無を確認
        let status = PCI_CONFIG.read_u16(self.bus, self.device, self.function, PCI_STATUS);
        if (status & PCI_STATUS_CAP_LIST) == 0 {
            return None;
        }

        // Capabilities Pointerを取得（下位2ビットは常に0）
        let mut cap_ptr =
            PCI_CONFIG.read_u8(self.bus, self.device, self.function, PCI_CAP_POINTER) & 0xFC;

        // Capabilityリストを辿る（最大48回でループ防止）
        for _ in 0..48 {
            if cap_ptr == 0 {
                break;
            }

            let cap_header =
                PCI_CONFIG.read_u16(self.bus, self.device, self.function, cap_ptr as u16);
            let current_id = (cap_header & 0xFF) as u8;
            let next_ptr = ((cap_header >> 8) & 0xFC) as u8;

            if current_id == cap_id {
                return Some(cap_ptr as u16);
            }

            cap_ptr = next_ptr;
        }

        None
    }

    /// デバイスがMSIをサポートしているか確認
    pub fn supports_msi(&self) -> bool {
        self.find_capability(capability_id::MSI).is_some()
    }

    /// デバイスがMSI-Xをサポートしているか確認
    pub fn supports_msix(&self) -> bool {
        self.find_capability(capability_id::MSIX).is_some()
    }

    /// BARを読み取る
    ///
    /// # Arguments
    /// * `bar_index` - BAR番号 (0-5)
    ///
    /// # Returns
    /// BAR情報。BARが未使用または無効な場合はNone
    ///
    /// # Notes
    /// 64ビットBARの場合、bar_indexは下位BARを指定します。
    /// 例: BAR0-1が64ビットBARの場合、bar_index=0を指定。
    pub fn read_bar(&self, bar_index: u8) -> Option<BarInfo> {
        // Type 0ヘッダのBAR範囲をチェック
        if bar_index > 5 {
            return None;
        }

        // BARレジスタオフセット: 0x10 + bar_index * 4
        let bar_offset = 0x10 + (bar_index as u16) * 4;
        let bar_value = PCI_CONFIG.read_u32(self.bus, self.device, self.function, bar_offset);

        // BAR値が0なら未使用
        if bar_value == 0 {
            return None;
        }

        // ビット0: 0=メモリ空間, 1=I/O空間
        let is_memory = (bar_value & 0x01) == 0;

        if is_memory {
            // メモリ空間BAR
            // ビット2-1: タイプ (00=32bit, 10=64bit)
            // ビット3: プリフェッチ可能
            let bar_type = (bar_value >> 1) & 0x03;
            let is_64bit = bar_type == 0x02;
            let prefetchable = (bar_value & 0x08) != 0;

            let base_address = if is_64bit {
                // 64ビットBAR: 次のBARと組み合わせる
                if bar_index > 4 {
                    // BAR5は64ビットBARになれない
                    return None;
                }
                let bar_upper =
                    PCI_CONFIG.read_u32(self.bus, self.device, self.function, bar_offset + 4);
                let low = (bar_value & 0xFFFF_FFF0) as u64;
                let high = (bar_upper as u64) << 32;
                high | low
            } else {
                // 32ビットBAR
                (bar_value & 0xFFFF_FFF0) as u64
            };

            Some(BarInfo {
                base_address,
                is_memory: true,
                is_64bit,
                prefetchable,
            })
        } else {
            // I/O空間BAR
            let base_address = (bar_value & 0xFFFF_FFFC) as u64;
            Some(BarInfo {
                base_address,
                is_memory: false,
                is_64bit: false,
                prefetchable: false,
            })
        }
    }

    /// デバイスのクラス名を取得
    pub fn class_name(&self) -> &'static str {
        match self.class_code {
            0x00 => "Unclassified",
            0x01 => "Mass Storage Controller",
            0x02 => "Network Controller",
            0x03 => "Display Controller",
            0x04 => "Multimedia Controller",
            0x05 => "Memory Controller",
            0x06 => "Bridge Device",
            0x07 => "Simple Communication Controller",
            0x08 => "Base System Peripheral",
            0x09 => "Input Device Controller",
            0x0A => "Docking Station",
            0x0B => "Processor",
            0x0C => "Serial Bus Controller",
            0x0D => "Wireless Controller",
            0x0E => "Intelligent Controller",
            0x0F => "Satellite Communication Controller",
            0x10 => "Encryption Controller",
            0x11 => "Signal Processing Controller",
            0xFF => "Unknown",
            _ => "Reserved",
        }
    }
}

/// PCI Configuration Space から32ビット値を読み込む
///
/// # Arguments
/// * `bus` - PCIバス番号 (0-255)
/// * `device` - デバイス番号 (0-31)
/// * `function` - ファンクション番号 (0-7)
/// * `offset` - レジスタオフセット (4バイトアラインメント)
fn pci_config_read_u32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    // アドレスを構築
    // bit 31: Enable bit (1 = enabled)
    // bits 30-24: Reserved (0)
    // bits 23-16: Bus number
    // bits 15-11: Device number
    // bits 10-8: Function number
    // bits 7-2: Register offset (DWORD aligned)
    // bits 1-0: Always 0
    let address: u32 = (1 << 31)
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC);

    unsafe {
        // CONFIG_ADDRESS レジスタにアドレスを書き込む
        asm!(
            "out dx, eax",
            in("dx") CONFIG_ADDRESS,
            in("eax") address,
            options(nomem, nostack, preserves_flags)
        );

        // CONFIG_DATA レジスタからデータを読み込む
        let data: u32;
        asm!(
            "in eax, dx",
            in("dx") CONFIG_DATA,
            out("eax") data,
            options(nomem, nostack, preserves_flags)
        );
        data
    }
}

/// ACPIからMMCONFIG情報を設定
///
/// # Arguments
/// * `base_address` - MCFGベースアドレス（物理アドレス）
/// * `segment` - PCIセグメントグループ（通常は0）
/// * `start_bus` - 開始バス番号
/// * `end_bus` - 終了バス番号
pub fn set_mmconfig(base_address: u64, segment: u16, start_bus: u8, end_bus: u8) {
    if segment != 0 {
        info!(
            "  Warning: PCI segment {} is not supported, ignoring MMCONFIG entry",
            segment
        );
        return;
    }

    MMCONFIG_BASE.store(base_address, Ordering::SeqCst);
    MMCONFIG_START_BUS.store(start_bus as u64, Ordering::SeqCst);
    MMCONFIG_END_BUS.store(end_bus as u64, Ordering::SeqCst);

    info!(
        "  MMCONFIG enabled: Base=0x{:X}, Buses={}-{}",
        base_address, start_bus, end_bus
    );
}

/// MMCONFIGが利用可能かチェック
fn is_mmconfig_available(bus: u8) -> bool {
    let base = MMCONFIG_BASE.load(Ordering::SeqCst);
    if base == 0 {
        return false;
    }

    let start_bus = MMCONFIG_START_BUS.load(Ordering::SeqCst) as u8;
    let end_bus = MMCONFIG_END_BUS.load(Ordering::SeqCst) as u8;

    start_bus <= bus && bus <= end_bus
}

/// MMCONFIG経由でPCI Configuration Spaceから32ビット値を読み込む
///
/// # Safety
///
/// 呼び出し元は以下を保証する必要があります:
/// - `is_mmconfig_available(bus)` が `true` を返すこと
/// - `device` < 32, `function` < 8
/// - `offset` < 4096 かつ 4バイト境界にアラインされていること
/// - 対象のPCI Configuration Spaceがカーネル空間にマッピング済みであること
///   （`KERNEL_VIRTUAL_BASE`を使用した直接マッピングが有効なこと）
unsafe fn mmconfig_read_u32(bus: u8, device: u8, function: u8, offset: u16) -> u32 {
    let base = MMCONFIG_BASE.load(Ordering::SeqCst);

    // MMCONFIGアドレス計算
    // Address = Base + (Bus << 20 | Device << 15 | Function << 12 | Offset)
    let phys_addr = base
        + ((bus as u64) << 20)
        + ((device as u64) << 15)
        + ((function as u64) << 12)
        + (offset as u64);

    // 高位仮想アドレスに変換
    let virt_addr = KERNEL_VIRTUAL_BASE + phys_addr;

    unsafe { read_volatile(virt_addr as *const u32) }
}

/// 統合されたPCI Configuration Space読み込み（MMCONFIG優先、フォールバック対応）
///
/// `PCI_CONFIG`のメソッドを呼び出すラッパー関数。
#[allow(dead_code)]
#[inline]
fn pci_unified_read_u32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    PCI_CONFIG.read_u32(bus, device, function, offset as u16)
}

/// 統合されたPCI Configuration Space から16ビット値を読み込む
///
/// `PCI_CONFIG`のメソッドを呼び出すラッパー関数。
#[inline]
fn pci_unified_read_u16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    PCI_CONFIG.read_u16(bus, device, function, offset as u16)
}

/// 統合されたPCI Configuration Space から8ビット値を読み込む
///
/// `PCI_CONFIG`のメソッドを呼び出すラッパー関数。
#[inline]
fn pci_unified_read_u8(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    PCI_CONFIG.read_u8(bus, device, function, offset as u16)
}

/// PCIバスをスキャンしてデバイスを列挙
pub fn scan_pci_bus() {
    let mmconfig_base = MMCONFIG_BASE.load(Ordering::SeqCst);
    if mmconfig_base != 0 {
        info!(
            "Scanning PCI bus (using MMCONFIG at 0x{:X})...",
            mmconfig_base
        );
    } else {
        info!("Scanning PCI bus (using legacy I/O ports)...");
    }

    let mut device_count = 0;

    // すべてのバスをスキャン (0-255)
    for bus in 0..=255u8 {
        // 各バスのすべてのデバイスをスキャン (0-31)
        for device in 0..32u8 {
            // ファンクション0をチェック
            if let Some(pci_dev) = PciDevice::read(bus, device, 0) {
                device_count += 1;
                print_device(&pci_dev);

                // ヘッダタイプのbit 7が1なら、マルチファンクションデバイス
                let is_multi_function = (pci_dev.header_type & 0x80) != 0;

                if is_multi_function {
                    // ファンクション1-7もスキャン
                    for function in 1..8u8 {
                        if let Some(func_dev) = PciDevice::read(bus, device, function) {
                            device_count += 1;
                            print_device(&func_dev);
                        }
                    }
                }
            }
        }
    }

    info!("PCI scan complete. Found {} device(s)", device_count);
}

/// 条件に一致するPCIデバイスを検索
pub fn find_device<F>(predicate: F) -> Option<PciDevice>
where
    F: Fn(&PciDevice) -> bool,
{
    for bus in 0..=255u8 {
        for device in 0..32u8 {
            if let Some(pci_dev) = PciDevice::read(bus, device, 0) {
                if predicate(&pci_dev) {
                    return Some(pci_dev);
                }
                if (pci_dev.header_type & 0x80) != 0 {
                    for function in 1..8u8 {
                        if let Some(func_dev) = PciDevice::read(bus, device, function) {
                            if predicate(&func_dev) {
                                return Some(func_dev);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// PCIデバイス情報を表示
fn print_device(dev: &PciDevice) {
    info!(
        "  [{:02X}:{:02X}.{}] {:04X}:{:04X} - {} (Class {:02X}:{:02X})",
        dev.bus,
        dev.device,
        dev.function,
        dev.vendor_id,
        dev.device_id,
        dev.class_name(),
        dev.class_code,
        dev.subclass
    );
}

// ============================================================================
// PciConfigAccess trait
// ============================================================================

/// PCI Configuration Spaceアクセスの抽象化
///
/// このトレイトは、PCI Configuration Spaceへの読み書きを抽象化します。
/// レガシーI/Oポート(0xCF8/0xCFC)やMMCONFIG(PCIe)など、
/// 異なるアクセス方式を統一的に扱うことができます。
///
/// # パラメータの有効範囲
///
/// - `bus`: 0-255
/// - `device`: 0-31
/// - `function`: 0-7
/// - `offset`: 0-255 (Legacy) または 0-4095 (MMCONFIG/Extended)
///
/// # スレッド安全性
///
/// **注意**: このトレイトの実装はスレッドセーフではありません。
/// 同じPCIデバイスへの並行アクセスはデータ競合を引き起こす可能性があります。
/// 呼び出し元は適切な排他制御（SpinLockなど）を行う必要があります。
///
/// # エラー動作
///
/// 存在しないデバイスまたは無効なオフセットへの読み込みは`0xFFFFFFFF`を返します。
#[allow(dead_code)]
pub trait PciConfigAccess {
    /// 32ビット値を読み込む
    ///
    /// `offset`は4バイト境界にアラインされている必要があります。
    fn read_u32(&self, bus: u8, device: u8, function: u8, offset: u16) -> u32;

    /// 32ビット値を書き込む
    ///
    /// `offset`は4バイト境界にアラインされている必要があります。
    fn write_u32(&self, bus: u8, device: u8, function: u8, offset: u16, value: u32);

    /// 16ビット値を読み込む
    fn read_u16(&self, bus: u8, device: u8, function: u8, offset: u16) -> u16 {
        let data = self.read_u32(bus, device, function, offset & 0xFFFC);
        ((data >> ((offset & 0x02) * 8)) & 0xFFFF) as u16
    }

    /// 8ビット値を読み込む
    fn read_u8(&self, bus: u8, device: u8, function: u8, offset: u16) -> u8 {
        let data = self.read_u32(bus, device, function, offset & 0xFFFC);
        ((data >> ((offset & 0x03) * 8)) & 0xFF) as u8
    }

    /// 16ビット値を書き込む
    ///
    /// **注意**: Read-Modify-Write操作のためアトミックではありません。
    fn write_u16(&self, bus: u8, device: u8, function: u8, offset: u16, value: u16) {
        let aligned = offset & 0xFFFC;
        let shift = (offset & 0x02) * 8;
        let current = self.read_u32(bus, device, function, aligned);
        let new_val = (current & !(0xFFFF << shift)) | ((value as u32) << shift);
        self.write_u32(bus, device, function, aligned, new_val);
    }

    /// 8ビット値を書き込む
    ///
    /// **注意**: Read-Modify-Write操作のためアトミックではありません。
    fn write_u8(&self, bus: u8, device: u8, function: u8, offset: u16, value: u8) {
        let aligned = offset & 0xFFFC;
        let shift = (offset & 0x03) * 8;
        let current = self.read_u32(bus, device, function, aligned);
        let new_val = (current & !(0xFF << shift)) | ((value as u32) << shift);
        self.write_u32(bus, device, function, aligned, new_val);
    }
}

/// レガシーI/Oポートアクセス
#[allow(dead_code)]
pub struct LegacyPciConfig;

impl PciConfigAccess for LegacyPciConfig {
    fn read_u32(&self, bus: u8, device: u8, function: u8, offset: u16) -> u32 {
        if offset >= 256 {
            // Extended Configuration Space（256-4095）はレガシーI/Oポートでは非対応
            return 0xFFFFFFFF;
        }
        pci_config_read_u32(bus, device, function, offset as u8)
    }

    fn write_u32(&self, bus: u8, device: u8, function: u8, offset: u16, value: u32) {
        if offset >= 256 {
            // Extended Configuration Space（256-4095）はレガシーI/Oポートでは非対応
            return;
        }
        pci_config_write_u32(bus, device, function, offset as u8, value);
    }
}

/// MMCONFIG/Legacy統合アクセス
#[allow(dead_code)]
pub struct UnifiedPciConfig;

impl PciConfigAccess for UnifiedPciConfig {
    fn read_u32(&self, bus: u8, device: u8, function: u8, offset: u16) -> u32 {
        if is_mmconfig_available(bus) {
            // SAFETY: MMCONFIGが利用可能なことを確認済み
            unsafe { mmconfig_read_u32(bus, device, function, offset) }
        } else {
            pci_config_read_u32(bus, device, function, offset as u8)
        }
    }

    fn write_u32(&self, bus: u8, device: u8, function: u8, offset: u16, value: u32) {
        if is_mmconfig_available(bus) {
            // SAFETY: MMCONFIGが利用可能なことを確認済み
            unsafe { mmconfig_write_u32(bus, device, function, offset, value) }
        } else {
            pci_config_write_u32(bus, device, function, offset as u8, value);
        }
    }
}

/// グローバルPCIアクセスインスタンス（統合方式）
#[allow(dead_code)]
pub static PCI_CONFIG: UnifiedPciConfig = UnifiedPciConfig;

// ============================================================================
// Write関数
// ============================================================================

/// レガシーI/OポートでPCI Configuration Spaceに32ビット値を書き込む
#[allow(dead_code)]
fn pci_config_write_u32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let address: u32 = (1 << 31)
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC);

    unsafe {
        // CONFIG_ADDRESS レジスタにアドレスを書き込む
        asm!(
            "out dx, eax",
            in("dx") CONFIG_ADDRESS,
            in("eax") address,
            options(nomem, nostack, preserves_flags)
        );

        // CONFIG_DATA レジスタにデータを書き込む
        asm!(
            "out dx, eax",
            in("dx") CONFIG_DATA,
            in("eax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

/// MMCONFIG経由でPCI Configuration Spaceに32ビット値を書き込む
///
/// # Safety
///
/// 呼び出し元は以下を保証する必要があります:
/// - `is_mmconfig_available(bus)` が `true` を返すこと
/// - `device` < 32, `function` < 8
/// - `offset` < 4096 かつ 4バイト境界にアラインされていること
/// - 対象のPCI Configuration Spaceがカーネル空間にマッピング済みであること
///   （`KERNEL_VIRTUAL_BASE`を使用した直接マッピングが有効なこと）
/// - 書き込み対象のレジスタが書き込み可能であること
#[allow(dead_code)]
unsafe fn mmconfig_write_u32(bus: u8, device: u8, function: u8, offset: u16, value: u32) {
    let base = MMCONFIG_BASE.load(Ordering::SeqCst);

    let phys_addr = base
        + ((bus as u64) << 20)
        + ((device as u64) << 15)
        + ((function as u64) << 12)
        + (offset as u64);

    let virt_addr = KERNEL_VIRTUAL_BASE + phys_addr;

    unsafe { write_volatile(virt_addr as *mut u32, value) }
}
