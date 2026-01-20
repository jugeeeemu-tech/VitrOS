// =============================================================================
// メモリアロケータ可視化機能
// cargo build --release --features visualize-allocator でビルドした場合のみ有効
// =============================================================================
//
// AllocatorObserverパターンを実装し、アロケータからの通知を受け取ります。
// SlabAllocatorはconst fn new()が必要なためジェネリクス化できませんが、
// フック関数 + 条件付きコンパイルでオブザーバーパターンを実現しています。

extern crate alloc;

use crate::allocator;
use crate::graphics::{draw_rect, draw_string};
use crate::info;
use alloc::format;
use core::arch::asm;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

// =============================================================================
// Observer側での状態管理
// アロケータからの通知を受けて使用中ブロック数を追跡
// =============================================================================

/// 各サイズクラスの使用中ブロック数
static USED_COUNTS: [AtomicUsize; 10] = [
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
];

/// フレームバッファベースアドレス
static FB_BASE: AtomicU64 = AtomicU64::new(0);
/// 画面幅
static SCREEN_WIDTH: AtomicU64 = AtomicU64::new(0);
/// 画面高さ
static SCREEN_HEIGHT: AtomicU64 = AtomicU64::new(0);

// =============================================================================
// AllocatorObserver フック関数
// allocator.rsから呼び出される
// =============================================================================

/// アロケート時のフック関数
///
/// # Arguments
/// * `class_idx` - サイズクラスのインデックス
/// * `ptr` - 割り当てられたポインタ
#[inline(always)]
pub fn on_allocate_hook(class_idx: usize, _ptr: *mut u8) {
    if class_idx < USED_COUNTS.len() {
        USED_COUNTS[class_idx].fetch_add(1, Ordering::Relaxed);
    }
}

/// デアロケート時のフック関数
///
/// # Arguments
/// * `class_idx` - サイズクラスのインデックス
/// * `ptr` - 解放されるポインタ
#[inline(always)]
pub fn on_deallocate_hook(class_idx: usize, _ptr: *mut u8) {
    if class_idx < USED_COUNTS.len() {
        let _ =
            USED_COUNTS[class_idx].fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_sub(1)
            });
    }
}

/// 指定サイズクラスの空きブロック数を計算
///
/// # Arguments
/// * `class_idx` - サイズクラスのインデックス
/// * `total_blocks` - サイズクラスの総ブロック数
fn count_free_blocks(class_idx: usize, total_blocks: usize) -> usize {
    let used = USED_COUNTS[class_idx].load(Ordering::Relaxed);
    total_blocks.saturating_sub(used)
}

// =============================================================================
// Framebuffer Observer フック関数
// main.rsから呼び出される
// =============================================================================

/// フレームバッファ初期化フック
///
/// # Arguments
/// * `fb_base` - フレームバッファベースアドレス
/// * `width` - 画面幅
/// * `height` - 画面高さ
pub fn on_framebuffer_init_hook(fb_base: u64, width: u32, height: u32) {
    FB_BASE.store(fb_base, Ordering::Relaxed);
    SCREEN_WIDTH.store(width as u64, Ordering::Relaxed);
    SCREEN_HEIGHT.store(height as u64, Ordering::Relaxed);
}

/// フレームバッファベースアドレスを取得
fn fb_base() -> u64 {
    FB_BASE.load(Ordering::Relaxed)
}

/// 画面サイズを取得
fn screen_size() -> (u32, u32) {
    (
        SCREEN_WIDTH.load(Ordering::Relaxed) as u32,
        SCREEN_HEIGHT.load(Ordering::Relaxed) as u32,
    )
}

// =============================================================================
// ユーティリティ関数
// =============================================================================

// アドレスをアラインメントに合わせて切り下げ
fn align_down(addr: usize, align: usize) -> usize {
    addr & !(align - 1)
}

// =============================================================================
// 描画関数
// =============================================================================

