//! ACPI (Advanced Configuration and Power Interface) サポート
//!
//! ACPI テーブルを読み取り、システム設定情報を取得します。
//! UEFI ブートローダーから RSDP アドレスを受け取り、XSDT/RSDT を解析します。

use crate::info;
use crate::paging::{KERNEL_VIRTUAL_BASE, phys_to_virt};
use vitros_common::boot_info::BootInfo;

/// RSDP (Root System Description Pointer) - ACPI 1.0
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct Rsdp {
    signature: [u8; 8], // "RSD PTR "
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_address: u32,
}

/// RSDP (Root System Description Pointer) - ACPI 2.0+
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct RsdpExtended {
    /// ACPI 1.0 部分
    rsdp_v1: Rsdp,
    /// 拡張部分の長さ
    length: u32,
    /// XSDT の物理アドレス（64ビット）
    xsdt_address: u64,
    /// 拡張チェックサム
    extended_checksum: u8,
    reserved: [u8; 3],
}

/// ACPI テーブル共通ヘッダ
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct AcpiTableHeader {
    signature: [u8; 4],    // テーブル識別子 (例: "APIC", "FACP")
    length: u32,           // テーブル全体の長さ
    revision: u8,          // テーブルリビジョン
    checksum: u8,          // チェックサム
    oem_id: [u8; 6],       // OEM ID
    oem_table_id: [u8; 8], // OEM テーブル ID
    oem_revision: u32,     // OEM リビジョン
    creator_id: u32,       // クリエータ ID
    creator_revision: u32, // クリエータリビジョン
}

impl AcpiTableHeader {
    /// シグネチャを文字列として取得
    fn signature_str(&self) -> &str {
        core::str::from_utf8(&self.signature).unwrap_or("????")
    }

    /// チェックサムを検証
    fn verify_checksum(&self) -> bool {
        let length = self.length;
        let bytes =
            unsafe { core::slice::from_raw_parts(self as *const _ as *const u8, length as usize) };

        let sum: u8 = bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
        sum == 0
    }
}

/// MADT (Multiple APIC Description Table) エントリタイプ
#[allow(dead_code)]
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MadtEntryType {
    ProcessorLocalApic = 0,
    IoApic = 1,
    InterruptSourceOverride = 2,
    NmiSource = 3,
    LocalApicNmi = 4,
    LocalApicAddressOverride = 5,
    ProcessorLocalX2Apic = 9,
}

/// MADT エントリ共通ヘッダ
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct MadtEntryHeader {
    entry_type: u8,
    length: u8,
}

/// MADT エントリ: Processor Local APIC
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct MadtProcessorLocalApic {
    header: MadtEntryHeader,
    acpi_processor_id: u8,
    apic_id: u8,
    flags: u32, // bit 0: Processor Enabled
}

/// MADT エントリ: I/O APIC
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct MadtIoApic {
    header: MadtEntryHeader,
    io_apic_id: u8,
    reserved: u8,
    io_apic_address: u32,
    global_system_interrupt_base: u32,
}

/// MADT (Multiple APIC Description Table) テーブル
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct Madt {
    header: AcpiTableHeader,
    local_apic_address: u32,
    flags: u32,
    // この後にエントリが続く
}

/// MCFG (Memory Mapped Configuration) テーブル
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct Mcfg {
    header: AcpiTableHeader,
    reserved: u64,
    // この後に Configuration Space Base Address Allocation Structures が続く
}

/// HPET (High Precision Event Timer) テーブル
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct HpetTable {
    header: AcpiTableHeader,
    event_timer_block_id: u32,
    base_address: HpetAddress,
    hpet_number: u8,
    minimum_tick: u16,
    page_protection: u8,
}

/// HPET Base Address (ACPI Generic Address Structure)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct HpetAddress {
    address_space_id: u8, // 0 = Memory
    register_bit_width: u8,
    register_bit_offset: u8,
    reserved: u8,
    address: u64,
}

/// MCFG Configuration Space Base Address Allocation Structure
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct McfgEntry {
    pub base_address: u64,
    pub pci_segment_group: u16,
    pub start_bus: u8,
    pub end_bus: u8,
    reserved: u32,
}

impl Rsdp {
    /// シグネチャが正しいか確認
    fn is_valid_signature(&self) -> bool {
        &self.signature == b"RSD PTR "
    }

