// =============================================================================
// Compositor描画パイプライン可視化機能
// cargo build --release --features visualize-pipeline でビルドした場合のみ有効
// =============================================================================

extern crate alloc;

use crate::graphics::buffer::DrawCommand;
use crate::graphics::region::Region;
use crate::graphics::{draw_char, draw_rect, draw_rect_outline, draw_string};
use alloc::vec::Vec;

// =============================================================================
// 色定義
// =============================================================================

/// 背景色（暗い青）
const COLOR_BACKGROUND: u32 = 0x001020;
/// タイトル色（黄色）
const COLOR_TITLE: u32 = 0xFFFF00;
/// キュー枠色（明るい青）
const COLOR_QUEUE_BORDER: u32 = 0x6060FF;
/// シャドウバッファ枠色（明るい緑）
const COLOR_SHADOW_BORDER: u32 = 0x60FF60;
/// フレームバッファ枠色（明るい赤）
const COLOR_FB_BORDER: u32 = 0xFF6060;
/// コマンドテキスト色（白）
const COLOR_TEXT: u32 = 0xFFFFFF;
/// ハイライト色（シアン）
const COLOR_HIGHLIGHT: u32 = 0x00FFFF;
/// 矢印色（黄色）
const COLOR_ARROW: u32 = 0xFFFF00;
/// Compositor枠色（紫）
const COLOR_COMPOSITOR_BORDER: u32 = 0xAA60AA;

// =============================================================================
// グローバル可視化状態（Compositor連携用）
// =============================================================================

use spin::Mutex as SpinMutex;

/// パイプラインフェーズ
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PipelinePhase {
    Idle,
    Snapshot,
    Rendering,
    Blit,
}

/// コマンド情報
#[derive(Clone)]
pub struct CommandInfo {
    pub command_type: &'static str,
    pub region_x: u32,
    pub region_y: u32,
}

