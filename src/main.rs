#![no_main]

extern crate alloc;

use core::arch::asm;
use core::fmt::Write;

mod uefi;
mod graphics;
mod io;
mod serial;
mod allocator;

use uefi::*;
use graphics::FramebufferWriter;

fn hlt() {
    unsafe {
        asm!("hlt");
    }
}

// 待機関数（簡易版）
fn wait_cycles(cycles: usize) {
    for _ in 0..cycles {
        unsafe {
            core::arch::asm!("nop", options(nomem, nostack));
        }
    }
}

// メモリタイプを文字列に変換
fn memory_type_str(mem_type: u32) -> &'static str {
    match mem_type {
        EFI_RESERVED_MEMORY_TYPE => "Reserved",
        EFI_LOADER_CODE => "LoaderCode",
        EFI_LOADER_DATA => "LoaderData",
        EFI_BOOT_SERVICES_CODE => "BSCode",
        EFI_BOOT_SERVICES_DATA => "BSData",
        EFI_RUNTIME_SERVICES_CODE => "RTCode",
        EFI_RUNTIME_SERVICES_DATA => "RTData",
        EFI_CONVENTIONAL_MEMORY => "Available",
        EFI_UNUSABLE_MEMORY => "Unusable",
        EFI_ACPI_RECLAIM_MEMORY => "ACPIReclaim",
        EFI_ACPI_MEMORY_NVS => "ACPINVS",
        EFI_MEMORY_MAPPED_IO => "MMIO",
        EFI_MEMORY_MAPPED_IO_PORT_SPACE => "MMIOPort",
        EFI_PAL_CODE => "PALCode",
        _ => "Unknown",
    }
}