    /// チェックサムを検証
    fn verify_checksum(&self) -> bool {
        let bytes = unsafe {
            core::slice::from_raw_parts(self as *const _ as *const u8, core::mem::size_of::<Rsdp>())
        };

        let sum: u8 = bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
        sum == 0
    }

    /// OEM ID を文字列として取得
    fn oem_id_str(&self) -> &str {
        core::str::from_utf8(&self.oem_id).unwrap_or("<invalid>")
    }
}

/// ACPI を初期化
///
/// # Arguments
/// * `boot_info` - ブートローダーから渡された情報（RSDP アドレスを含む）
pub fn init(boot_info: &BootInfo) {
    info!("Initializing ACPI...");

    if boot_info.rsdp_address == 0 {
        info!("RSDP address not provided by bootloader. ACPI not available.");
        return;
    }

    // RSDP の物理アドレスを高位仮想アドレスに変換
    let rsdp_virt_addr = phys_to_virt(boot_info.rsdp_address).unwrap_or_else(|_| {
        info!("Failed to convert RSDP address, falling back to direct addition");
        KERNEL_VIRTUAL_BASE + boot_info.rsdp_address
    });
    let rsdp = unsafe { &*(rsdp_virt_addr as *const Rsdp) };

    if !rsdp.is_valid_signature() {
        info!("Invalid RSDP signature. ACPI not available.");
        return;
    }

    if !rsdp.verify_checksum() {
        info!("RSDP checksum verification failed. ACPI not available.");
        return;
    }

    info!("RSDP found at 0x{:016X}", boot_info.rsdp_address);
    info!("  OEM ID: {}", rsdp.oem_id_str());
    info!("  Revision: {}", rsdp.revision);

    if rsdp.revision >= 2 {
        // ACPI 2.0+ - XSDT を使用
        let rsdp_ext = unsafe { &*(rsdp_virt_addr as *const RsdpExtended) };
        // packed struct のフィールドはローカル変数にコピー
        let xsdt_addr = rsdp_ext.xsdt_address;
        info!("  ACPI 2.0+ detected");
        info!("  XSDT Address: 0x{:016X}", xsdt_addr);

        parse_xsdt(xsdt_addr);
    } else {
        // ACPI 1.0 - RSDT を使用
        // packed struct のフィールドはローカル変数にコピー
        let rsdt_addr = rsdp.rsdt_address;
        info!("  ACPI 1.0 detected");
        info!("  RSDT Address: 0x{:08X}", rsdt_addr);

        parse_rsdt(rsdt_addr as u64);
    }
}

/// XSDT (Extended System Description Table) を解析
fn parse_xsdt(xsdt_phys_addr: u64) {
    if xsdt_phys_addr == 0 {
        return;
    }

    // 物理アドレスを高位仮想アドレスに変換
    let xsdt_virt_addr = KERNEL_VIRTUAL_BASE + xsdt_phys_addr;
    let header = unsafe { &*(xsdt_virt_addr as *const AcpiTableHeader) };

    if header.signature_str() != "XSDT" {
        info!("Invalid XSDT signature: {}", header.signature_str());
        return;
    }

    if !header.verify_checksum() {
        info!("XSDT checksum verification failed");
        return;
    }

    // テーブルエントリ数を計算（ヘッダ以降が64ビットアドレスの配列）
    let header_size = core::mem::size_of::<AcpiTableHeader>();
    let entry_count = (header.length as usize - header_size) / 8;

    info!("XSDT parsed successfully. Tables found: {}", entry_count);

    // エントリのアドレス配列にアクセス
    let entries_ptr = (xsdt_virt_addr + header_size as u64) as *const u64;

    for i in 0..entry_count {
        // packed 構造体の後なのでアンアラインドアクセスが必要
        let table_phys_addr = unsafe { entries_ptr.add(i).read_unaligned() };
        let table_virt_addr = KERNEL_VIRTUAL_BASE + table_phys_addr;
        let table_header = unsafe { &*(table_virt_addr as *const AcpiTableHeader) };

        info!(
            "  [{}] {} at 0x{:016X}",
            i,
            table_header.signature_str(),
            table_phys_addr
        );

        // APIC (MADT) テーブルを見つけたら解析
        if table_header.signature_str() == "APIC" {
            parse_madt(table_phys_addr);
        }
        // MCFG テーブルを見つけたら解析
        else if table_header.signature_str() == "MCFG" {
            parse_mcfg(table_phys_addr);
        }
        // HPET テーブルを見つけたら解析
        // HPETは必須ではないのでエラー時はログ出力して継続
        else if table_header.signature_str() == "HPET" {
            if let Err(e) = parse_hpet(table_phys_addr) {
                info!(
                    "HPET initialization failed: {:?}, continuing without HPET",
                    e
                );
            }
        }
    }
}