/// バッファキュー情報（各WriterBufferの状態）
#[derive(Clone, Default)]
pub struct BufferQueueInfo {
    /// バッファ名（タスク名など）
    pub name: &'static str,
    /// キュー内のコマンド数（タスクボックス表示用）
    pub pending_commands: usize,
    /// コマンドタイプのリスト（タスクボックス表示用、最新5件）
    pub command_types: [Option<&'static str>; 5],
    /// Compositor内部で処理中のコマンドタイプ（Compositor内部表示用）
    pub compositor_commands: [Option<&'static str>; 5],
    /// アニメーション開始時のtick（このパイプ独自のタイミング）
    pub anim_start_tick: u32,
    /// 処理中フラグ
    pub is_processing: bool,
    /// 処理済みコマンド数（Compositor内で何番目まで処理したか）
    pub processed_count: usize,
    /// 総コマンド数（処理開始時のコマンド数）
    pub total_commands: usize,
}

/// パイプ内を流れるコマンド（アニメーション用）
#[derive(Clone)]
pub struct FlowingCommand {
    /// どのバッファ（パイプ）に属するか
    pub buffer_index: usize,
    /// コマンドタイプ
    pub cmd_type: &'static str,
    /// パイプ内の位置（1.0 = 右端（開始）、0.0 = 左端（終点））
    pub position: f32,
    /// 終点に到達したか
    pub arrived: bool,
}

/// Blitアニメーション（Shadow → FB への転送を可視化）
#[derive(Clone)]
pub struct BlitAnimation {
    /// Dirty regionの位置（ミニバッファ内のスケールで、0-400, 0-300）
    pub dirty_x: u32,
    pub dirty_y: u32,
    pub dirty_w: u32,
    pub dirty_h: u32,
    /// アニメーション進行度（0.0 = Shadow側、1.0 = FB側に到達）
    pub progress: f32,
    /// アニメーション完了後にblitを実行するか
    pub pending_blit: bool,
}

/// バッファコピーアニメーション（Task → Compositor への転送を可視化）
#[derive(Clone)]
pub struct BufferCopyAnimation {
    /// コピー元のバッファインデックス
    pub source_buffer_index: usize,
    /// コピーするコマンド数
    pub command_count: usize,
    /// アニメーション進行度（0.0 = Task側、1.0 = Compositor内部に到達）
    pub progress: f32,
}

/// ミニチュア可視化状態
pub struct MiniVisualizationState {
    pub mini_shadow: MiniBuffer,
    pub mini_fb: MiniBuffer,
    pub phase: PipelinePhase,
    pub current_command: Option<CommandInfo>,
    /// 各バッファのキュー情報
    pub buffer_queues: [BufferQueueInfo; 4],
    /// バッファ数
    pub buffer_count: usize,
    /// パイプを流れるコマンド（アニメーション用）
    pub flowing_commands: [Option<FlowingCommand>; 8],
    /// Blitアニメーション（Shadow → FBの転送可視化）
    pub blit_animation: Option<BlitAnimation>,
    /// バッファコピーアニメーション（Task → Compositorの転送可視化）
    pub buffer_copy_animation: Option<BufferCopyAnimation>,
    /// 累積dirty region（レンダリング中に更新、blit時に使用）
    pub cumulative_dirty: Option<(u32, u32, u32, u32)>, // (x, y, w, h)
    /// アニメーションティック
    pub animation_tick: u32,
    /// コマンド追加クールダウン（次に追加可能になるまでのティック数）
    pub add_cooldown: u32,
    pub command_count: u64,
    pub frame_count: u64,
}

impl MiniVisualizationState {
    pub fn new() -> Self {
        Self {
            mini_shadow: MiniBuffer::new(MINI_WIDTH, MINI_HEIGHT),
            mini_fb: MiniBuffer::new(MINI_WIDTH, MINI_HEIGHT),
            phase: PipelinePhase::Idle,
            current_command: None,
            buffer_queues: [const {
                BufferQueueInfo {
                    name: "",
                    pending_commands: 0,
                    command_types: [None; 5],
                    compositor_commands: [None; 5],
                    anim_start_tick: 0,
                    is_processing: false,
                    processed_count: 0,
                    total_commands: 0,
                }
            }; 4],
            buffer_count: 0,
            flowing_commands: [const { None }; 8],
            blit_animation: None,
            buffer_copy_animation: None,
            cumulative_dirty: None,
            animation_tick: 0,
            add_cooldown: 0,
            command_count: 0,
            frame_count: 0,
        }
    }

    /// バッファコピーアニメーションを開始
    ///
    /// Compositorがタスクのバッファを処理し始めるときに呼び出す
    pub fn start_buffer_copy_animation(&mut self, buffer_index: usize, command_count: usize) {
        self.buffer_copy_animation = Some(BufferCopyAnimation {
            source_buffer_index: buffer_index,
            command_count,
            progress: 0.0,
        });
    }

    /// Blitアニメーションを開始（累積dirty regionを使用）
    ///
    /// 累積されたdirty regionでアニメーションを開始し、累積をクリアする
    pub fn start_blit_animation_from_cumulative(&mut self) {
        if let Some((x, y, w, h)) = self.cumulative_dirty.take() {
            self.blit_animation = Some(BlitAnimation {
                dirty_x: x,
                dirty_y: y,
                dirty_w: w,
                dirty_h: h,
                progress: 0.0,
                pending_blit: true,
            });
        }
    }

    /// 累積dirty regionを拡張
    ///
    /// レンダリング中に呼ばれ、dirty regionを累積する
    pub fn expand_dirty(&mut self, x: u32, y: u32, w: u32, h: u32) {
        match self.cumulative_dirty {
            Some((cx, cy, cw, ch)) => {
                // バウンディングボックスをマージ
                let min_x = cx.min(x);
                let min_y = cy.min(y);
                let max_x = (cx + cw).max(x + w);
                let max_y = (cy + ch).max(y + h);
                self.cumulative_dirty = Some((min_x, min_y, max_x - min_x, max_y - min_y));
            }
            None => {
                self.cumulative_dirty = Some((x, y, w, h));
            }
        }
    }

    /// バッファキュー情報を更新
    pub fn update_buffer_queue(
        &mut self,
        index: usize,
        name: &'static str,
        commands: &[&'static str],
    ) {
        if index >= 4 {
            return;
        }
        self.buffer_queues[index].name = name;
        self.buffer_queues[index].pending_commands = commands.len();
        // 最新5件を記録
        for i in 0..5 {
            self.buffer_queues[index].command_types[i] = commands.get(i).copied();
        }
        if index >= self.buffer_count {
            self.buffer_count = index + 1;
        }
    }

    /// コマンドをパイプに追加（Compositorから呼ばれる）
    ///
    /// コマンドは右端(position=1.0)から開始し、UIタスクがアニメーションを進行させる
    pub fn add_command_to_pipe(&mut self, buffer_idx: usize, cmd_type: &'static str) {
        // 空きスロットを探す
        for slot in self.flowing_commands.iter_mut() {
            if slot.is_none() {
                *slot = Some(FlowingCommand {
                    buffer_index: buffer_idx,
                    cmd_type,
                    position: 1.0, // 右端から開始
                    arrived: false,
                });
                return;
            }
        }
        // 満杯なら追加しない
    }

    /// コマンドが終点に到着したかチェック（Compositorのポーリング用）
    pub fn is_command_arrived(&self, buffer_idx: usize) -> bool {
        for slot in self.flowing_commands.iter() {
            if let Some(cmd) = slot {
                if cmd.buffer_index == buffer_idx && cmd.arrived {
                    return true;
                }
            }
        }
        false
    }

    /// 到着したコマンドを削除（Compositorが処理完了後に呼ぶ）
    pub fn remove_arrived_command(&mut self, buffer_idx: usize) {
        for slot in self.flowing_commands.iter_mut() {
            if let Some(cmd) = slot {
                if cmd.buffer_index == buffer_idx && cmd.arrived {
                    *slot = None;
                    return;
                }
            }
        }
    }

    /// 互換性のため残す（使わない）
    #[allow(dead_code)]
    pub fn add_flowing_command(&mut self, _cmd_type: &'static str) {
        // 新しい add_command_to_pipe を使用
    }

    /// アニメーションを更新（UIタスクから呼ばれる）
    ///
    /// position を減少させ、0以下になったら arrived=true にする
    /// 削除はCompositorが行う
    pub fn tick_animation(&mut self) {
        self.animation_tick = self.animation_tick.wrapping_add(1);

        // クールダウンを減少
        if self.add_cooldown > 0 {
            self.add_cooldown -= 1;
        }

        // パイプ内のコマンドを進める（右→左、position: 1.0→0.0）
        for slot in self.flowing_commands.iter_mut() {
            if let Some(cmd) = slot {
                if !cmd.arrived {
                    cmd.position -= 0.06; // 1フレームで6%進む（約17フレーム=270msで通過）
                    if cmd.position <= 0.0 {
                        cmd.position = 0.0;
                        cmd.arrived = true; // 終点に到達、Compositorが取り出すまで待機
                    }
                }
                // arrived=true のコマンドは終点で停止（削除はCompositorが行う）
            }
        }

        // Blitアニメーションを進める（Shadow → FB、progress: 0.0→1.0）
        if let Some(ref mut blit_anim) = self.blit_animation {
            blit_anim.progress += 0.08; // 約12フレーム=200msで完了
            if blit_anim.progress >= 1.0 {
                blit_anim.progress = 1.0;
                // アニメーション完了 - 実際のblitを実行
                if blit_anim.pending_blit {
                    self.mini_shadow.blit_to(&mut self.mini_fb);
                    blit_anim.pending_blit = false;
                    self.frame_count += 1;
                }
                // アニメーション終了、次のフレームで削除
                self.blit_animation = None;
            }
        }

        // バッファコピーアニメーションを進める（Task → Compositor、progress: 0.0→1.0）
        if let Some(ref mut copy_anim) = self.buffer_copy_animation {
            copy_anim.progress += 0.12; // 約8フレーム=130msで完了（速め）
            if copy_anim.progress >= 1.0 {
                copy_anim.progress = 1.0;
                // アニメーション完了
                self.buffer_copy_animation = None;
            }
        }
    }
}

lazy_static::lazy_static! {
    /// グローバル可視化状態
    pub static ref MINI_VIS_STATE: SpinMutex<Option<MiniVisualizationState>> =
        SpinMutex::new(None);
}

/// 可視化状態を初期化
pub fn init_visualization_state() {
    let mut state = MINI_VIS_STATE.lock();
    *state = Some(MiniVisualizationState::new());
    crate::info!("Pipeline visualization state initialized");
}

/// 可視化モードを開始
///
/// VisualizationUIタスクを登録し、スケジューラを開始する。
/// この関数は戻らない。
pub fn start_visualization() -> ! {
    use crate::sched::{self, nice};
    use alloc::boxed::Box;

    init_visualization_state();

    let vis_ui = Box::new(
        sched::Task::new("VisualizationUI", nice::MIN, visualization_ui_task)
            .expect("Failed to create VisualizationUI task"),
    );
    sched::add_task(*vis_ui);

    crate::info!("Pipeline visualization mode - starting scheduler");
    sched::schedule();
    unreachable!();
}

// =============================================================================
// Compositor連携用ヘルパー関数
// =============================================================================

/// 可視化モードが有効かどうかを判定
pub fn is_visualization_mode() -> bool {
    MINI_VIS_STATE.lock().is_some()
}

/// バッファ数を更新
pub fn update_buffer_count(count: usize) {
    if let Some(ref mut vis_state) = *MINI_VIS_STATE.lock() {
        vis_state.buffer_count = count.min(4);
    }
}

/// バッファ処理開始時の可視化状態を更新
///
/// # Arguments
/// * `buffer_idx` - バッファインデックス
/// * `commands_count` - コマンド数
pub fn start_buffer_processing(buffer_idx: usize, commands_count: usize) {
    if let Some(ref mut vis_state) = *MINI_VIS_STATE.lock() {
        if buffer_idx < 4 {
            vis_state.buffer_queues[buffer_idx].anim_start_tick = vis_state.animation_tick;
            vis_state.buffer_queues[buffer_idx].is_processing = true;
            vis_state.buffer_queues[buffer_idx].total_commands = commands_count;
            vis_state.buffer_queues[buffer_idx].processed_count = 0;
            vis_state.start_buffer_copy_animation(buffer_idx, commands_count);
        }
    }
}

/// バッファコピーアニメーションが完了するまで待機
pub fn wait_for_buffer_copy_animation() {
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
}

/// バッファコピー完了後の可視化状態を更新
///
/// command_types → compositor_commands にコピー後、command_typesをクリア
pub fn complete_buffer_copy(buffer_idx: usize) {
    if let Some(ref mut vis_state) = *MINI_VIS_STATE.lock() {
        if buffer_idx < 4 {
            vis_state.buffer_queues[buffer_idx].compositor_commands =
                vis_state.buffer_queues[buffer_idx].command_types;
            vis_state.buffer_queues[buffer_idx].pending_commands = 0;
            vis_state.buffer_queues[buffer_idx].command_types = [None; 5];
        }
    }
}

/// コマンド処理の可視化状態を更新
///
/// # Arguments
/// * `buffer_idx` - バッファインデックス
/// * `cmd_idx` - 処理中のコマンドインデックス
/// * `region` - 描画領域
/// * `cmd` - 描画コマンド
/// * `screen_width` - 画面幅
/// * `screen_height` - 画面高さ
///
/// # Returns
/// コマンドタイプ文字列
pub fn process_command_visualization(
    buffer_idx: usize,
    cmd_idx: usize,
    region: &Region,
    cmd: &DrawCommand,
    screen_width: u32,
    screen_height: u32,
) -> &'static str {
    let cmd_type: &'static str = match cmd {
        DrawCommand::Clear { .. } => "Clear",
        DrawCommand::FillRect { .. } => "FillRect",
        DrawCommand::DrawString { .. } => "DrawString",
        DrawCommand::DrawChar { .. } => "DrawChar",
    };

    if let Some(ref mut vis_state) = *MINI_VIS_STATE.lock() {
        vis_state.phase = PipelinePhase::Rendering;
        let (dx, dy, dw, dh) =
            vis_state
                .mini_shadow
                .render_command(region, cmd, screen_width, screen_height);
        vis_state.expand_dirty(dx, dy, dw, dh);
        vis_state.current_command = Some(CommandInfo {
            command_type: cmd_type,
            region_x: region.x,
            region_y: region.y,
        });
        vis_state.command_count += 1;
        if buffer_idx < 4 {
            vis_state.buffer_queues[buffer_idx].processed_count = cmd_idx + 1;
        }
    }

    cmd_type
}

/// バッファ処理完了の可視化状態を更新
pub fn complete_buffer_processing(buffer_idx: usize) {
    if let Some(ref mut vis_state) = *MINI_VIS_STATE.lock() {
        if buffer_idx < 4 {
            vis_state.buffer_queues[buffer_idx].is_processing = false;
            vis_state.buffer_queues[buffer_idx].compositor_commands = [None; 5];
            vis_state.buffer_queues[buffer_idx].processed_count = 0;
            vis_state.buffer_queues[buffer_idx].total_commands = 0;
        }
    }
}

/// Blitアニメーションを開始
pub fn start_blit_animation() {
    if let Some(ref mut vis_state) = *MINI_VIS_STATE.lock() {
        vis_state.phase = PipelinePhase::Blit;
        vis_state.start_blit_animation_from_cumulative();
    }
}

/// Blitアニメーションが完了するまで待機
pub fn wait_for_blit_animation() {
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
}

/// Blit完了後にフェーズをIdleに戻す
pub fn set_phase_idle() {
    if let Some(ref mut vis_state) = *MINI_VIS_STATE.lock() {
        vis_state.phase = PipelinePhase::Idle;
    }
}

/// 可視化モードでのフレーム処理
///
/// Compositorから呼び出され、可視化モード時の全フレーム処理を担当。
/// 通常のシャドウバッファへの描画の代わりに、ミニバッファへの描画と
/// アニメーション制御を行う。
///
/// # Returns
/// 可視化モードで処理した場合は`true`、可視化モードでない場合は`false`
pub fn process_frame_if_visualization(
    buffers_snapshot: &alloc::sync::Arc<alloc::vec::Vec<crate::graphics::buffer::SharedBuffer>>,
    screen_width: u32,
    screen_height: u32,
) -> bool {
    // 可視化モードでなければ早期リターン
    if !is_visualization_mode() {
        return false;
    }

    // バッファ数を更新
    update_buffer_count(buffers_snapshot.len());

    // 各バッファを処理
    for (buffer_idx, buffer) in buffers_snapshot.iter().enumerate() {
        if let Some(mut buf) = buffer.try_lock() {
            if buf.is_dirty() {
                let region = buf.region();
                let commands = buf.commands();

                // コマンドをローカルにコピー（ロック解放のため）
                let commands_copy: alloc::vec::Vec<_> = commands.iter().cloned().collect();
                drop(buf); // バッファのロックを解放

                // このバッファの処理開始
                start_buffer_processing(buffer_idx, commands_copy.len());

                // バッファコピーアニメーション完了を待機
                wait_for_buffer_copy_animation();

                // コピー完了: コマンドをCompositor内部に移動
                complete_buffer_copy(buffer_idx);

                // コピー完了後、コマンド処理開始前にウェイト（可視化用）
                crate::sched::sleep_ms(500);

                for (cmd_idx, cmd) in commands_copy.iter().enumerate() {
                    // コマンドを処理（ミニシャドウバッファに描画）
                    process_command_visualization(
                        buffer_idx,
                        cmd_idx,
                        &region,
                        cmd,
                        screen_width,
                        screen_height,
                    );

                    // 各コマンド処理後に待機（アニメーション可視化用、0.5秒）
                    crate::sched::sleep_ms(500);
                }

                // このバッファの処理完了
                complete_buffer_processing(buffer_idx);

                // バッファを再ロックしてクリア
                if let Some(mut buf) = buffer.try_lock() {
                    buf.clear_commands();
                    // 所有タスクを起床
                    if let Some(id) = buf.owner_task_id() {
                        drop(buf);
                        crate::sched::unblock_task(crate::sched::TaskId::from_u64(id));
                    }
                }
            }
        }
    }

    // ミニシャドウ → ミニFB へのblitアニメーション
    start_blit_animation();
    wait_for_blit_animation();
    set_phase_idle();

    true
}

/// タスクのflush()から呼ばれる: バッファ内のコマンド情報を更新
///
/// タスクがflush()を呼んだときに即座に可視化状態を更新する。
/// これにより、タスクボックス内にコマンドが表示されるタイミングが
/// 実際のシステムと同期する。
///
/// # Arguments
/// * `buffer_index` - バッファのインデックス（vis_buffer_index）
/// * `command_count` - バッファ内のコマンド数
/// * `command_types` - コマンドタイプのリスト（最大5件）
pub fn update_buffer_on_flush(
    buffer_index: usize,
    command_count: usize,
    command_types: [Option<&'static str>; 5],
) {
    if let Some(ref mut state) = *MINI_VIS_STATE.lock() {
        if buffer_index < 4 {
            let name = match buffer_index {
                0 => "Buffer0",
                1 => "Buffer1",
                2 => "Buffer2",
                _ => "Buffer3",
            };
            state.buffer_queues[buffer_index].name = name;
            state.buffer_queues[buffer_index].pending_commands = command_count;
            state.buffer_queues[buffer_index].command_types = command_types;
            if buffer_index >= state.buffer_count {
                state.buffer_count = buffer_index + 1;
            }
        }
    }
}

// =============================================================================
// レイアウト座標定義 (1024x768想定)
// =============================================================================
//
// 新レイアウト:
// +------------------------------------------------------------------+
// | タイトル                                                          |
// +------------------------------------------------------------------+
// |  +------------------+   ←   +------------------+                  |
// |  | Frame Buffer     |       | Shadow Buffer    |                  |
// |  |  (ミニFB)        |       |  (ミニシャドウ)  |                  |
// |  +------------------+       +------------------+                  |
// +------------------------------------------------------------------+
// |         +-------------------+              ↑                     |
// |         | Task1             |              |                     |
// |         | +---------------+ | ══[copy]══>  |                     |
// |         | |Buf [C][S][D]  | |      ┐    +----------+            |
// |         | +---------------+ |      ├──> |Compositor|            |
// |         +-------------------+      │    +----------+            |
// |         +-------------------+      │    (Shadow真下)            |
// |         | Task2             | ═════╪═══>                        |
// |         | |Buf [...]    |  |      ┘                             |
// |         +-------------------+                                    |
// |  Stats: ...                                                      |
// +------------------------------------------------------------------+

/// タイトル位置
const TITLE_X: usize = 10;
const TITLE_Y: usize = 8;

/// ミニチュアバッファサイズ（拡大版: 約1/2.5スケール）
const MINI_WIDTH: usize = 400;
const MINI_HEIGHT: usize = 300;

/// フレームバッファパネル（上段左）
const FB_PANEL_X: usize = 30;
const FB_PANEL_Y: usize = 30;
const FB_PANEL_WIDTH: usize = MINI_WIDTH + 20; // 420
const FB_PANEL_HEIGHT: usize = MINI_HEIGHT + 30; // 330

/// シャドウバッファパネル（上段右）
const SHADOW_PANEL_X: usize = 520;
const SHADOW_PANEL_Y: usize = 30;
const SHADOW_PANEL_WIDTH: usize = MINI_WIDTH + 20; // 420
const SHADOW_PANEL_HEIGHT: usize = MINI_HEIGHT + 30; // 330

/// 下段エリアのY開始位置
const LOWER_AREA_Y: usize = 380;

/// Taskボックス群（下段、Shadowバッファの左側に配置）
/// 各タスクボックスは内部にバッファ表示を含む
const TASK_BOX_X: usize = 280;
const TASK_BOX_Y_START: usize = LOWER_AREA_Y + 10; // 390
const TASK_BOX_WIDTH: usize = 180;
const TASK_BOX_HEIGHT: usize = 55;
const TASK_BOX_SPACING: usize = 65;

/// タスクボックス内のバッファ表示領域
const BUFFER_INNER_X: usize = 5; // タスクボックス内のオフセット
const BUFFER_INNER_Y: usize = 18;
const BUFFER_INNER_WIDTH: usize = 170;
const BUFFER_INNER_HEIGHT: usize = 30;

/// パイプ（キュー）の設定 - バッファコピーを表現
const PIPE_START_X: usize = TASK_BOX_X + TASK_BOX_WIDTH + 5; // 465
const PIPE_END_X: usize = 610; // Compositor手前 (COMPOSITOR_X - 10)
const PIPE_LENGTH: usize = PIPE_END_X - PIPE_START_X; // 145

/// Compositorボックス（Shadowバッファの真下に配置、内部バッファ表示付き）
/// Shadow: X=520, W=420 → 中央=730 → Compositor中央も730
const COMPOSITOR_X: usize = 620; // 730 - 220/2 = 620
const COMPOSITOR_Y: usize = LOWER_AREA_Y + 5; // 385
const COMPOSITOR_WIDTH: usize = 220;
const COMPOSITOR_HEIGHT: usize = 130;

/// Compositor内部バッファ表示領域
const COMP_BUFFER_X: usize = 10;
const COMP_BUFFER_Y: usize = 35;
const COMP_BUFFER_WIDTH: usize = 200;
const COMP_BUFFER_HEIGHT: usize = 35;

/// ステップ情報位置（下段下部）
const STEP_INFO_X: usize = 50;
const STEP_INFO_Y: usize = 700;

// =============================================================================
// MiniBuffer構造体
// =============================================================================

/// ミニチュアバッファ（シャドウ/FB表示用）
///
/// 画面の縮小版を表示するためのバッファ
pub struct MiniBuffer {
    /// ピクセルデータ（u32 = 0xRRGGBB）
    buffer: Vec<u32>,
    /// バッファの幅
    pub width: usize,
    /// バッファの高さ
    pub height: usize,
}

impl MiniBuffer {
    /// 新しいMiniBufferを作成
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            buffer: alloc::vec![0u32; width * height],
            width,
            height,
        }
    }

