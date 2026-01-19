//! ACPI (Advanced Configuration and Power Interface) サポート
//!
//! ACPI テーブルを読み取り、システム設定情報を取得します。
//! UEFI ブートローダーから RSDP アドレスを受け取り、XSDT/RSDT を解析します。

use crate::info;
use crate::paging::{PagingError, phys_to_virt};
use core::sync::atomic::{AtomicU64, Ordering};
use vitros_common::boot_info::BootInfo;

/// ACPIテーブル長の最大値（100MB）
/// 悪意あるデータや破損データによる範囲外アクセスを防ぐ
const MAX_ACPI_TABLE_LENGTH: u32 = 100 * 1024 * 1024;

/// ACPIテーブル長の最小値（ヘッダサイズ）
const MIN_ACPI_TABLE_LENGTH: usize = core::mem::size_of::<AcpiTableHeader>();

/// ACPIテーブル解析時のエラー型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpiError {
    /// アドレス変換に失敗
    AddressConversionFailed,
    /// チェックサム検証に失敗
    ChecksumFailed,
    /// サポートされていない形式
    NotSupported,
    /// ページング操作に失敗
    PagingError(PagingError),
}

impl From<PagingError> for AcpiError {
    fn from(e: PagingError) -> Self {
        AcpiError::PagingError(e)
    }
}

impl core::fmt::Display for AcpiError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            AcpiError::AddressConversionFailed => write!(f, "Address conversion failed"),
            AcpiError::ChecksumFailed => write!(f, "Checksum verification failed"),
            AcpiError::NotSupported => write!(f, "Not supported"),
            AcpiError::PagingError(e) => write!(f, "Paging error: {}", e),
        }
    }
}

/// MADTから取得したLocal APICの物理アドレス
/// 0の場合はMADT未解析またはアドレス未取得
static LOCAL_APIC_ADDRESS: AtomicU64 = AtomicU64::new(0);

/// MADTから取得したLocal APICアドレスを返す
///
/// ACPIテーブル解析後に呼び出すこと。
/// MADTが見つかっていない場合や解析前はNoneを返す。
pub fn get_local_apic_address() -> Option<u64> {
    let addr = LOCAL_APIC_ADDRESS.load(Ordering::SeqCst);
    if addr == 0 { None } else { Some(addr) }
}

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

/// SDT（System Description Table）エントリの抽象化
///
/// XSDTは64ビット、RSDTは32ビットのエントリを持つため、
/// この差を抽象化してparse処理を共通化する。
trait SdtEntry {
    /// エントリのバイトサイズ
    const ENTRY_SIZE: usize;
    /// 期待されるシグネチャ
    const SIGNATURE: &'static str;

    /// ポインタからアドレスを読み取る
    ///
    /// # Safety
    /// ptrは有効なメモリを指しており、ENTRY_SIZE分のバイトが読み取り可能であること
    unsafe fn read_address(ptr: *const u8) -> u64;
}

/// XSDT用エントリ（64ビットアドレス）
struct Xsdt;

impl SdtEntry for Xsdt {
    const ENTRY_SIZE: usize = 8;
    const SIGNATURE: &'static str = "XSDT";

    unsafe fn read_address(ptr: *const u8) -> u64 {
        // SAFETY: 呼び出し元がptrの有効性を保証する
        unsafe { (ptr as *const u64).read_unaligned() }
    }
}

/// RSDT用エントリ（32ビットアドレス）
struct Rsdt;

impl SdtEntry for Rsdt {
    const ENTRY_SIZE: usize = 4;
    const SIGNATURE: &'static str = "RSDT";

