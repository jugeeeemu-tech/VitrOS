//! VitrOS Kernel Library
//!
//! カーネルのモジュールをライブラリとして公開し、テスト可能にする。

#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner::runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

// 全モジュールを公開
pub mod acpi;
pub mod addr;
pub mod allocator;
pub mod apic;
pub mod debug_overlay;
pub mod gdt;
pub mod graphics;
pub mod hpet;
pub mod idt;
pub mod io;
pub mod msi;
pub mod msr;
pub mod mtrr;
pub mod paging;
pub mod pci;
pub mod pit;
pub mod sched;
pub mod serial;
pub mod stack;
pub mod sync;
pub mod timer;
pub mod timer_device;

// テストフレームワーク
pub mod test_runner;

#[cfg(feature = "visualize-allocator")]
pub mod allocator_visualization;

#[cfg(feature = "visualize-pipeline")]
pub mod pipeline_visualization;

// テストモード用のエントリーポイント
// ブートローダーは kernel_main を呼び出すため、テストでも同じシンボルを使用
#[cfg(test)]
#[unsafe(no_mangle)]
pub extern "efiapi" fn kernel_main(_boot_info: u64) -> ! {
    // テストではBootInfoは使用しない
    serial::init();
    test_main();
    loop {
        // SAFETY: hlt命令はCPUを低消費電力状態にする特権命令。
        // 無限ループで安全に待機する。
        unsafe {
            core::arch::asm!("hlt");
        }
    }
}

// テストモード用のパニックハンドラ
#[cfg(test)]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    serial_println!("[failed]");
    serial_println!("{}", info);
    test_runner::exit_qemu(test_runner::QemuExitCode::Failed);
}
