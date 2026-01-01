//! Global Descriptor Table (GDT) 実装
//!
//! x86_64アーキテクチャでは、セグメンテーションはほぼ使用されませんが、
//! 特権レベル（Ring 0/3）の管理とTSS（Interrupt Stack Table用）のためにGDTは必須です。

use crate::info;
use crate::paging::KERNEL_VIRTUAL_BASE;
use core::arch::asm;

/// GDT操作のエラー型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GdtError {
    /// 初期化失敗
    InitFailed,
    /// 無効なアドレス
    InvalidAddress,
}

impl core::fmt::Display for GdtError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            GdtError::InitFailed => write!(f, "GDT initialization failed"),
            GdtError::InvalidAddress => write!(f, "Invalid GDT address"),
        }
    }
}

/// 現在高位アドレス空間で実行されているかチェック
fn is_higher_half() -> bool {
    let rip: u64;
    unsafe {
        asm!("lea {}, [rip]", out(reg) rip, options(nomem, nostack));
    }
    rip >= KERNEL_VIRTUAL_BASE
}

/// Task State Segment (TSS) - x86_64用
/// x86_64では、セグメント切り替えではなく、主にInterrupt Stack Table (IST)のために使用
#[repr(C, packed)]
pub struct TaskStateSegment {
    reserved_1: u32,
    /// Ring 0のスタックポインタ（特権レベル変更時に使用）
    pub rsp0: u64,
    /// Ring 1のスタックポインタ（通常未使用）
    pub rsp1: u64,
    /// Ring 2のスタックポインタ（通常未使用）
    pub rsp2: u64,
    reserved_2: u64,
    /// Interrupt Stack Table 1（特定の割り込みハンドラ用の専用スタック）
    pub ist1: u64,
    /// Interrupt Stack Table 2
    pub ist2: u64,
    /// Interrupt Stack Table 3
    pub ist3: u64,
    /// Interrupt Stack Table 4
    pub ist4: u64,
    /// Interrupt Stack Table 5
    pub ist5: u64,
    /// Interrupt Stack Table 6
    pub ist6: u64,
    /// Interrupt Stack Table 7
    pub ist7: u64,
    reserved_3: u64,
    reserved_4: u16,
    /// I/Oマップベースアドレス
    pub iomap_base: u16,
}

impl TaskStateSegment {
    /// 新しいTSSを作成（すべてのフィールドを0で初期化）
    pub const fn new() -> Self {
        Self {
            reserved_1: 0,
            rsp0: 0,
            rsp1: 0,
            rsp2: 0,
            reserved_2: 0,
            ist1: 0,
            ist2: 0,
            ist3: 0,
            ist4: 0,
            ist5: 0,
            ist6: 0,
            ist7: 0,
            reserved_3: 0,
            reserved_4: 0,
            iomap_base: 0x68, // TSSのサイズ（I/Oマップなし）
        }
    }
}

/// GDTエントリ（64ビット）
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct GdtEntry {
    limit_low: u16,
    base_low: u16,
    base_middle: u8,
    access: u8,
    granularity: u8,
    base_high: u8,
}

impl GdtEntry {
    /// NULLディスクリプタを作成
    const fn null() -> Self {
        Self {
            limit_low: 0,
            base_low: 0,
            base_middle: 0,
            access: 0,
            granularity: 0,
            base_high: 0,
        }
    }

    /// コードセグメントディスクリプタを作成
    ///
    /// # Arguments
    /// * `dpl` - Descriptor Privilege Level (0 = カーネル, 3 = ユーザー)
    const fn code_segment(dpl: u8) -> Self {
        Self {
            limit_low: 0,
            base_low: 0,
            base_middle: 0,
            // Present | DPL | Code | Executable | Readable
            access: 0b10011010 | ((dpl & 0b11) << 5),
            // Long mode (bit 5) | 64-bit (bit 5)
            granularity: 0b00100000,
            base_high: 0,
        }
    }

    /// データセグメントディスクリプタを作成
    ///
    /// # Arguments
    /// * `dpl` - Descriptor Privilege Level (0 = カーネル, 3 = ユーザー)
    const fn data_segment(dpl: u8) -> Self {
        Self {
            limit_low: 0,
            base_low: 0,
            base_middle: 0,
            // Present | DPL | Data | Writable
            access: 0b10010010 | ((dpl & 0b11) << 5),
            granularity: 0,
            base_high: 0,
        }
    }
}

/// TSS用の16バイトディスクリプタ（x86_64ではTSSは16バイト）
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct TssDescriptor {
    low: GdtEntry,
    high: GdtEntry,
}

impl TssDescriptor {
    /// TSSディスクリプタを作成
    ///
    /// # Arguments
    /// * `tss_addr` - TSSのアドレス
    const fn new(tss_addr: u64) -> Self {
        let limit = (core::mem::size_of::<TaskStateSegment>() - 1) as u32;

        // 下位8バイト
        let low = GdtEntry {
            limit_low: (limit & 0xFFFF) as u16,
            base_low: (tss_addr & 0xFFFF) as u16,
            base_middle: ((tss_addr >> 16) & 0xFF) as u8,
            // Present | DPL=0 | Type=0b1001 (Available TSS 64-bit)
            access: 0b10001001,
            // Limit[19:16] | Flags
            granularity: ((limit >> 16) & 0x0F) as u8,
            base_high: ((tss_addr >> 24) & 0xFF) as u8,
        };

        // 上位8バイト（ベースアドレスの上位32ビット）
        let high = GdtEntry {
            limit_low: ((tss_addr >> 32) & 0xFFFF) as u16,
            base_low: ((tss_addr >> 48) & 0xFFFF) as u16,
            base_middle: 0,
            access: 0,
            granularity: 0,
            base_high: 0,
        };

        Self { low, high }
    }
}