    /// バッファをクリア
    pub fn clear(&mut self, color: u32) {
        for pixel in self.buffer.iter_mut() {
            *pixel = color;
        }
    }

    /// 矩形を描画
    pub fn draw_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: u32) {
        for dy in 0..h {
            let py = y + dy;
            if py >= self.height {
                break;
            }
            for dx in 0..w {
                let px = x + dx;
                if px >= self.width {
                    break;
                }
                self.buffer[py * self.width + px] = color;
            }
        }
    }

    /// 文字を描画（8x8フォント）
    pub fn draw_char(&mut self, x: usize, y: usize, ch: u8, color: u32) {
        use crate::graphics::FONT_8X8;
        if ch < 32 || ch > 126 {
            return;
        }
        let font_index = (ch - 32) as usize;
        let glyph = FONT_8X8[font_index];

        for row in 0..8 {
            let py = y + row;
            if py >= self.height {
                break;
            }
            let glyph_row = glyph[row];
            for col in 0..8 {
                let px = x + col;
                if px >= self.width {
                    break;
                }
                if (glyph_row >> col) & 1 == 1 {
                    self.buffer[py * self.width + px] = color;
                }
            }
        }
    }

    /// 文字列を描画
    pub fn draw_string(&mut self, x: usize, y: usize, s: &str, color: u32) {
        let mut cur_x = x;
        for ch in s.bytes() {
            self.draw_char(cur_x, y, ch, color);
            cur_x += 8;
            if cur_x >= self.width {
                break;
            }
        }
    }

    /// 別のMiniBufferにコピー
    ///
    /// # Arguments
    /// * `dest` - コピー先のMiniBuffer（同サイズである必要あり）
    pub fn blit_to(&self, dest: &mut MiniBuffer) {
        debug_assert_eq!(self.width, dest.width);
        debug_assert_eq!(self.height, dest.height);
        dest.buffer.copy_from_slice(&self.buffer);
    }

    /// バッファの内容をフレームバッファに描画
    pub fn blit_to_fb(&self, fb_base: u64, fb_width: u32, dest_x: usize, dest_y: usize) {
        let fb = fb_base as *mut u32;
        let stride = fb_width as usize;

        for y in 0..self.height {
            let src_offset = y * self.width;
            let dest_offset = (dest_y + y) * stride + dest_x;
            for x in 0..self.width {
                // SAFETY: 呼び出し元が描画範囲の有効性を保証
                unsafe {
                    *fb.add(dest_offset + x) = self.buffer[src_offset + x];
                }
            }
        }
    }

    /// 描画コマンドをミニバッファにレンダリング（可視化用）
    ///
    /// スケールに応じてコマンドをミニバッファに描画
    /// 戻り値: 描画されたdirty region (x, y, w, h) in mini-buffer coordinates
    pub fn render_command(
        &mut self,
        region: &Region,
        cmd: &DrawCommand,
        screen_width: u32,
        screen_height: u32,
    ) -> (u32, u32, u32, u32) {
        // ミニバッファサイズに基づくスケール変換
        let mini_w = self.width;
        let mini_h = self.height;
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
                self.draw_rect(x, y, w, h, *color);
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
                self.draw_rect(sx, sy, sw, sh, *color);
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
                self.draw_rect(sx, sy, sw, 2, *color);
                (sx as u32, sy as u32, sw as u32, 2)
            }
            DrawCommand::DrawChar { x, y, color, .. } => {
                let global_x = region.x + x;
                let global_y = region.y + y;
                let sx = scale_x(global_x);
                let sy = scale_y(global_y);
                self.draw_rect(sx, sy, 2, 2, *color);
                (sx as u32, sy as u32, 2, 2)
            }
        }
    }
}

