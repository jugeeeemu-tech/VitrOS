//! MSI (Message Signaled Interrupt) サポート
//!
//! PCIデバイスのMSI割り込みを設定・管理します。

use crate::pci::{PCI_CONFIG, PciConfigAccess, PciDevice, capability_id};

/// MSI Capability レジスタオフセット（Capability先頭からの相対）
mod msi_reg {
    /// Message Control (2バイト)
    pub const MESSAGE_CONTROL: u16 = 0x02;
    /// Message Address (4バイト)
    pub const MESSAGE_ADDRESS: u16 = 0x04;
    /// Message Address Upper (4バイト、64ビット対応時のみ)
    pub const MESSAGE_ADDRESS_UPPER: u16 = 0x08;
    /// Message Data (2バイト) - 32ビットアドレス時
    pub const MESSAGE_DATA_32: u16 = 0x08;
    /// Message Data (2バイト) - 64ビットアドレス時
    pub const MESSAGE_DATA_64: u16 = 0x0C;
}

/// Message Control レジスタのビットフィールド
mod message_control {
    /// MSI Enable ビット
    pub const ENABLE: u16 = 1 << 0;
    /// 64ビットアドレス対応ビット
    pub const ADDR_64BIT: u16 = 1 << 7;
}

/// MSI-X Capability レジスタオフセット（Capability先頭からの相対）
mod msix_reg {
    /// Message Control (2バイト)
    pub const MESSAGE_CONTROL: u16 = 0x02;
    /// Table Offset/BIR (4バイト)
    pub const TABLE_OFFSET_BIR: u16 = 0x04;
    /// PBA Offset/BIR (4バイト)
    pub const PBA_OFFSET_BIR: u16 = 0x08;
}

/// MSI-X Message Control レジスタのビットフィールド
mod msix_message_control {
    /// MSI-X Enable ビット
    pub const ENABLE: u16 = 1 << 15;
    /// Function Mask ビット
    pub const FUNCTION_MASK: u16 = 1 << 14;
    /// Table Size マスク（下位11ビット）
    pub const TABLE_SIZE_MASK: u16 = 0x07FF;
}

/// MSI-X テーブルエントリのオフセット
mod msix_table_entry {
    /// Message Address (下位32ビット)
    pub const MSG_ADDR: u32 = 0x00;
    /// Message Upper Address (上位32ビット)
    pub const MSG_UPPER_ADDR: u32 = 0x04;
    /// Message Data
    pub const MSG_DATA: u32 = 0x08;
    /// Vector Control
    pub const VECTOR_CONTROL: u32 = 0x0C;
    /// エントリサイズ（16バイト）
    pub const SIZE: u32 = 0x10;
}

/// Vector Control のビットフィールド
mod msix_vector_control {
    /// Mask ビット
    pub const MASK: u32 = 1 << 0;
}

/// LAPIC MSI Address ベース (x86/x86_64)
const LAPIC_MSI_ADDRESS_BASE: u32 = 0xFEE0_0000;

/// x86の例外ベクタ範囲の終端（0-31は例外用）
const MIN_MSI_VECTOR: u8 = 32;
/// MSI/MSI-Xで使用可能なベクタの最大値
const MAX_MSI_VECTOR: u8 = 239;

/// PCI Command Registerオフセット
const PCI_COMMAND: u16 = 0x04;

/// PCI Command Register: Interrupt Disable ビット
const PCI_COMMAND_INTX_DISABLE: u16 = 1 << 10;

/// INTx（レガシーPCI割り込み）を無効化
fn disable_intx(device: &PciDevice) {
    let command = PCI_CONFIG.read_u16(device.bus, device.device, device.function, PCI_COMMAND);
    PCI_CONFIG.write_u16(
        device.bus,
        device.device,
        device.function,
        PCI_COMMAND,
        command | PCI_COMMAND_INTX_DISABLE,
    );
}

/// INTx（レガシーPCI割り込み）を再有効化
fn enable_intx(device: &PciDevice) {
    let command = PCI_CONFIG.read_u16(device.bus, device.device, device.function, PCI_COMMAND);
    PCI_CONFIG.write_u16(
        device.bus,
        device.device,
        device.function,
        PCI_COMMAND,
        command & !PCI_COMMAND_INTX_DISABLE,
    );
}

