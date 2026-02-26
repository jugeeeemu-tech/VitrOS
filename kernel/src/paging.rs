//! x86_64 ページングシステム実装
//! 4段階のページテーブル（PML4, PDP, PD, PT）を管理
//! ハイヤーハーフカーネル（高位アドレス空間へのマッピング）をサポート

use core::arch::asm;
use core::ptr::{addr_of, addr_of_mut};

/// ハイヤーハーフカーネルのベースアドレス（上位カノニカルアドレス空間）
/// x86_64のカノニカルアドレス空間の上位半分の開始位置
pub const KERNEL_VIRTUAL_BASE: u64 = 0xFFFF_8000_0000_0000;

// リンカスクリプトで定義されたセクション境界シンボル
unsafe extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __data_end: u8;
    static __bss_start: u8;
    static __bss_end: u8;
}

/// ページング操作のエラー型
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagingError {
    /// 無効なアドレス（null、アライメント不正など）
    InvalidAddress,
    /// アドレス変換に失敗
    AddressConversionFailed,
    /// Guard Page設定に失敗
    GuardPageSetupFailed,
    /// ページテーブル初期化に失敗
    PageTableInitFailed,
    /// アドレスがサポート範囲外
    AddressOutOfRange,
    /// CPU機能がサポートされていない
    FeatureNotSupported,
    /// 既存のマッピングと競合（PT/PD参照が既に存在）
    ExistingMappingConflict,
    /// ページテーブル用フレームの確保に失敗
    FrameAllocationFailed,
}

impl core::fmt::Display for PagingError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            PagingError::InvalidAddress => write!(f, "Invalid address"),
            PagingError::AddressConversionFailed => write!(f, "Address conversion failed"),
            PagingError::GuardPageSetupFailed => write!(f, "Guard page setup failed"),
            PagingError::PageTableInitFailed => write!(f, "Page table initialization failed"),
            PagingError::AddressOutOfRange => write!(f, "Address out of supported range"),
            PagingError::FeatureNotSupported => write!(f, "CPU feature not supported"),
            PagingError::ExistingMappingConflict => {
                write!(f, "Existing page table mapping conflict")
            }
            PagingError::FrameAllocationFailed => write!(f, "Page-table frame allocation failed"),
        }
    }
}

/// ページテーブルエントリ数（512エントリ）
const PAGE_TABLE_ENTRY_COUNT: usize = 512;

/// ページサイズ（4KB）
pub const PAGE_SIZE: usize = 4096;

/// 2MBヒュージページサイズ
pub const HUGE_PAGE_SIZE_2MB: usize = 2 * 1024 * 1024;

/// 1GBヒュージページサイズ
pub const HUGE_PAGE_SIZE_1GB: usize = 1024 * 1024 * 1024;

/// 2MBヒュージページのオフセットマスク（下位21ビット）
const HUGE_PAGE_2MB_OFFSET_MASK: u64 = 0x1F_FFFF;

/// 1GBヒュージページのオフセットマスク（下位30ビット）
const HUGE_PAGE_1GB_OFFSET_MASK: u64 = 0x3FFF_FFFF;

/// ページオフセットマスク（下位12ビット）
const PAGE_OFFSET_MASK: u64 = 0xFFF;

/// ページテーブルエントリから物理アドレスを抽出するためのマスク
/// ビット12〜51が物理アドレス（4KB境界アライメント、最大52ビット物理アドレス対応）
const PHYSICAL_ADDRESS_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// アドレスが2MB境界にアライメントされているかチェック
#[inline]
pub fn is_2mb_aligned(addr: u64) -> bool {
    addr & HUGE_PAGE_2MB_OFFSET_MASK == 0
}

/// アドレスが1GB境界にアライメントされているかチェック
#[inline]
pub fn is_1gb_aligned(addr: u64) -> bool {
    addr & HUGE_PAGE_1GB_OFFSET_MASK == 0
}

/// 物理アドレスを仮想アドレスに変換
///
/// # Arguments
/// * `phys_addr` - 物理アドレス
///
/// # Returns
/// 変換された仮想アドレス、またはエラー
///
/// # Errors
/// * `PagingError::InvalidAddress` - 物理アドレスが0（null）の場合
pub fn phys_to_virt(phys_addr: u64) -> Result<u64, PagingError> {
    if phys_addr == 0 {
        return Err(PagingError::InvalidAddress);
    }
    Ok(phys_addr + KERNEL_VIRTUAL_BASE)
}

/// 仮想アドレスを物理アドレスに変換
///
/// # Arguments
/// * `virt_addr` - 仮想アドレス（KERNEL_VIRTUAL_BASE以上であること）
///
/// # Returns
/// 変換された物理アドレス、またはエラー
///
/// # Errors
/// * `PagingError::InvalidAddress` - 仮想アドレスがKERNEL_VIRTUAL_BASE未満の場合
/// * `PagingError::AddressConversionFailed` - アンダーフローが発生した場合
pub fn virt_to_phys(virt_addr: u64) -> Result<u64, PagingError> {
    if virt_addr < KERNEL_VIRTUAL_BASE {
        return Err(PagingError::InvalidAddress);
    }
    virt_addr
        .checked_sub(KERNEL_VIRTUAL_BASE)
        .ok_or(PagingError::AddressConversionFailed)
}

/// ページテーブルエントリのフラグ
#[allow(dead_code)]
#[repr(u64)]
pub enum PageTableFlags {
    Present = 1 << 0,        // エントリが有効
    Writable = 1 << 1,       // 書き込み可能
    UserAccessible = 1 << 2, // ユーザーモードからアクセス可能
    WriteThrough = 1 << 3,   // ライトスルーキャッシング
    CacheDisable = 1 << 4,   // キャッシュ無効
    Accessed = 1 << 5,       // アクセスされた
    Dirty = 1 << 6,          // 書き込まれた（PTのみ）
    HugePage = 1 << 7,       // 2MB/1GBページ
    Global = 1 << 8,         // グローバルページ
    NoExecute = 1 << 63,     // 実行禁止
}

/// ページテーブルエントリ
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct PageTableEntry {
    entry: u64,
}

impl PageTableEntry {
    /// 新しい空のエントリを作成
    pub const fn new() -> Self {
        Self { entry: 0 }
    }

    /// エントリが有効かどうか
    #[allow(dead_code)]
    pub fn is_present(&self) -> bool {
        (self.entry & PageTableFlags::Present as u64) != 0
    }

    /// フラグを設定
    #[allow(dead_code)]
    pub fn set_flags(&mut self, flags: u64) {
        self.entry |= flags;
    }

    /// 物理アドレスを設定（12ビットシフト済みの値）
    #[allow(dead_code)]
    pub fn set_address(&mut self, addr: u64) {
        // 下位12ビットをクリア（4KBアライメント）
        let addr_masked = addr & PHYSICAL_ADDRESS_MASK;
        // フラグをクリアして新しいアドレスを設定
        self.entry = (self.entry & PAGE_OFFSET_MASK) | addr_masked;
    }

    /// エントリを完全に設定（アドレス + フラグ）
    pub fn set(&mut self, addr: u64, flags: u64) {
        // 既存のエントリを完全にクリアしてから設定
        let addr_masked = addr & PHYSICAL_ADDRESS_MASK;
        self.entry = addr_masked | flags;
    }

    /// 物理アドレスを取得
    #[allow(dead_code)]
    pub fn get_address(&self) -> u64 {
        self.entry & PHYSICAL_ADDRESS_MASK
    }

    /// エントリの生の値を取得（デバッグ用）
    pub fn get_raw(&self) -> u64 {
        self.entry
    }

    /// エントリがHugePageフラグを持っているかどうか
    pub fn is_huge_page(&self) -> bool {
        (self.entry & PageTableFlags::HugePage as u64) != 0
    }
}

/// ページテーブル（PML4, PDP, PD, PTすべてに共通の構造）
#[derive(Clone, Copy)]
#[repr(align(4096))]
pub struct PageTable {
    entries: [PageTableEntry; PAGE_TABLE_ENTRY_COUNT],
}

impl PageTable {
    /// 新しい空のページテーブルを作成
    pub const fn new() -> Self {
        Self {
            entries: [PageTableEntry::new(); PAGE_TABLE_ENTRY_COUNT],
        }
    }

    /// 指定インデックスのエントリを取得
    pub fn entry(&mut self, index: usize) -> &mut PageTableEntry {
        &mut self.entries[index]
    }

    /// テーブルの物理アドレスを取得
    /// カーネルは高位アドレスで動作しているため、KERNEL_VIRTUAL_BASEを引いて物理アドレスに変換
    ///
    /// # Errors
    /// * `PagingError::InvalidAddress` - 仮想アドレスがKERNEL_VIRTUAL_BASE未満の場合
    /// * `PagingError::AddressConversionFailed` - アドレス変換に失敗した場合
    pub fn physical_address(&self) -> Result<u64, PagingError> {
        let virt_addr = self as *const _ as u64;
        virt_to_phys(virt_addr)
    }