// =============================================================================
// 可視化UIタスク（リアルタイム）
// =============================================================================

// =============================================================================
// 可視化用デモタスク
// =============================================================================

/// tick値に応じた色を取得（視認性の高い色をサイクル）
fn get_demo_color() -> u32 {
    const COLORS: [u32; 6] = [
        0xFF5555, // 赤
        0x55FF55, // 緑
        0x5555FF, // 青
        0xFFFF55, // 黄
        0xFF55FF, // マゼンタ
        0x55FFFF, // シアン
    ];
    let tick = crate::timer::current_tick();
    COLORS[(tick as usize) % COLORS.len()]
}

/// デモタスク1: カウンタを表示し続ける
extern "C" fn demo_task1() -> ! {
    crate::info!("[DemoTask1] Started");

    let region = crate::graphics::Region::new(400, 500, 350, 20);
    let buffer =
        crate::graphics::compositor::register_writer(region).expect("Failed to register writer");
    let mut writer = crate::graphics::TaskWriter::new(buffer, 0xFFFFFFFF);

    let mut counter = 0u64;
    loop {
        writer.set_color(get_demo_color());
        writer.clear(0x00000000);
        let tick = crate::timer::current_tick();
        let _ = core::fmt::Write::write_fmt(
            &mut writer,
            format_args!("[Task1] Count:{} Tick:{}", counter, tick),
        );
        writer.sync_flush();
        counter += 1;
    }
}

