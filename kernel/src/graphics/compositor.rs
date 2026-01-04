//! Compositor - 各Writerのバッファを合成してフレームバッファに描画

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use lazy_static::lazy_static;
use spin::Mutex as SpinMutex;

/// フレームカウント（Compositorが描画したフレーム数）
static FRAME_COUNT: AtomicU64 = AtomicU64::new(0);

/// 画面幅
static SCREEN_WIDTH: AtomicU32 = AtomicU32::new(0);

/// 画面高さ
static SCREEN_HEIGHT: AtomicU32 = AtomicU32::new(0);

use super::buffer::{DrawCommand, SharedBuffer};
use super::region::Region;
use super::shadow_buffer::ShadowBuffer;

/// Compositorの設定
#[derive(Clone)]
pub struct CompositorConfig {
    /// フレームバッファのベースアドレス
    pub fb_base: u64,
    /// フレームバッファの幅
    pub fb_width: u32,
    /// フレームバッファの高さ
    pub fb_height: u32,
    /// リフレッシュ間隔（tick数）
    #[allow(dead_code)]
    pub refresh_interval_ticks: u64,
}

/// Compositor（シングルトン）
///
/// 全てのWriterバッファを管理します。
/// シャドウバッファはcompositor_task()内でローカルに所有し、
/// トリプルバッファリングを実現します。
pub struct Compositor {
    /// 設定
    config: CompositorConfig,
    /// 登録されたバッファのリスト（Copy-on-Write方式でスナップショット取得可能）
    buffers: Arc<Vec<SharedBuffer>>,
}

impl Compositor {
    /// 新しいCompositorを作成
    ///
    /// # Arguments
    /// * `config` - Compositorの設定
    pub fn new(config: CompositorConfig) -> Self {
        Self {
            config,
            buffers: Arc::new(Vec::new()),
        }
    }

    /// 新しいWriterを登録し、そのバッファへの参照を返す
    ///
    /// Copy-on-Write方式: 新しいVecを作成してバッファを追加し、Arcを置き換えます。
    /// これにより、既存のスナップショットは影響を受けません。
    ///
    /// # Arguments
    /// * `region` - Writer用の描画領域
    ///
    /// # Returns
    /// 共有バッファへの参照
    pub fn register_writer(&mut self, region: Region) -> SharedBuffer {
        let buffer = Arc::new(crate::sync::BlockingMutex::new(
            super::buffer::WriterBuffer::new(region),
        ));

        // Copy-on-Write: 新しいVecを作成して追加
        let mut new_buffers = Vec::clone(&self.buffers);
        #[cfg(feature = "visualize-pipeline")]
        let buffer_index = new_buffers.len();
        new_buffers.push(Arc::clone(&buffer));
        self.buffers = Arc::new(new_buffers);

        // 可視化モード: バッファインデックスを設定
        #[cfg(feature = "visualize-pipeline")]
        {
            buffer.lock().set_vis_buffer_index(buffer_index);
        }

        buffer
    }

    /// バッファリストのスナップショットを取得
    ///
    /// Arcのクローンを返すため、非常に高速です。
    /// スナップショット取得後にregister_writer()が呼ばれても、
    /// スナップショットは影響を受けません（Copy-on-Write）。
    pub fn get_buffers_snapshot(&self) -> Arc<Vec<SharedBuffer>> {
        Arc::clone(&self.buffers)
    }

    /// フレームバッファ設定を取得
    pub fn get_config(&self) -> &CompositorConfig {
        &self.config
    }
}