    /// 全エントリをクリア
    pub fn clear(&mut self) {
        for entry in &mut self.entries {
            entry.entry = 0;
        }
    }

    /// テーブル内に有効なエントリが存在しないか確認
    pub fn is_empty(&self) -> bool {
        self.entries.iter().all(|entry| !entry.is_present())
    }

    /// 指定インデックスのエントリを読み取り専用で取得（デバッグビルド用）
    #[cfg(debug_assertions)]
    pub fn get_entry(&self, index: usize) -> &PageTableEntry {
        &self.entries[index]
    }
}

/// CR3レジスタを読み取る
pub fn read_cr3() -> u64 {
    let value: u64;
    unsafe {
        asm!("mov {}, cr3", out(reg) value, options(nomem, nostack));
    }
    value
}

/// CR3レジスタに値を書き込む（ページテーブルベースアドレスを設定）
pub fn write_cr3(pml4_addr: u64) {
    unsafe {
        asm!("mov cr3, {}", in(reg) pml4_addr, options(nostack));
    }
}

/// CR3レジスタをリロード（TLBフラッシュ）
///
/// 現在のCPUのTLBのみをフラッシュする。
///
/// # TODO: マルチコア対応
/// マルチコア環境では他CPUへのIPIによるTLB shootdownが必要。
pub fn reload_cr3() {
    let cr3 = read_cr3();
    write_cr3(cr3);
}