/// GDT（Global Descriptor Table）
/// x86_64では、NULL + Code/Data×2 + TSS（16バイト）の構成
#[repr(C, align(16))]
struct Gdt {
    null: GdtEntry,        // 0x00: NULL
    kernel_code: GdtEntry, // 0x08: カーネルコード (Ring 0)
    kernel_data: GdtEntry, // 0x10: カーネルデータ (Ring 0)
    user_code: GdtEntry,   // 0x18: ユーザーコード (Ring 3)
    user_data: GdtEntry,   // 0x20: ユーザーデータ (Ring 3)
    tss: TssDescriptor,    // 0x28: TSS（16バイト）
}

impl Gdt {
    /// 新しいGDTを作成
    /// TSSアドレスは初期化時に設定されるため、ここではダミー値を使用
    const fn new() -> Self {
        Self {
            null: GdtEntry::null(),
            kernel_code: GdtEntry::code_segment(0),
            kernel_data: GdtEntry::data_segment(0),
            user_code: GdtEntry::code_segment(3),
            user_data: GdtEntry::data_segment(3),
            tss: TssDescriptor::new(0), // ダミー値（後で更新）
        }
    }

    /// TSSディスクリプタを更新
    fn set_tss(&mut self, tss_addr: u64) {
        self.tss = TssDescriptor::new(tss_addr);
    }
}

/// GDTR（GDT Register）用の構造体
#[repr(C, packed)]
struct Gdtr {
    limit: u16,
    base: u64,
}

/// グローバルGDTインスタンス
static mut GDT: Gdt = Gdt::new();

/// グローバルTSSインスタンス
static mut TSS: TaskStateSegment = TaskStateSegment::new();

/// Double Fault用のISTスタック（16KB）
/// Linux kernelと同様、Double Faultハンドラ用の専用スタックを提供
#[repr(align(16))]
struct DoubleFaultStack([u8; 16384]);

static mut DOUBLE_FAULT_STACK: DoubleFaultStack = DoubleFaultStack([0; 16384]);

/// セグメントセレクタ
pub mod selector {
    /// カーネルコードセグメントセレクタ
    pub const KERNEL_CODE: u16 = 0x08;
    /// カーネルデータセグメントセレクタ
    #[allow(dead_code)]
    pub const KERNEL_DATA: u16 = 0x10;
    /// ユーザーコードセグメントセレクタ（RPL=3を含む）
    #[allow(dead_code)]
    pub const USER_CODE: u16 = 0x18 | 3;
    /// ユーザーデータセグメントセレクタ（RPL=3を含む）
    #[allow(dead_code)]
    pub const USER_DATA: u16 = 0x20 | 3;
    /// TSSセグメントセレクタ
    pub const TSS: u16 = 0x28;
}

/// Double Fault用のISTインデックス
pub const DOUBLE_FAULT_IST_INDEX: u8 = 1;

/// GDTを初期化してロード
pub fn init() -> Result<(), GdtError> {
    unsafe {
        // TSSを初期化（Double Fault用のISTスタックを設定）
        let double_fault_stack_top = (&raw const DOUBLE_FAULT_STACK as u64)
            + core::mem::size_of::<DoubleFaultStack>() as u64;

        TSS.ist1 = double_fault_stack_top;

        info!("TSS initialized:");
        info!(
            "  IST1 (Double Fault stack): 0x{:016X}",
            double_fault_stack_top
        );

        // GDTにTSSディスクリプタを設定
        let tss_addr = &raw const TSS as u64;
        let gdt_ptr = core::ptr::addr_of_mut!(GDT);
        core::ptr::write(
            core::ptr::addr_of_mut!((*gdt_ptr).tss),
            TssDescriptor::new(tss_addr),
        );

        info!("TSS descriptor set in GDT at 0x{:016X}", tss_addr);

        // GDTのアドレスを取得（カーネルが高位アドレスでリンクされているため既に高位）
        let gdt_addr = &raw const GDT as u64;

        let gdtr = Gdtr {
            limit: (core::mem::size_of::<Gdt>() - 1) as u16,
            base: gdt_addr,
        };

        // LGDT命令でGDTをロード
        asm!(
            "lgdt [{}]",
            in(reg) &gdtr,
            options(readonly, nostack, preserves_flags)
        );

        // コードセグメントをリロード（far return）
        asm!(
            "push {sel}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            sel = in(reg) selector::KERNEL_CODE as u64,
            tmp = lateout(reg) _,
            options(preserves_flags)
        );

        // データセグメントレジスタをリロード
        asm!(
            "mov ds, {0:x}",
            "mov es, {0:x}",
            "mov fs, {0:x}",
            "mov gs, {0:x}",
            "mov ss, {0:x}",
            in(reg) selector::KERNEL_DATA,
            options(nostack, preserves_flags)
        );

        // LTR命令でTSSをロード
        asm!(
            "ltr {0:x}",
            in(reg) selector::TSS,
            options(nostack, preserves_flags)
        );

        info!("TSS loaded into TR register");
    }
    Ok(())
}
