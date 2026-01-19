//! x86_64 ページングシステム実装
//! 4段階のページテーブル（PML4, PDP, PD, PT）を管理
//! ハイヤーハーフカーネル（高位アドレス空間へのマッピング）をサポート

use core::arch::asm;
use core::ptr::addr_of_mut;

/// ハイヤーハーフカーネルのベースアドレス（上位カノニカルアドレス空間）
/// x86_64のカノニカルアドレス空間の上位半分の開始位置
pub const KERNEL_VIRTUAL_BASE: u64 = 0xFFFF_8000_0000_0000;

/// ページング操作のエラー型
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagingError {
    /// 無効なアドレス（null、範囲外など）
    InvalidAddress,
    /// アドレス変換に失敗
    AddressConversionFailed,
    /// Guard Page設定に失敗
    GuardPageSetupFailed,
    /// ページテーブル初期化に失敗
    PageTableInitFailed,
}

impl core::fmt::Display for PagingError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            PagingError::InvalidAddress => write!(f, "Invalid address"),
            PagingError::AddressConversionFailed => write!(f, "Address conversion failed"),
            PagingError::GuardPageSetupFailed => write!(f, "Guard page setup failed"),
            PagingError::PageTableInitFailed => write!(f, "Page table initialization failed"),
        }
    }
}

/// ページテーブルエントリ数（512エントリ）
const PAGE_TABLE_ENTRY_COUNT: usize = 512;

/// ページサイズ（4KB）
pub const PAGE_SIZE: usize = 4096;

/// 1つのPage Tableがカバーする領域サイズ（2MB = 512 * 4KB）
const PAGE_TABLE_COVERAGE: u64 = (PAGE_TABLE_ENTRY_COUNT * PAGE_SIZE) as u64;

/// ページオフセットマスク（下位12ビット）
const PAGE_OFFSET_MASK: u64 = 0xFFF;

