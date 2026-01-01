//! x86_64 ページングシステム実装
//! 4段階のページテーブル（PML4, PDP, PD, PT）を管理
//! ハイヤーハーフカーネル（高位アドレス空間へのマッピング）をサポート

use core::arch::asm;
use core::ptr::addr_of_mut;

/// ハイヤーハーフカーネルのベースアドレス（上位カノニカルアドレス空間）
/// x86_64のカノニカルアドレス空間の上位半分の開始位置
pub const KERNEL_VIRTUAL_BASE: u64 = 0xFFFF_8000_0000_0000;

/// ページング操作のエラー型
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
    /// ACPIアドレスが無効
    AcpiAddressInvalid,
    /// チェックサム検証失敗
    ChecksumFailed,
}

impl core::fmt::Display for PagingError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            PagingError::InvalidAddress => write!(f, "Invalid address"),
            PagingError::AddressConversionFailed => write!(f, "Address conversion failed"),
            PagingError::GuardPageSetupFailed => write!(f, "Guard page setup failed"),
            PagingError::PageTableInitFailed => write!(f, "Page table initialization failed"),
            PagingError::AcpiAddressInvalid => write!(f, "ACPI address is invalid"),
            PagingError::ChecksumFailed => write!(f, "Checksum verification failed"),
        }
    }
}

/// ページテーブルエントリ数（512エントリ）
const PAGE_TABLE_ENTRY_COUNT: usize = 512;

/// ページサイズ（4KB）
pub const PAGE_SIZE: usize = 4096;

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
    pub fn is_present(&self) -> bool {
        (self.entry & PageTableFlags::Present as u64) != 0
    }

    /// フラグを設定
    pub fn set_flags(&mut self, flags: u64) {
        self.entry |= flags;
    }

    /// 物理アドレスを設定（12ビットシフト済みの値）
    pub fn set_address(&mut self, addr: u64) {
        // 下位12ビットをクリア（4KBアライメント）
        let addr_masked = addr & 0x000F_FFFF_FFFF_F000;
        // フラグをクリアして新しいアドレスを設定
        self.entry = (self.entry & 0xFFF) | addr_masked;
    }

    /// エントリを完全に設定（アドレス + フラグ）
    pub fn set(&mut self, addr: u64, flags: u64) {
        // 既存のエントリを完全にクリアしてから設定
        let addr_masked = addr & 0x000F_FFFF_FFFF_F000;
        self.entry = addr_masked | flags;
    }

    /// 物理アドレスを取得
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
pub fn reload_cr3() {
    let cr3 = read_cr3();
    write_cr3(cr3);
}

/// カーネル専用スタック（64KB）
/// クレート内でのみ公開（kernel_mainから参照するため）
#[repr(align(16))]
pub(crate) struct KernelStack([u8; 65536]);

/// カーネルスタックの実体
/// クレート内でのみ公開（kernel_mainのインラインアセンブリから参照するため）
pub(crate) static mut KERNEL_STACK: KernelStack = KernelStack([0; 65536]);

/// カーネルスタックに切り替える
/// この関数を呼ぶと、UEFIから継承した低位アドレスのスタックから
/// カーネル専用の高位アドレスのスタックに切り替わる
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
static mut KERNEL_PML4: PageTable = PageTable::new();
static mut KERNEL_PDP_LOW: PageTable = PageTable::new(); // 低位アドレス用（0x0〜）- 互換性のため残す
static mut KERNEL_PDP_HIGH: PageTable = PageTable::new(); // 高位アドレス用（0xFFFF_8000_0000_0000〜）

// Page Directory（2MBページを使用するため、4GB分確保）
static mut KERNEL_PD_LOW: [PageTable; 4] = [
    PageTable::new(),
    PageTable::new(),
    PageTable::new(),
    PageTable::new(),
];

static mut KERNEL_PD_HIGH: [PageTable; 4] = [
    PageTable::new(),
    PageTable::new(),
    PageTable::new(),
    PageTable::new(),
];

// Page Table（4GB全体を4KBページでマップするため2,048個のPTが必要）
// 各PT = 512エントリ × 4KB = 2MB
// 4GB = 2,048個のPT
static mut KERNEL_PT_LOW: [PageTable; 2048] = [PageTable::new(); 2048];
static mut KERNEL_PT_HIGH: [PageTable; 2048] = [PageTable::new(); 2048];

