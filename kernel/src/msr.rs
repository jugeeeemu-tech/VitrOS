//! MSR (Model Specific Register) 操作モジュール
//!
//! x86_64のMSRへのアクセスを提供します。

use core::arch::asm;

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
