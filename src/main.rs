#![no_main]

extern crate alloc;

mod allocator;
mod boot;
mod boot_info;
mod graphics;
mod io;
mod kernel;
mod serial;
mod uefi;

#[cfg(feature = "visualize-allocator")]
mod allocator_visualization;

use uefi::*;

/// UEFI エントリポイント
/// ブート処理を実行後、カーネルに制御を移す
#[unsafe(no_mangle)]
extern "efiapi" fn efi_main(
    image_handle: EfiHandle,
    system_table: *mut EfiSystemTable,
) -> EfiStatus {
    // シリアルポートを初期化
    serial::init();
    println!("=== je4OS Bootloader ===");
    info!("Serial output initialized");

    // ========================================
    // ブートローダ処理（最小限）
    // ========================================
    // メモリマップ取得、フレームバッファ初期化、ブートサービス終了
    let (boot_info, mut writer, _map_key) = boot::boot_and_prepare(image_handle, system_table);

    info!("Bootloader finished, starting kernel...");
    println!("\n=== Transferring control to kernel ===\n");

    // ========================================
    // カーネルに制御を移す
    // ========================================
    kernel::kernel_main(&boot_info, &mut writer);
}