#[unsafe(no_mangle)]
extern "efiapi" fn efi_main(
    image_handle: EfiHandle,
    system_table: *mut EfiSystemTable,
) -> EfiStatus {
    // シリアルポートを初期化
    serial::init();
    println!("=== je4OS Bootloader ===");
    info!("Serial output initialized");
    info!("Locating Graphics Output Protocol...");

    // SAFETY: system_table は UEFI から渡される有効なポインタ
    let boot_services = unsafe { (*system_table).boot_services };

    // Graphics Output Protocol を検索
    let mut gop: *mut EfiGraphicsOutputProtocol = core::ptr::null_mut();

    // SAFETY: UEFI 関数の呼び出し
    let status = unsafe {
        ((*boot_services).locate_protocol)(
            &EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID,
            core::ptr::null_mut(),
            &mut gop as *mut *mut _ as *mut *mut core::ffi::c_void,
        )
    };

    if status != EFI_SUCCESS {
        error!("Failed to locate GOP!");
        loop {
            hlt()
        }
    }

    info!("GOP found successfully");

    // SAFETY: GOP から有効なフレームバッファ情報を取得
    let (fb_base, fb_size, width, height) = unsafe {
        let mode = (*gop).mode;
        let mode_info = (*mode).info;
        (
            (*mode).frame_buffer_base,
            (*mode).frame_buffer_size,
            (*mode_info).horizontal_resolution,
            (*mode_info).vertical_resolution,
        )
    };

    // SAFETY: フレームバッファへの直接書き込み
    unsafe {
        let fb_ptr = fb_base as *mut u32;
        let pixel_count = fb_size / 4;
        for i in 0..pixel_count {
            *fb_ptr.add(i) = 0x00000000;
        }
    }

    // FramebufferWriter を作成
    let mut writer = FramebufferWriter::new(fb_base, width, height, 0xFFFFFFFF);
    writer.set_position(10, 10);

    // writeln! マクロでテキストを描画
    let _ = writeln!(writer, "je4OS - Memory Map");

    // メモリマップを取得
    let mut map_size: usize = 0;
    let mut map_key: usize = 0;
    let mut descriptor_size: usize = 0;
    let mut descriptor_version: u32 = 0;

    // SAFETY: UEFI 関数呼び出し - メモリマップサイズ取得
    unsafe {
        ((*boot_services).get_memory_map)(
            &mut map_size,
            core::ptr::null_mut(),
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        );
    }

    // バッファを確保（スタック上に）
    let mut buffer = [0u8; 4096 * 4];
    map_size = buffer.len();

    // SAFETY: UEFI 関数呼び出し - 実際のメモリマップ取得
    let status = unsafe {
        ((*boot_services).get_memory_map)(
            &mut map_size,
            buffer.as_mut_ptr() as *mut EfiMemoryDescriptor,
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        )
    };

    if status == EFI_SUCCESS {
        let entry_count = map_size / descriptor_size;
        info!("Memory map retrieved: {} entries", entry_count);

        // メモリマップを表示
        writer.set_position(10, 30);
        let max_display = 20;

        println!("\nMemory Map (first {} entries):", max_display.min(entry_count));
        for i in 0..entry_count.min(max_display) {
            let offset = i * descriptor_size;

            // SAFETY: バッファ内の有効なメモリディスクリプタを参照
            let desc = unsafe {
                &*(buffer.as_ptr().add(offset) as *const EfiMemoryDescriptor)
            };

            let type_str = memory_type_str(desc.r#type);
            println!("  {:<12} 0x{:016X}  Pages: 0x{:X}",
                type_str,
                desc.physical_start,
                desc.number_of_pages
            );

            let _ = writeln!(
                writer,
                "{:<12} 0x{:016X}  Pages: 0x{:X}",
                type_str,
                desc.physical_start,
                desc.number_of_pages
            );
        }

        let _ = writeln!(writer, "");
        let _ = writeln!(writer, "Total entries: {}", entry_count);

        // メモリマップから利用可能なメモリを見つけてアロケータを初期化
        let mut largest_start = 0;
        let mut largest_size = 0;

        for i in 0..entry_count {
            let offset = i * descriptor_size;
            let desc = unsafe {
                &*(buffer.as_ptr().add(offset) as *const EfiMemoryDescriptor)
            };

            // EFI_CONVENTIONAL_MEMORY（利用可能なメモリ）を探す
            if desc.r#type == EFI_CONVENTIONAL_MEMORY {
                let size = desc.number_of_pages * 4096; // 1ページ = 4KB
                if size > largest_size {
                    largest_start = desc.physical_start as usize;
                    largest_size = size;
                }
            }
        }

        if largest_size > 0 {
            // ヒープとして使用するサイズ（可視化のため256KBに制限）
            let heap_size = (largest_size as usize).min(256 * 1024);
            unsafe {
                allocator::init_heap(largest_start, heap_size);
            }
        } else {
            error!("No usable memory found!");
        }
    }

    // SAFETY: UEFI 関数呼び出し - ブートサービス終了
    info!("Exiting boot services...");
    let status = unsafe {
        ((*boot_services).exit_boot_services)(image_handle, map_key)
    };

    writer.set_position(10, 280);
    if status == EFI_SUCCESS {
        info!("Boot services exited successfully!");
        let _ = writeln!(writer, "Boot Services Exited!");
        writer.set_position(10, 300);

        // スラブアロケータの可視化（複数サイズクラス表示）
        let _ = writeln!(writer, "=== Memory Allocator Visualization ===");

        // 初期状態を表示
        draw_code_snippet(&mut writer, &[
            "// Initial state",
            "// No allocations yet",
        ]);
        draw_memory_grids_multi(&mut writer, "Initial State");
        wait_cycles(150_000_000);

        // テスト1: 16Bクラス
        info!("\n=== Test 1: Vec<u8> (16B class) ===");

        let vec1: alloc::vec::Vec<u8> = (0..12).collect();

        draw_code_snippet(&mut writer, &[
            "let vec1: Vec<u8>",
            "  = (0..12).collect();",
            "",
            "// 12 x u8 = 12B",
            "// -> 16B size class",
        ]);
        draw_memory_grids_multi(&mut writer, "After 16B alloc");
        info!("Allocated Vec<u8> (12 elements = 12B -> 16B)");
        wait_cycles(150_000_000);

        // テスト2: 64Bクラス
        info!("\n=== Test 2: Vec<u8> (64B class) ===");

        let vec2: alloc::vec::Vec<u8> = (0..50).collect();

        draw_code_snippet(&mut writer, &[
            "let vec2: Vec<u8>",
            "  = (0..50).collect();",
            "",
            "// 50 x u8 = 50B",
            "// -> 64B size class",
        ]);
        draw_memory_grids_multi(&mut writer, "After 16B + 64B");
        info!("Allocated Vec<u8> (50 elements = 50B -> 64B)");
        wait_cycles(150_000_000);

        // テスト3: 128Bクラス
        info!("\n=== Test 3: Vec<u64> (128B class) ===");

        let vec3: alloc::vec::Vec<u64> = (0..10).collect();

        draw_code_snippet(&mut writer, &[
            "let vec3: Vec<u64>",
            "  = (0..10).collect();",
            "",
            "// 10 x u64 = 80B",
            "// -> 128B size class",
        ]);
        draw_memory_grids_multi(&mut writer, "16B+64B+128B");
        info!("Allocated Vec<u64> (10 elements = 80B -> 128B)");
        wait_cycles(150_000_000);

        // テスト4: 256Bクラスを追加
        info!("\n=== Test 4: Vec<u64> (256B class) ===");

        let vec4: alloc::vec::Vec<u64> = (0..25).collect();

        draw_code_snippet(&mut writer, &[
            "let vec4: Vec<u64>",
            "  = (0..25).collect();",
            "",
            "// 25 x u64 = 200B",
            "// -> 256B size class",
        ]);
        draw_memory_grids_multi(&mut writer, "All 4 sizes");
        info!("Allocated Vec<u64> (25 elements = 200B -> 256B)");
        wait_cycles(150_000_000);

        // テスト5: 64Bと256Bを解放
        info!("\n=== Test 5: Free 64B and 256B ===");

        drop(vec2);
        drop(vec4);

        draw_code_snippet(&mut writer, &[
            "drop(vec2);",
            "drop(vec4);",
            "",
            "// Freed 64B and 256B",
            "// 16B + 128B remain",
        ]);
        draw_memory_grids_multi(&mut writer, "After freeing 2");
        info!("Freed 64B and 256B blocks");
        wait_cycles(150_000_000);

        // テスト6: 全て解放
        info!("\n=== Test 6: Free all ===");

        drop(vec1);
        drop(vec3);

        draw_code_snippet(&mut writer, &[
            "drop(vec1);",
            "drop(vec3);",
            "",
            "// All freed!",
        ]);
        draw_memory_grids_multi(&mut writer, "All freed");
        info!("All blocks freed");
        wait_cycles(150_000_000);

    } else {
        error!("Failed to exit boot services! Status: 0x{:X}", status);
        writer.set_color(0xFF0000); // 赤色
        let _ = writeln!(writer, "Exit failed!");
    }

    println!("\nHalting system...");

    loop {
        hlt()
    }
}