// 画面左側にコードスニペットを表示
pub fn draw_code_snippet(code_lines: &[&str]) {
    let fb_base = fb_base();
    let (screen_width, _) = screen_size();

    // 左側の領域をクリア
    // SAFETY: fb_baseはFramebufferWriterから取得した有効なフレームバッファアドレス。
    // 描画範囲(0, 280, 400, 320)は1024x768の画面サイズ内に収まる。
    unsafe {
        draw_rect(fb_base, screen_width, 0, 280, 400, 320, 0x000000);
    }

    let start_x = 10;
    let mut y = 290;

    // タイトル
    // SAFETY: fb_baseは有効なフレームバッファアドレス。
    // start_x=10, y=305程度は画面サイズ内に収まる。
    unsafe {
        draw_string(fb_base, screen_width, start_x, y, "Code:", 0xFFFF00);
    }
    y += 15;

    // コード行を描画
    for line in code_lines {
        // SAFETY: fb_baseは有効なフレームバッファアドレス。
        // start_x=10, yは320から増加するが画面サイズ内に収まる。
        unsafe {
            draw_string(fb_base, screen_width, start_x, y, line, 0x00FFFF);
        }
        y += 10;
    }
}

// 複数のサイズクラスをコンパクトに並べて表示
pub fn draw_memory_grids_multi(title: &str) {
    let size_classes = allocator::SIZE_CLASSES;

    let fb_base = fb_base();
    let (screen_width, _) = screen_size();

    // 右側の領域をクリア（x=400以降）
    // SAFETY: fb_baseはFramebufferWriterから取得した有効なフレームバッファアドレス。
    // 描画範囲(400, 280, 624, 320)は1024x768の画面サイズ内に収まる。
    unsafe {
        draw_rect(fb_base, screen_width, 400, 280, 624, 320, 0x000000);
    }

    // タイトルを描画
    // SAFETY: fb_baseは有効なフレームバッファアドレス。
    // 座標(410, 290)は画面サイズ内に収まる。
    unsafe {
        draw_string(fb_base, screen_width, 410, 290, title, 0xFFFF00);
    }

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

        let free_count = count_free_blocks(class_idx, total_blocks);
        let used_count = total_blocks.saturating_sub(free_count);

        // グリッドの位置を計算（3列レイアウト）
        let col = class_idx % 3;
        let row = class_idx / 3;
        let grid_x = start_x + col * (grid_pixel_size + 20);
        let grid_y = start_y + row * (grid_pixel_size + 35);

        // サイズクラスラベル
        let label = format!("{}B", size);
        // SAFETY: fb_baseは有効なフレームバッファアドレス。
        // grid_x, grid_yは画面レイアウト内で計算され、境界内に収まる。
        unsafe {
            draw_string(fb_base, screen_width, grid_x, grid_y - 12, &label, 0xFFFFFF);
        }

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

            // SAFETY: fb_baseは有効なフレームバッファアドレス。
            // x, yはgrid_x/grid_yから計算され、cell_size=3なので境界内に収まる。
            unsafe {
                draw_rect(fb_base, screen_width, x, y, cell_size, cell_size, color);
            }
        }

        // 使用率を表示
        let usage_pct = if total_blocks > 0 {
            (used_count * 100) / total_blocks
        } else {
            0
        };
        let usage = format!("{}%", usage_pct);
        // SAFETY: fb_baseは有効なフレームバッファアドレス。
        // grid_x+25, grid_y+grid_pixel_size+3は画面レイアウト内で計算され、境界内に収まる。
        unsafe {
            draw_string(
                fb_base,
                screen_width,
                grid_x + 25,
                grid_y + grid_pixel_size + 3,
                &usage,
                0xAAAAAA,
            );
        }
    }

    // 凡例
    let legend_y = start_y + 2 * (grid_pixel_size + 35) + 5;
    // SAFETY: fb_baseは有効なフレームバッファアドレス。
    // start_x=410, legend_yは画面下部だが1024x768の画面サイズ内に収まる。
    // 描画する矩形と文字列はいずれも小さく、境界を超えることはない。
    unsafe {
        draw_rect(fb_base, screen_width, start_x, legend_y, 8, 8, 0xFF0000);
        draw_string(
            fb_base,
            screen_width,
            start_x + 12,
            legend_y,
            "Used",
            0xFFFFFF,
        );
        draw_rect(
            fb_base,
            screen_width,
            start_x + 60,
            legend_y,
            8,
            8,
            0x00FF00,
        );
        draw_string(
            fb_base,
            screen_width,
            start_x + 72,
            legend_y,
            "Free",
            0xFFFFFF,
        );
    }
}

// =============================================================================
// 可視化テスト実行
// =============================================================================

