//! デバッグオーバーレイ
//!
//! 画面右上にFPSやシステム情報を表示するデバッグオーバーレイを提供します。

use crate::graphics::{Region, TaskWriter, compositor};
use crate::timer;
use core::fmt::Write;

/// オーバーレイの幅（20文字 * 8px）
const OVERLAY_WIDTH: u32 = 160;

/// オーバーレイの高さ（6行 * 10px）
const OVERLAY_HEIGHT: u32 = 60;

/// 画面端からのマージン
const MARGIN: u32 = 10;

/// 更新間隔（ミリ秒）
const UPDATE_INTERVAL_MS: u64 = 1000;

/// デバッグオーバーレイタスクのエントリポイント
pub extern "C" fn debug_overlay_task() -> ! {
    crate::info!("[DebugOverlay] Started");

    // 画面サイズを取得
    let (screen_width, _screen_height) = compositor::screen_size();

    // 画面右上に配置
    let region = Region::new(
        screen_width - OVERLAY_WIDTH - MARGIN,
        MARGIN,
        OVERLAY_WIDTH,
        OVERLAY_HEIGHT,
    );

    let buffer = compositor::register_writer(region).expect("Failed to register debug overlay");
    let mut writer = TaskWriter::new(buffer, 0xFFFFFFFF); // 白色

    // FPS計算用の変数
    let mut last_tick = timer::current_tick();
    let mut last_frame_count = compositor::frame_count();

    loop {
        let current_tick = timer::current_tick();
        let current_frame_count = compositor::frame_count();

        // FPS計算: (フレーム差分) / (時間差分[秒])
        let tick_delta = current_tick.saturating_sub(last_tick);
        let frame_delta = current_frame_count.saturating_sub(last_frame_count);

        let frequency = timer::frequency_hz();
        let fps = if tick_delta > 0 && frequency > 0 {
            (frame_delta * frequency) / tick_delta
        } else {
            0
        };

        // Uptime計算（秒）
        let uptime_secs = if frequency > 0 {
            current_tick / frequency
        } else {
            0
        };

        // 画面をクリアして描画
        writer.clear(0x00000000); // 黒背景
        let _ = writeln!(writer, "vitrOS Debug");
        let _ = writeln!(writer, "-----------");
        let _ = writeln!(writer, "FPS: {}", fps);
        let _ = writeln!(writer, "Tick: {}", current_tick);
        let _ = writeln!(writer, "Uptime: {}s", uptime_secs);

        // 次の計算のために保存
        last_tick = current_tick;
        last_frame_count = current_frame_count;

        // 1秒待機
        crate::sched::sleep_ms(UPDATE_INTERVAL_MS);
    }
}
