//! Per-task Writer

use super::buffer::{DrawCommand, SharedBuffer};

/// タスクごとのWriter
///
/// 各タスクが独自のWriterインスタンスを持ち、
/// 描画コマンドを自身のバッファに追加します。
pub struct TaskWriter {
    /// 共有バッファへの参照
    buffer: SharedBuffer,
    /// カーソル位置（ローカル座標）
    cursor_x: u32,
    cursor_y: u32,
    /// 現在の文字色
    color: u32,
}

impl TaskWriter {
    /// 新しいWriterを作成
    ///
    /// # Arguments
    /// * `buffer` - 共有バッファへの参照
    /// * `color` - 初期文字色
    pub fn new(buffer: SharedBuffer, color: u32) -> Self {
        Self {
            buffer,
            cursor_x: 0,
            cursor_y: 0,
            color,
        }
    }

    /// カーソル位置を設定
    ///
    /// # Arguments
    /// * `x` - X座標（ローカル座標）
    /// * `y` - Y座標（ローカル座標）
    pub fn set_position(&mut self, x: u32, y: u32) {
        self.cursor_x = x;
        self.cursor_y = y;
    }

    /// 文字色を設定
    ///
    /// # Arguments
    /// * `color` - 新しい文字色（0xRRGGBB形式）
    pub fn set_color(&mut self, color: u32) {
        self.color = color;
    }

    /// 領域をクリア
    ///
    /// # Arguments
    /// * `bg_color` - 背景色
    pub fn clear(&mut self, bg_color: u32) {
        let mut buf = self.buffer.lock();
        buf.push_command(DrawCommand::Clear { color: bg_color });
    }

    /// 改行処理
    fn newline(&mut self) {
        self.cursor_x = 0;
        self.cursor_y += 10; // 行の高さ（8ピクセル文字 + 2ピクセル間隔）
    }
}

impl core::fmt::Write for TaskWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let mut buf = self.buffer.lock();
        let region = buf.region();

        for ch in s.bytes() {
            if ch == b'\n' {
                // 改行処理（インライン展開）
                self.cursor_x = 0;
                self.cursor_y += 10;
            } else {
                // 領域内に収まるかチェック
                if self.cursor_x + 8 > region.width {
                    // 改行処理（インライン展開）
                    self.cursor_x = 0;
                    self.cursor_y += 10;
                }

                // 縦方向のオーバーフロー処理
                if self.cursor_y + 8 > region.height {
                    // 領域をクリアして先頭に戻る
                    buf.push_command(DrawCommand::Clear { color: 0x00000000 });
                    self.cursor_y = 0;
                }

                buf.push_command(DrawCommand::DrawChar {
                    x: self.cursor_x,
                    y: self.cursor_y,
                    ch,
                    color: self.color,
                });

                self.cursor_x += 8;
            }
        }
        Ok(())
    }
}