    unsafe fn read_address(ptr: *const u8) -> u64 {
        // SAFETY: 呼び出し元がptrの有効性を保証する
        unsafe { (ptr as *const u32).read_unaligned() as u64 }
    }
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
    ///
    /// # Safety
    /// - selfが有効なACPIテーブルヘッダを指していること
    /// - self.lengthバイトのメモリが読み取り可能であること
    unsafe fn verify_checksum(&self) -> bool {
        let length = self.length as usize;

        // テーブル長の検証（破損データや悪意あるデータからの保護）
        if length < MIN_ACPI_TABLE_LENGTH || self.length > MAX_ACPI_TABLE_LENGTH {
            return false;
        }

        let bytes = unsafe { core::slice::from_raw_parts(self as *const _ as *const u8, length) };

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
///
/// # Returns
/// 成功時は`Ok(())`、失敗時は`Err(AcpiError)`
///
/// # Errors
/// * `AcpiError::AddressConversionFailed` - RSDPアドレスの変換に失敗した場合
/// * `AcpiError::ChecksumFailed` - RSDP/XSDT/RSDTのチェックサム検証に失敗した場合
/// * `AcpiError::NotSupported` - RSDPシグネチャが無効な場合
pub fn init(boot_info: &BootInfo) -> Result<(), AcpiError> {
    info!("Initializing ACPI...");

    if boot_info.rsdp_address == 0 {
        return Err(AcpiError::AddressConversionFailed);
    }

    // RSDP の物理アドレスを高位仮想アドレスに変換
    let rsdp_virt_addr =
        phys_to_virt(boot_info.rsdp_address).map_err(|_| AcpiError::AddressConversionFailed)?;
    // SAFETY: phys_to_virtで変換した有効なアドレス。ACPIテーブルはUEFIが配置し
    // カーネル実行中有効。#[repr(C, packed)]により非アラインアクセスが許可される。
    let rsdp = unsafe { &*(rsdp_virt_addr as *const Rsdp) };

    if !rsdp.is_valid_signature() {
        return Err(AcpiError::NotSupported);
    }

    if !rsdp.verify_checksum() {
        return Err(AcpiError::ChecksumFailed);
    }

    info!("RSDP found at 0x{:016X}", boot_info.rsdp_address);
    info!("  OEM ID: {}", rsdp.oem_id_str());
    info!("  Revision: {}", rsdp.revision);

    if rsdp.revision >= 2 {
        // ACPI 2.0+ - XSDT を使用
        // SAFETY: phys_to_virtで変換した有効なアドレス。revision >= 2 で拡張ヘッダの
        // 存在が保証される。#[repr(C, packed)]により非アラインアクセスが許可される。
        let rsdp_ext = unsafe { &*(rsdp_virt_addr as *const RsdpExtended) };
        // packed struct のフィールドはローカル変数にコピー
        let xsdt_addr = rsdp_ext.xsdt_address;
        info!("  ACPI 2.0+ detected");
        info!("  XSDT Address: 0x{:016X}", xsdt_addr);

        parse_xsdt(xsdt_addr)?;
    } else {
        // ACPI 1.0 - RSDT を使用
        // packed struct のフィールドはローカル変数にコピー
        let rsdt_addr = rsdp.rsdt_address;
        info!("  ACPI 1.0 detected");
        info!("  RSDT Address: 0x{:08X}", rsdt_addr);

        parse_rsdt(rsdt_addr as u64)?;
    }

    Ok(())
}

/// XSDT (Extended System Description Table) を解析
///
/// # Errors
/// * `AcpiError::AddressConversionFailed` - XSDTアドレスの変換に失敗した場合
/// * `AcpiError::ChecksumFailed` - チェックサム検証に失敗した場合
/// * `AcpiError::NotSupported` - シグネチャが無効な場合
fn parse_xsdt(xsdt_phys_addr: u64) -> Result<(), AcpiError> {
    parse_sdt::<Xsdt>(xsdt_phys_addr)
}

/// RSDT (Root System Description Table) を解析
///
/// # Errors
/// * `AcpiError::AddressConversionFailed` - RSDTアドレスの変換に失敗した場合
/// * `AcpiError::ChecksumFailed` - チェックサム検証に失敗した場合
/// * `AcpiError::NotSupported` - シグネチャが無効な場合
fn parse_rsdt(rsdt_phys_addr: u64) -> Result<(), AcpiError> {
    parse_sdt::<Rsdt>(rsdt_phys_addr)
}

/// XSDT/RSDTの共通解析ロジック
///
/// 型パラメータEでエントリサイズを抽象化し、XSDT（64ビット）と
/// RSDT（32ビット）の両方を同じコードで処理する。
///
/// # Errors
/// * `AcpiError::AddressConversionFailed` - SDTアドレスの変換に失敗した場合
/// * `AcpiError::ChecksumFailed` - チェックサム検証に失敗した場合
/// * `AcpiError::NotSupported` - シグネチャが無効な場合
fn parse_sdt<E: SdtEntry>(sdt_phys_addr: u64) -> Result<(), AcpiError> {
    // 物理アドレスを高位仮想アドレスに変換（0チェックも含む）
    let sdt_virt_addr =
        phys_to_virt(sdt_phys_addr).map_err(|_| AcpiError::AddressConversionFailed)?;
    // SAFETY: phys_to_virtで変換した有効なアドレス。ACPIテーブルはUEFIが配置し
    // カーネル実行中有効。#[repr(C, packed)]により非アラインアクセスが許可される。
    let header = unsafe { &*(sdt_virt_addr as *const AcpiTableHeader) };

    if header.signature_str() != E::SIGNATURE {
        info!(
            "Invalid {} signature: {}",
            E::SIGNATURE,
            header.signature_str()
        );
        return Err(AcpiError::NotSupported);
    }

    // SAFETY: headerはphys_to_virtで変換された有効なポインタから参照しており、
    // header.lengthバイトのメモリはACPIテーブルとして読み取り可能
    if !unsafe { header.verify_checksum() } {
        info!("{} checksum verification failed", E::SIGNATURE);
        return Err(AcpiError::ChecksumFailed);
    }

    // テーブルエントリ数を計算
    let header_size = core::mem::size_of::<AcpiTableHeader>();
    let entry_count = (header.length as usize - header_size) / E::ENTRY_SIZE;

    info!(
        "{} parsed successfully. Tables found: {}",
        E::SIGNATURE,
        entry_count
    );

    // エントリのアドレス配列にアクセス
    let entries_base = (sdt_virt_addr + header_size as u64) as *const u8;

    for i in 0..entry_count {
        // packed 構造体の後なのでアンアラインドアクセスが必要
        let entry_ptr = unsafe { entries_base.add(i * E::ENTRY_SIZE) };
        let table_phys_addr = unsafe { E::read_address(entry_ptr) };

        let table_virt_addr = match phys_to_virt(table_phys_addr) {
            Ok(addr) => addr,
            Err(_) => continue,
        };
        // SAFETY: phys_to_virtで変換した有効なアドレス。ACPIテーブルはUEFIが配置し
        // カーネル実行中有効。#[repr(C, packed)]により非アラインアクセスが許可される。
        let table_header = unsafe { &*(table_virt_addr as *const AcpiTableHeader) };

        info!(
            "  [{}] {} at 0x{:016X}",
            i,
            table_header.signature_str(),
            table_phys_addr
        );

        // 各ACPIテーブルを解析（必須ではないのでエラー時はログ出力して継続）
        match table_header.signature_str() {
            "APIC" => {
                if let Err(e) = parse_madt(table_phys_addr) {
                    info!("MADT parsing failed: {:?}, continuing without MADT", e);
                }
            }
            "MCFG" => {
                if let Err(e) = parse_mcfg(table_phys_addr) {
                    info!("MCFG parsing failed: {:?}, continuing without MCFG", e);
                }
            }
            "HPET" => {
                if let Err(e) = parse_hpet(table_phys_addr) {
                    info!(
                        "HPET initialization failed: {:?}, continuing without HPET",
                        e
                    );
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// MADT (Multiple APIC Description Table) を解析
///
/// # Errors
/// * `AcpiError::AddressConversionFailed` - MADTテーブルのアドレス変換に失敗した場合
/// * `AcpiError::ChecksumFailed` - チェックサム検証に失敗した場合
fn parse_madt(madt_phys_addr: u64) -> Result<(), AcpiError> {
    // 物理アドレスを高位仮想アドレスに変換（0チェックも含む）
    let madt_virt_addr =
        phys_to_virt(madt_phys_addr).map_err(|_| AcpiError::AddressConversionFailed)?;
    // SAFETY: phys_to_virtで変換した有効なアドレス。ACPIテーブルはUEFIが配置し
    // カーネル実行中有効。#[repr(C, packed)]により非アラインアクセスが許可される。
    let madt = unsafe { &*(madt_virt_addr as *const Madt) };

    // チェックサムを検証
    // SAFETY: madtはphys_to_virtで変換された有効なポインタから参照しており、
    // header.lengthバイトのメモリはACPIテーブルとして読み取り可能
    if !unsafe { madt.header.verify_checksum() } {
        return Err(AcpiError::ChecksumFailed);
    }

    // packed struct のフィールドはローカル変数にコピー
    let local_apic_addr = madt.local_apic_address;
    let flags = madt.flags;
    let table_length = madt.header.length;

    // Local APICアドレスをグローバル変数に保存
    LOCAL_APIC_ADDRESS.store(local_apic_addr as u64, Ordering::SeqCst);

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
        // SAFETY: current_addrはMADTテーブル内の有効なエントリを指す。
        // ループ条件でentries_end未満を検証済み。#[repr(C, packed)]により非アラインアクセスが許可される。
        let entry_header = unsafe { &*(current_addr as *const MadtEntryHeader) };

        // packed struct のフィールドはローカル変数にコピー
        let entry_type = entry_header.entry_type;
        let entry_length = entry_header.length;

        match entry_type {
            0 => {
                // Processor Local APIC
                // SAFETY: entry_type == 0 でProcessor Local APICエントリであることを確認済み。
                // current_addrはMADTテーブル内の有効なアドレス。#[repr(C, packed)]により非アラインアクセスが許可される。
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
                // SAFETY: entry_type == 1 でI/O APICエントリであることを確認済み。
                // current_addrはMADTテーブル内の有効なアドレス。#[repr(C, packed)]により非アラインアクセスが許可される。
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

    Ok(())
}

/// MCFG (Memory Mapped Configuration) を解析
///
/// # Errors
/// * `AcpiError::AddressConversionFailed` - MCFGテーブルのアドレス変換に失敗した場合
/// * `AcpiError::ChecksumFailed` - チェックサム検証に失敗した場合
fn parse_mcfg(mcfg_phys_addr: u64) -> Result<(), AcpiError> {
    // 物理アドレスを高位仮想アドレスに変換（0チェックも含む）
    let mcfg_virt_addr =
        phys_to_virt(mcfg_phys_addr).map_err(|_| AcpiError::AddressConversionFailed)?;
    // SAFETY: phys_to_virtで変換した有効なアドレス。ACPIテーブルはUEFIが配置し
    // カーネル実行中有効。#[repr(C, packed)]により非アラインアクセスが許可される。
    let mcfg = unsafe { &*(mcfg_virt_addr as *const Mcfg) };

    // チェックサムを検証
    // SAFETY: mcfgはphys_to_virtで変換された有効なポインタから参照しており、
    // header.lengthバイトのメモリはACPIテーブルとして読み取り可能
    if !unsafe { mcfg.header.verify_checksum() } {
        return Err(AcpiError::ChecksumFailed);
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
        // SAFETY: current_addrはMCFGテーブル内の有効なエントリを指す。
        // ループ条件でentries_end未満を検証済み。#[repr(C, packed)]により非アラインアクセスが許可される。
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

    Ok(())
}

/// HPET (High Precision Event Timer) テーブルを解析
///
/// # Errors
/// * `AcpiError::AddressConversionFailed` - HPETテーブルのアドレス変換に失敗した場合
/// * `AcpiError::ChecksumFailed` - チェックサム検証に失敗した場合
/// * `AcpiError::NotSupported` - HPETがI/O空間にある場合（未サポート）
/// * `AcpiError::PagingError` - HPETのMMIOマッピングに失敗した場合
fn parse_hpet(hpet_phys_addr: u64) -> Result<(), AcpiError> {
    // 物理アドレスを高位仮想アドレスに変換（0チェックも含む）
    let hpet_virt_addr =
        phys_to_virt(hpet_phys_addr).map_err(|_| AcpiError::AddressConversionFailed)?;
    // SAFETY: phys_to_virtで変換した有効なアドレス。ACPIテーブルはUEFIが配置し
    // カーネル実行中有効。#[repr(C, packed)]により非アラインアクセスが許可される。
    let hpet = unsafe { &*(hpet_virt_addr as *const HpetTable) };

    // チェックサムを検証
    // SAFETY: hpetはphys_to_virtで変換された有効なポインタから参照しており、
    // header.lengthバイトのメモリはACPIテーブルとして読み取り可能
    if !unsafe { hpet.header.verify_checksum() } {
        return Err(AcpiError::ChecksumFailed);
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
        return Err(AcpiError::NotSupported);
    }

    // HPETモジュールを初期化
    crate::hpet::init(base_address)?;
    Ok(())
}