/// メモリアロケータの可視化テストを実行
pub fn run_visualization_tests() {
    // スラブアロケータの可視化（複数サイズクラス表示）
    info!("=== Memory Allocator Visualization ===");

    // 初期状態を表示
    draw_code_snippet(&["// Initial state", "// No allocations yet"]);
    draw_memory_grids_multi("Initial State");
    crate::hpet::delay_ms(5000);

    // テスト1: 16Bクラス
    info!("\n=== Test 1: Vec<u8> (16B class) ===");

    let vec1: alloc::vec::Vec<u8> = (0..12).collect();

    draw_code_snippet(&[
        "let vec1: Vec<u8>",
        "  = (0..12).collect();",
        "",
        "// 12 x u8 = 12B",
        "// -> 16B size class",
    ]);
    draw_memory_grids_multi("After 16B alloc");
    info!("Allocated Vec<u8> (12 elements = 12B -> 16B)");
    crate::hpet::delay_ms(5000);

    // テスト2: 64Bクラス
    info!("\n=== Test 2: Vec<u8> (64B class) ===");

    let vec2: alloc::vec::Vec<u8> = (0..50).collect();

    draw_code_snippet(&[
        "let vec2: Vec<u8>",
        "  = (0..50).collect();",
        "",
        "// 50 x u8 = 50B",
        "// -> 64B size class",
    ]);
    draw_memory_grids_multi("After 16B + 64B");
    info!("Allocated Vec<u8> (50 elements = 50B -> 64B)");
    crate::hpet::delay_ms(5000);

    // テスト3: 128Bクラス
    info!("\n=== Test 3: Vec<u64> (128B class) ===");

    let vec3: alloc::vec::Vec<u64> = (0..10).collect();

    draw_code_snippet(&[
        "let vec3: Vec<u64>",
        "  = (0..10).collect();",
        "",
        "// 10 x u64 = 80B",
        "// -> 128B size class",
    ]);
    draw_memory_grids_multi("16B+64B+128B");
    info!("Allocated Vec<u64> (10 elements = 80B -> 128B)");
    crate::hpet::delay_ms(5000);

    // テスト4: 256Bクラスを追加
    info!("\n=== Test 4: Vec<u64> (256B class) ===");

    let vec4: alloc::vec::Vec<u8> = (0..25).collect();

    draw_code_snippet(&[
        "let vec4: Vec<u64>",
        "  = (0..25).collect();",
        "",
        "// 25 x u64 = 200B",
        "// -> 256B size class",
    ]);
    draw_memory_grids_multi("8B+16B+64B+128B");
    info!("Allocated Vec<u8> (25 elements = 200B -> 256B)");
    crate::hpet::delay_ms(5000);

    // テスト5: 8Bクラスを追加
    info!("\n=== Test 5: Vec<u8> (8B class) ===");

    let vec5: alloc::vec::Vec<u8> = (0..8).collect();

    draw_code_snippet(&[
        "let vec5: Vec<u8>",
        "  = (0..8).collect();",
        "",
        "// 8 x u8 = 8B",
        "// -> 8B size class",
    ]);
    draw_memory_grids_multi("All 5 sizes");
    info!("Allocated Vec<u64> (8 elements = 8B -> 8B)");
    crate::hpet::delay_ms(5000);

    // テスト6: 64Bと256Bを解放
    info!("\n=== Test 6: Free 64B and 256B ===");

    drop(vec2);
    drop(vec4);

    draw_code_snippet(&[
        "drop(vec2);",
        "drop(vec4);",
        "",
        "// Freed 64B and 256B",
        "// 8B + 16B + 128B remain",
    ]);
    draw_memory_grids_multi("After freeing 2");
    info!("Freed 64B and 256B blocks");
    crate::hpet::delay_ms(5000);

    // テスト7: 全て解放
    info!("\n=== Test 7: Free all ===");

    drop(vec1);
    drop(vec3);
    drop(vec5);

    draw_code_snippet(&[
        "drop(vec1);",
        "drop(vec3);",
        "drop(vec5);",
        "",
        "// All freed!",
    ]);
    draw_memory_grids_multi("All freed");
    info!("All blocks freed");
    crate::hpet::delay_ms(5000);
    loop {
        // SAFETY: hlt命令はCPUを低消費電力状態にする特権命令。
        // 次の割り込みで復帰するため、メモリ安全性に影響しない。
        // カーネルモード（Ring 0）で実行されることが前提。
        unsafe {
            asm!("hlt");
        }
    }
}
