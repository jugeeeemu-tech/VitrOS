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
use super::compositor_observer::{CompositorObserver, NoOpObserver};
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
///
/// # 型パラメータ
/// * `O` - CompositorObserverトレイトを実装するオブザーバー型
///   デフォルトはNoOpObserver（ZST）で、ゼロコスト抽象化を実現
pub struct Compositor<O: CompositorObserver = NoOpObserver> {
    /// 設定
    config: CompositorConfig,
    /// 登録されたバッファのリスト（Copy-on-Write方式でスナップショット取得可能）
    buffers: Arc<Vec<SharedBuffer>>,
    /// オブザーバー
    observer: O,
}

impl<O: CompositorObserver> Compositor<O> {
    /// 新しいCompositorを作成
    ///
    /// # Arguments
    /// * `config` - Compositorの設定
    /// * `observer` - コンポジタのイベントを監視するオブザーバー
    pub fn new(config: CompositorConfig, observer: O) -> Self {
        Self {
            config,
            buffers: Arc::new(Vec::new()),
            observer,
        }
    }

    /// 新しいWriterを登録し、そのバッファへの参照を返す
    ///
    /// Copy-on-Write方式: 新しいVecを作成してバッファを追加し、Arcを置き換えます。
    /// これにより、既存のスナップショットは影響を受けません。
    ///
    /// # Arguments
    /// * `region` - Writer用の描画領域
    /// * `task_id` - 登録するタスクのID
    ///
    /// # Returns
    /// 共有バッファへの参照
    pub fn register_writer(&mut self, region: Region, task_id: u64) -> SharedBuffer {
        let buffer = Arc::new(crate::sync::BlockingMutex::new(
            super::buffer::WriterBuffer::new(region),
        ));

        // Copy-on-Write: 新しいVecを作成して追加
        let mut new_buffers = Vec::clone(&self.buffers);
        let buffer_index = new_buffers.len();
        new_buffers.push(Arc::clone(&buffer));
        self.buffers = Arc::new(new_buffers);

        // オブザーバーに通知
        self.observer
            .on_buffer_registered(buffer_index, &buffer, task_id);

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

    /// オブザーバーへの可変参照を取得
    pub fn observer_mut(&mut self) -> &mut O {
        &mut self.observer
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

// グローバルCompositor型エイリアス
// featureフラグで可視化オブザーバーを切り替え

#[cfg(feature = "visualize-pipeline")]
use crate::pipeline_visualization::PipelineVisualizationObserver;

#[cfg(feature = "visualize-pipeline")]
type GlobalCompositor = Compositor<PipelineVisualizationObserver>;

#[cfg(not(feature = "visualize-pipeline"))]
type GlobalCompositor = Compositor<NoOpObserver>;

// グローバルCompositorインスタンス
lazy_static! {
    /// グローバルCompositorインスタンス
    /// 初期化前はNone
    static ref COMPOSITOR: SpinMutex<Option<GlobalCompositor>> = SpinMutex::new(None);
}

/// Compositorを初期化
///
/// # Arguments
/// * `config` - Compositorの設定
pub fn init_compositor(config: CompositorConfig) {
    // 画面サイズをグローバル変数に保存
    SCREEN_WIDTH.store(config.fb_width, Ordering::Relaxed);
    SCREEN_HEIGHT.store(config.fb_height, Ordering::Relaxed);

    // オブザーバーを作成（featureに応じて切り替え）
    #[cfg(feature = "visualize-pipeline")]
    let mut observer = PipelineVisualizationObserver::new();
    #[cfg(not(feature = "visualize-pipeline"))]
    let mut observer = NoOpObserver;

    // オブザーバーに初期化を通知
    observer.on_init(config.fb_base);

    let mut comp = COMPOSITOR.lock();
    *comp = Some(Compositor::new(config, observer));
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
    // タスクIDを取得（ロック取得前に取得）
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
        match comp.as_mut() {
            Some(c) => Some(c.register_writer(region, task_id)),
            None => None,
        }
    };

    // 割り込みを元の状態に復元
    unsafe {
        if flags & 0x200 != 0 {
            core::arch::asm!("sti", options(nomem, nostack));
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

        // 可視化モード: 可視化処理を実行して通常レンダリングをスキップ
        #[cfg(feature = "visualize-pipeline")]
        if crate::pipeline_visualization::is_visualization_mode() {
            crate::pipeline_visualization::process_frame_if_visualization(
                &buffers_snapshot,
                config.fb_width,
                config.fb_height,
            );
            FRAME_COUNT.fetch_add(1, Ordering::Relaxed);
            crate::sched::sleep_ms(16);
            continue;
        }

        // 通常モード: 各バッファを処理
        for buffer in buffers_snapshot.iter() {
            if let Some(mut buf) = buffer.try_lock() {
                if buf.is_dirty() {
                    let region = buf.region();
                    let commands = buf.commands();
                    render_commands_to(&mut shadow_buffer, &region, commands);
                    buf.clear_commands();
                }
            }
        }

        // Phase 4: シャドウバッファをハードウェアFBに転送（割り込み有効）
        // dirty_rectがある場合のみ転送され、転送後にdirty_rectはクリアされる
        let _blitted = unsafe { shadow_buffer.blit_to(config.fb_base) };

        FRAME_COUNT.fetch_add(1, Ordering::Relaxed);

        // 次のリフレッシュまで待機（約60fps = 16ms間隔）
        crate::sched::sleep_ms(16);
    }
}