/// ページングシステムを初期化してCR3に設定
/// 物理メモリの直接マッピング（Direct Mapping）を実装
/// - 低位アドレス（0-4GB）: identity mapping（互換性のため残す）
/// - 高位アドレス（0xFFFF_8000_0000_0000+）: カーネル用の直接マッピング
///
/// # Errors
/// * `PagingError::AddressConversionFailed` - アドレス変換に失敗した場合
/// * `PagingError::GuardPageSetupFailed` - Guard Page設定に失敗した場合
pub fn init() -> Result<(), PagingError> {
    unsafe {
        // 生ポインタを取得
        let pml4 = addr_of_mut!(KERNEL_PML4);
        let pdp_low = addr_of_mut!(KERNEL_PDP_LOW);
        let pdp_high = addr_of_mut!(KERNEL_PDP_HIGH);
        let pd_low = addr_of_mut!(KERNEL_PD_LOW);
        let pd_high = addr_of_mut!(KERNEL_PD_HIGH);
        let pt_low = addr_of_mut!(KERNEL_PT_LOW);
        let pt_high = addr_of_mut!(KERNEL_PT_HIGH);

        // すべてのテーブルをクリア
        (*pml4).clear();
        (*pdp_low).clear();
        (*pdp_high).clear();
        for i in 0..4 {
            (*pd_low)[i].clear();
            (*pd_high)[i].clear();
        }
        for i in 0..2048 {
            (*pt_low)[i].clear();
            (*pt_high)[i].clear();
        }

        // 基本フラグ: Present + Writable
        let flags = PageTableFlags::Present as u64 | PageTableFlags::Writable as u64;

        // === 低位アドレスのマッピング（Identity Mapping）===
        // 互換性のため残す（boot_info、APIC、フレームバッファなど）
        // PML4[0] -> PDP_LOW
        (*pml4).entry(0).set((*pdp_low).physical_address()?, flags);

        // PDP_LOW[0-3] -> PD_LOW[0-3]（4GB分）
        for i in 0..4 {
            (*pdp_low)
                .entry(i)
                .set((*pd_low)[i].physical_address()?, flags);
        }

        // === 高位アドレスのマッピング（Direct Mapping）===
        // 0xFFFF_8000_0000_0000は、PML4インデックス256に対応
        // PML4[256] -> PDP_HIGH
        (*pml4)
            .entry(256)
            .set((*pdp_high).physical_address()?, flags);

        // PDP_HIGH[0-3] -> PD_HIGH[0-3]（4GB分）
        for i in 0..4 {
            (*pdp_high)
                .entry(i)
                .set((*pd_high)[i].physical_address()?, flags);
        }

        // === 4GB全体を4KBページでマッピング ===
        // 各PD（4個）が512個のPTを参照し、各PTが512個の4KBページをマップ
        // 合計: 4 × 512 × 512 × 4KB = 4GB

        // 各PDエントリにPTをリンク
        for pd_idx in 0..4 {
            for entry_idx in 0..PAGE_TABLE_ENTRY_COUNT {
                let pt_idx = pd_idx * PAGE_TABLE_ENTRY_COUNT + entry_idx;
                (*pd_low)[pd_idx]
                    .entry(entry_idx)
                    .set((*pt_low)[pt_idx].physical_address()?, flags);
                (*pd_high)[pd_idx]
                    .entry(entry_idx)
                    .set((*pt_high)[pt_idx].physical_address()?, flags);
            }
        }

        // 各PTのエントリに4KBページをマップ
        for pt_idx in 0..2048 {
            for page_idx in 0..PAGE_TABLE_ENTRY_COUNT {
                // 物理アドレス = (PT番号 × 2MB) + (ページ番号 × 4KB)
                let physical_addr =
                    ((pt_idx * PAGE_TABLE_ENTRY_COUNT + page_idx) * PAGE_SIZE) as u64;
                (*pt_low)[pt_idx].entry(page_idx).set(physical_addr, flags);
                (*pt_high)[pt_idx].entry(page_idx).set(physical_addr, flags);
            }
        }

        // === Guard Page の設定 ===
        // スタック領域の直前のページをGuard Page（Present=0）に設定
        let stack_virt_addr = addr_of_mut!(KERNEL_STACK) as u64;
        let guard_page_virt_addr = stack_virt_addr
            .checked_sub(PAGE_SIZE as u64)
            .ok_or(PagingError::GuardPageSetupFailed)?;

        // 仮想アドレスを物理アドレスに変換（virt_to_phys()を使用）
        let guard_page_phys_addr = virt_to_phys(guard_page_virt_addr)?;

        // Guard PageのPTエントリを特定
        // higher-half kernelの場合、カーネルベースからのオフセットを使ってインデックスを計算
        // 仮想アドレス全体から計算すると高位ビットが影響するため、物理オフセットから計算
        let physical_offset = virt_to_phys(guard_page_virt_addr)?;

        // 4GB全体でのページ番号を計算
        let page_num = (physical_offset >> 12) as usize;

        // PT配列内のインデックスとPT内のエントリ番号を計算
        let pt_array_idx = page_num / PAGE_TABLE_ENTRY_COUNT;
        let page_idx_in_pt = page_num % PAGE_TABLE_ENTRY_COUNT;

        // インデックスの範囲検証
        if pt_array_idx >= 2048 {
            return Err(PagingError::GuardPageSetupFailed);
        }
        if page_idx_in_pt >= PAGE_TABLE_ENTRY_COUNT {
            return Err(PagingError::GuardPageSetupFailed);
        }

        // Guard PageのPTエントリをPresent=0に設定（アクセス時にPage Faultが発生）
        (*pt_high)[pt_array_idx]
            .entry(page_idx_in_pt)
            .set(guard_page_phys_addr, 0);

        // デバッグ: Guard Page設定を確認
        use crate::info;
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

        // CR3レジスタにPML4のアドレスを設定
        let pml4_addr = (*pml4).physical_address()?;
        write_cr3(pml4_addr);

        Ok(())
    }
}