/// MSI/MSI-X設定時のエラー
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsiError {
    /// デバイスがMSI/MSI-Xをサポートしていない
    NotSupported,
    /// 無効なベクタ番号（32-239の範囲外）
    InvalidVector { vector: u8 },
    /// 無効なエントリインデックス（MSI-X）
    InvalidEntry { index: u16, table_size: u16 },
    /// BAR読み取り失敗（MSI-X）
    InvalidBar { bar_index: u8 },
    /// 要求されたベクタ数がテーブルサイズを超過（MSI-X）
    TooManyVectors { requested: usize, available: u16 },
    /// MMIOマッピング失敗（MSI-X）
    MappingFailed,
}

/// MSI設定情報
#[derive(Debug, Clone, Copy)]
pub struct MsiConfig {
    /// 割り込みベクタ番号
    pub vector: u8,
    /// MSI Capabilityのオフセット
    pub cap_offset: u16,
}

/// MSI-X Capability情報
#[derive(Debug, Clone, Copy)]
pub struct MsixCapability {
    /// Capabilityのオフセット
    pub cap_offset: u16,
    /// テーブルサイズ（エントリ数 = Table Size + 1）
    pub table_size: u16,
    /// テーブルのBAR番号 (BIR: BAR Indicator Register)
    pub table_bir: u8,
    /// テーブルのオフセット（BAR内）
    pub table_offset: u32,
    /// PBAのBAR番号
    pub pba_bir: u8,
    /// PBAのオフセット（BAR内）
    pub pba_offset: u32,
}

/// PCIデバイスのMSIを設定
///
/// # Arguments
/// * `device` - MSIを設定するPCIデバイス
/// * `vector` - 割り込みベクタ番号（48-239推奨）
///
/// # Returns
/// 成功時はMsiConfig、失敗時はMsiError
///
/// # ベクタ番号の推奨範囲
/// - 0-31: CPU例外用（使用不可）
/// - 32-47: システム予約
/// - 48-239: デバイスMSI用（推奨）
/// - 240-254: 予約
/// - 255: スプリアス割り込み
pub fn configure_msi(device: &PciDevice, vector: u8) -> Result<MsiConfig, MsiError> {
    // ベクタ番号の検証
    if vector < MIN_MSI_VECTOR || vector > MAX_MSI_VECTOR {
        return Err(MsiError::InvalidVector { vector });
    }

    // MSI Capabilityを検索
    let cap_offset = device
        .find_capability(capability_id::MSI)
        .ok_or(MsiError::NotSupported)?;

    let bus = device.bus;
    let dev = device.device;
    let func = device.function;

    // Message Controlを読み取り
    let msg_ctrl = PCI_CONFIG.read_u16(bus, dev, func, cap_offset + msi_reg::MESSAGE_CONTROL);
    let is_64bit = (msg_ctrl & message_control::ADDR_64BIT) != 0;

    // MSIを一旦無効化
    PCI_CONFIG.write_u16(
        bus,
        dev,
        func,
        cap_offset + msi_reg::MESSAGE_CONTROL,
        msg_ctrl & !message_control::ENABLE,
    );

    // Message Address を設定（LAPIC向け、Destination=0, Fixed delivery）
    PCI_CONFIG.write_u32(
        bus,
        dev,
        func,
        cap_offset + msi_reg::MESSAGE_ADDRESS,
        LAPIC_MSI_ADDRESS_BASE,
    );

    // Message Data を設定（ベクタ番号、Edge trigger, Fixed delivery mode）
    let data_offset = if is_64bit {
        // 64ビット対応: Upper Addressを0に設定
        PCI_CONFIG.write_u32(
            bus,
            dev,
            func,
            cap_offset + msi_reg::MESSAGE_ADDRESS_UPPER,
            0,
        );
        msi_reg::MESSAGE_DATA_64
    } else {
        msi_reg::MESSAGE_DATA_32
    };
    PCI_CONFIG.write_u16(bus, dev, func, cap_offset + data_offset, vector as u16);

    // MSIを有効化
    PCI_CONFIG.write_u16(
        bus,
        dev,
        func,
        cap_offset + msi_reg::MESSAGE_CONTROL,
        msg_ctrl | message_control::ENABLE,
    );

    // INTx割り込みを無効化（MSI使用時は不要）
    disable_intx(device);

    Ok(MsiConfig { vector, cap_offset })
}

