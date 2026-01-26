//! カスタムテストフレームワーク
//!
//! QEMUの`isa-debug-exit`デバイスを使用してテスト結果を報告する。

use crate::io::port_write_u8;

/// QEMU終了コード
///
/// isa-debug-exitデバイスは (value << 1) | 1 を終了コードとして返す。
/// - Success (0x10) → (0x10 << 1) | 1 = 0x21 = 33
/// - Failed (0x11) → (0x11 << 1) | 1 = 0x23 = 35
#[derive(Debug, Clone, Copy)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10, // QEMU exit code: 33
    Failed = 0x11,  // QEMU exit code: 35
}

/// QEMUを指定した終了コードで終了する
///
/// ポート0xf4に書き込むことでQEMUを終了させる。
pub fn exit_qemu(code: QemuExitCode) -> ! {
    // SAFETY: isa-debug-exitデバイスに書き込んでQEMUを終了する。
    // このデバイスはQEMU起動時に設定されている。
    unsafe {
        port_write_u8(0xf4, code as u8);
    }
    // 終了しない場合のフォールバック
    loop {
        // SAFETY: hlt命令でCPUを停止させる。
        unsafe {
            core::arch::asm!("hlt");
        }
    }
}

/// テスト可能なオブジェクトのトレイト
pub trait Testable {
    fn run(&self);
}

impl<T: Fn()> Testable for T {
    fn run(&self) {
        crate::serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        crate::serial_println!("[ok]");
    }
}

/// テストランナー
///
/// すべてのテストを実行し、成功時にQEMUを終了コード33で終了する。
pub fn runner(tests: &[&dyn Testable]) {
    crate::serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    exit_qemu(QemuExitCode::Success);
}

// ============================================================================
// サンプルテスト
// ============================================================================

#[test_case]
fn trivial_assertion() {
    assert_eq!(1, 1);
}

#[test_case]
fn test_serial_print() {
    crate::serial_print!("test output ");
}
