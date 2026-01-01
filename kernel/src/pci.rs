//! PCI (Peripheral Component Interconnect) バススキャン実装
//!
//! PCIデバイスを列挙し、設定空間にアクセスします。
//! MMCONFIG (MCFG経由) を優先し、利用できない場合はレガシーI/Oポートを使用します。

use crate::info;
use crate::paging::KERNEL_VIRTUAL_BASE;
use core::arch::asm;
use core::ptr::read_volatile;
use core::sync::atomic::{AtomicU64, Ordering};

/// PCI Configuration Address レジスタ (I/Oポート 0xCF8)
const CONFIG_ADDRESS: u16 = 0xCF8;

/// PCI Configuration Data レジスタ (I/Oポート 0xCFC)
const CONFIG_DATA: u16 = 0xCFC;

/// MMCONFIG設定
/// base_address: MCFGテーブルから取得したベースアドレス（0の場合は未設定）
static MMCONFIG_BASE: AtomicU64 = AtomicU64::new(0);
static MMCONFIG_START_BUS: AtomicU64 = AtomicU64::new(0);
static MMCONFIG_END_BUS: AtomicU64 = AtomicU64::new(0);

/// PCIデバイス情報
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
/// この関数はMMCONFIGが有効な場合のみ呼び出すべきです
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
fn pci_unified_read_u32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    if is_mmconfig_available(bus) {
        // MMCONFIG利用可能 - こちらを優先
        unsafe { mmconfig_read_u32(bus, device, function, offset as u16) }
    } else {
        // レガシーI/Oポートにフォールバック
        pci_config_read_u32(bus, device, function, offset)
    }
}

/// 統合されたPCI Configuration Space から16ビット値を読み込む
fn pci_unified_read_u16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    let data = pci_unified_read_u32(bus, device, function, offset & 0xFC);
    let shift = (offset & 0x02) * 8;
    ((data >> shift) & 0xFFFF) as u16
}

/// 統合されたPCI Configuration Space から8ビット値を読み込む
fn pci_unified_read_u8(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    let data = pci_unified_read_u32(bus, device, function, offset & 0xFC);
    let shift = (offset & 0x03) * 8;
    ((data >> shift) & 0xFF) as u8
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