/// PCIデバイスのMSIを無効化
///
/// # Arguments
/// * `device` - MSIを無効化するPCIデバイス
///
/// # Returns
/// 成功時はOk(()), デバイスがMSI非対応ならErr(MsiError::NotSupported)
pub fn disable_msi(device: &PciDevice) -> Result<(), MsiError> {
    let cap_offset = device
        .find_capability(capability_id::MSI)
        .ok_or(MsiError::NotSupported)?;

    let bus = device.bus;
    let dev = device.device;
    let func = device.function;

    // Message Controlを読み取り、Enableビットをクリア
    let msg_ctrl = PCI_CONFIG.read_u16(bus, dev, func, cap_offset + msi_reg::MESSAGE_CONTROL);
    PCI_CONFIG.write_u16(
        bus,
        dev,
        func,
        cap_offset + msi_reg::MESSAGE_CONTROL,
        msg_ctrl & !message_control::ENABLE,
    );

    // INTx割り込みを再有効化
    enable_intx(device);

    Ok(())
}

// ============================================================================
// MSI-X実装
// ============================================================================

/// PCIデバイスのMSI-X Capabilityを検出
///
/// # Arguments
/// * `device` - 検査するPCIデバイス
///
/// # Returns
/// MSI-X Capabilityが見つかった場合はMsixCapability、なければNone
pub fn detect_msix(device: &PciDevice) -> Option<MsixCapability> {
    // MSI-X Capabilityを検索
    let cap_offset = device.find_capability(capability_id::MSIX)?;

    let bus = device.bus;
    let dev = device.device;
    let func = device.function;

    // Message Controlを読み取り
    let msg_ctrl = PCI_CONFIG.read_u16(bus, dev, func, cap_offset + msix_reg::MESSAGE_CONTROL);
    let table_size = (msg_ctrl & msix_message_control::TABLE_SIZE_MASK) + 1;

    // Table Offset/BIRを読み取り
    let table_offset_bir =
        PCI_CONFIG.read_u32(bus, dev, func, cap_offset + msix_reg::TABLE_OFFSET_BIR);
    let table_bir = (table_offset_bir & 0x07) as u8;
    let table_offset = table_offset_bir & 0xFFFF_FFF8;

    // PBA Offset/BIRを読み取り
    let pba_offset_bir = PCI_CONFIG.read_u32(bus, dev, func, cap_offset + msix_reg::PBA_OFFSET_BIR);
    let pba_bir = (pba_offset_bir & 0x07) as u8;
    let pba_offset = pba_offset_bir & 0xFFFF_FFF8;

    Some(MsixCapability {
        cap_offset,
        table_size,
        table_bir,
        table_offset,
        pba_bir,
        pba_offset,
    })
}

/// MSI-X設定情報
///
/// MSI-Xテーブルへのアクセスを提供します。
/// MMIOアクセス用構造体のため、意図しない複製を防ぐためClone/Copyは実装しません。
#[derive(Debug)]
pub struct MsixConfig {
    /// MSI-X Capability情報
    pub capability: MsixCapability,
    /// テーブルの仮想アドレス
    table_virt_addr: u64,
}

impl MsixConfig {
    /// 新しいMsixConfigを作成
    ///
    /// # Arguments
    /// * `capability` - MSI-X Capability情報
    /// * `table_virt_addr` - マッピング済みテーブルの仮想アドレス
    pub fn new(capability: MsixCapability, table_virt_addr: u64) -> Self {
        Self {
            capability,
            table_virt_addr,
        }
    }

    /// テーブルサイズ（エントリ数）を取得
    pub fn table_size(&self) -> u16 {
        self.capability.table_size
    }

