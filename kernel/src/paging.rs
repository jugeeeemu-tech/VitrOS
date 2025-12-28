//! x86_64 ページングシステム実装
//! 4段階のページテーブル（PML4, PDP, PD, PT）を管理
//! ハイヤーハーフカーネル（高位アドレス空間へのマッピング）をサポート

use core::arch::asm;
use core::ptr::addr_of_mut;

/// ハイヤーハーフカーネルのベースアドレス（上位カノニカルアドレス空間）
/// x86_64のカノニカルアドレス空間の上位半分の開始位置
pub const KERNEL_VIRTUAL_BASE: u64 = 0xFFFF_8000_0000_0000;

/// ページテーブルエントリ数（512エントリ）
const PAGE_TABLE_ENTRY_COUNT: usize = 512;

/// ページサイズ（4KB）
pub const PAGE_SIZE: usize = 4096;

/// ページテーブルエントリのフラグ
#[repr(u64)]
pub enum PageTableFlags {
    Present = 1 << 0,       // エントリが有効
    Writable = 1 << 1,      // 書き込み可能
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
        self.set_address(addr);
        self.set_flags(flags);
    }

    /// 物理アドレスを取得
    pub fn get_address(&self) -> u64 {
        self.entry & 0x000F_FFFF_FFFF_F000
    }
}

/// ページテーブル（PML4, PDP, PD, PTすべてに共通の構造）
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
    pub fn physical_address(&self) -> u64 {
        let virt_addr = self as *const _ as u64;
        virt_addr - KERNEL_VIRTUAL_BASE
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
static mut KERNEL_PDP_LOW: PageTable = PageTable::new();   // 低位アドレス用（0x0〜）- 互換性のため残す
static mut KERNEL_PDP_HIGH: PageTable = PageTable::new();  // 高位アドレス用（0xFFFF_8000_0000_0000〜）

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

/// ページングシステムを初期化してCR3に設定
/// 物理メモリの直接マッピング（Direct Mapping）を実装
/// - 低位アドレス（0-4GB）: identity mapping（互換性のため残す）
/// - 高位アドレス（0xFFFF_8000_0000_0000+）: カーネル用の直接マッピング
pub fn init() {
    unsafe {
        // 生ポインタを取得
        let pml4 = addr_of_mut!(KERNEL_PML4);
        let pdp_low = addr_of_mut!(KERNEL_PDP_LOW);
        let pdp_high = addr_of_mut!(KERNEL_PDP_HIGH);
        let pd_low = addr_of_mut!(KERNEL_PD_LOW);
        let pd_high = addr_of_mut!(KERNEL_PD_HIGH);

        // すべてのテーブルをクリア
        (*pml4).clear();
        (*pdp_low).clear();
        (*pdp_high).clear();
        for i in 0..4 {
            (*pd_low)[i].clear();
            (*pd_high)[i].clear();
        }

        // 基本フラグ: Present + Writable
        let flags = PageTableFlags::Present as u64 | PageTableFlags::Writable as u64;

        // === 低位アドレスのマッピング（Identity Mapping）===
        // 互換性のため残す（boot_info、APIC、フレームバッファなど）
        // PML4[0] -> PDP_LOW
        (*pml4).entry(0).set((*pdp_low).physical_address(), flags);

        // PDP_LOW[0-3] -> PD_LOW[0-3]（4GB分）
        for i in 0..4 {
            (*pdp_low).entry(i).set((*pd_low)[i].physical_address(), flags);
        }

        // === 高位アドレスのマッピング（Direct Mapping）===
        // 0xFFFF_8000_0000_0000は、PML4インデックス256に対応
        // PML4[256] -> PDP_HIGH
        (*pml4).entry(256).set((*pdp_high).physical_address(), flags);

        // PDP_HIGH[0-3] -> PD_HIGH[0-3]（4GB分）
        for i in 0..4 {
            (*pdp_high).entry(i).set((*pd_high)[i].physical_address(), flags);
        }

        // 最初の4GBを両方のアドレス空間にマッピング（2MBページ使用）
        // 物理アドレス 0x0〜0x100000000を:
        // - 仮想アドレス 0x0〜0x100000000 (identity)
        // - 仮想アドレス 0xFFFF_8000_0000_0000〜 (direct mapping)
        let huge_flags = flags | PageTableFlags::HugePage as u64;
        for pd_idx in 0..4 {
            for entry_idx in 0..PAGE_TABLE_ENTRY_COUNT {
                let physical_addr = ((pd_idx * PAGE_TABLE_ENTRY_COUNT + entry_idx) * 2 * 1024 * 1024) as u64;
                (*pd_low)[pd_idx].entry(entry_idx).set(physical_addr, huge_flags);
                (*pd_high)[pd_idx].entry(entry_idx).set(physical_addr, huge_flags);
            }
        }

        // CR3レジスタにPML4のアドレスを設定
        let pml4_addr = (*pml4).physical_address();
        write_cr3(pml4_addr);
    }
}