// スラブアロケータの状態をグラフィカルに描画
fn draw_allocator_state(writer: &mut FramebufferWriter, label: &str) {
    use graphics::{draw_rect, draw_rect_outline, draw_string};

    let allocator = allocator::get_allocator();
    let size_classes = allocator::get_size_classes();

    let fb_base = writer.fb_base;
    let screen_width = writer.width;

    // 固定位置に描画（上書き方式）
    let start_x = 500;
    let start_y = 30;
    let mut current_y = start_y;

    // 描画領域を黒でクリア（700x400ピクセル）
    draw_rect(fb_base, screen_width, start_x - 10, start_y - 10, 700, 400, 0x000000);

    // タイトルを描画
    let title = format!("Allocator: {}", label);
    draw_string(fb_base, screen_width, start_x, current_y, &title, 0xFFFF00);
    current_y += 20;

    // 各サイズクラスを視覚化
    for i in 0..size_classes.len() {
        let size = size_classes[i];
        let free_count = allocator.count_free_blocks(i);

        // 総ブロック数を計算（初期化時の値）
        let slab_size = (32 * 1024 * 1024) / size_classes.len(); // 32MB / 10
        let aligned_size = align_down(slab_size, size);
        let total_blocks = aligned_size / size;
        let used_blocks = total_blocks - free_count;

        // ラベルを描画
        let label_text = format!("{:4}B:", size);
        draw_string(fb_base, screen_width, start_x, current_y, &label_text, 0xFFFFFF);

        // バーの幅を計算（最大400ピクセル）
        let bar_x = start_x + 50;
        let bar_width = 400;
        let bar_height = 15;

        // 背景（空き）: 緑
        draw_rect(fb_base, screen_width, bar_x, current_y, bar_width, bar_height, 0x004000);

        // 使用中の部分: 赤
        if total_blocks > 0 {
            let used_width = (bar_width * used_blocks) / total_blocks;
            if used_width > 0 {
                draw_rect(fb_base, screen_width, bar_x, current_y, used_width, bar_height, 0xFF0000);
            }
        }

        // 枠線
        draw_rect_outline(fb_base, screen_width, bar_x, current_y, bar_width, bar_height, 0xFFFFFF);

        // 数値表示
        let stats_text = format!("{}/{}", used_blocks, total_blocks);
        draw_string(fb_base, screen_width, bar_x + bar_width + 10, current_y, &stats_text, 0xFFFFFF);

        current_y += bar_height + 5;
    }

    // 大きなサイズ用領域
    current_y += 10;
    let (used, total) = allocator.large_alloc_usage();

    draw_string(fb_base, screen_width, start_x, current_y, "Large:", 0xFFFFFF);

    let bar_x = start_x + 50;
    let bar_width = 400;
    let bar_height = 15;

    // 背景（空き）: 緑
    draw_rect(fb_base, screen_width, bar_x, current_y, bar_width, bar_height, 0x004000);

    // 使用中: 赤
    if total > 0 {
        let used_width = (bar_width * used) / total;
        if used_width > 0 {
            draw_rect(fb_base, screen_width, bar_x, current_y, used_width, bar_height, 0xFF0000);
        }
    }

    // 枠線
    draw_rect_outline(fb_base, screen_width, bar_x, current_y, bar_width, bar_height, 0xFFFFFF);

    // 数値表示（KB単位）
    let stats_text = format!("{}/{} KB", used / 1024, total / 1024);
    draw_string(fb_base, screen_width, bar_x + bar_width + 10, current_y, &stats_text, 0xFFFFFF);

    current_y += bar_height + 10;

    // 凡例を表示
    current_y += 10;
    draw_rect(fb_base, screen_width, start_x, current_y, 20, 10, 0xFF0000);
    draw_string(fb_base, screen_width, start_x + 25, current_y, "= Used", 0xFFFFFF);
    draw_rect(fb_base, screen_width, start_x + 100, current_y, 20, 10, 0x004000);
    draw_string(fb_base, screen_width, start_x + 125, current_y, "= Free", 0xFFFFFF);
}