    /// エントリにMessage Address/Dataを設定
    ///
    /// # Arguments
    /// * `index` - エントリインデックス（0から始まる）
    /// * `vector` - 割り込みベクタ番号（32-239）
    ///
    /// # Returns
    /// 成功時はOk(()), 失敗時はMsiError
    pub fn configure_entry(&self, index: u16, vector: u8) -> Result<(), MsiError> {
        // インデックスの検証
        if index >= self.capability.table_size {
            return Err(MsiError::InvalidEntry {
                index,
                table_size: self.capability.table_size,
            });
        }

        // ベクタ番号の検証
        if vector < MIN_MSI_VECTOR || vector > MAX_MSI_VECTOR {
            return Err(MsiError::InvalidVector { vector });
        }

        // エントリのアドレスを計算
        let entry_addr = self.table_virt_addr + (index as u64) * (msix_table_entry::SIZE as u64);

        // SAFETY:
        // - table_virt_addrはconfigure_msix()でBAR物理アドレスから
        //   phys_to_virt()を使用して生成された仮想アドレス
        // - MSI-Xテーブルエントリは16バイト境界にアライメントされている（PCI仕様）
        // - 各フィールドアクセスは4バイト境界にアライメントされている
        // - インデックスは上記で検証済みでtable_size未満
        unsafe {
            // Message Address (LAPIC向け)
            let addr_ptr = entry_addr as *mut u32;
            core::ptr::write_volatile(addr_ptr, LAPIC_MSI_ADDRESS_BASE);

            // Message Upper Address (0)
            let upper_addr_ptr = (entry_addr + msix_table_entry::MSG_UPPER_ADDR as u64) as *mut u32;
            core::ptr::write_volatile(upper_addr_ptr, 0);

            // Message Data (ベクタ番号)
            let data_ptr = (entry_addr + msix_table_entry::MSG_DATA as u64) as *mut u32;
            core::ptr::write_volatile(data_ptr, vector as u32);
        }

        Ok(())
    }

    /// エントリをマスク（割り込みを無効化）
    ///
    /// # Arguments
    /// * `index` - エントリインデックス
    pub fn mask_entry(&self, index: u16) -> Result<(), MsiError> {
        if index >= self.capability.table_size {
            return Err(MsiError::InvalidEntry {
                index,
                table_size: self.capability.table_size,
            });
        }

        let entry_addr = self.table_virt_addr + (index as u64) * (msix_table_entry::SIZE as u64);
        let ctrl_ptr = (entry_addr + msix_table_entry::VECTOR_CONTROL as u64) as *mut u32;

        // SAFETY:
        // - table_virt_addrはconfigure_msix()でBAR物理アドレスから
        //   phys_to_virt()を使用して生成された仮想アドレス
        // - ctrl_ptrは4バイト境界にアライメントされている（PCI仕様）
        // - インデックスは上記で検証済みでtable_size未満
        unsafe {
            let ctrl = core::ptr::read_volatile(ctrl_ptr);
            core::ptr::write_volatile(ctrl_ptr, ctrl | msix_vector_control::MASK);
        }

        Ok(())
    }

    /// エントリをアンマスク（割り込みを有効化）
    ///
    /// # Arguments
    /// * `index` - エントリインデックス
    pub fn unmask_entry(&self, index: u16) -> Result<(), MsiError> {
        if index >= self.capability.table_size {
            return Err(MsiError::InvalidEntry {
                index,
                table_size: self.capability.table_size,
            });
        }

        let entry_addr = self.table_virt_addr + (index as u64) * (msix_table_entry::SIZE as u64);
        let ctrl_ptr = (entry_addr + msix_table_entry::VECTOR_CONTROL as u64) as *mut u32;

        // SAFETY:
        // - table_virt_addrはconfigure_msix()でBAR物理アドレスから
        //   phys_to_virt()を使用して生成された仮想アドレス
        // - ctrl_ptrは4バイト境界にアライメントされている（PCI仕様）
        // - インデックスは上記で検証済みでtable_size未満
        unsafe {
            let ctrl = core::ptr::read_volatile(ctrl_ptr);
            core::ptr::write_volatile(ctrl_ptr, ctrl & !msix_vector_control::MASK);
        }

        Ok(())
    }

    /// 全エントリをマスク
    pub fn mask_all(&self) {
        for i in 0..self.capability.table_size {
            // インデックスは必ずtable_size未満なのでエラーは発生しない
            let result = self.mask_entry(i);
            debug_assert!(
                result.is_ok(),
                "mask_entry failed unexpectedly: {:?}",
                result
            );
        }
    }
}

