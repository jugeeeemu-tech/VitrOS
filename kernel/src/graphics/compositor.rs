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
    /// シャドウフレームバッファ（バックバッファ）
    shadow_buffer: ShadowBuffer,
}

impl Compositor {
    /// 新しいCompositorを作成
    ///
    /// # Arguments
    /// * `config` - Compositorの設定
    pub fn new(config: CompositorConfig) -> Self {
        let shadow_buffer = ShadowBuffer::new(config.fb_width, config.fb_height);
        Self {
            config,
            buffers: Vec::new(),
            shadow_buffer,
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
    /// シャドウバッファに描画後、ハードウェアフレームバッファに一括転送します。
    pub fn compose_frame(&mut self) {
        // まず全バッファからコマンドを収集（借用の分離）
        let mut collected: Vec<(Region, Vec<DrawCommand>)> = Vec::new();
        for buffer in &self.buffers {
            // try_lockを使用して、ロック中のバッファはスキップ
            if let Some(mut buf) = buffer.try_lock() {
                if buf.is_dirty() {
                    let region = buf.region();
                    let commands = buf.take_commands();
                    collected.push((region, commands));
                }
            }
        }

        // 収集したコマンドをシャドウバッファにレンダリング
        for (region, commands) in &collected {
            self.render_commands(region, commands);
        }

        // シャドウバッファをハードウェアフレームバッファに転送
        unsafe {
            self.shadow_buffer.blit_to(self.config.fb_base);
        }

        // フレームカウントをインクリメント
        FRAME_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    /// コマンドをシャドウバッファに描画
    ///
    /// # Arguments
    /// * `region` - 描画領域
    /// * `commands` - 描画コマンドのスライス
    fn render_commands(&mut self, region: &Region, commands: &[DrawCommand]) {
        let shadow_base = self.shadow_buffer.base_addr();
        let shadow_width = self.shadow_buffer.width();

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
                    self.shadow_buffer.mark_dirty(region);
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
                    self.shadow_buffer
                        .mark_dirty(&Region::new(global_x, global_y, 8, 8));
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
                    self.shadow_buffer
                        .mark_dirty(&Region::new(global_x, global_y, text_width, 8));
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
                    self.shadow_buffer
                        .mark_dirty(&Region::new(global_x, global_y, *width, *height));
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

    result
}

/// Compositorタスクのエントリポイント
///
/// 無限ループでフレームを合成し続けます。
pub extern "C" fn compositor_task() -> ! {
    crate::info!("[Compositor] Started");

    loop {
        // フレームを合成（割り込み無効でロック取得）
        {
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

            {
                let mut comp = COMPOSITOR.lock();
                if let Some(compositor) = comp.as_mut() {
                    compositor.compose_frame();
                }
            }

            // 割り込みを元の状態に復元
            unsafe {
                if flags & 0x200 != 0 {
                    core::arch::asm!("sti", options(nomem, nostack));
                }
            }
        }

        // 次のリフレッシュまで待機（約60fps = 16ms間隔）
        crate::sched::sleep_ms(16);
    }
}