/// 直写対象の物理範囲
#[derive(Clone, Copy)]
struct DirectMapRange {
    start: u64,
    end: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectMapLeafSize {
    Page4Kb,
    Huge2Mb,
    Huge1Gb,
}

#[inline]
fn is_direct_map_ram_type(region_type: u32) -> bool {
    use vitros_common::uefi::{
        EFI_ACPI_RECLAIM_MEMORY, EFI_BOOT_SERVICES_CODE, EFI_BOOT_SERVICES_DATA,
        EFI_CONVENTIONAL_MEMORY, EFI_LOADER_CODE, EFI_LOADER_DATA,
    };
    matches!(
        region_type,
        EFI_CONVENTIONAL_MEMORY
            | EFI_LOADER_CODE
            | EFI_LOADER_DATA
            | EFI_BOOT_SERVICES_CODE
            | EFI_BOOT_SERVICES_DATA
            | EFI_ACPI_RECLAIM_MEMORY
    )
}

#[inline]
fn ranges_overlap(start: u64, end: u64, other_start: u64, other_end: u64) -> bool {
    start < other_end && end > other_start
}

#[inline]
fn select_direct_map_leaf_size(
    phys_addr: u64,
    range_end: u64,
    kernel_start: u64,
    kernel_end: u64,
    use_1gb_pages: bool,
) -> DirectMapLeafSize {
    if use_1gb_pages && is_1gb_aligned(phys_addr) {
        if let Some(end) = phys_addr.checked_add(HUGE_PAGE_SIZE_1GB as u64) {
            if end <= range_end && !ranges_overlap(phys_addr, end, kernel_start, kernel_end) {
                return DirectMapLeafSize::Huge1Gb;
            }
        }
    }

    if is_2mb_aligned(phys_addr) {
        if let Some(end) = phys_addr.checked_add(HUGE_PAGE_SIZE_2MB as u64) {
            if end <= range_end && !ranges_overlap(phys_addr, end, kernel_start, kernel_end) {
                return DirectMapLeafSize::Huge2Mb;
            }
        }
    }

    DirectMapLeafSize::Page4Kb
}

#[inline]
fn next_direct_map_progress_threshold(current: u64, total: u64) -> u64 {
    if current < DIRECT_MAP_PROGRESS_CHUNK_BYTES {
        core::cmp::min(DIRECT_MAP_PROGRESS_CHUNK_BYTES, total)
    } else {
        core::cmp::min(current.saturating_add(DIRECT_MAP_PROGRESS_CHUNK_BYTES), total)
    }
}

/// メモリマップから直写対象範囲（System RAM）を抽出して正規化する
///
/// # Arguments
/// * `memory_regions` - UEFIメモリマップのスライス
///
/// # Returns
/// 正規化済み範囲配列、範囲数、総ページ数、最大終端アドレス（exclusive）
fn extract_direct_map_ranges(
    memory_regions: &[vitros_common::boot_info::MemoryRegion],
) -> Result<
    (
        [DirectMapRange; vitros_common::boot_info::MAX_MEMORY_REGIONS],
        usize,
        u64,
        u64,
    ),
    PagingError,
> {
    let mut ranges = [DirectMapRange { start: 0, end: 0 };
        vitros_common::boot_info::MAX_MEMORY_REGIONS];
    let mut count = 0;

    // direct-map対象のSystem RAMを抽出し、4KB境界へ丸める
    for region in memory_regions {
        if !is_direct_map_ram_type(region.region_type) || region.size == 0 {
            continue;
        }

        let region_end = region
            .start
            .checked_add(region.size)
            .ok_or(PagingError::AddressConversionFailed)?;
        let start_aligned = region.start & !(PAGE_SIZE as u64 - 1);
        let end_aligned = region_end
            .checked_add(PAGE_SIZE as u64 - 1)
            .ok_or(PagingError::AddressConversionFailed)?
            & !(PAGE_SIZE as u64 - 1);

        if start_aligned >= end_aligned {
            continue;
        }
        if count >= ranges.len() {
            break;
        }

        ranges[count] = DirectMapRange {
            start: start_aligned,
            end: end_aligned,
        };
        count += 1;
    }

    if count == 0 {
        return Ok((ranges, 0, 0, 0));
    }

    // 開始アドレスでソート（挿入ソート: 小規模配列向け）
    for i in 1..count {
        let key = ranges[i];
        let mut j = i;
        while j > 0 && ranges[j - 1].start > key.start {
            ranges[j] = ranges[j - 1];
            j -= 1;
        }
        ranges[j] = key;
    }

    // 隣接/重複範囲をマージ
    let mut write_idx = 0usize;
    for i in 1..count {
        let curr = ranges[i];
        let write = &mut ranges[write_idx];
        if curr.start <= write.end {
            write.end = write.end.max(curr.end);
        } else {
            write_idx += 1;
            ranges[write_idx] = curr;
        }
    }

    let merged_count = write_idx + 1;
    let mut total_pages = 0u64;
    let mut max_end = 0u64;
    for range in ranges.iter().take(merged_count) {
        total_pages = total_pages
            .checked_add((range.end - range.start) / PAGE_SIZE as u64)
            .ok_or(PagingError::AddressConversionFailed)?;
        max_end = max_end.max(range.end);
    }

    Ok((ranges, merged_count, total_pages, max_end))
}

// グローバルページテーブルを静的に確保
// 物理メモリの直接マッピング（Direct Mapping）を実装

/// 直写フェーズ開始直後の進捗ログ出力間隔（最初の1本）
const DIRECT_MAP_EARLY_PROGRESS_BYTES: u64 = 1 * 1024 * 1024;
/// 直写フェーズの通常進捗ログ出力間隔
const DIRECT_MAP_PROGRESS_CHUNK_BYTES: u64 = 8 * 1024 * 1024;

static mut KERNEL_PML4: PageTable = PageTable::new();

/// ページングシステムを初期化してCR3に設定
/// 物理メモリの直接マッピング（Direct Mapping）を実装
/// - 低位アドレス（0x0〜）: アンマップ（ハイヤーハーフカーネル）
/// - 高位アドレス（0xFFFF_8000_0000_0000+）: カーネル用の直接マッピング
///
/// UEFIメモリマップに基づいて、実際に利用可能なメモリ範囲のみをマッピングする。
///
/// # Arguments
/// * `boot_info` - ブートローダから渡されたメモリ情報
///
/// # Errors
/// * `PagingError::AddressConversionFailed` - アドレス変換に失敗した場合
/// * `PagingError::GuardPageSetupFailed` - Guard Page設定に失敗した場合
pub fn init(boot_info: &vitros_common::boot_info::BootInfo) -> Result<(), PagingError> {
    use crate::info;

    unsafe {
        let pml4 = addr_of_mut!(KERNEL_PML4);
        (*pml4).clear();

        let direct_4kb_flags = PageTableFlags::Present as u64 | PageTableFlags::Writable as u64;
        let direct_huge_flags = direct_4kb_flags | PageTableFlags::HugePage as u64;
        let mut skipped_kernel_pages = 0u64;

        let kernel_text_start = virt_to_phys(addr_of!(__text_start) as u64)?;
        let kernel_bss_end = virt_to_phys(addr_of!(__bss_end) as u64)?;
        let kernel_image_size = kernel_bss_end
            .checked_sub(kernel_text_start)
            .ok_or(PagingError::PageTableInitFailed)?;
        crate::frame_allocator::reserve_range(kernel_text_start, kernel_image_size)
            .map_err(|_| PagingError::FrameAllocationFailed)?;
        info!(
            "Paging: reserved kernel image frames phys=0x{:X}-0x{:X}",
            kernel_text_start, kernel_bss_end
        );
        if let Some(stack_guard_virt) = crate::stack::guard_page_address() {
            let stack_top_virt = crate::stack::stack_top();
            let stack_guard_phys = virt_to_phys(stack_guard_virt)?;
            let stack_top_phys = virt_to_phys(stack_top_virt)?;
            let stack_size = stack_top_phys
                .checked_sub(stack_guard_phys)
                .ok_or(PagingError::PageTableInitFailed)?;
            crate::frame_allocator::reserve_range(stack_guard_phys, stack_size)
                .map_err(|_| PagingError::FrameAllocationFailed)?;
            info!(
                "Paging: reserved kernel stack frames phys=0x{:X}-0x{:X}",
                stack_guard_phys, stack_top_phys
            );
        }

        let memory_region_count = boot_info.memory_map_count.min(boot_info.memory_map.len());
        let memory_regions = &boot_info.memory_map[..memory_region_count];
        let (direct_map_ranges, direct_map_range_count, total_pages, _) =
            extract_direct_map_ranges(memory_regions)?;

        let use_1gb_pages = supports_1gb_pages();

        let total_bytes = total_pages
            .checked_mul(PAGE_SIZE as u64)
            .ok_or(PagingError::AddressConversionFailed)?;

        info!(
            "Paging: mapping {} pages from {} RAM ranges ({} MB)",
            total_pages,
            direct_map_range_count,
            total_bytes >> 20
        );
        info!(
            "Paging: direct-map policy {} + kernel W^X (kernel 4KB only)",
            if use_1gb_pages {
                "1GB -> 2MB -> 4KB"
            } else {
                "2MB -> 4KB (CPU 1GB page unsupported)"
            }
        );

        let mut scanned_pages = 0u64;
        let mut mapped_pages = 0u64;
        let mut mapped_1gb_leaves = 0u64;
        let mut mapped_2mb_leaves = 0u64;
        let mut mapped_4kb_leaves = 0u64;
        let mut next_progress_bytes = core::cmp::min(DIRECT_MAP_EARLY_PROGRESS_BYTES, total_bytes);
        let mut last_logged_progress_bytes = 0u64;
        info!(
            "Paging: direct-map pass start ({} pages, ranges={}, first_chunk={} MB, chunk={} MB)",
            total_pages,
            direct_map_range_count,
            DIRECT_MAP_EARLY_PROGRESS_BYTES >> 20,
            DIRECT_MAP_PROGRESS_CHUNK_BYTES >> 20
        );

        let mut current_pt: *mut PageTable = core::ptr::null_mut();
        for range in direct_map_ranges.iter().take(direct_map_range_count) {
            let mut current_path: Option<(usize, usize, usize)> = None;
            let mut physical_addr = range.start;
            while physical_addr < range.end {
                let leaf_size = select_direct_map_leaf_size(
                    physical_addr,
                    range.end,
                    kernel_text_start,
                    kernel_bss_end,
                    use_1gb_pages,
                );

                let step_bytes = match leaf_size {
                    DirectMapLeafSize::Huge1Gb => {
                        map_1gb_page(pml4, physical_addr, direct_huge_flags)?;
                        mapped_1gb_leaves += 1;
                        mapped_pages += (HUGE_PAGE_SIZE_1GB / PAGE_SIZE) as u64;
                        current_path = None;
                        HUGE_PAGE_SIZE_1GB as u64
                    }
                    DirectMapLeafSize::Huge2Mb => {
                        map_2mb_page(pml4, physical_addr, direct_huge_flags)?;
                        mapped_2mb_leaves += 1;
                        mapped_pages += (HUGE_PAGE_SIZE_2MB / PAGE_SIZE) as u64;
                        current_path = None;
                        HUGE_PAGE_SIZE_2MB as u64
                    }
                    DirectMapLeafSize::Page4Kb => {
                        // カーネル領域はセクション毎のマッピング用に予約（4KBマッピングをスキップ）
                        if physical_addr >= kernel_text_start && physical_addr < kernel_bss_end {
                            skipped_kernel_pages += 1;
                        } else {
                            let (pml4_idx, pdp_idx, pd_idx, pt_idx) =
                                direct_map_table_indices(physical_addr)?;
                            let key = (pml4_idx, pdp_idx, pd_idx);
                            if current_path != Some(key) {
                                let pdp = ensure_pdp(pml4, pml4_idx)?;
                                let pd = ensure_pd(pdp, pdp_idx)?;
                                current_pt = ensure_pt(pd, pd_idx)?;
                                current_path = Some(key);
                            }
                            (*current_pt).entry(pt_idx).set(physical_addr, direct_4kb_flags);
                            mapped_pages += 1;
                            mapped_4kb_leaves += 1;
                        }

                        PAGE_SIZE as u64
                    }
                };

                scanned_pages += step_bytes / PAGE_SIZE as u64;
                let scanned_bytes = scanned_pages * PAGE_SIZE as u64;

                while scanned_bytes >= next_progress_bytes && next_progress_bytes < total_bytes {
                    let percent = if total_bytes == 0 {
                        100
                    } else {
                        next_progress_bytes.saturating_mul(100) / total_bytes
                    };
                    info!(
                        "Paging: direct-map progress {}/{} MB ({}%, scanned_pages={}, mapped_pages={}, skipped_kernel={})",
                        next_progress_bytes >> 20,
                        total_bytes >> 20,
                        percent,
                        scanned_pages,
                        mapped_pages,
                        skipped_kernel_pages
                    );
                    last_logged_progress_bytes = next_progress_bytes;
                    next_progress_bytes = next_direct_map_progress_threshold(next_progress_bytes, total_bytes);
                }

                if scanned_pages == total_pages && scanned_bytes > last_logged_progress_bytes {
                    let percent = if total_bytes == 0 {
                        100
                    } else {
                        scanned_bytes.saturating_mul(100) / total_bytes
                    };
                    info!(
                        "Paging: direct-map progress {}/{} MB ({}%, scanned_pages={}, mapped_pages={}, skipped_kernel={})",
                        scanned_bytes >> 20,
                        total_bytes >> 20,
                        percent,
                        scanned_pages,
                        mapped_pages,
                        skipped_kernel_pages
                    );
                    last_logged_progress_bytes = scanned_bytes;
                }

                physical_addr = physical_addr
                    .checked_add(step_bytes)
                    .ok_or(PagingError::AddressConversionFailed)?;
            }
        }

        info!(
            "Paging: direct-map pass complete (scanned_pages={}, mapped_pages={}, leaves[1GB/2MB/4KB]={}/{}/{}, skipped_kernel={})",
            scanned_pages,
            mapped_pages,
            mapped_1gb_leaves,
            mapped_2mb_leaves,
            mapped_4kb_leaves,
            skipped_kernel_pages
        );

        if skipped_kernel_pages > 0 {
            info!(
                "Skipped {} pages for kernel section mapping",
                skipped_kernel_pages
            );
        }

        #[cfg(not(test))]
        if let Some(guard_page_virt_addr) = crate::stack::guard_page_address() {
            let guard_page_phys_addr = virt_to_phys(guard_page_virt_addr)?;
            if clear_4kb_mapping_entry(pml4, guard_page_phys_addr).is_err() {
                return Err(PagingError::GuardPageSetupFailed);
            }

            // デバッグ: Guard Page設定を確認（リリースビルドでは省略）
            #[cfg(debug_assertions)]
            {
                let (pml4_idx, pdp_idx, pd_idx, pt_idx) =
                    direct_map_table_indices(guard_page_phys_addr)?;
                let pdp = walk_table(pml4, pml4_idx)?;
                let pd = walk_table(pdp, pdp_idx)?;
                let pt = walk_table(pd, pd_idx)?;
                info!("Guard Page setup:");
                info!("  Virtual address: 0x{:016X}", guard_page_virt_addr);
                info!("  Physical offset: 0x{:X}", guard_page_phys_addr);
                info!(
                    "  PML4/PDP/PD/PT index: {}/{}/{}/{}",
                    pml4_idx, pdp_idx, pd_idx, pt_idx
                );
                info!("  Entry value: 0x{:016X}", (*pt).entry(pt_idx).get_raw());
                info!(
                    "  Entry is Present: {}",
                    (*pt).entry(pt_idx).get_raw() & 1 != 0
                );
            }
        }

        {
            let text_start = virt_to_phys(addr_of!(__text_start) as u64)?;
            let text_end = virt_to_phys(addr_of!(__text_end) as u64)?;
            let rodata_start = virt_to_phys(addr_of!(__rodata_start) as u64)?;
            let rodata_end = virt_to_phys(addr_of!(__rodata_end) as u64)?;
            let data_start = virt_to_phys(addr_of!(__data_start) as u64)?;
            let bss_end = virt_to_phys(addr_of!(__bss_end) as u64)?;

            let text_flags = PageTableFlags::Present as u64;
            for phys in (text_start..text_end).step_by(PAGE_SIZE) {
                map_4kb_page(pml4, phys, text_flags)?;
            }

            let rodata_flags = PageTableFlags::Present as u64 | PageTableFlags::NoExecute as u64;
            for phys in (rodata_start..rodata_end).step_by(PAGE_SIZE) {
                map_4kb_page(pml4, phys, rodata_flags)?;
            }

            let data_flags = PageTableFlags::Present as u64
                | PageTableFlags::Writable as u64
                | PageTableFlags::NoExecute as u64;
            for phys in (data_start..bss_end).step_by(PAGE_SIZE) {
                map_4kb_page(pml4, phys, data_flags)?;
            }

            info!(
                "Kernel sections mapped with W^X: .text=0x{:X}-0x{:X}, .rodata=0x{:X}-0x{:X}, .data/.bss=0x{:X}-0x{:X}",
                text_start, text_end, rodata_start, rodata_end, data_start, bss_end
            );
        }

        let pml4_addr = (*pml4).physical_address()?;
        write_cr3(pml4_addr);

        Ok(())
    }
}

// =============================================================================
// MMIO マッピング関連
// =============================================================================

/// RFLAGS の IF (Interrupt Flag) ビット（ビット9）
const RFLAGS_IF: u64 = 1 << 9;

/// 割り込みが無効であることを確認
///
/// スレッドセーフティのため、ページテーブル操作は割り込み無効状態で
/// 行われることを検証する。
fn assert_interrupts_disabled(context: &str) {
    let rflags: u64;
    // SAFETY: RFLAGSレジスタをスタックにプッシュしてから読み取る標準的な方法。
    // この操作はメモリ安全性に影響しない。
    unsafe {
        asm!("pushfq; pop {}", out(reg) rflags, options(nomem, preserves_flags));
    }
    assert!(
        (rflags & RFLAGS_IF) == 0,
        "{}: must be called with interrupts disabled",
        context
    );
}

#[inline]
fn direct_map_table_indices(phys_addr: u64) -> Result<(usize, usize, usize, usize), PagingError> {
    let virt_addr = KERNEL_VIRTUAL_BASE
        .checked_add(phys_addr)
        .ok_or(PagingError::AddressConversionFailed)?;
    let pml4_idx = ((virt_addr >> 39) & 0x1FF) as usize;
    let pdp_idx = ((virt_addr >> 30) & 0x1FF) as usize;
    let pd_idx = ((virt_addr >> 21) & 0x1FF) as usize;
    let pt_idx = ((virt_addr >> 12) & 0x1FF) as usize;
    Ok((pml4_idx, pdp_idx, pd_idx, pt_idx))
}

#[inline]
fn table_link_flags() -> u64 {
    PageTableFlags::Present as u64 | PageTableFlags::Writable as u64
}

unsafe fn table_from_entry(entry: &PageTableEntry) -> Result<*mut PageTable, PagingError> {
    if !entry.is_present() {
        return Err(PagingError::PageTableInitFailed);
    }
    if entry.is_huge_page() {
        return Err(PagingError::ExistingMappingConflict);
    }
    let table_phys = entry.get_address();
    let table_virt = phys_to_virt(table_phys)?;
    Ok(table_virt as *mut PageTable)
}

#[cfg_attr(test, allow(dead_code))]
unsafe fn walk_table(
    parent: *mut PageTable,
    entry_idx: usize,
) -> Result<*mut PageTable, PagingError> {
    unsafe { table_from_entry((*parent).entry(entry_idx)) }
}

unsafe fn walk_table_if_present(
    parent: *mut PageTable,
    entry_idx: usize,
) -> Result<Option<*mut PageTable>, PagingError> {
    unsafe {
        let entry = (*parent).entry(entry_idx);
        if !entry.is_present() {
            return Ok(None);
        }
        table_from_entry(entry).map(Some)
    }
}

unsafe fn alloc_page_table_frame() -> Result<*mut PageTable, PagingError> {
    let frame_phys = crate::frame_allocator::alloc_frame()
        .map_err(|_| PagingError::FrameAllocationFailed)?;
    let frame_virt = match phys_to_virt(frame_phys) {
        Ok(addr) => addr,
        Err(_) => {
            let _ = crate::frame_allocator::free_frame(frame_phys);
            return Err(PagingError::AddressConversionFailed);
        }
    };
    let frame_ptr = frame_virt as *mut PageTable;

    unsafe {
        (*frame_ptr).clear();
    }

    Ok(frame_ptr)
}

unsafe fn ensure_child_table(
    parent: *mut PageTable,
    entry_idx: usize,
) -> Result<*mut PageTable, PagingError> {
    unsafe {
        let entry = (*parent).entry(entry_idx);
        if entry.is_present() {
            return table_from_entry(entry);
        }

        let table = alloc_page_table_frame()?;
        let table_phys = virt_to_phys(table as u64)?;
        entry.set(table_phys, table_link_flags());
        Ok(table)
    }
}

unsafe fn ensure_pdp(pml4: *mut PageTable, pml4_idx: usize) -> Result<*mut PageTable, PagingError> {
    unsafe { ensure_child_table(pml4, pml4_idx) }
}

unsafe fn ensure_pd(pdp: *mut PageTable, pdp_idx: usize) -> Result<*mut PageTable, PagingError> {
    unsafe { ensure_child_table(pdp, pdp_idx) }
}

unsafe fn ensure_pt(pd: *mut PageTable, pd_idx: usize) -> Result<*mut PageTable, PagingError> {
    unsafe { ensure_child_table(pd, pd_idx) }
}

unsafe fn map_4kb_page(
    pml4: *mut PageTable,
    phys_addr: u64,
    flags: u64,
) -> Result<(), PagingError> {
    unsafe {
        let (pml4_idx, pdp_idx, pd_idx, pt_idx) = direct_map_table_indices(phys_addr)?;
        let pdp = ensure_pdp(pml4, pml4_idx)?;
        let pd = ensure_pd(pdp, pdp_idx)?;
        let pt = ensure_pt(pd, pd_idx)?;
        (*pt).entry(pt_idx).set(phys_addr, flags);
        Ok(())
    }
}

unsafe fn map_2mb_page(
    pml4: *mut PageTable,
    phys_addr: u64,
    flags: u64,
) -> Result<(), PagingError> {
    if !is_2mb_aligned(phys_addr) {
        return Err(PagingError::InvalidAddress);
    }

    unsafe {
        let (pml4_idx, pdp_idx, pd_idx, _) = direct_map_table_indices(phys_addr)?;
        let pdp = ensure_pdp(pml4, pml4_idx)?;
        let pd = ensure_pd(pdp, pdp_idx)?;
        let entry = (*pd).entry(pd_idx);
        if entry.is_present() && !entry.is_huge_page() {
            return Err(PagingError::ExistingMappingConflict);
        }
        entry.set(phys_addr, flags);
        Ok(())
    }
}

unsafe fn map_1gb_page(
    pml4: *mut PageTable,
    phys_addr: u64,
    flags: u64,
) -> Result<(), PagingError> {
    if !is_1gb_aligned(phys_addr) {
        return Err(PagingError::InvalidAddress);
    }

    unsafe {
        let (pml4_idx, pdp_idx, _, _) = direct_map_table_indices(phys_addr)?;
        let pdp = ensure_pdp(pml4, pml4_idx)?;
        let entry = (*pdp).entry(pdp_idx);
        if entry.is_present() && !entry.is_huge_page() {
            return Err(PagingError::ExistingMappingConflict);
        }
        entry.set(phys_addr, flags);
        Ok(())
    }
}

#[cfg_attr(test, allow(dead_code))]
unsafe fn clear_4kb_mapping_entry(pml4: *mut PageTable, phys_addr: u64) -> Result<(), PagingError> {
    unsafe {
        let (pml4_idx, pdp_idx, pd_idx, pt_idx) = direct_map_table_indices(phys_addr)?;
        let pdp = walk_table(pml4, pml4_idx)?;
        let pd = walk_table(pdp, pdp_idx)?;
        let pt = walk_table(pd, pd_idx)?;
        (*pt).entry(pt_idx).set(phys_addr, 0);
        Ok(())
    }
}

unsafe fn free_table_frame(entry: &mut PageTableEntry) -> Result<(), PagingError> {
    if !entry.is_present() || entry.is_huge_page() {
        return Ok(());
    }

    let table_phys = entry.get_address();
    entry.set(0, 0);
    crate::frame_allocator::free_frame(table_phys).map_err(|_| PagingError::FrameAllocationFailed)
}

unsafe fn prune_empty_tables(
    pml4: *mut PageTable,
    pml4_idx: usize,
    pdp_idx: usize,
) -> Result<(), PagingError> {
    unsafe {
        let pdp_entry = (*pml4).entry(pml4_idx);
        if !pdp_entry.is_present() || pdp_entry.is_huge_page() {
            return Ok(());
        }

        let pdp = table_from_entry(pdp_entry)?;
        let pd_entry = (*pdp).entry(pdp_idx);
        if pd_entry.is_present() && !pd_entry.is_huge_page() {
            let pd = table_from_entry(pd_entry)?;
            if (*pd).is_empty() {
                free_table_frame(pd_entry)?;
            }
        }

        if (*pdp).is_empty() {
            free_table_frame(pdp_entry)?;
        }

        Ok(())
    }
}

/// MMIO領域をUC（Uncacheable）属性でマッピングする
///
/// init()でスキップされたMMIO領域を、デバイス使用前に動的にマッピングする。
/// キャッシュ無効（UC）属性でマッピングされるため、MMIOレジスタへのアクセスが
/// 正しく行われることが保証される。
///
/// # Safety Preconditions
/// * この関数はシングルコア環境または割り込み無効状態で呼び出すこと
/// * カーネル初期化段階（BSP上でAPが起動する前）での使用を想定
/// * 同じアドレスに対して複数回呼び出された場合、既存のマッピングを上書きする
///
/// # TODO: マルチコア対応
/// マルチコア環境ではspinlockまたはmutexによる排他制御が必要。
/// 現在はカーネル初期化段階でのみ使用されるため未実装。
///
/// # Arguments
/// * `phys_addr` - マッピングする物理アドレス（4KB境界にアライメントされている必要がある）
/// * `size` - マッピングするサイズ（バイト単位、4KB単位に切り上げられる）
///
/// # Returns
/// マッピングされた仮想アドレス、またはエラー
///
/// # Errors
/// * `PagingError::InvalidAddress` - アドレスが4KB境界にアライメントされていない場合
/// * `PagingError::AddressOutOfRange` - 直接マップ可能範囲外の物理アドレスの場合
/// * `PagingError::ExistingMappingConflict` - 既存HugePageマッピングと衝突した場合
/// * `PagingError::FrameAllocationFailed` - ページテーブル用フレーム確保に失敗した場合
pub fn map_mmio(phys_addr: u64, size: u64) -> Result<u64, PagingError> {
    use crate::info;

    // 割り込みが無効であることを確認
    assert_interrupts_disabled("map_mmio");

    // 4KB境界アライメントチェック
    if phys_addr & PAGE_OFFSET_MASK != 0 {
        return Err(PagingError::InvalidAddress);
    }

    // 必要なページ数を計算（切り上げ）
    let page_count = ((size + PAGE_SIZE as u64 - 1) / PAGE_SIZE as u64) as usize;

    // UC属性フラグ: Present | Writable | CacheDisable
    let uc_flags = PageTableFlags::Present as u64
        | PageTableFlags::Writable as u64
        | PageTableFlags::CacheDisable as u64;

    unsafe {
        let pml4 = addr_of_mut!(KERNEL_PML4);

        for i in 0..page_count {
            let addr = phys_addr + (i * PAGE_SIZE) as u64;
            let (pml4_idx, pdp_idx, pd_idx, pt_idx) = direct_map_table_indices(addr)?;
            let pdp = ensure_pdp(pml4, pml4_idx)?;
            let pd = ensure_pd(pdp, pdp_idx)?;
            let pt = ensure_pt(pd, pd_idx)?;

            // デバッグビルド時のみ重複マッピングを警告
            #[cfg(debug_assertions)]
            {
                if (*pt).get_entry(pt_idx).is_present() {
                    info!(
                        "Warning: map_mmio overwriting existing mapping at 0x{:X}",
                        addr
                    );
                }
            }

            // UC属性でページテーブルエントリを設定
            (*pt).entry(pt_idx).set(addr, uc_flags);
        }

        // TLBフラッシュ
        reload_cr3();
    }

    // 仮想アドレスを計算して返す
    let virt_addr = phys_to_virt(phys_addr)?;

    info!(
        "MMIO mapped: phys=0x{:X} -> virt=0x{:X} ({} pages, UC)",
        phys_addr, virt_addr, page_count
    );

    Ok(virt_addr)
}

// =============================================================================
// 2MB ヒュージページ マッピング関連
// =============================================================================

/// 2MB範囲の既存4KBマッピングをクリアする
///
/// ヒュージページをマッピングする前に、対象範囲の4KBページエントリを
/// クリア（Present=0）にする。これにより、PDエントリをヒュージページに
/// 変更しても、元のPTエントリが不整合な状態にならない。
///
/// # Arguments
/// * `phys_addr` - クリアする2MB範囲の開始物理アドレス（2MB境界）
///
/// # Errors
/// * `PagingError::InvalidAddress` - アドレスが2MB境界にアライメントされていない場合
/// * `PagingError::AddressOutOfRange` - アドレスがサポート範囲外の場合
fn clear_4kb_mappings_for_huge_page(phys_addr: u64) -> Result<(), PagingError> {
    use crate::info;

    // 2MB境界アライメントチェック
    if !is_2mb_aligned(phys_addr) {
        return Err(PagingError::InvalidAddress);
    }

    let (pml4_idx, pdp_idx, pd_idx, _) = direct_map_table_indices(phys_addr)?;

    unsafe {
        let pml4 = addr_of_mut!(KERNEL_PML4);
        let Some(pdp) = walk_table_if_present(pml4, pml4_idx)? else {
            return Ok(());
        };
        let Some(pd) = walk_table_if_present(pdp, pdp_idx)? else {
            return Ok(());
        };

        let pd_entry = (*pd).entry(pd_idx);
        if !pd_entry.is_present() || pd_entry.is_huge_page() {
            return Ok(());
        }

        let pt_phys = pd_entry.get_address();
        pd_entry.set(0, 0);
        crate::frame_allocator::free_frame(pt_phys)
            .map_err(|_| PagingError::FrameAllocationFailed)?;
        prune_empty_tables(pml4, pml4_idx, pdp_idx)?;
    }

    info!(
        "Cleared 4KB mappings for huge page at 0x{:X} (PML4/PDP/PD={}/{}/{})",
        phys_addr, pml4_idx, pdp_idx, pd_idx
    );

    Ok(())
}

/// 2MBヒュージページをマッピングする
///
/// PDレベルでHugePageフラグを設定し、2MBの連続した物理メモリを
/// 単一のページテーブルエントリでマッピングする。
///
/// # Safety Preconditions
/// * この関数はシングルコア環境または割り込み無効状態で呼び出すこと
/// * 同じ仮想アドレス範囲に4KBページが既にマッピングされている場合、
///   そのPTエントリは無効化されないため、先にunmapする必要がある
///
/// # TODO: マルチコア対応
/// マルチコア環境ではspinlockまたはmutexによる排他制御が必要。
///
/// # Arguments
/// * `phys_addr` - マッピングする物理アドレス（2MB境界にアライメントされている必要がある）
/// * `additional_flags` - 追加のページフラグ。以下のフラグが有効:
///   - `PageTableFlags::CacheDisable` - MMIO領域用のキャッシュ無効化
///   - `PageTableFlags::WriteThrough` - ライトスルーキャッシュ
///   - `PageTableFlags::NoExecute` - 実行禁止
///
///   注: Present, Writable, HugePageは内部で自動設定されるため指定不要
///
/// # Returns
/// マッピングされた仮想アドレス、またはエラー
///
/// # Errors
/// * `PagingError::InvalidAddress` - アドレスが2MB境界にアライメントされていない場合
/// * `PagingError::AddressOutOfRange` - ページテーブルのインデックスが範囲外の場合
/// * `PagingError::ExistingMappingConflict` - 既存のPT参照が存在する場合
pub fn map_huge_2mb(phys_addr: u64, additional_flags: u64) -> Result<u64, PagingError> {
    use crate::info;

    // 割り込みが無効であることを確認
    assert_interrupts_disabled("map_huge_2mb");

    // 2MB境界アライメントチェック
    if !is_2mb_aligned(phys_addr) {
        return Err(PagingError::InvalidAddress);
    }

    let (pml4_idx, pdp_idx, pd_idx, _) = direct_map_table_indices(phys_addr)?;

    // HugePageフラグ: Present | Writable | HugePage + 追加フラグ
    let huge_flags = PageTableFlags::Present as u64
        | PageTableFlags::Writable as u64
        | PageTableFlags::HugePage as u64
        | additional_flags;

    unsafe {
        let pml4 = addr_of_mut!(KERNEL_PML4);
        let pdp = ensure_pdp(pml4, pml4_idx)?;
        let pd = ensure_pd(pdp, pdp_idx)?;

        // 既存のマッピング競合チェック
        // エントリがPresent=1かつHugePage=0の場合、PT参照が設定されている
        let existing_entry = (*pd).entry(pd_idx);
        if existing_entry.is_present() && !existing_entry.is_huge_page() {
            return Err(PagingError::ExistingMappingConflict);
        }

        // PDエントリにHugePageフラグ付きで物理アドレスを設定
        existing_entry.set(phys_addr, huge_flags);

        // TLBフラッシュ
        reload_cr3();
    }

    // 仮想アドレスを計算して返す
    let virt_addr = phys_to_virt(phys_addr)?;

    info!(
        "Huge 2MB page mapped: phys=0x{:X} -> virt=0x{:X}",
        phys_addr, virt_addr
    );

    Ok(virt_addr)
}

/// 2MBヒュージページのマッピングを解除する
///
/// PDレベルのエントリをクリアし、マッピングを解除する。
///
/// # Safety Preconditions
/// * この関数はシングルコア環境または割り込み無効状態で呼び出すこと
///
/// # TODO: マルチコア対応
/// マルチコア環境ではspinlockまたはmutexによる排他制御が必要。
///
/// # Arguments
/// * `phys_addr` - マッピング解除する物理アドレス（2MB境界にアライメントされている必要がある）
///
/// # Errors
/// * `PagingError::InvalidAddress` - アドレスが2MB境界にアライメントされていない場合
/// * `PagingError::AddressOutOfRange` - ページテーブルのインデックスが範囲外の場合
#[allow(dead_code)]
pub fn unmap_huge_2mb(phys_addr: u64) -> Result<(), PagingError> {
    use crate::info;

    // 割り込みが無効であることを確認
    assert_interrupts_disabled("unmap_huge_2mb");

    // 2MB境界アライメントチェック
    if !is_2mb_aligned(phys_addr) {
        return Err(PagingError::InvalidAddress);
    }

    let (pml4_idx, pdp_idx, pd_idx, _) = direct_map_table_indices(phys_addr)?;

    unsafe {
        let pml4 = addr_of_mut!(KERNEL_PML4);
        let Some(pdp) = walk_table_if_present(pml4, pml4_idx)? else {
            return Ok(());
        };
        let Some(pd) = walk_table_if_present(pdp, pdp_idx)? else {
            return Ok(());
        };

        // PDエントリをクリア（Present=0）
        (*pd).entry(pd_idx).set(0, 0);
        prune_empty_tables(pml4, pml4_idx, pdp_idx)?;

        // TLBフラッシュ
        reload_cr3();
    }

    info!("Huge 2MB page unmapped: phys=0x{:X}", phys_addr);

    Ok(())
}

// =============================================================================
// 1GB ヒュージページ マッピング関連
// =============================================================================

/// 1GBページサポートのキャッシュ (0xFF=未チェック, 0=非対応, 1=対応)
static SUPPORTS_1GB_PAGES_CACHE: core::sync::atomic::AtomicU8 =
    core::sync::atomic::AtomicU8::new(0xFF);

/// 1GBヒュージページがサポートされているか確認
///
/// CPUID.80000001H:EDX\[bit 26\] (Page1GB) で確認
/// 結果はキャッシュされ、2回目以降はキャッシュから返す
fn supports_1gb_pages() -> bool {
    use core::sync::atomic::Ordering;

    match SUPPORTS_1GB_PAGES_CACHE.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let result = check_1gb_pages_support_cpuid();
            SUPPORTS_1GB_PAGES_CACHE.store(result as u8, Ordering::Relaxed);
            result
        }
    }
}