/// MSI-Xを設定して有効化
///
/// # Arguments
/// * `device` - MSI-Xを設定するPCIデバイス
/// * `vectors` - 設定する割り込みベクタ番号のスライス（各エントリに対応）
///
/// # Returns
/// 成功時はMsixConfig、失敗時はMsiError
///
/// # Notes
/// - vectorsの長さはテーブルサイズ以下である必要があります
/// - 各ベクタ番号は32-239の範囲内である必要があります
/// - テーブルのマッピングにはphys_to_virtを使用した直接マッピングを使用します
pub fn configure_msix(device: &PciDevice, vectors: &[u8]) -> Result<MsixConfig, MsiError> {
    use crate::paging::phys_to_virt;

    // MSI-X Capabilityを検出
    let capability = detect_msix(device).ok_or(MsiError::NotSupported)?;

    // ベクタ数の検証
    if vectors.len() > capability.table_size as usize {
        return Err(MsiError::TooManyVectors {
            requested: vectors.len(),
            available: capability.table_size,
        });
    }

    // 各ベクタ番号の検証
    for &v in vectors {
        if v < MIN_MSI_VECTOR || v > MAX_MSI_VECTOR {
            return Err(MsiError::InvalidVector { vector: v });
        }
    }

    // BARからテーブルの物理アドレスを取得
    let bar_info = device
        .read_bar(capability.table_bir)
        .ok_or(MsiError::InvalidBar {
            bar_index: capability.table_bir,
        })?;

    if !bar_info.is_memory {
        return Err(MsiError::InvalidBar {
            bar_index: capability.table_bir,
        });
    }

    // テーブルの物理アドレスを計算（オーバーフローチェック付き）
    let table_phys_addr = bar_info
        .base_address
        .checked_add(capability.table_offset as u64)
        .ok_or(MsiError::InvalidBar {
            bar_index: capability.table_bir,
        })?;

    // 仮想アドレスに変換（既存関数を使用）
    let table_virt_addr = phys_to_virt(table_phys_addr).map_err(|_| MsiError::MappingFailed)?;

    let config = MsixConfig::new(capability, table_virt_addr);

    let bus = device.bus;
    let dev = device.device;
    let func = device.function;

    // MSI-Xを一旦無効化
    let msg_ctrl = PCI_CONFIG.read_u16(
        bus,
        dev,
        func,
        capability.cap_offset + msix_reg::MESSAGE_CONTROL,
    );
    PCI_CONFIG.write_u16(
        bus,
        dev,
        func,
        capability.cap_offset + msix_reg::MESSAGE_CONTROL,
        msg_ctrl & !msix_message_control::ENABLE,
    );

    // 全エントリをマスク
    config.mask_all();

    // 各エントリを設定
    for (i, &vector) in vectors.iter().enumerate() {
        config.configure_entry(i as u16, vector)?;
    }

    // 設定したエントリをアンマスク
    for i in 0..vectors.len() {
        config.unmask_entry(i as u16)?;
    }

    // MSI-Xを有効化
    PCI_CONFIG.write_u16(
        bus,
        dev,
        func,
        capability.cap_offset + msix_reg::MESSAGE_CONTROL,
        msg_ctrl | msix_message_control::ENABLE,
    );

    // INTx割り込みを無効化（MSI-X使用時は不要）
    disable_intx(device);

    Ok(config)
}

/// MSI-Xを無効化
///
/// # Arguments
/// * `device` - MSI-Xを無効化するPCIデバイス
///
/// # Returns
/// 成功時はOk(()), デバイスがMSI-X非対応ならErr(MsiError::NotSupported)
pub fn disable_msix(device: &PciDevice) -> Result<(), MsiError> {
    let cap_offset = device
        .find_capability(capability_id::MSIX)
        .ok_or(MsiError::NotSupported)?;

    let bus = device.bus;
    let dev = device.device;
    let func = device.function;

    // Message Controlを読み取り、Enableビットをクリア
    let msg_ctrl = PCI_CONFIG.read_u16(bus, dev, func, cap_offset + msix_reg::MESSAGE_CONTROL);
    PCI_CONFIG.write_u16(
        bus,
        dev,
        func,
        cap_offset + msix_reg::MESSAGE_CONTROL,
        msg_ctrl & !msix_message_control::ENABLE,
    );

    // INTx割り込みを再有効化
    enable_intx(device);

    Ok(())
}