/// コマンドをシャドウバッファに描画（Compositorから独立した関数）
///
/// compositor_task()内でローカルに所有するシャドウバッファに描画します。
/// これにより、割り込み有効状態で描画処理を実行できます。
///
/// # Arguments
/// * `shadow_buffer` - 描画先のシャドウバッファ
/// * `region` - 描画領域
/// * `commands` - 描画コマンドのスライス
fn render_commands_to(shadow_buffer: &mut ShadowBuffer, region: &Region, commands: &[DrawCommand]) {
    let shadow_base = shadow_buffer.base_addr();
    let shadow_width = shadow_buffer.width();

    for cmd in commands {
        match cmd {
            DrawCommand::Clear { color } => {
                // 領域全体をクリア
                unsafe {
                    super::draw_rect(
                        shadow_base,
                        shadow_width,
                        region.x as usize,
                        region.y as usize,
                        region.width as usize,
                        region.height as usize,
                        *color,
                    );
                }
                shadow_buffer.mark_dirty(region);
            }
            DrawCommand::DrawChar { x, y, ch, color } => {
                // ローカル座標をグローバル座標に変換
                let global_x = region.x + x;
                let global_y = region.y + y;
                unsafe {
                    super::draw_char(
                        shadow_base,
                        shadow_width,
                        global_x as usize,
                        global_y as usize,
                        *ch,
                        *color,
                    );
                }
                // 8x8文字のdirty rect
                shadow_buffer.mark_dirty(&Region::new(global_x, global_y, 8, 8));
            }
            DrawCommand::DrawString { x, y, text, color } => {
                let global_x = region.x + x;
                let global_y = region.y + y;
                unsafe {
                    super::draw_string(
                        shadow_base,
                        shadow_width,
                        global_x as usize,
                        global_y as usize,
                        text,
                        *color,
                    );
                }
                // 文字列全体のdirty rect（幅 = 文字数 * 8）
                let text_width = (text.len() as u32) * 8;
                shadow_buffer.mark_dirty(&Region::new(global_x, global_y, text_width, 8));
            }
            DrawCommand::FillRect {
                x,
                y,
                width,
                height,
                color,
            } => {
                let global_x = region.x + x;
                let global_y = region.y + y;
                unsafe {
                    super::draw_rect(
                        shadow_base,
                        shadow_width,
                        global_x as usize,
                        global_y as usize,
                        *width as usize,
                        *height as usize,
                        *color,
                    );
                }
                shadow_buffer.mark_dirty(&Region::new(global_x, global_y, *width, *height));
            }
        }
    }
}

/// ミニバッファ用のレンダリング（可視化機能用）
///
/// スケールに応じてコマンドをミニバッファに描画
/// 戻り値: 描画されたdirty region (x, y, w, h) in mini-buffer coordinates
#[cfg(feature = "visualize-pipeline")]
fn render_command_to_mini(
    mini_buffer: &mut crate::pipeline_visualization::MiniBuffer,
    region: &Region,
    cmd: &DrawCommand,
    screen_width: u32,
    screen_height: u32,
) -> (u32, u32, u32, u32) {
    // ミニバッファサイズに基づくスケール変換
    let mini_w = mini_buffer.width;
    let mini_h = mini_buffer.height;
    let scale_x = |x: u32| -> usize { (x as usize * mini_w) / screen_width as usize };
    let scale_y = |y: u32| -> usize { (y as usize * mini_h) / screen_height as usize };
    let scale_w = |w: u32| -> usize { ((w as usize * mini_w) / screen_width as usize).max(1) };
    let scale_h = |h: u32| -> usize { ((h as usize * mini_h) / screen_height as usize).max(1) };

    match cmd {
        DrawCommand::Clear { color } => {
            let x = scale_x(region.x);
            let y = scale_y(region.y);
            let w = scale_w(region.width);
            let h = scale_h(region.height);
            mini_buffer.draw_rect(x, y, w, h, *color);
            (x as u32, y as u32, w as u32, h as u32)
        }
        DrawCommand::FillRect {
            x,
            y,
            width,
            height,
            color,
        } => {
            let global_x = region.x + x;
            let global_y = region.y + y;
            let sx = scale_x(global_x);
            let sy = scale_y(global_y);
            let sw = scale_w(*width);
            let sh = scale_h(*height);
            mini_buffer.draw_rect(sx, sy, sw, sh, *color);
            (sx as u32, sy as u32, sw as u32, sh as u32)
        }
        DrawCommand::DrawString { x, y, text, color } => {
            // 文字列は点として表現（スケールが小さいため）
            let global_x = region.x + x;
            let global_y = region.y + y;
            let sx = scale_x(global_x);
            let sy = scale_y(global_y);
            let text_width = (text.len() as u32) * 8;
            let sw = scale_w(text_width).max(2);
            mini_buffer.draw_rect(sx, sy, sw, 2, *color);
            (sx as u32, sy as u32, sw as u32, 2)
        }
        DrawCommand::DrawChar { x, y, color, .. } => {
            let global_x = region.x + x;
            let global_y = region.y + y;
            let sx = scale_x(global_x);
            let sy = scale_y(global_y);
            mini_buffer.draw_rect(sx, sy, 2, 2, *color);
            (sx as u32, sy as u32, 2, 2)
        }
    }
}

// グローバルCompositorインスタンス
lazy_static! {
    /// グローバルCompositorインスタンス
    /// 初期化前はNone
    static ref COMPOSITOR: SpinMutex<Option<Compositor>> = SpinMutex::new(None);
}