/// デモタスク2: カウンタを表示し続ける
extern "C" fn demo_task2() -> ! {
    crate::info!("[DemoTask2] Started");

    let region = crate::graphics::Region::new(400, 520, 300, 20);
    let buffer =
        crate::graphics::compositor::register_writer(region).expect("Failed to register writer");
    let mut writer = crate::graphics::TaskWriter::new(buffer, 0xFFFFFFFF);

    let mut counter = 0u64;
    loop {
        writer.set_color(get_demo_color());
        writer.clear(0x00000000);
        let _ = core::fmt::Write::write_fmt(
            &mut writer,
            format_args!("[Task2 Med ] Count: {}", counter),
        );
        writer.sync_flush();
        counter += 1;
    }
}

/// デモタスク3: カウンタを表示し続ける
extern "C" fn demo_task3() -> ! {
    crate::info!("[DemoTask3] Started");

    let region = crate::graphics::Region::new(400, 540, 300, 20);
    let buffer =
        crate::graphics::compositor::register_writer(region).expect("Failed to register writer");
    let mut writer = crate::graphics::TaskWriter::new(buffer, 0xFFFFFFFF);

    let mut counter = 0u64;
    loop {
        writer.set_color(get_demo_color());
        writer.clear(0x00000000);
        let _ = core::fmt::Write::write_fmt(
            &mut writer,
            format_args!("[Task3 Low ] Count: {}", counter),
        );
        writer.sync_flush();
        counter += 1;
    }
}

/// 可視化用デモタスクを登録
fn register_demo_tasks() {
    use crate::sched::{self, nice};
    use alloc::boxed::Box;

    crate::info!("[VisualizationUI] Registering demo tasks");

    let t1 = Box::new(
        sched::Task::new("DemoTask1", nice::DEFAULT - 5, demo_task1)
            .expect("Failed to create DemoTask1"),
    );
    sched::add_task(*t1);

    let t2 = Box::new(
        sched::Task::new("DemoTask2", nice::DEFAULT, demo_task2)
            .expect("Failed to create DemoTask2"),
    );
    sched::add_task(*t2);

    let t3 = Box::new(
        sched::Task::new("DemoTask3", nice::MAX, demo_task3).expect("Failed to create DemoTask3"),
    );
    sched::add_task(*t3);

    crate::info!("[VisualizationUI] Demo tasks registered");
}

/// パイプライン可視化UIタスク
///
/// Compositorと並行して実行され、パイプラインの状態を
/// 画面にリアルタイム表示します。
pub extern "C" fn visualization_ui_task() -> ! {
    crate::info!("[VisualizationUI] Started");

    // デモタスクを登録
    register_demo_tasks();

    // 少し待機してからCompositorから情報を取得
    crate::sched::sleep_ms(100);

    // 画面サイズを取得
    let (screen_width, screen_height) = crate::graphics::compositor::screen_size();

    // フレームバッファ情報を取得
    let fb_base = crate::graphics::compositor::fb_base();

    if fb_base == 0 {
        crate::error!("[VisualizationUI] Failed to get framebuffer base");
        loop {
            crate::sched::sleep_ms(1000);
        }
    }

    // ローカルのMiniBufferを保持（キャプチャ用）
    let mut local_shadow = MiniBuffer::new(MINI_WIDTH, MINI_HEIGHT);
    let mut local_fb = MiniBuffer::new(MINI_WIDTH, MINI_HEIGHT);

    // ダブルバッファリング用バックバッファ（チラツキ軽減）
    let back_buffer_size = (screen_width as usize) * (screen_height as usize);
    let mut back_buffer: Vec<u32> = alloc::vec![0u32; back_buffer_size];
    let back_base = back_buffer.as_mut_ptr() as u64;

    crate::info!("[VisualizationUI] Initialized with back buffer, starting UI loop");

    // ローカルにキュー情報を保持
    let mut local_buffer_queues: [BufferQueueInfo; 4] = [const {
        BufferQueueInfo {
            name: "",
            pending_commands: 0,
            command_types: [None; 5],
            compositor_commands: [None; 5],
            anim_start_tick: 0,
            is_processing: false,
            processed_count: 0,
            total_commands: 0,
        }
    }; 4];
    let mut local_blit_anim: Option<BlitAnimation> = None;
    let mut local_buffer_copy_anim: Option<BufferCopyAnimation> = None;

    loop {
        // グローバル状態をローカルにコピー + アニメーション更新
        let (phase, cmd_info, cmd_count, frame_count, buffer_count, _anim_tick) = {
            if let Some(ref mut state) = *MINI_VIS_STATE.lock() {
                // アニメーション更新
                state.tick_animation();
                // ミニバッファをコピー
                state.mini_shadow.blit_to(&mut local_shadow);
                state.mini_fb.blit_to(&mut local_fb);
                // キュー情報をコピー
                local_buffer_queues.clone_from(&state.buffer_queues);
                // Blitアニメーションをコピー
                local_blit_anim = state.blit_animation.clone();
                // バッファコピーアニメーションをコピー
                local_buffer_copy_anim = state.buffer_copy_animation.clone();
                (
                    state.phase,
                    state.current_command.clone(),
                    state.command_count,
                    state.frame_count,
                    state.buffer_count,
                    state.animation_tick,
                )
            } else {
                local_blit_anim = None;
                local_buffer_copy_anim = None;
                (PipelinePhase::Idle, None, 0, 0, 0, 0)
            }
        };

        // バックバッファをクリア
        unsafe {
            draw_rect(
                back_base,
                screen_width,
                0,
                0,
                screen_width as usize,
                screen_height as usize,
                COLOR_BACKGROUND,
            );
        }

        // タイトル（バックバッファに描画）
        unsafe {
            draw_string(
                back_base,
                screen_width,
                TITLE_X,
                TITLE_Y,
                "Compositor Pipeline Visualization (LIVE)",
                COLOR_TITLE,
            );
        }

        // フレームバッファパネル（ミニFB表示）
        draw_panel_with_mini(
            back_base,
            screen_width,
            FB_PANEL_X,
            FB_PANEL_Y,
            "Frame Buffer",
            COLOR_FB_BORDER,
            &local_fb,
            phase == PipelinePhase::Blit,
        );

        // シャドウバッファパネル（ミニシャドウ表示）
        draw_panel_with_mini(
            back_base,
            screen_width,
            SHADOW_PANEL_X,
            SHADOW_PANEL_Y,
            "Shadow Buffer",
            COLOR_SHADOW_BORDER,
            &local_shadow,
            phase == PipelinePhase::Rendering,
        );

        // Compositorボックス（内部バッファ表示付き）
        // バッファコピーアニメーション中は内部バッファを表示しない
        let copy_in_progress = local_buffer_copy_anim.is_some();
        draw_compositor_indicator(
            back_base,
            screen_width,
            COMPOSITOR_X,
            COMPOSITOR_Y,
            phase,
            &local_buffer_queues,
            buffer_count,
            copy_in_progress,
        );

        // タスクボックス群（バッファ情報付き）
        draw_task_boxes_with_queues(back_base, screen_width, &local_buffer_queues, buffer_count);

        // 統計情報
        let stats = alloc::format!(
            "Frame:{} Cmds:{} Bufs:{} {:?}",
            frame_count,
            cmd_count,
            buffer_count,
            phase
        );
        unsafe {
            draw_string(
                back_base,
                screen_width,
                STEP_INFO_X,
                STEP_INFO_Y,
                &stats,
                COLOR_TEXT,
            );
        }

        // コマンド情報
        if let Some(ref info) = cmd_info {
            let cmd_text = alloc::format!(
                "Last: {} at ({}, {})",
                info.command_type,
                info.region_x,
                info.region_y
            );
            unsafe {
                draw_string(
                    back_base,
                    screen_width,
                    STEP_INFO_X,
                    STEP_INFO_Y + 15,
                    &cmd_text,
                    COLOR_HIGHLIGHT,
                );
            }
        }

        // 矢印描画
        draw_flow_arrows(back_base, screen_width, phase);

        // Blitアニメーション描画（dirty regionがShadow→FBへ移動）
        if let Some(ref blit_anim) = local_blit_anim {
            draw_blit_animation(back_base, screen_width, blit_anim);
        }

        // バッファコピーアニメーション描画（Task → Compositor）
        if let Some(ref copy_anim) = local_buffer_copy_anim {
            draw_buffer_copy_animation(back_base, screen_width, copy_anim, &local_buffer_queues);
        }

        // バックバッファをフレームバッファに一括転送（チラツキ軽減）
        unsafe {
            let fb = fb_base as *mut u32;
            let back = back_buffer.as_ptr();
            core::ptr::copy_nonoverlapping(back, fb, back_buffer_size);
        }

        // 16ms待機（約60fps）
        crate::sched::sleep_ms(16);
    }
}