// アドレスをアラインメントに合わせて切り下げ（draw_allocator_stateで使用）
fn align_down(addr: usize, align: usize) -> usize {
    addr & !(align - 1)
}

// 画面左側にコードスニペットを表示
fn draw_code_snippet(writer: &mut FramebufferWriter, code_lines: &[&str]) {
    use graphics::{draw_rect, draw_string};

    let fb_base = writer.fb_base;
    let screen_width = writer.width;

    // 左側の領域をクリア
    draw_rect(fb_base, screen_width, 0, 280, 400, 320, 0x000000);

    let start_x = 10;
    let mut y = 290;

    // タイトル
    draw_string(fb_base, screen_width, start_x, y, "Code:", 0xFFFF00);
    y += 15;

    // コード行を描画
    for line in code_lines {
        draw_string(fb_base, screen_width, start_x, y, line, 0x00FFFF);
        y += 10;
    }
}

// 複数のサイズクラスをコンパクトに並べて表示
fn draw_memory_grids_multi(writer: &mut FramebufferWriter, title: &str) {
    use graphics::{draw_rect, draw_string};

    let allocator = allocator::get_allocator();
    let size_classes = allocator::get_size_classes();

    let fb_base = writer.fb_base;
    let screen_width = writer.width;

    // 右側の領域をクリア（x=400以降）
    draw_rect(fb_base, screen_width, 400, 280, 624, 320, 0x000000);

    // タイトルを描画
    draw_string(fb_base, screen_width, 410, 290, title, 0xFFFF00);

    let heap_size = 256 * 1024; // 256KB

    // 各サイズクラスを3列で並べて表示（最大6個まで）
    let grid_cols_per_class = 20; // 各グリッドは20x20セル
    let cell_size = 3; // 各セル3x3ピクセル
    let grid_pixel_size = grid_cols_per_class * (cell_size + 1); // 約80ピクセル

    let start_x = 410;
    let start_y = 310;
    let classes_to_show = 6.min(size_classes.len()); // 画面に収まる範囲で6個まで

    for class_idx in 0..classes_to_show {
        let size = size_classes[class_idx];
        let slab_size = (heap_size / 2) / size_classes.len();
        let aligned_size = align_down(slab_size, size);
        let total_blocks = aligned_size / size;

        let free_count = allocator.count_free_blocks(class_idx);
        let used_count = total_blocks - free_count;

        // グリッドの位置を計算（3列レイアウト）
        let col = class_idx % 3;
        let row = class_idx / 3;
        let grid_x = start_x + col * (grid_pixel_size + 20);
        let grid_y = start_y + row * (grid_pixel_size + 35);

        // サイズクラスラベル
        let label = format!("{}B", size);
        draw_string(fb_base, screen_width, grid_x, grid_y - 12, &label, 0xFFFFFF);

        // グリッドを描画（最大400ブロックまで = 20x20）
        let max_display = (grid_cols_per_class * grid_cols_per_class).min(total_blocks);

        for i in 0..max_display {
            let grid_row = i / grid_cols_per_class;
            let grid_col = i % grid_cols_per_class;

            let x = grid_x + grid_col * (cell_size + 1);
            let y = grid_y + grid_row * (cell_size + 1);

            let color = if i < used_count {
                0xFF0000 // 赤: 使用中
            } else {
                0x00FF00 // 緑: 空き
            };

            draw_rect(fb_base, screen_width, x, y, cell_size, cell_size, color);
        }

        // 使用率を表示
        let usage_pct = if total_blocks > 0 {
            (used_count * 100) / total_blocks
        } else {
            0
        };
        let usage = format!("{}%", usage_pct);
        draw_string(fb_base, screen_width, grid_x + 25, grid_y + grid_pixel_size + 3, &usage, 0xAAAAAA);
    }

    // 凡例
    let legend_y = start_y + 2 * (grid_pixel_size + 35) + 5;
    draw_rect(fb_base, screen_width, start_x, legend_y, 8, 8, 0xFF0000);
    draw_string(fb_base, screen_width, start_x + 12, legend_y, "Used", 0xFFFFFF);
    draw_rect(fb_base, screen_width, start_x + 60, legend_y, 8, 8, 0x00FF00);
    draw_string(fb_base, screen_width, start_x + 72, legend_y, "Free", 0xFFFFFF);
}