/// CPUIDで1GBページサポートを確認（内部実装）
fn check_1gb_pages_support_cpuid() -> bool {
    // まず拡張CPUID機能の最大値を確認
    let max_extended: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "pop rbx",
            inout("eax") 0x80000000u32 => max_extended,
            out("ecx") _,
            lateout("edx") _,
            options(nomem, nostack),
        );
    }

    // 拡張CPUID 0x80000001 がサポートされていない場合は1GBページ非対応
    if max_extended < 0x80000001 {
        return false;
    }

    // 拡張CPUID 0x80000001 でPage1GBフラグを確認
    let edx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "pop rbx",
            inout("eax") 0x80000001u32 => _,
            out("ecx") _,
            lateout("edx") edx,
            options(nomem, nostack),
        );
    }
    (edx & (1 << 26)) != 0
}

/// 1GBヒュージページをマッピングする
///
/// PDPレベルでHugePageフラグを設定し、1GBの連続した物理メモリを
/// 単一のページテーブルエントリでマッピングする。
///
/// # Safety Preconditions
/// * この関数はシングルコア環境または割り込み無効状態で呼び出すこと
/// * 同じ仮想アドレス範囲に2MB/4KBページが既にマッピングされている場合、
///   そのPD/PTエントリは無効化されないため、先にunmapする必要がある
///
/// # TODO: マルチコア対応
/// マルチコア環境ではspinlockまたはmutexによる排他制御が必要。
///
/// # Arguments
/// * `phys_addr` - マッピングする物理アドレス（1GB境界にアライメントされている必要がある）
/// * `additional_flags` - 追加のページフラグ。以下のフラグが有効:
///   - `PageTableFlags::CacheDisable` - MMIO領域用のキャッシュ無効化
///   - `PageTableFlags::WriteThrough` - ライトスルーキャッシュ
///   - `PageTableFlags::NoExecute` - 実行禁止
///
///   注: Present, Writable, HugePageは内部で自動設定されるため指定不要
///
/// # Returns
/// マッピングされた仮想アドレス、またはエラー
///
/// # Errors
/// * `PagingError::InvalidAddress` - アドレスが1GB境界にアライメントされていない場合
/// * `PagingError::AddressOutOfRange` - ページテーブルのインデックスが範囲外の場合
/// * `PagingError::FeatureNotSupported` - CPUが1GBヒュージページをサポートしていない場合
/// * `PagingError::ExistingMappingConflict` - 既存のPD参照が存在する場合
#[allow(dead_code)]
pub fn map_huge_1gb(phys_addr: u64, additional_flags: u64) -> Result<u64, PagingError> {
    use crate::info;

    // 割り込みが無効であることを確認
    assert_interrupts_disabled("map_huge_1gb");

    // 1GB境界アライメントチェック
    if !is_1gb_aligned(phys_addr) {
        return Err(PagingError::InvalidAddress);
    }

    // 1GBヒュージページのCPUサポートチェック
    if !supports_1gb_pages() {
        return Err(PagingError::FeatureNotSupported);
    }

    let (pml4_idx, pdp_idx, _, _) = direct_map_table_indices(phys_addr)?;

    // HugePageフラグ: Present | Writable | HugePage + 追加フラグ
    let huge_flags = PageTableFlags::Present as u64
        | PageTableFlags::Writable as u64
        | PageTableFlags::HugePage as u64
        | additional_flags;

    unsafe {
        let pml4 = addr_of_mut!(KERNEL_PML4);
        let pdp = ensure_pdp(pml4, pml4_idx)?;

        // 既存のマッピング競合チェック
        // エントリがPresent=1かつHugePage=0の場合、PD参照が設定されている
        let existing_entry = (*pdp).entry(pdp_idx);
        if existing_entry.is_present() && !existing_entry.is_huge_page() {
            return Err(PagingError::ExistingMappingConflict);
        }

        // PDPエントリにHugePageフラグ付きで物理アドレスを設定
        existing_entry.set(phys_addr, huge_flags);

        // TLBフラッシュ
        reload_cr3();
    }

    // 仮想アドレスを計算して返す
    let virt_addr = phys_to_virt(phys_addr)?;

    info!(
        "Huge 1GB page mapped: phys=0x{:X} -> virt=0x{:X}",
        phys_addr, virt_addr
    );

    Ok(virt_addr)
}

