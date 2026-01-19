//! MSR (Model Specific Register) 操作モジュール
//!
//! x86_64のMSRへのアクセスを提供します。

use core::arch::asm;

// =============================================================================
// MTRR (Memory Type Range Registers) 関連 MSR アドレス
// =============================================================================

/// MTRR Capability Register - MTRRの機能を確認
pub const IA32_MTRRCAP: u32 = 0xFE;

/// MTRR Default Type Register - デフォルトメモリタイプ
pub const IA32_MTRR_DEF_TYPE: u32 = 0x2FF;

/// MTRR Physical Base 0 - 可変範囲MTRRのベースアドレス（最初）
pub const IA32_MTRR_PHYSBASE0: u32 = 0x200;

/// MTRR Physical Mask 0 - 可変範囲MTRRのマスク（最初）
pub const IA32_MTRR_PHYSMASK0: u32 = 0x201;

/// Page Attribute Table - PAT設定
pub const IA32_PAT: u32 = 0x277;

/// MSRを読み込む
///
/// # Safety
/// - msrが有効なMSRアドレスであること
/// - Ring 0で実行されること
#[inline]
pub unsafe fn read(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: 呼び出し元が有効なMSRアドレスを指定することを保証する。
    // RDMSR命令はRing 0でのみ実行可能であり、カーネルモードで動作している。
    unsafe {
        asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nostack, preserves_flags)
        );
    }
    ((high as u64) << 32) | (low as u64)
}

/// MSRに書き込む
///
/// # Safety
/// - msrが有効な書き込み可能MSRアドレスであること
/// - valueがそのMSRに対して有効な値であること
/// - Ring 0で実行されること
#[inline]
pub unsafe fn write(msr: u32, value: u64) {
    let low = (value & 0xFFFFFFFF) as u32;
    let high = ((value >> 32) & 0xFFFFFFFF) as u32;
    // SAFETY: 呼び出し元が有効なMSRアドレスと値を指定することを保証する。
    // WRMSR命令はRing 0でのみ実行可能であり、カーネルモードで動作している。
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") low,
            in("edx") high,
            options(nostack, preserves_flags)
        );
    }
}