/// Compositorを初期化
///
/// # Arguments
/// * `config` - Compositorの設定
pub fn init_compositor(config: CompositorConfig) {
    // 画面サイズをグローバル変数に保存
    SCREEN_WIDTH.store(config.fb_width, Ordering::Relaxed);
    SCREEN_HEIGHT.store(config.fb_height, Ordering::Relaxed);

    let mut comp = COMPOSITOR.lock();
    *comp = Some(Compositor::new(config));
}

/// フレームカウントを取得
///
/// Compositorが描画したフレーム数を返します。
pub fn frame_count() -> u64 {
    FRAME_COUNT.load(Ordering::Relaxed)
}

/// 画面サイズを取得
///
/// # Returns
/// (幅, 高さ) のタプル
pub fn screen_size() -> (u32, u32) {
    (
        SCREEN_WIDTH.load(Ordering::Relaxed),
        SCREEN_HEIGHT.load(Ordering::Relaxed),
    )
}

/// フレームバッファのベースアドレスを取得
///
/// # Returns
/// フレームバッファのベースアドレス。Compositorが未初期化なら0
#[cfg(feature = "visualize-pipeline")]
pub fn fb_base() -> u64 {
    let comp = COMPOSITOR.lock();
    comp.as_ref().map(|c| c.get_config().fb_base).unwrap_or(0)
}

/// 新しいWriterを登録（タスク作成時に呼ばれる）
///
/// # Arguments
/// * `region` - Writer用の描画領域
///
/// # Returns
/// 共有バッファへの参照。Compositorが未初期化ならNone
///
/// # Note
/// 割り込みを無効化してロックを取得することで、
/// ロック保持中にプリエンプトされることを防ぎます。
pub fn register_writer(region: Region) -> Option<SharedBuffer> {
    // 可視化モード: 現在のタスクIDを取得
    #[cfg(feature = "visualize-pipeline")]
    let task_id = crate::sched::current_task_id().as_u64();

    // 割り込みを無効化してロック取得（スピンロック競合回避）
    let flags = unsafe {
        let flags: u64;
        core::arch::asm!(
            "pushfq",
            "pop {}",
            "cli",
            out(reg) flags,
            options(nomem, nostack)
        );
        flags
    };

    let result = {
        let mut comp = COMPOSITOR.lock();
        comp.as_mut().map(|c| c.register_writer(region))
    };

    // 割り込みを元の状態に復元
    unsafe {
        if flags & 0x200 != 0 {
            core::arch::asm!("sti", options(nomem, nostack));
        }
    }

    // 可視化モード: バッファに所有タスクIDを設定
    #[cfg(feature = "visualize-pipeline")]
    if let Some(ref buffer) = result {
        if let Some(mut buf) = buffer.try_lock() {
            buf.set_owner_task_id(task_id);
        }
    }

    result
}