/// 1GBヒュージページのマッピングを解除する
///
/// PDPレベルのエントリをクリアし、マッピングを解除する。
///
/// # Safety Preconditions
/// * この関数はシングルコア環境または割り込み無効状態で呼び出すこと
///
/// # TODO: マルチコア対応
/// マルチコア環境ではspinlockまたはmutexによる排他制御が必要。
///
/// # Arguments
/// * `phys_addr` - マッピング解除する物理アドレス（1GB境界にアライメントされている必要がある）
///
/// # Errors
/// * `PagingError::InvalidAddress` - アドレスが1GB境界にアライメントされていない場合
/// * `PagingError::AddressOutOfRange` - ページテーブルのインデックスが範囲外の場合
#[allow(dead_code)]
pub fn unmap_huge_1gb(phys_addr: u64) -> Result<(), PagingError> {
    use crate::info;

    // 割り込みが無効であることを確認
    assert_interrupts_disabled("unmap_huge_1gb");

    // 1GB境界アライメントチェック
    if !is_1gb_aligned(phys_addr) {
        return Err(PagingError::InvalidAddress);
    }

    let (pml4_idx, pdp_idx, _, _) = direct_map_table_indices(phys_addr)?;

    unsafe {
        let pml4 = addr_of_mut!(KERNEL_PML4);
        let Some(pdp) = walk_table_if_present(pml4, pml4_idx)? else {
            return Ok(());
        };

        // PDPエントリをクリア（Present=0）
        (*pdp).entry(pdp_idx).set(0, 0);
        prune_empty_tables(pml4, pml4_idx, pdp_idx)?;

        // TLBフラッシュ
        reload_cr3();
    }

    info!("Huge 1GB page unmapped: phys=0x{:X}", phys_addr);

    Ok(())
}

