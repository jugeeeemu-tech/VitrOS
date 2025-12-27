//! Global Descriptor Table (GDT) 実装
//!
//! x86_64アーキテクチャでは、セグメンテーションはほぼ使用されませんが、
//! 特権レベル（Ring 0/3）の管理のためにGDTは必須です。

use core::arch::asm;

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

/// GDT（Global Descriptor Table）
#[repr(C, align(16))]
struct Gdt {
    entries: [GdtEntry; 5],
}

impl Gdt {
    /// 新しいGDTを作成
    const fn new() -> Self {
        Self {
            entries: [
                GdtEntry::null(),              // 0x00: NULL
                GdtEntry::code_segment(0),     // 0x08: カーネルコード (Ring 0)
                GdtEntry::data_segment(0),     // 0x10: カーネルデータ (Ring 0)
                GdtEntry::code_segment(3),     // 0x18: ユーザーコード (Ring 3)
                GdtEntry::data_segment(3),     // 0x20: ユーザーデータ (Ring 3)
            ],
        }
    }
}

/// GDTR（GDT Register）用の構造体
#[repr(C, packed)]
struct Gdtr {
    limit: u16,
    base: u64,
}

/// グローバルGDTインスタンス
static GDT: Gdt = Gdt::new();

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
}

/// GDTを初期化してロード
pub fn init() {
    unsafe {
        let gdtr = Gdtr {
            limit: (core::mem::size_of::<Gdt>() - 1) as u16,
            base: &raw const GDT as u64,
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
    }
}