/// RSDT (Root System Description Table) を解析
fn parse_rsdt(rsdt_phys_addr: u64) {
    if rsdt_phys_addr == 0 {
        return;
    }

    // 物理アドレスを高位仮想アドレスに変換
    let rsdt_virt_addr = KERNEL_VIRTUAL_BASE + rsdt_phys_addr;
    let header = unsafe { &*(rsdt_virt_addr as *const AcpiTableHeader) };

    if header.signature_str() != "RSDT" {
        info!("Invalid RSDT signature: {}", header.signature_str());
        return;
    }

    if !header.verify_checksum() {
        info!("RSDT checksum verification failed");
        return;
    }

    // テーブルエントリ数を計算（ヘッダ以降が32ビットアドレスの配列）
    let header_size = core::mem::size_of::<AcpiTableHeader>();
    let entry_count = (header.length as usize - header_size) / 4;

    info!("RSDT parsed successfully. Tables found: {}", entry_count);

    // エントリのアドレス配列にアクセス
    let entries_ptr = (rsdt_virt_addr + header_size as u64) as *const u32;

    for i in 0..entry_count {
        // packed 構造体の後なのでアンアラインドアクセスが必要
        let table_phys_addr = unsafe { entries_ptr.add(i).read_unaligned() } as u64;
        let table_virt_addr = KERNEL_VIRTUAL_BASE + table_phys_addr;
        let table_header = unsafe { &*(table_virt_addr as *const AcpiTableHeader) };

        info!(
            "  [{}] {} at 0x{:016X}",
            i,
            table_header.signature_str(),
            table_phys_addr
        );

        // APIC (MADT) テーブルを見つけたら解析
        if table_header.signature_str() == "APIC" {
            parse_madt(table_phys_addr);
        }
        // MCFG テーブルを見つけたら解析
        else if table_header.signature_str() == "MCFG" {
            parse_mcfg(table_phys_addr);
        }
        // HPET テーブルを見つけたら解析
        // HPETは必須ではないのでエラー時はログ出力して継続
        else if table_header.signature_str() == "HPET" {
            if let Err(e) = parse_hpet(table_phys_addr) {
                info!(
                    "HPET initialization failed: {:?}, continuing without HPET",
                    e
                );
            }
        }
    }
}

/// MADT (Multiple APIC Description Table) を解析
fn parse_madt(madt_phys_addr: u64) {
    if madt_phys_addr == 0 {
        return;
    }

    // 物理アドレスを高位仮想アドレスに変換
    let madt_virt_addr = KERNEL_VIRTUAL_BASE + madt_phys_addr;
    let madt = unsafe { &*(madt_virt_addr as *const Madt) };

    // チェックサムを検証
    if !madt.header.verify_checksum() {
        info!("MADT checksum verification failed");
        return;
    }

    // packed struct のフィールドはローカル変数にコピー
    let local_apic_addr = madt.local_apic_address;
    let flags = madt.flags;
    let table_length = madt.header.length;

    info!("MADT found:");
    info!("  Local APIC Address: 0x{:08X}", local_apic_addr);
    info!("  Flags: 0x{:08X}", flags);

    // エントリの開始位置と終了位置を計算
    let madt_header_size = core::mem::size_of::<Madt>();
    let entries_start = madt_virt_addr + madt_header_size as u64;
    let entries_end = madt_virt_addr + table_length as u64;

    let mut current_addr = entries_start;
    let mut cpu_count = 0;
    let mut io_apic_count = 0;

    // エントリをイテレート
    while current_addr < entries_end {
        let entry_header = unsafe { &*(current_addr as *const MadtEntryHeader) };

        // packed struct のフィールドはローカル変数にコピー
        let entry_type = entry_header.entry_type;
        let entry_length = entry_header.length;

        match entry_type {
            0 => {
                // Processor Local APIC
                let apic_entry = unsafe { &*(current_addr as *const MadtProcessorLocalApic) };
                let acpi_id = apic_entry.acpi_processor_id;
                let apic_id = apic_entry.apic_id;
                let entry_flags = apic_entry.flags;

                // bit 0 が 1 なら有効なプロセッサ
                if (entry_flags & 1) != 0 {
                    cpu_count += 1;
                    info!(
                        "  CPU #{}: ACPI ID={}, APIC ID={}, Enabled",
                        cpu_count - 1,
                        acpi_id,
                        apic_id
                    );
                }
            }
            1 => {
                // I/O APIC
                let io_apic_entry = unsafe { &*(current_addr as *const MadtIoApic) };
                let io_apic_id = io_apic_entry.io_apic_id;
                let io_apic_address = io_apic_entry.io_apic_address;
                let gsi_base = io_apic_entry.global_system_interrupt_base;

                io_apic_count += 1;
                info!(
                    "  I/O APIC #{}: ID={}, Address=0x{:08X}, GSI Base={}",
                    io_apic_count - 1,
                    io_apic_id,
                    io_apic_address,
                    gsi_base
                );
            }
            _ => {
                // その他のエントリタイプはスキップ
            }
        }

        // 次のエントリへ
        current_addr += entry_length as u64;
    }

    info!(
        "MADT Summary: {} CPU(s), {} I/O APIC(s)",
        cpu_count, io_apic_count
    );
}

