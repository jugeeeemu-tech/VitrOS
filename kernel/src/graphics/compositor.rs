//! Compositor - 各Writerのバッファを合成してフレームバッファに描画

use alloc::sync::Arc;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex as SpinMutex;

use super::buffer::{DrawCommand, SharedBuffer};
use super::region::Region;

/// Compositorの設定
pub struct CompositorConfig {
    /// フレームバッファのベースアドレス
    pub fb_base: u64,
    /// フレームバッファの幅
    pub fb_width: u32,
    /// フレームバッファの高さ
    pub fb_height: u32,
    /// リフレッシュ間隔（tick数）
    pub refresh_interval_ticks: u64,
}

/// Compositor（シングルトン）
///
/// 全てのWriterバッファをポーリングして、フレームバッファに合成します。
pub struct Compositor {
    /// 設定
    config: CompositorConfig,
    /// 登録されたバッファのリスト
    buffers: Vec<SharedBuffer>,
}

impl Compositor {
    /// 新しいCompositorを作成
    ///
    /// # Arguments
    /// * `config` - Compositorの設定
    pub fn new(config: CompositorConfig) -> Self {
        Self {
            config,
            buffers: Vec::new(),
        }
    }

    /// 新しいWriterを登録し、そのバッファへの参照を返す
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
        self.buffers.push(Arc::clone(&buffer));
        buffer
    }

    /// 1フレームを合成
    ///
    /// 全バッファをポーリングし、dirty=trueのバッファのみ描画します。
    /// try_lock()を使用して、ロック中のバッファはスキップします。
    pub fn compose_frame(&mut self) {
        for buffer in &self.buffers {
            // try_lockを使用して、ロック中のバッファはスキップ
            if let Some(mut buf) = buffer.try_lock() {
                if buf.is_dirty() {
                    let region = buf.region();
                    let commands = buf.take_commands();
                    self.render_commands(&region, &commands);
                }
            }
        }
    }

    /// コマンドをフレームバッファに描画
    ///
    /// # Arguments
    /// * `region` - 描画領域
    /// * `commands` - 描画コマンドのスライス
    fn render_commands(&self, region: &Region, commands: &[DrawCommand]) {
        for cmd in commands {
            match cmd {
                DrawCommand::Clear { color } => {
                    // 領域全体をクリア
                    unsafe {
                        super::draw_rect(
                            self.config.fb_base,
                            self.config.fb_width,
                            region.x as usize,
                            region.y as usize,
                            region.width as usize,
                            region.height as usize,
                            *color,
                        );
                    }
                }
                DrawCommand::DrawChar { x, y, ch, color } => {
                    // ローカル座標をグローバル座標に変換
                    let global_x = region.x + x;
                    let global_y = region.y + y;
                    unsafe {
                        super::draw_char(
                            self.config.fb_base,
                            self.config.fb_width,
                            global_x as usize,
                            global_y as usize,
                            *ch,
                            *color,
                        );
                    }
                }
                DrawCommand::DrawString { x, y, text, color } => {
                    let global_x = region.x + x;
                    let global_y = region.y + y;
                    unsafe {
                        super::draw_string(
                            self.config.fb_base,
                            self.config.fb_width,
                            global_x as usize,
                            global_y as usize,
                            text,
                            *color,
                        );
                    }
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
                            self.config.fb_base,
                            self.config.fb_width,
                            global_x as usize,
                            global_y as usize,
                            *width as usize,
                            *height as usize,
                            *color,
                        );
                    }
                }
            }
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
    let mut comp = COMPOSITOR.lock();
    *comp = Some(Compositor::new(config));
}

/// 新しいWriterを登録（タスク作成時に呼ばれる）
///
/// # Arguments
/// * `region` - Writer用の描画領域
///
/// # Returns
/// 共有バッファへの参照。Compositorが未初期化ならNone
pub fn register_writer(region: Region) -> Option<SharedBuffer> {
    let mut comp = COMPOSITOR.lock();
    comp.as_mut().map(|c| c.register_writer(region))
}

/// Compositorタスクのエントリポイント
///
/// 無限ループでフレームを合成し続けます。
pub extern "C" fn compositor_task() -> ! {
    crate::info!("[Compositor] Started");

    loop {
        // フレームを合成
        {
            let mut comp = COMPOSITOR.lock();
            if let Some(compositor) = comp.as_mut() {
                compositor.compose_frame();
            }
        }

        // 次のリフレッシュまで待機
        // シンプルなビジーウェイト（将来的にはタイマーベースのスリープに変更）
        for _ in 0..100_000 {
            core::hint::spin_loop();
        }
    }
}