/// カーネル空間のPML4エントリインデックス (0xFFFF_8000_0000_0000に対応)
/// x86_64では仮想アドレスのビット47:39がPML4インデックスとなる
const PML4_KERNEL_INDEX: usize = 256;

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
        let addr_masked = addr & 0x000F_FFFF_FFFF_F000;
        // フラグをクリアして新しいアドレスを設定
        self.entry = (self.entry & PAGE_OFFSET_MASK) | addr_masked;
    }

    /// エントリを完全に設定（アドレス + フラグ）
    pub fn set(&mut self, addr: u64, flags: u64) {
        // 既存のエントリを完全にクリアしてから設定
        let addr_masked = addr & 0x000F_FFFF_FFFF_F000;
        self.entry = addr_masked | flags;
    }

    /// 物理アドレスを取得
    #[allow(dead_code)]
    pub fn get_address(&self) -> u64 {
        self.entry & 0x000F_FFFF_FFFF_F000
    }

    /// エントリの生の値を取得（デバッグ用）
    pub fn get_raw(&self) -> u64 {
        self.entry
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

/// 指定した物理アドレスがMMIO領域かどうかを判定
///
/// UEFIメモリマップに基づいて、EFI_MEMORY_MAPPED_IOまたは
/// EFI_MEMORY_MAPPED_IO_PORT_SPACEタイプの領域に含まれるかを確認する。
///
/// # Arguments
/// * `phys_addr` - 判定する物理アドレス
/// * `memory_regions` - UEFIメモリマップのスライス（有効範囲のみ）
fn is_mmio_region(
    phys_addr: u64,
    memory_regions: &[vitros_common::boot_info::MemoryRegion],
) -> bool {
    use vitros_common::uefi::{EFI_MEMORY_MAPPED_IO, EFI_MEMORY_MAPPED_IO_PORT_SPACE};

    for region in memory_regions {
        let region_end = region.start + region.size;

        if phys_addr >= region.start && phys_addr < region_end {
            if region.region_type == EFI_MEMORY_MAPPED_IO
                || region.region_type == EFI_MEMORY_MAPPED_IO_PORT_SPACE
            {
                return true;
            }
        }
    }
    false
}

/// カーネル専用スタック（64KB）
/// クレート内でのみ公開（kernel_mainから参照するため）
#[allow(dead_code)]
#[repr(align(16))]
pub(crate) struct KernelStack([u8; 65536]);

/// カーネルスタックの実体
/// クレート内でのみ公開（kernel_mainのインラインアセンブリから参照するため）
pub(crate) static mut KERNEL_STACK: KernelStack = KernelStack([0; 65536]);

/// カーネルスタックに切り替える
/// この関数を呼ぶと、UEFIから継承した低位アドレスのスタックから
/// カーネル専用の高位アドレスのスタックに切り替わる
#[allow(dead_code)]
#[unsafe(naked)]
pub unsafe extern "C" fn switch_to_kernel_stack() {
    core::arch::naked_asm!(
        // 古いスタックからリターンアドレスをポップ（raxに保存）
        "pop rax",

        // 新しいスタックのアドレスをロード
        "lea rsp, [rip + {kernel_stack}]",
        "add rsp, {stack_size}",

        // リターンアドレスを新しいスタックにプッシュ
        "push rax",

        // リターン（新しいスタックから）
        "ret",

        kernel_stack = sym KERNEL_STACK,
        stack_size = const core::mem::size_of::<KernelStack>(),
    )
}

// グローバルページテーブルを静的に確保
// 物理メモリの直接マッピング（Direct Mapping）を実装

/// 最大サポートメモリ（GB単位）
/// 静的配列のサイズを決定する - 8GBまでサポート
/// MMIOホール（3-4GB付近）を超えてメモリマッピングするため8GBに拡張
pub const MAX_SUPPORTED_MEMORY_GB: usize = 8;

/// Page Table数（各PTは2MBをカバー）
/// 8GB = 4096個のPT（512 * 8 = 4096）
const PT_COUNT: usize = MAX_SUPPORTED_MEMORY_GB * 512;

static mut KERNEL_PML4: PageTable = PageTable::new();
static mut KERNEL_PDP_HIGH: PageTable = PageTable::new(); // 高位アドレス用（0xFFFF_8000_0000_0000〜）

// Page Directory（4GB分確保、高位アドレスのみ）
static mut KERNEL_PD_HIGH: [PageTable; MAX_SUPPORTED_MEMORY_GB] =
    [PageTable::new(); MAX_SUPPORTED_MEMORY_GB];

// Page Table（4GB全体を4KBページでマップするため2,048個のPTが必要、高位アドレスのみ）
// 各PT = 512エントリ × 4KB = 2MB
// 4GB = 2,048個のPT
// 低位アドレスはアンマップ（ハイヤーハーフカーネル）
static mut KERNEL_PT_HIGH: [PageTable; PT_COUNT] = [PageTable::new(); PT_COUNT];

/// ページングシステムを初期化してCR3に設定
/// 物理メモリの直接マッピング（Direct Mapping）を実装
/// - 低位アドレス（0x0〜）: アンマップ（ハイヤーハーフカーネル）
/// - 高位アドレス（0xFFFF_8000_0000_0000+）: カーネル用の直接マッピング
///
/// UEFIメモリマップに基づいて、実際に利用可能なメモリ範囲のみをマッピングする。
/// 最大サポートメモリは MAX_SUPPORTED_MEMORY_GB (8GB) まで。
///
/// # Arguments
/// * `boot_info` - ブートローダから渡されたメモリ情報
///
/// # Errors
/// * `PagingError::AddressConversionFailed` - アドレス変換に失敗した場合
/// * `PagingError::GuardPageSetupFailed` - Guard Page設定に失敗した場合
pub fn init(boot_info: &vitros_common::boot_info::BootInfo) -> Result<(), PagingError> {
    // サポートする最大アドレスを計算
    let max_supported = (MAX_SUPPORTED_MEMORY_GB as u64) << 30; // 8GB
    let actual_max = boot_info.max_physical_address.min(max_supported);

    // 必要なPD数とPT数を計算
    // 1 PT = 512 * 4KB = 2MB
    let required_pt_count = ((actual_max + PAGE_TABLE_COVERAGE - 1) / PAGE_TABLE_COVERAGE) as usize;
    let required_pd_count = (required_pt_count + 511) / 512;

    use crate::info;
    info!(
        "Paging: Mapping {} MB of physical memory",
        actual_max / (1 << 20)
    );
    info!(
        "Paging: Using {} PDs and {} PTs",
        required_pd_count, required_pt_count
    );

    unsafe {
        // 生ポインタを取得（高位アドレス用のみ）
        let pml4 = addr_of_mut!(KERNEL_PML4);
        let pdp_high = addr_of_mut!(KERNEL_PDP_HIGH);
        let pd_high = addr_of_mut!(KERNEL_PD_HIGH);
        let pt_high = addr_of_mut!(KERNEL_PT_HIGH);

        // すべてのテーブルをクリア
        (*pml4).clear();
        (*pdp_high).clear();
        for i in 0..MAX_SUPPORTED_MEMORY_GB {
            (*pd_high)[i].clear();
        }
        for i in 0..PT_COUNT {
            (*pt_high)[i].clear();
        }

        // 基本フラグ: Present + Writable
        let flags = PageTableFlags::Present as u64 | PageTableFlags::Writable as u64;

        // === PML4の設定 ===
        // 低位アドレス（0x0〜）はアンマップ（ハイヤーハーフカーネル）
        // PML4[0]は設定しない（Present=0のまま）

        // PML4[PML4_KERNEL_INDEX] -> PDP_HIGH (高位アドレス用: 0xFFFF_8000_0000_0000〜)
        (*pml4)
            .entry(PML4_KERNEL_INDEX)
            .set((*pdp_high).physical_address()?, flags);

        // === 必要なPDPエントリのみ設定（高位のみ）===
        for i in 0..required_pd_count {
            (*pdp_high)
                .entry(i)
                .set((*pd_high)[i].physical_address()?, flags);
        }

        // === 必要なPTのみリンク（高位のみ）===
        for pt_idx in 0..required_pt_count {
            let pd_idx = pt_idx / PAGE_TABLE_ENTRY_COUNT;
            let entry_idx = pt_idx % PAGE_TABLE_ENTRY_COUNT;

            (*pd_high)[pd_idx]
                .entry(entry_idx)
                .set((*pt_high)[pt_idx].physical_address()?, flags);
        }

        // === 必要なページのみマッピング（高位のみ）===
        // MMIO領域はスキップし、後でmap_mmio()でUC属性でマッピングする
        let mut skipped_mmio_pages = 0usize;
        // メモリマップのスライスをループ外で一度だけ作成
        let memory_region_count = boot_info.memory_map_count.min(boot_info.memory_map.len());
        let memory_regions = &boot_info.memory_map[..memory_region_count];
        for pt_idx in 0..required_pt_count {
            for page_idx in 0..PAGE_TABLE_ENTRY_COUNT {
                let physical_addr =
                    ((pt_idx * PAGE_TABLE_ENTRY_COUNT + page_idx) * PAGE_SIZE) as u64;
                if physical_addr < actual_max {
                    if is_mmio_region(physical_addr, memory_regions) {
                        // MMIO領域はスキップ（Present=0のまま）
                        skipped_mmio_pages += 1;
                    } else {
                        (*pt_high)[pt_idx].entry(page_idx).set(physical_addr, flags);
                    }
                }
            }
        }
        if skipped_mmio_pages > 0 {
            info!("Skipped {} pages as MMIO regions", skipped_mmio_pages);
        }

        // === Guard Page の設定 ===
        // スタック領域の直前のページをGuard Page（Present=0）に設定
        let stack_virt_addr = addr_of_mut!(KERNEL_STACK) as u64;
        let guard_page_virt_addr = stack_virt_addr
            .checked_sub(PAGE_SIZE as u64)
            .ok_or(PagingError::GuardPageSetupFailed)?;

        // 仮想アドレスを物理アドレスに変換
        let guard_page_phys_addr = virt_to_phys(guard_page_virt_addr)?;
        let physical_offset = guard_page_phys_addr;

        // ページ番号を計算
        let page_num = (physical_offset >> 12) as usize;

        // PT配列内のインデックスとPT内のエントリ番号を計算
        let pt_array_idx = page_num / PAGE_TABLE_ENTRY_COUNT;
        let page_idx_in_pt = page_num % PAGE_TABLE_ENTRY_COUNT;

        // インデックスの範囲検証
        if pt_array_idx >= PT_COUNT {
            return Err(PagingError::GuardPageSetupFailed);
        }
        if page_idx_in_pt >= PAGE_TABLE_ENTRY_COUNT {
            return Err(PagingError::GuardPageSetupFailed);
        }

        // Guard PageのPTエントリをPresent=0に設定（アクセス時にPage Faultが発生）
        // 高位アドレスのみ設定（低位はアンマップ済み）
        (*pt_high)[pt_array_idx]
            .entry(page_idx_in_pt)
            .set(guard_page_phys_addr, 0);

        // デバッグ: Guard Page設定を確認（リリースビルドでは省略）
        #[cfg(debug_assertions)]
        {
            info!("Guard Page setup:");
            info!("  Virtual address: 0x{:016X}", guard_page_virt_addr);
            info!("  Physical offset: 0x{:X}", physical_offset);
            info!("  Page number: {}", page_num);
            info!("  PT array index: {}", pt_array_idx);
            info!("  Entry in PT: {}", page_idx_in_pt);
            info!(
                "  Entry value: 0x{:016X}",
                (*pt_high)[pt_array_idx].entry(page_idx_in_pt).get_raw()
            );
            info!(
                "  Entry is Present: {}",
                (*pt_high)[pt_array_idx].entry(page_idx_in_pt).get_raw() & 1 != 0
            );
        }

        // CR3レジスタにPML4のアドレスを設定
        let pml4_addr = (*pml4).physical_address()?;
        write_cr3(pml4_addr);

        Ok(())
    }
}

// =============================================================================
// MMIO マッピング関連
// =============================================================================

/// 割り込みが無効であることを確認（デバッグビルドのみ）
///
/// スレッドセーフティのため、ページテーブル操作は割り込み無効状態で
/// 行われることを検証する。
#[cfg(debug_assertions)]
fn assert_interrupts_disabled(context: &str) {
    let rflags: u64;
    // SAFETY: RFLAGSレジスタをスタックにプッシュしてから読み取る標準的な方法。
    // この操作はメモリ安全性に影響しない。
    unsafe {
        asm!("pushfq; pop {}", out(reg) rflags, options(nomem, preserves_flags));
    }
    // IF (Interrupt Flag) はbit 9
    debug_assert!(
        (rflags & 0x200) == 0,
        "{}: must be called with interrupts disabled",
        context
    );
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
/// * `PagingError::PageTableInitFailed` - ページテーブルのインデックスが範囲外の場合
pub fn map_mmio(phys_addr: u64, size: u64) -> Result<u64, PagingError> {
    use crate::info;

    // デバッグビルド時：割り込みが無効であることを確認
    #[cfg(debug_assertions)]
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
        let pt_high = addr_of_mut!(KERNEL_PT_HIGH);

        for i in 0..page_count {
            let addr = phys_addr + (i * PAGE_SIZE) as u64;

            // ページ番号を計算
            let page_num = (addr >> 12) as usize;

            // PT配列内のインデックスとPT内のエントリ番号を計算
            let pt_array_idx = page_num / PAGE_TABLE_ENTRY_COUNT;
            let page_idx_in_pt = page_num % PAGE_TABLE_ENTRY_COUNT;

            // インデックスの範囲検証
            if pt_array_idx >= PT_COUNT {
                return Err(PagingError::PageTableInitFailed);
            }

            // デバッグビルド時のみ重複マッピングを警告
            #[cfg(debug_assertions)]
            {
                if (*pt_high)[pt_array_idx]
                    .get_entry(page_idx_in_pt)
                    .is_present()
                {
                    info!(
                        "Warning: map_mmio overwriting existing mapping at 0x{:X}",
                        addr
                    );
                }
            }

            // UC属性でページテーブルエントリを設定
            (*pt_high)[pt_array_idx]
                .entry(page_idx_in_pt)
                .set(addr, uc_flags);
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