/// MCFG (Memory Mapped Configuration) を解析
fn parse_mcfg(mcfg_phys_addr: u64) {
    if mcfg_phys_addr == 0 {
        return;
    }

    // 物理アドレスを高位仮想アドレスに変換
    let mcfg_virt_addr = KERNEL_VIRTUAL_BASE + mcfg_phys_addr;
    let mcfg = unsafe { &*(mcfg_virt_addr as *const Mcfg) };

    // チェックサムを検証
    if !mcfg.header.verify_checksum() {
        info!("MCFG checksum verification failed");
        return;
    }

    // packed struct のフィールドはローカル変数にコピー
    let table_length = mcfg.header.length;

    info!("MCFG found:");

    // エントリの開始位置と終了位置を計算
    let mcfg_header_size = core::mem::size_of::<Mcfg>();
    let entries_start = mcfg_virt_addr + mcfg_header_size as u64;
    let entries_end = mcfg_virt_addr + table_length as u64;

    let entry_size = core::mem::size_of::<McfgEntry>();
    let entry_count = (table_length as usize - mcfg_header_size) / entry_size;

    info!("  Configuration Space Entries: {}", entry_count);

    let mut current_addr = entries_start;
    let mut index = 0;

    while current_addr < entries_end {
        let entry = unsafe { &*(current_addr as *const McfgEntry) };

        // packed struct のフィールドはローカル変数にコピー
        let base_addr = entry.base_address;
        let segment = entry.pci_segment_group;
        let start_bus = entry.start_bus;
        let end_bus = entry.end_bus;

        info!(
            "  [{}] Base: 0x{:016X}, Segment: {}, Buses: {}-{}",
            index, base_addr, segment, start_bus, end_bus
        );

        // PCIモジュールにMMCONFIG情報を通知
        crate::pci::set_mmconfig(base_addr, segment, start_bus, end_bus);

        current_addr += entry_size as u64;
        index += 1;
    }
}

/// HPET (High Precision Event Timer) テーブルを解析
///
/// # Errors
/// * `PagingError` - HPETのMMIOマッピングに失敗した場合
fn parse_hpet(hpet_phys_addr: u64) -> Result<(), crate::paging::PagingError> {
    if hpet_phys_addr == 0 {
        return Ok(());
    }

    // 物理アドレスを高位仮想アドレスに変換
    let hpet_virt_addr = KERNEL_VIRTUAL_BASE + hpet_phys_addr;
    let hpet = unsafe { &*(hpet_virt_addr as *const HpetTable) };

    // チェックサムを検証
    if !hpet.header.verify_checksum() {
        info!("HPET checksum verification failed");
        return Ok(());
    }

    // packed struct のフィールドはローカル変数にコピー
    let base_address = hpet.base_address.address;
    let address_space = hpet.base_address.address_space_id;

    info!("HPET found:");
    info!("  Base Address: 0x{:016X}", base_address);
    info!(
        "  Address Space: {}",
        if address_space == 0 { "Memory" } else { "I/O" }
    );

    // メモリ空間のみサポート
    if address_space != 0 {
        info!("  HPET in I/O space not supported");
        return Ok(());
    }

    // HPETモジュールを初期化
    crate::hpet::init(base_address)?;
    Ok(())
}