// =============================================================================
// 可視化UIヘルパー関数
// =============================================================================

fn draw_panel_with_mini(
    fb_base: u64,
    fb_width: u32,
    x: usize,
    y: usize,
    label: &str,
    border_color: u32,
    mini: &MiniBuffer,
    highlight: bool,
) {
    let color = if highlight {
        COLOR_HIGHLIGHT
    } else {
        border_color
    };

    // 枠線（パネルサイズを定数から計算）
    let panel_width = mini.width + 20;
    let panel_height = mini.height + 30;

    unsafe {
        draw_rect_outline(fb_base, fb_width, x, y, panel_width, panel_height, color);
    }

    // ラベル（中央寄せ）
    let label_width = label.len() * 8;
    let label_x = x + (panel_width - label_width) / 2;
    unsafe {
        draw_string(fb_base, fb_width, label_x, y + 5, label, COLOR_TEXT);
    }

    // ミニバッファを表示（パネル内に中央配置）
    let mini_x = x + (panel_width - mini.width) / 2;
    mini.blit_to_fb(fb_base, fb_width, mini_x, y + 22);
}

fn draw_compositor_indicator(
    fb_base: u64,
    fb_width: u32,
    x: usize,
    y: usize,
    phase: PipelinePhase,
    buffer_queues: &[BufferQueueInfo; 4],
    buffer_count: usize,
    copy_in_progress: bool,
) {
    let highlight = matches!(phase, PipelinePhase::Rendering);
    let color = if highlight {
        COLOR_HIGHLIGHT
    } else {
        COLOR_COMPOSITOR_BORDER
    };

    unsafe {
        // 外枠
        draw_rect_outline(
            fb_base,
            fb_width,
            x,
            y,
            COMPOSITOR_WIDTH,
            COMPOSITOR_HEIGHT,
            color,
        );

        // ラベル
        let label = "Compositor";
        let label_x = x + (COMPOSITOR_WIDTH - label.len() * 8) / 2;
        draw_string(fb_base, fb_width, label_x, y + 5, label, COLOR_TEXT);

        // フェーズインジケータ
        let phase_text = match phase {
            PipelinePhase::Idle => "Idle",
            PipelinePhase::Snapshot => "Snapshot",
            PipelinePhase::Rendering => "RENDER",
            PipelinePhase::Blit => "Blit",
        };
        let phase_x = x + (COMPOSITOR_WIDTH - phase_text.len() * 8) / 2;
        draw_string(
            fb_base,
            fb_width,
            phase_x,
            y + 18,
            phase_text,
            if highlight {
                COLOR_HIGHLIGHT
            } else {
                COLOR_TEXT
            },
        );

        // 内部バッファ枠
        let buf_x = x + COMP_BUFFER_X;
        let buf_y = y + COMP_BUFFER_Y;
        draw_rect_outline(
            fb_base,
            fb_width,
            buf_x,
            buf_y,
            COMP_BUFFER_WIDTH,
            COMP_BUFFER_HEIGHT,
            if highlight {
                COLOR_QUEUE_BORDER
            } else {
                0x404040
            },
        );

        // バッファコピー中は「コピー中」を表示、完了後に内部バッファを表示
        if copy_in_progress {
            // コピーアニメーション中
            draw_string(
                fb_base,
                fb_width,
                buf_x + 50,
                buf_y + 12,
                "(copying...)",
                COLOR_HIGHLIGHT,
            );
        } else {
            // 処理中のバッファを探して、そのコマンドを表示
            let mut processing_info: Option<&BufferQueueInfo> = None;
            for i in 0..buffer_count.min(4) {
                if buffer_queues[i].is_processing {
                    processing_info = Some(&buffer_queues[i]);
                    break;
                }
            }

            if let Some(info) = processing_info {
                // 処理中のタスク名と進捗を表示
                let progress_text = alloc::format!(
                    "< {} ({}/{})",
                    info.name,
                    info.processed_count,
                    info.total_commands
                );
                draw_string(
                    fb_base,
                    fb_width,
                    buf_x + 3,
                    buf_y + 3,
                    &progress_text,
                    0x808080,
                );

                // コマンドブロックを表示（処理済みはグレーアウト、現在処理中をハイライト）
                let cmd_block_w = 32;
                let cmd_block_h = 18;
                let cmd_y = buf_y + 14;
                let cmd_start_x = buf_x + 5;
                let processed = info.processed_count;

                for j in 0..5 {
                    let cmd_x = cmd_start_x + j * (cmd_block_w + 4);
                    if let Some(cmd_type) = info.compositor_commands[j] {
                        // 処理状態に応じた色
                        let is_processed = j < processed;
                        let is_current = j == processed;
                        let block_color = if is_processed {
                            0x303030 // 処理済み: 暗いグレー
                        } else if is_current {
                            0x60A060 // 処理中: 緑
                        } else {
                            0x404080 // 待機中: 青紫
                        };
                        let text_color = if is_processed {
                            0x606060 // 処理済み: グレー
                        } else {
                            COLOR_TEXT
                        };

                        draw_rect(
                            fb_base,
                            fb_width,
                            cmd_x,
                            cmd_y,
                            cmd_block_w,
                            cmd_block_h,
                            block_color,
                        );
                        let initial = cmd_type.bytes().next().unwrap_or(b'?');
                        draw_char(
                            fb_base,
                            fb_width,
                            cmd_x + 12,
                            cmd_y + 5,
                            initial,
                            text_color,
                        );
                        if is_current {
                            // 処理中マーカー
                            draw_char(
                                fb_base,
                                fb_width,
                                cmd_x + 2,
                                cmd_y + 5,
                                b'>',
                                COLOR_HIGHLIGHT,
                            );
                        } else if is_processed {
                            // 処理済みマーカー（チェックマーク風）
                            draw_char(fb_base, fb_width, cmd_x + 2, cmd_y + 5, b'*', 0x505050);
                        }
                    } else {
                        // 空スロット
                        draw_rect_outline(
                            fb_base,
                            fb_width,
                            cmd_x,
                            cmd_y,
                            cmd_block_w,
                            cmd_block_h,
                            0x303030,
                        );
                    }
                }
            } else {
                // 処理中でない場合
                draw_string(
                    fb_base,
                    fb_width,
                    buf_x + 50,
                    buf_y + 12,
                    "(empty)",
                    0x606060,
                );
            }
        } // end of else (not copy_in_progress)

        // 矢印: 内部バッファ → Shadow（上向き）
        let arrow_x = x + COMPOSITOR_WIDTH / 2 - 20;
        draw_string(
            fb_base,
            fb_width,
            arrow_x,
            y + COMP_BUFFER_Y + COMP_BUFFER_HEIGHT + 8,
            "-> Shadow",
            0x808080,
        );
    }
}