// 特定サイズクラスのメモリブロックをグリッド表示（旧バージョン、未使用）
fn draw_memory_grid(writer: &mut FramebufferWriter, class_idx: usize, label: &str) {
    use graphics::{draw_rect, draw_string};

    let allocator = allocator::get_allocator();
    let size_classes = allocator::get_size_classes();

    if class_idx >= size_classes.len() {
        return;
    }

    let size = size_classes[class_idx];
    let fb_base = writer.fb_base;
    let screen_width = writer.width;

    // 固定位置に描画
    let start_x = 500;
    let start_y = 30;

    // 描画領域をクリア
    draw_rect(fb_base, screen_width, start_x - 10, start_y - 10, 700, 450, 0x000000);

    // タイトル
    let title = format!("{} - {}B blocks", label, size);
    draw_string(fb_base, screen_width, start_x, start_y, &title, 0xFFFF00);

    // 総ブロック数を計算
    let heap_size = 256 * 1024; // 256KB
    let slab_size = (heap_size / 2) / size_classes.len();
    let aligned_size = align_down(slab_size, size);
    let total_blocks = aligned_size / size;

    // グリッドサイズを計算（1ブロック = 1セル、最大40x40グリッド）
    let max_display = 1600; // 40x40
    let blocks_to_display = total_blocks.min(max_display);
    let grid_cols = 40;
    let grid_rows = (blocks_to_display + grid_cols - 1) / grid_cols;
    let cell_size = 8; // 各セルは8x8ピクセル

    let grid_y = start_y + 20;

    // 簡易的に: 最初のN個が使用中、残りが空きと仮定
    // （実際にはフリーリストを辿る必要がある）
    let free_count = allocator.count_free_blocks(class_idx);
    let used_count = total_blocks - free_count;

    // グリッドを描画
    for row in 0..grid_rows {
        for col in 0..grid_cols {
            let block_idx = row * grid_cols + col;
            if block_idx >= blocks_to_display {
                break;
            }

            let x = start_x + col * (cell_size + 1);
            let y = grid_y + row * (cell_size + 1);

            // 使用中 vs 空き（簡略版: 先頭から順に使用中と仮定）
            let color = if block_idx < used_count {
                0xFF0000 // 赤: 使用中
            } else {
                0x00FF00 // 緑: 空き
            };

            draw_rect(fb_base, screen_width, x, y, cell_size, cell_size, color);
        }
    }

    // 凡例と統計
    let legend_y = grid_y + grid_rows * (cell_size + 1) + 10;
    draw_rect(fb_base, screen_width, start_x, legend_y, 10, 10, 0xFF0000);
    draw_string(fb_base, screen_width, start_x + 15, legend_y, "= Used", 0xFFFFFF);
    draw_rect(fb_base, screen_width, start_x + 100, legend_y, 10, 10, 0x00FF00);
    draw_string(fb_base, screen_width, start_x + 115, legend_y, "= Free", 0xFFFFFF);

    let stats = format!("Total: {} blocks, Used: {}, Free: {}",
                       total_blocks, used_count, free_count);
    draw_string(fb_base, screen_width, start_x, legend_y + 20, &stats, 0xFFFFFF);

    let display_note = format!("Showing {} / {} blocks", blocks_to_display, total_blocks);
    draw_string(fb_base, screen_width, start_x, legend_y + 40, &display_note, 0xAAAAAA);
}