// =============================================================================
// フレームバッファ ヒュージページ設定
// =============================================================================

/// フレームバッファを可能であれば2MBヒュージページでマッピングする
///
/// フレームバッファが2MB境界にアライメントされている場合、ヒュージページで
/// マッピングしてTLB効率を向上させる。非アラインの場合は通常の4KBページ
/// （既存のマッピング）を使用する。
///
/// # Safety Preconditions
/// * この関数はinit()の後、割り込み無効状態で呼び出すこと
///
/// # Arguments
/// * `fb_base` - フレームバッファの物理ベースアドレス
/// * `fb_size` - フレームバッファのサイズ（バイト単位）
///
/// # Returns
/// マッピングされた仮想アドレス、またはエラー
///
/// # Errors
/// * `PagingError::InvalidAddress` - アドレス変換に失敗した場合
/// * `PagingError::PageTableInitFailed` - ページテーブル設定に失敗した場合
pub fn map_framebuffer_huge(fb_base: u64, fb_size: u64) -> Result<u64, PagingError> {
    use crate::info;

    // 割り込みが無効であることを確認
    assert_interrupts_disabled("map_framebuffer_huge");

    // 2MB境界アライメントチェック
    if !is_2mb_aligned(fb_base) {
        info!(
            "Framebuffer not 2MB aligned (0x{:X}), using 4KB pages",
            fb_base
        );
        // 非アラインの場合はinit()でマッピング済みの4KBページを使用
        return phys_to_virt(fb_base);
    }

    // 必要な2MBページ数を計算
    let huge_page_count =
        ((fb_size + HUGE_PAGE_SIZE_2MB as u64 - 1) / HUGE_PAGE_SIZE_2MB as u64) as usize;

    info!(
        "Mapping framebuffer with {} huge 2MB pages: phys=0x{:X}, size={}",
        huge_page_count, fb_base, fb_size
    );

    // 各2MBページをマッピング（MMIO領域のためCacheDisableフラグを設定）
    // 既存の4KBマッピングがある場合は先にクリアする
    for i in 0..huge_page_count {
        let page_addr = fb_base + (i as u64 * HUGE_PAGE_SIZE_2MB as u64);
        // 既存の4KBマッピングをクリアしてからヒュージページをマッピング
        clear_4kb_mappings_for_huge_page(page_addr)?;
        map_huge_2mb(
            page_addr,
            PageTableFlags::CacheDisable as u64 | PageTableFlags::NoExecute as u64,
        )?;
    }

    info!("Framebuffer huge page mapping complete");
    phys_to_virt(fb_base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vitros_common::boot_info::{BootInfo, MemoryRegion};
    use vitros_common::uefi::{
        EFI_ACPI_RECLAIM_MEMORY, EFI_BOOT_SERVICES_DATA, EFI_CONVENTIONAL_MEMORY,
        EFI_LOADER_CODE, EFI_MEMORY_MAPPED_IO, EFI_MEMORY_MAPPED_IO_PORT_SPACE,
        EFI_RUNTIME_SERVICES_DATA,
    };

    const TEST_TABLE_ALLOC_REGION_SIZE: u64 = 0x80_0000; // 8 MiB

    fn setup_table_allocator_for_test() {
        crate::frame_allocator::reset_for_test();

        let mut boot_info = BootInfo::new();
        boot_info.memory_map[0] = MemoryRegion {
            start: 0x1000,
            size: TEST_TABLE_ALLOC_REGION_SIZE,
            region_type: EFI_CONVENTIONAL_MEMORY,
        };
        boot_info.memory_map_count = 1;
        boot_info.max_physical_address = TEST_TABLE_ALLOC_REGION_SIZE;

        crate::frame_allocator::init(&boot_info).expect("frame allocator init failed");

        unsafe {
            let pml4 = addr_of_mut!(KERNEL_PML4);
            (*pml4).clear();
        }
    }

    #[test_case]
    fn test_extract_direct_map_ranges_filters_system_ram_and_aligns() {
        let regions = [
            MemoryRegion {
                start: 0x1003,
                size: 0x1FFD,
                region_type: EFI_CONVENTIONAL_MEMORY,
            },
            MemoryRegion {
                start: 0x3000,
                size: 0x1000,
                region_type: EFI_LOADER_CODE,
            },
            MemoryRegion {
                start: 0x5000,
                size: 0x1000,
                region_type: EFI_BOOT_SERVICES_DATA,
            },
            MemoryRegion {
                start: 0x7000,
                size: 0x1000,
                region_type: EFI_ACPI_RECLAIM_MEMORY,
            },
            MemoryRegion {
                start: 0x8000,
                size: 0x1000,
                region_type: EFI_RUNTIME_SERVICES_DATA,
            },
            MemoryRegion {
                start: 0x9000,
                size: 0x2000,
                region_type: EFI_MEMORY_MAPPED_IO,
            },
            MemoryRegion {
                start: 0xB000,
                size: 0x1000,
                region_type: EFI_MEMORY_MAPPED_IO_PORT_SPACE,
            },
        ];

        let (ranges, count, total_pages, max_end) =
            extract_direct_map_ranges(&regions).expect("extract direct-map ranges failed");

        assert_eq!(count, 3);
        assert_eq!(ranges[0].start, 0x1000);
        assert_eq!(ranges[0].end, 0x4000);
        assert_eq!(ranges[1].start, 0x5000);
        assert_eq!(ranges[1].end, 0x6000);
        assert_eq!(ranges[2].start, 0x7000);
        assert_eq!(ranges[2].end, 0x8000);
        assert_eq!(total_pages, 5);
        assert_eq!(max_end, 0x8000);
    }

    #[test_case]
    fn test_extract_direct_map_ranges_merges_overlap_and_adjacent() {
        let regions = [
            MemoryRegion {
                start: 0x1000,
                size: 0x1000,
                region_type: EFI_CONVENTIONAL_MEMORY,
            },
            MemoryRegion {
                start: 0x1800,
                size: 0x2000,
                region_type: EFI_CONVENTIONAL_MEMORY,
            },
            MemoryRegion {
                start: 0x4000,
                size: 0x1000,
                region_type: EFI_CONVENTIONAL_MEMORY,
            },
        ];

        let (ranges, count, total_pages, max_end) =
            extract_direct_map_ranges(&regions).expect("extract direct-map ranges failed");

        assert_eq!(count, 1);
        assert_eq!(ranges[0].start, 0x1000);
        assert_eq!(ranges[0].end, 0x5000);
        assert_eq!(total_pages, 4);
        assert_eq!(max_end, 0x5000);
    }

    #[test_case]
    fn test_extract_direct_map_ranges_sorted_and_empty() {
        let regions = [
            MemoryRegion {
                start: 0x9000,
                size: 0x1000,
                region_type: EFI_CONVENTIONAL_MEMORY,
            },
            MemoryRegion {
                start: 0x2000,
                size: 0x1000,
                region_type: EFI_ACPI_RECLAIM_MEMORY,
            },
        ];

        let (ranges, count, total_pages, max_end) =
            extract_direct_map_ranges(&regions).expect("extract direct-map ranges failed");
        assert_eq!(count, 2);
        assert_eq!(ranges[0].start, 0x2000);
        assert_eq!(ranges[0].end, 0x3000);
        assert_eq!(ranges[1].start, 0x9000);
        assert_eq!(ranges[1].end, 0xA000);
        assert_eq!(total_pages, 2);
        assert_eq!(max_end, 0xA000);

        let empty_regions: [MemoryRegion; 0] = [];
        let (_, empty_count, empty_pages, empty_max_end) =
            extract_direct_map_ranges(&empty_regions).expect("empty extraction failed");
        assert_eq!(empty_count, 0);
        assert_eq!(empty_pages, 0);
        assert_eq!(empty_max_end, 0);
    }

    #[test_case]
    fn test_select_direct_map_leaf_size_prefers_larger_pages() {
        let kernel_start = 0x1000_0000;
        let kernel_end = 0x1008_0000;

        let one_gb_aligned = 0x8000_0000;
        let range_end = one_gb_aligned + HUGE_PAGE_SIZE_1GB as u64;
        assert_eq!(
            select_direct_map_leaf_size(one_gb_aligned, range_end, kernel_start, kernel_end, true),
            DirectMapLeafSize::Huge1Gb
        );
        assert_eq!(
            select_direct_map_leaf_size(
                one_gb_aligned,
                range_end,
                kernel_start,
                kernel_end,
                false
            ),
            DirectMapLeafSize::Huge2Mb
        );

        assert_eq!(
            select_direct_map_leaf_size(0x8123_4000, range_end, kernel_start, kernel_end, true),
            DirectMapLeafSize::Page4Kb
        );
    }

    #[test_case]
    fn test_select_direct_map_leaf_size_avoids_kernel_overlap() {
        let one_gb_aligned = 0x4000_0000;
        let range_end = one_gb_aligned + HUGE_PAGE_SIZE_1GB as u64;
        let kernel_start = one_gb_aligned + 0x2000_0000;
        let kernel_end = kernel_start + PAGE_SIZE as u64;

        // 1GBチャンク全体はカーネルと重なるため、2MBへフォールバックする
        assert_eq!(
            select_direct_map_leaf_size(one_gb_aligned, range_end, kernel_start, kernel_end, true),
            DirectMapLeafSize::Huge2Mb
        );

        // カーネルを含む2MBチャンクは4KBへフォールバックする
        assert_eq!(
            select_direct_map_leaf_size(kernel_start & !((HUGE_PAGE_SIZE_2MB as u64) - 1), range_end, kernel_start, kernel_end, true),
            DirectMapLeafSize::Page4Kb
        );
    }

    #[test_case]
    fn test_is_2mb_aligned() {
        // 2MB = 0x20_0000
        assert!(is_2mb_aligned(0));
        assert!(is_2mb_aligned(0x20_0000));
        assert!(is_2mb_aligned(0x40_0000));
        assert!(!is_2mb_aligned(0x1));
        assert!(!is_2mb_aligned(0x1000)); // 4KB
        assert!(!is_2mb_aligned(0x20_0001));
    }

    #[test_case]
    fn test_is_1gb_aligned() {
        // 1GB = 0x4000_0000
        assert!(is_1gb_aligned(0));
        assert!(is_1gb_aligned(0x4000_0000));
        assert!(is_1gb_aligned(0x8000_0000));
        assert!(!is_1gb_aligned(0x1));
        assert!(!is_1gb_aligned(0x20_0000)); // 2MB
        assert!(!is_1gb_aligned(0x4000_0001));
    }

    #[test_case]
    fn test_direct_map_indices_cross_512gb_boundary() {
        let boundary = 512u64 * HUGE_PAGE_SIZE_1GB as u64;
        let just_before = boundary - PAGE_SIZE as u64;

        let (pml4_before, pdp_before, pd_before, pt_before) =
            direct_map_table_indices(just_before).expect("index before boundary");
        let (pml4_after, pdp_after, pd_after, pt_after) =
            direct_map_table_indices(boundary).expect("index at boundary");

        assert_eq!(pml4_before, 256);
        assert_eq!(pdp_before, 511);
        assert_eq!(pd_before, 511);
        assert_eq!(pt_before, 511);

        assert_eq!(pml4_after, 257);
        assert_eq!(pdp_after, 0);
        assert_eq!(pd_after, 0);
        assert_eq!(pt_after, 0);
    }

    #[test_case]
    fn test_ensure_tables_and_prune_empty_chain() {
        setup_table_allocator_for_test();

        let phys_addr = 512u64 * HUGE_PAGE_SIZE_1GB as u64; // PML4[257] を使う
        unsafe {
            let pml4 = addr_of_mut!(KERNEL_PML4);
            let map_flags = PageTableFlags::Present as u64 | PageTableFlags::Writable as u64;
            map_4kb_page(pml4, phys_addr, map_flags).expect("map_4kb_page failed");

            let (pml4_idx, pdp_idx, pd_idx, pt_idx) =
                direct_map_table_indices(phys_addr).expect("indices failed");
            assert_eq!(pml4_idx, 257);

            let pdp = walk_table(pml4, pml4_idx).expect("walk pdp failed");
            let pd = walk_table(pdp, pdp_idx).expect("walk pd failed");
            let pt = walk_table(pd, pd_idx).expect("walk pt failed");
            assert!((*pt).entry(pt_idx).is_present());

            clear_4kb_mappings_for_huge_page(phys_addr).expect("clear 4kb mappings failed");
            assert!(
                !(*pml4).entry(pml4_idx).is_present(),
                "PML4 entry should be pruned when chain is empty"
            );
        }
    }

    #[test_case]
    fn test_map_huge_2mb_replace_and_restore_4kb() {
        setup_table_allocator_for_test();

        let phys_addr = (512u64 * HUGE_PAGE_SIZE_1GB as u64) + HUGE_PAGE_SIZE_2MB as u64;
        unsafe {
            let pml4 = addr_of_mut!(KERNEL_PML4);
            let map_flags = PageTableFlags::Present as u64 | PageTableFlags::Writable as u64;
            map_4kb_page(pml4, phys_addr, map_flags).expect("initial 4kb map failed");
        }

        crate::io::without_interrupts(|| {
            assert_eq!(
                map_huge_2mb(phys_addr, 0),
                Err(PagingError::ExistingMappingConflict)
            );
        });

        clear_4kb_mappings_for_huge_page(phys_addr).expect("clear 4kb mapping failed");

        crate::io::without_interrupts(|| {
            map_huge_2mb(phys_addr, PageTableFlags::NoExecute as u64).expect("map_huge_2mb failed");
        });

        unsafe {
            let pml4 = addr_of_mut!(KERNEL_PML4);
            let (pml4_idx, pdp_idx, pd_idx, _) =
                direct_map_table_indices(phys_addr).expect("indices failed");
            let pdp = walk_table(pml4, pml4_idx).expect("walk pdp failed");
            let pd = walk_table(pdp, pdp_idx).expect("walk pd failed");
            let pd_entry = (*pd).entry(pd_idx);
            assert!(pd_entry.is_present());
            assert!(pd_entry.is_huge_page());
        }

        crate::io::without_interrupts(|| {
            unmap_huge_2mb(phys_addr).expect("unmap_huge_2mb failed");
        });

        unsafe {
            let pml4 = addr_of_mut!(KERNEL_PML4);
            let (pml4_idx, _, _, _) = direct_map_table_indices(phys_addr).expect("indices failed");
            assert!(
                !(*pml4).entry(pml4_idx).is_present(),
                "Huge-page unmap should prune empty upper tables"
            );

            let map_flags = PageTableFlags::Present as u64 | PageTableFlags::Writable as u64;
            map_4kb_page(pml4, phys_addr, map_flags).expect("restore 4kb map failed");

            let (pml4_idx, pdp_idx, pd_idx, pt_idx) =
                direct_map_table_indices(phys_addr).expect("indices failed");
            let pdp = walk_table(pml4, pml4_idx).expect("walk pdp failed");
            let pd = walk_table(pdp, pdp_idx).expect("walk pd failed");
            let pt = walk_table(pd, pd_idx).expect("walk pt failed");
            let pt_entry = (*pt).entry(pt_idx);
            assert!(pt_entry.is_present());
            assert!(!pt_entry.is_huge_page());
        }
    }

    #[test_case]
    fn test_map_mmio_high_bar_crosses_512gb_boundary() {
        setup_table_allocator_for_test();

        let phys_addr = (512u64 * HUGE_PAGE_SIZE_1GB as u64) + 0x20_0000; // PML4[257] 側

        crate::io::without_interrupts(|| {
            let virt_addr = map_mmio(phys_addr, PAGE_SIZE as u64).expect("map_mmio failed");
            assert_eq!(virt_addr, KERNEL_VIRTUAL_BASE + phys_addr);
        });

        unsafe {
            let pml4 = addr_of_mut!(KERNEL_PML4);
            let (pml4_idx, pdp_idx, pd_idx, pt_idx) =
                direct_map_table_indices(phys_addr).expect("indices failed");
            assert!(pml4_idx >= 257);

            let pdp = walk_table(pml4, pml4_idx).expect("walk pdp failed");
            let pd = walk_table(pdp, pdp_idx).expect("walk pd failed");
            let pt = walk_table(pd, pd_idx).expect("walk pt failed");

            let entry = (*pt).entry(pt_idx);
            let uc_flags = PageTableFlags::Present as u64
                | PageTableFlags::Writable as u64
                | PageTableFlags::CacheDisable as u64;
            assert!(entry.is_present());
            assert_eq!(entry.get_address(), phys_addr);
            assert_eq!(entry.get_raw() & uc_flags, uc_flags);
        }
    }

    #[test_case]
    fn test_phys_to_virt_valid() {
        // 有効な物理アドレス
        let result = phys_to_virt(0x1000);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), KERNEL_VIRTUAL_BASE + 0x1000);
    }

    #[test_case]
    fn test_phys_to_virt_null() {
        // nullアドレスはエラー
        let result = phys_to_virt(0);
        assert_eq!(result, Err(PagingError::InvalidAddress));
    }

    #[test_case]
    fn test_virt_to_phys_valid() {
        // 有効な仮想アドレス（KERNEL_VIRTUAL_BASE以上）
        let virt_addr = KERNEL_VIRTUAL_BASE + 0x1000;
        let result = virt_to_phys(virt_addr);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0x1000);
    }

    #[test_case]
    fn test_virt_to_phys_invalid() {
        // KERNEL_VIRTUAL_BASE未満はエラー
        let result = virt_to_phys(0x1000);
        assert_eq!(result, Err(PagingError::InvalidAddress));
    }
}