fn draw_flow_arrows(fb_base: u64, fb_width: u32, current_phase: PipelinePhase) {
    // Compositor → Shadow 矢印（垂直、上向き）
    let comp_to_shadow_x = COMPOSITOR_X + COMPOSITOR_WIDTH / 2;
    let comp_to_shadow_y_start = COMPOSITOR_Y - 5;
    let comp_to_shadow_y_end = FB_PANEL_Y + FB_PANEL_HEIGHT + 10;
    let arrow_height = comp_to_shadow_y_start - comp_to_shadow_y_end;

    let comp_color = if current_phase == PipelinePhase::Rendering {
        COLOR_HIGHLIGHT
    } else {
        COLOR_ARROW
    };
    unsafe {
        // 垂直線
        draw_rect(
            fb_base,
            fb_width,
            comp_to_shadow_x,
            comp_to_shadow_y_end,
            2,
            arrow_height,
            comp_color,
        );
        // 矢印（上向き ^）
        draw_char(
            fb_base,
            fb_width,
            comp_to_shadow_x - 3,
            comp_to_shadow_y_end - 8,
            b'^',
            comp_color,
        );
    }

    // Shadow → FB 矢印（水平、左向き）
    let shadow_to_fb_x = FB_PANEL_X + FB_PANEL_WIDTH + 5;
    let shadow_to_fb_y = FB_PANEL_Y + FB_PANEL_HEIGHT / 2;
    let arrow_width = SHADOW_PANEL_X - shadow_to_fb_x - 5;

    let blit_color = if current_phase == PipelinePhase::Blit {
        COLOR_HIGHLIGHT
    } else {
        COLOR_ARROW
    };
    unsafe {
        // 水平線
        draw_rect(
            fb_base,
            fb_width,
            shadow_to_fb_x,
            shadow_to_fb_y,
            arrow_width,
            2,
            blit_color,
        );
        // 矢印（左向き <）
        draw_char(
            fb_base,
            fb_width,
            shadow_to_fb_x - 8,
            shadow_to_fb_y - 3,
            b'<',
            blit_color,
        );
    }
}

/// Blitアニメーション描画（dirty regionがShadow→FBへ移動）
///
/// progressに応じて矩形がシャドウバッファパネルからFBパネルへ移動する
fn draw_blit_animation(fb_base: u64, fb_width: u32, blit_anim: &BlitAnimation) {
    // シャドウパネル内のdirty region位置（ミニバッファ内座標からパネル座標へ）
    let shadow_mini_x = SHADOW_PANEL_X + 10;
    let shadow_mini_y = SHADOW_PANEL_Y + 25;
    let fb_mini_x = FB_PANEL_X + 10;
    let fb_mini_y = FB_PANEL_Y + 25;

    // dirty regionの位置（ミニバッファ内座標）
    let dx = blit_anim.dirty_x as usize;
    let dy = blit_anim.dirty_y as usize;
    let dw = blit_anim.dirty_w as usize;
    let dh = blit_anim.dirty_h as usize;

    // 開始位置（シャドウパネル内）
    let start_x = shadow_mini_x + dx;
    let start_y = shadow_mini_y + dy;

    // 終了位置（FBパネル内）
    let end_x = fb_mini_x + dx;
    let end_y = fb_mini_y + dy;

    // 現在位置（補間）
    let progress = blit_anim.progress;
    let current_x = start_x as f32 + (end_x as f32 - start_x as f32) * progress;
    let current_y = start_y as f32 + (end_y as f32 - start_y as f32) * progress;

    // dirty region矩形を描画（半透明風にアウトラインのみ）
    let color = 0xFFFF00; // 黄色（ハイライト）
    unsafe {
        draw_rect_outline(
            fb_base,
            fb_width,
            current_x as usize,
            current_y as usize,
            dw.max(4),
            dh.max(4),
            color,
        );
        // 内側にも小さい矩形を描いて視認性向上
        if dw > 8 && dh > 8 {
            draw_rect_outline(
                fb_base,
                fb_width,
                current_x as usize + 2,
                current_y as usize + 2,
                dw - 4,
                dh - 4,
                color,
            );
        }
    }
}

/// 各タスクのキューをパイプとして表示（左から右へ流れる）
///
/// [Task1] ─[C][S]───> ┐
/// [Task2] ─[C][S]───> ├──> [Compositor]
/// [Task3] ─[C][S]───> ┘
fn draw_pipe_queue(
    fb_base: u64,
    fb_width: u32,
    buffer_queues: &[BufferQueueInfo; 4],
    buffer_count: usize,
    flowing_commands: &[Option<FlowingCommand>; 8],
) {
    // パイプの位置とサイズ（グローバル定数を使用）
    const PIPE_HEIGHT: usize = 16;
    const CMD_BLOCK_WIDTH: usize = 20;

    let count = buffer_count.min(4);
    if count == 0 {
        return;
    }

    // 各バッファのパイプを描画
    for i in 0..count {
        let pipe_y =
            TASK_BOX_Y_START + i * TASK_BOX_SPACING + TASK_BOX_HEIGHT / 2 - PIPE_HEIGHT / 2;
        let info = &buffer_queues[i];

        // パイプの色（処理中ならハイライト）
        let pipe_color = if info.is_processing {
            COLOR_HIGHLIGHT
        } else {
            COLOR_QUEUE_BORDER
        };

        // パイプ本体（水平線、タスクボックス右端からCompositor手前まで）
        unsafe {
            draw_rect(
                fb_base,
                fb_width,
                PIPE_START_X,
                pipe_y + PIPE_HEIGHT / 2 - 1,
                PIPE_LENGTH,
                3,
                pipe_color,
            );
        }

        // 矢印（パイプ終端、右端 → Compositor方向）
        unsafe {
            draw_char(
                fb_base,
                fb_width,
                PIPE_END_X + 2,
                pipe_y + PIPE_HEIGHT / 2 - 4,
                b'>',
                pipe_color,
            );
        }
    }

    // FlowingCommand を描画（各コマンドの position に基づいて）
    for cmd_opt in flowing_commands.iter() {
        if let Some(cmd) = cmd_opt {
            if cmd.buffer_index >= count {
                continue;
            }

            let pipe_y =
                TASK_BOX_Y_START + cmd.buffer_index * TASK_BOX_SPACING + TASK_BOX_HEIGHT / 2 - 8;

            // position: 1.0 = 左端（開始、タスク側）、0.0 = 右端（終点、Compositor側）
            // cmd_x = PIPE_START_X + (1.0 - position) * PIPE_LENGTH
            let progress = 1.0 - cmd.position;
            let cmd_x = PIPE_START_X + ((progress * PIPE_LENGTH as f32) as usize);
            let cmd_y = pipe_y;

            // コマンドタイプに応じた色
            let cmd_color = match cmd.cmd_type {
                "Clear" => 0xFF4040,      // 赤
                "FillRect" => 0x40FF40,   // 緑
                "DrawString" => 0x4040FF, // 青
                "DrawChar" => 0xFFFF40,   // 黄
                _ => 0xFFFFFF,            // 白
            };

            // 到着したコマンドは白で強調（待機中を表現）
            let final_color = if cmd.arrived {
                0xFFFFFF // 白で強調
            } else {
                cmd_color
            };

            // コマンドブロック
            unsafe {
                draw_rect(
                    fb_base,
                    fb_width,
                    cmd_x,
                    cmd_y,
                    CMD_BLOCK_WIDTH,
                    14,
                    final_color,
                );
            }

            // 頭文字
            let ch = cmd.cmd_type.as_bytes().first().copied().unwrap_or(b'?');
            unsafe {
                draw_char(fb_base, fb_width, cmd_x + 6, cmd_y + 3, ch, 0x000000);
            }
        }
    }

    // 合流点を示す縦線（Compositor手前）
    if count > 1 {
        let merge_x = PIPE_END_X + 12;
        let merge_y_start = TASK_BOX_Y_START + TASK_BOX_HEIGHT / 2;
        let merge_y_end = TASK_BOX_Y_START + (count - 1) * TASK_BOX_SPACING + TASK_BOX_HEIGHT / 2;
        unsafe {
            draw_rect(
                fb_base,
                fb_width,
                merge_x,
                merge_y_start,
                2,
                merge_y_end - merge_y_start + 3,
                COLOR_ARROW,
            );
        }
    }
}