/// Compositorタスクのエントリポイント
///
/// ダブルバッファリング方式でフレームを合成します。
/// - HWフレームバッファ: モニター表示中
/// - シャドウバッファ: レンダリング先 → HWへ転送
///
/// 割り込み無効時間を最小化（スナップショット取得時のみ）し、
/// レンダリングとblitは割り込み有効状態で実行します。
pub extern "C" fn compositor_task() -> ! {
    crate::info!("[Compositor] Started (double buffering)");

    // 初期化: 設定を取得（短いクリティカルセクション）
    let config = {
        let flags = unsafe {
            let flags: u64;
            core::arch::asm!(
                "pushfq",
                "pop {}",
                "cli",
                out(reg) flags,
                options(nomem, nostack)
            );
            flags
        };

        let cfg = {
            let comp = COMPOSITOR.lock();
            comp.as_ref().map(|c| c.get_config().clone())
        };

        unsafe {
            if flags & 0x200 != 0 {
                core::arch::asm!("sti", options(nomem, nostack));
            }
        }

        cfg.expect("Compositor not initialized")
    };

    // シャドウバッファをタスクローカルで所有（ダブルバッファリング）
    let mut shadow_buffer = ShadowBuffer::new(config.fb_width, config.fb_height);

    crate::info!(
        "[Compositor] Shadow buffer initialized: {}x{}",
        config.fb_width,
        config.fb_height
    );

    loop {
        // Phase 1: バッファリストのスナップショット取得（割り込み無効、数μs）
        let buffers_snapshot = {
            let flags = unsafe {
                let flags: u64;
                core::arch::asm!(
                    "pushfq",
                    "pop {}",
                    "cli",
                    out(reg) flags,
                    options(nomem, nostack)
                );
                flags
            };

            let snapshot = {
                let comp = COMPOSITOR.lock();
                comp.as_ref().map(|c| c.get_buffers_snapshot())
            };

            unsafe {
                if flags & 0x200 != 0 {
                    core::arch::asm!("sti", options(nomem, nostack));
                }
            }

            match snapshot {
                Some(s) => s,
                None => {
                    crate::sched::sleep_ms(16);
                    continue;
                }
            }
        };

        // Phase 2+3: 各バッファから直接レンダリング（アロケーションフリー）
        // ロックを取得したままレンダリングし、終わったらクリア

        // 可視化モード判定
        #[cfg(feature = "visualize-pipeline")]
        let vis_mode = {
            use crate::pipeline_visualization::MINI_VIS_STATE;
            MINI_VIS_STATE.lock().is_some()
        };
        #[cfg(not(feature = "visualize-pipeline"))]
        let vis_mode = false;

        // 可視化モード: バッファ数のみ更新（command_typesはflush()で更新される）
        #[cfg(feature = "visualize-pipeline")]
        if vis_mode {
            use crate::pipeline_visualization::MINI_VIS_STATE;
            if let Some(ref mut vis_state) = *MINI_VIS_STATE.lock() {
                vis_state.buffer_count = buffers_snapshot.len().min(4);
            }
        }

        for (buffer_idx, buffer) in buffers_snapshot.iter().enumerate() {
            if let Some(mut buf) = buffer.try_lock() {
                if buf.is_dirty() {
                    let region = buf.region();
                    let commands = buf.commands();

                    if vis_mode {
                        // 可視化モード: ミニシャドウバッファに描画（シャドウバッファの代わり）
                        #[cfg(feature = "visualize-pipeline")]
                        {
                            use crate::pipeline_visualization::{
                                CommandInfo, MINI_VIS_STATE, PipelinePhase,
                            };
                            // コマンドをローカルにコピー（ロック解放のため）
                            let commands_copy: alloc::vec::Vec<_> =
                                commands.iter().cloned().collect();
                            drop(buf); // バッファのロックを解放

                            // このバッファの処理開始: アニメーション開始時刻を記録
                            if let Some(ref mut vis_state) = *MINI_VIS_STATE.lock() {
                                if buffer_idx < 4 {
                                    vis_state.buffer_queues[buffer_idx].anim_start_tick =
                                        vis_state.animation_tick;
                                    vis_state.buffer_queues[buffer_idx].is_processing = true;
                                    // 進捗情報を初期化
                                    vis_state.buffer_queues[buffer_idx].total_commands =
                                        commands_copy.len();
                                    vis_state.buffer_queues[buffer_idx].processed_count = 0;
                                    // バッファコピーアニメーションを開始
                                    vis_state.start_buffer_copy_animation(
                                        buffer_idx,
                                        commands_copy.len(),
                                    );
                                }
                            }

                            // バッファコピーアニメーション完了を待機
                            loop {
                                let copy_done = {
                                    if let Some(ref vis_state) = *MINI_VIS_STATE.lock() {
                                        vis_state.buffer_copy_animation.is_none()
                                    } else {
                                        true
                                    }
                                };
                                if copy_done {
                                    break;
                                }
                                crate::sched::sleep_ms(16);
                            }

                            // コピー完了: コマンドをCompositor内部に移動
                            // command_types → compositor_commands にコピー後、command_typesをクリア
                            if let Some(ref mut vis_state) = *MINI_VIS_STATE.lock() {
                                if buffer_idx < 4 {
                                    // Compositor内部表示用にコピー
                                    vis_state.buffer_queues[buffer_idx].compositor_commands =
                                        vis_state.buffer_queues[buffer_idx].command_types;
                                    // タスクのバッファ表示を空にする
                                    vis_state.buffer_queues[buffer_idx].pending_commands = 0;
                                    vis_state.buffer_queues[buffer_idx].command_types = [None; 5];
                                }
                            }

                            // コピー完了後、コマンド処理開始前にウェイト（可視化用）
                            crate::sched::sleep_ms(500);

                            for (cmd_idx, cmd) in commands_copy.iter().enumerate() {
                                // コマンドタイプを取得
                                let cmd_type: &'static str = match cmd {
                                    DrawCommand::Clear { .. } => "Clear",
                                    DrawCommand::FillRect { .. } => "FillRect",
                                    DrawCommand::DrawString { .. } => "DrawString",
                                    DrawCommand::DrawChar { .. } => "DrawChar",
                                };

                                // コマンドを処理（ミニシャドウバッファに描画）
                                if let Some(ref mut vis_state) = *MINI_VIS_STATE.lock() {
                                    vis_state.phase = PipelinePhase::Rendering;
                                    let (dx, dy, dw, dh) = render_command_to_mini(
                                        &mut vis_state.mini_shadow,
                                        &region,
                                        cmd,
                                        config.fb_width,
                                        config.fb_height,
                                    );
                                    // dirty regionを累積
                                    vis_state.expand_dirty(dx, dy, dw, dh);
                                    // 最新コマンド情報を記録
                                    vis_state.current_command = Some(CommandInfo {
                                        command_type: cmd_type,
                                        region_x: region.x,
                                        region_y: region.y,
                                    });
                                    vis_state.command_count += 1;
                                    // 処理済みカウントをインクリメント
                                    if buffer_idx < 4 {
                                        vis_state.buffer_queues[buffer_idx].processed_count =
                                            cmd_idx + 1;
                                    }
                                }

                                // 各コマンド処理後に待機（アニメーション可視化用、0.5秒）
                                crate::sched::sleep_ms(500);
                            }

                            // このバッファの処理完了
                            if let Some(ref mut vis_state) = *MINI_VIS_STATE.lock() {
                                if buffer_idx < 4 {
                                    vis_state.buffer_queues[buffer_idx].is_processing = false;
                                    // Compositor内部表示用の情報をリセット
                                    vis_state.buffer_queues[buffer_idx].compositor_commands =
                                        [None; 5];
                                    vis_state.buffer_queues[buffer_idx].processed_count = 0;
                                    vis_state.buffer_queues[buffer_idx].total_commands = 0;
                                }
                            }

                            // バッファを再ロックしてクリア
                            if let Some(mut buf) = buffer.try_lock() {
                                buf.clear_commands();
                                // 所有タスクを起床
                                if let Some(id) = buf.owner_task_id() {
                                    drop(buf);
                                    crate::sched::unblock_task(crate::sched::TaskId::from_u64(id));
                                }
                            }
                            continue; // 次のバッファへ
                        }
                    } else {
                        // 通常モード: シャドウバッファに描画
                        render_commands_to(&mut shadow_buffer, &region, commands);
                    }

                    // 可視化モード: 所有タスクを起床（処理完了通知）
                    #[cfg(feature = "visualize-pipeline")]
                    let owner_id = if vis_mode { buf.owner_task_id() } else { None };

                    // 容量を維持したままクリア（再アロケーションなし）
                    buf.clear_commands();

                    // 可視化モード: バッファのロック解放後にタスクを起床
                    #[cfg(feature = "visualize-pipeline")]
                    if let Some(id) = owner_id {
                        drop(buf);
                        crate::sched::unblock_task(crate::sched::TaskId::from_u64(id));
                        continue; // bufはdropされたので次のバッファへ
                    }
                }
            }
        }

        // Phase 4: シャドウバッファをハードウェアFBに転送（割り込み有効）
        // dirty_rectがある場合のみ転送され、転送後にdirty_rectはクリアされる
        if !vis_mode {
            let _blitted = unsafe { shadow_buffer.blit_to(config.fb_base) };
        }

        // 可視化モード: ミニシャドウ → ミニFB へのblitアニメーション開始
        #[cfg(feature = "visualize-pipeline")]
        if vis_mode {
            use crate::pipeline_visualization::{MINI_VIS_STATE, PipelinePhase};
            if let Some(ref mut vis_state) = *MINI_VIS_STATE.lock() {
                vis_state.phase = PipelinePhase::Blit;
                // blitアニメーションを開始（実際のblitはtick_animationで完了時に実行）
                vis_state.start_blit_animation_from_cumulative();
            }

            // Blitアニメーション完了を待機
            loop {
                let blit_done = {
                    if let Some(ref vis_state) = *MINI_VIS_STATE.lock() {
                        vis_state.blit_animation.is_none()
                    } else {
                        true
                    }
                };
                if blit_done {
                    break;
                }
                crate::sched::sleep_ms(16);
            }

            // Blit完了後にフェーズをIdleに戻す
            if let Some(ref mut vis_state) = *MINI_VIS_STATE.lock() {
                vis_state.phase = PipelinePhase::Idle;
            }
        }

        FRAME_COUNT.fetch_add(1, Ordering::Relaxed);

        // 次のリフレッシュまで待機（約60fps = 16ms間隔）
        crate::sched::sleep_ms(16);
    }
}
