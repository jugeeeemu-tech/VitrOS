//! MSI (Message Signaled Interrupt) サポート
//!
//! PCIデバイスのMSI割り込みを設定・管理します。

use crate::pci::{capability_id, PciConfigAccess, PciDevice, PCI_CONFIG};

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

/// LAPIC MSI Address ベース (x86/x86_64)
const LAPIC_MSI_ADDRESS_BASE: u32 = 0xFEE0_0000;

/// MSI設定時のエラー
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsiError {
    /// デバイスがMSIをサポートしていない
    NotSupported,
    /// 無効なベクタ番号（32-239の範囲外）
    InvalidVector,
}

/// MSI設定情報
#[derive(Debug, Clone, Copy)]
pub struct MsiConfig {
    /// 割り込みベクタ番号
    pub vector: u8,
    /// MSI Capabilityのオフセット
    pub cap_offset: u16,
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
    // ベクタ番号の検証（32以上239以下）
    if vector < 32 || vector > 239 {
        return Err(MsiError::InvalidVector);
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
        PCI_CONFIG.write_u32(bus, dev, func, cap_offset + msi_reg::MESSAGE_ADDRESS_UPPER, 0);
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

    Ok(())
}