/// タスクボックス群（内部にバッファ表示）
///
/// 各タスクボックスは以下の構造:
/// +---------------------------+
/// | TaskName                  |
/// | +---------------------+   |
/// | | Buffer  [C][S][D]   |   |
/// | +---------------------+   |
/// +---------------------------+
fn draw_task_boxes_with_queues(
    fb_base: u64,
    fb_width: u32,
    buffer_queues: &[BufferQueueInfo; 4],
    buffer_count: usize,
) {
    // 表示するバッファ数（最大4）
    let count = buffer_count.min(4);
    if count == 0 {
        // バッファがない場合はデフォルト表示
        let task_names = ["Task 1", "Task 2", "Task 3"];
        for (i, name) in task_names.iter().enumerate() {
            let y = TASK_BOX_Y_START + i * TASK_BOX_SPACING;
            unsafe {
                // タスク外枠
                draw_rect_outline(
                    fb_base,
                    fb_width,
                    TASK_BOX_X,
                    y,
                    TASK_BOX_WIDTH,
                    TASK_BOX_HEIGHT,
                    COLOR_TEXT,
                );
                // タスク名
                draw_string(fb_base, fb_width, TASK_BOX_X + 5, y + 3, name, COLOR_TEXT);
                // バッファ枠（内部）
                draw_rect_outline(
                    fb_base,
                    fb_width,
                    TASK_BOX_X + BUFFER_INNER_X,
                    y + BUFFER_INNER_Y,
                    BUFFER_INNER_WIDTH,
                    BUFFER_INNER_HEIGHT,
                    0x606060,
                );
                // "Buffer" ラベル
                draw_string(
                    fb_base,
                    fb_width,
                    TASK_BOX_X + BUFFER_INNER_X + 3,
                    y + BUFFER_INNER_Y + 10,
                    "Buffer",
                    0x808080,
                );
            }
        }
    } else {
        // 実際のバッファ情報を表示
        for i in 0..count {
            let info = &buffer_queues[i];
            let y = TASK_BOX_Y_START + i * TASK_BOX_SPACING;

            // タスク枠の色（処理中ならハイライト）
            let task_border_color = if info.is_processing {
                COLOR_HIGHLIGHT
            } else {
                COLOR_TEXT
            };

            // バッファ枠の色（コマンドがあれば青）
            let buffer_border_color = if info.pending_commands > 0 {
                COLOR_QUEUE_BORDER
            } else {
                0x404040
            };

            unsafe {
                // タスク外枠
                draw_rect_outline(
                    fb_base,
                    fb_width,
                    TASK_BOX_X,
                    y,
                    TASK_BOX_WIDTH,
                    TASK_BOX_HEIGHT,
                    task_border_color,
                );

                // タスク名
                draw_string(
                    fb_base,
                    fb_width,
                    TASK_BOX_X + 5,
                    y + 3,
                    info.name,
                    COLOR_TEXT,
                );

                // バッファ枠（内部）
                draw_rect_outline(
                    fb_base,
                    fb_width,
                    TASK_BOX_X + BUFFER_INNER_X,
                    y + BUFFER_INNER_Y,
                    BUFFER_INNER_WIDTH,
                    BUFFER_INNER_HEIGHT,
                    buffer_border_color,
                );

                // バッファ内のコマンド表示（最大5個を小さなブロックで表現）
                let cmd_block_width = 28;
                let cmd_block_height = 18;
                let cmd_y = y + BUFFER_INNER_Y + 6;
                let cmd_start_x = TASK_BOX_X + BUFFER_INNER_X + 5;

                for j in 0..5 {
                    let cmd_x = cmd_start_x + j * (cmd_block_width + 3);
                    if let Some(cmd_type) = info.command_types[j] {
                        // コマンドブロックを描画
                        draw_rect(
                            fb_base,
                            fb_width,
                            cmd_x,
                            cmd_y,
                            cmd_block_width,
                            cmd_block_height,
                            0x404080,
                        );
                        // コマンドタイプの頭文字
                        let initial = cmd_type.bytes().next().unwrap_or(b'?');
                        draw_char(
                            fb_base,
                            fb_width,
                            cmd_x + 10,
                            cmd_y + 5,
                            initial,
                            COLOR_TEXT,
                        );
                    } else {
                        // 空スロット
                        draw_rect_outline(
                            fb_base,
                            fb_width,
                            cmd_x,
                            cmd_y,
                            cmd_block_width,
                            cmd_block_height,
                            0x303030,
                        );
                    }
                }
            }
        }
    }

    // 矢印は draw_pipe_queue で描画されるため、ここでは描画しない
}

/// バッファコピーアニメーション描画（Task → Compositorへの転送を可視化）
///
/// タスクボックス内のバッファがCompositorの内部バッファにコピーされる様子を表現
/// progressに応じてバッファ枠がタスクからCompositorへ移動する
fn draw_buffer_copy_animation(
    fb_base: u64,
    fb_width: u32,
    copy_anim: &BufferCopyAnimation,
    _buffer_queues: &[BufferQueueInfo; 4],
) {
    let idx = copy_anim.source_buffer_index;
    if idx >= 4 {
        return;
    }

    // タスクボックス内のバッファ開始位置
    let task_buf_x = TASK_BOX_X + BUFFER_INNER_X;
    let task_buf_y = TASK_BOX_Y_START + idx * TASK_BOX_SPACING + BUFFER_INNER_Y;

    // Compositor内部バッファの位置
    let comp_buf_x = COMPOSITOR_X + COMP_BUFFER_X;
    let comp_buf_y = COMPOSITOR_Y + COMP_BUFFER_Y;

    // 補間
    let progress = copy_anim.progress;
    let current_x = task_buf_x as f32 + (comp_buf_x as f32 - task_buf_x as f32) * progress;
    let current_y = task_buf_y as f32 + (comp_buf_y as f32 - task_buf_y as f32) * progress;

    // 幅も補間（タスク側のサイズからCompositor側のサイズへ）
    let start_w = BUFFER_INNER_WIDTH;
    let end_w = COMP_BUFFER_WIDTH;
    let current_w = start_w as f32 + (end_w as f32 - start_w as f32) * progress;

    let start_h = BUFFER_INNER_HEIGHT;
    let end_h = COMP_BUFFER_HEIGHT;
    let current_h = start_h as f32 + (end_h as f32 - start_h as f32) * progress;

    // 色（移動中は明るいシアン）
    let color = 0x40FFFF;

    unsafe {
        // バッファ枠を描画
        draw_rect_outline(
            fb_base,
            fb_width,
            current_x as usize,
            current_y as usize,
            current_w as usize,
            current_h as usize,
            color,
        );

        // 内側にもう一つ枠を描いて視認性向上
        if current_w > 8.0 && current_h > 8.0 {
            draw_rect_outline(
                fb_base,
                fb_width,
                current_x as usize + 2,
                current_y as usize + 2,
                (current_w - 4.0) as usize,
                (current_h - 4.0) as usize,
                color,
            );
        }

        // コマンド数を表示
        let count_text = alloc::format!("{} cmds", copy_anim.command_count);
        draw_string(
            fb_base,
            fb_width,
            current_x as usize + 5,
            current_y as usize + (current_h / 2.0 - 4.0) as usize,
            &count_text,
            COLOR_TEXT,
        );
    }
}
