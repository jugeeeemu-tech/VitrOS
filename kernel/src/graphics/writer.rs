//! Per-task Writer

use super::buffer::{DrawCommand, SharedBuffer};
use super::region::Region;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// タスクごとのWriter
///
/// 各タスクが独自のWriterインスタンスを持ち、
/// 描画コマンドをローカルバッファに蓄積し、
/// flush()で共有バッファに一括転送します。
///
/// これにより、1フレームの描画で1回のロック取得のみで済み、
/// ロック競合を大幅に削減します。
///
/// 最適化: 連続する文字をDrawStringにバッチ化することで、
/// コマンド数を大幅に削減し、パフォーマンスを向上させます。
pub struct TaskWriter {
    /// 共有バッファへの参照
    buffer: SharedBuffer,
    /// ローカルコマンドバッファ（ロックなしで追加可能）
    local_commands: Vec<DrawCommand>,
    /// 描画領域（領域チェック用にキャッシュ）
    region: Region,
    /// カーソル位置（ローカル座標）
    cursor_x: u32,
    cursor_y: u32,
    /// 現在の文字色
    color: u32,
    /// 現在蓄積中の文字列（バッチ化用）
    pending_text: String,
    /// 蓄積中の文字列の開始X座標
    pending_x: u32,
    /// 蓄積中の文字列の開始Y座標
    pending_y: u32,
}

impl TaskWriter {
    /// 新しいWriterを作成
    ///
    /// # Arguments
    /// * `buffer` - 共有バッファへの参照
    /// * `color` - 初期文字色
    pub fn new(buffer: SharedBuffer, color: u32) -> Self {
        // 共有バッファからregionを取得してキャッシュ
        let region = buffer.lock().region();
        Self {
            buffer,
            local_commands: Vec::with_capacity(32), // バッチ化により必要なコマンド数が減少
            region,
            cursor_x: 0,
            cursor_y: 0,
            color,
            pending_text: String::with_capacity(128), // 文字列バッファを事前確保
            pending_x: 0,
            pending_y: 0,
        }
    }

    /// カーソル位置を設定
    ///
    /// # Arguments
    /// * `x` - X座標（ローカル座標）
    /// * `y` - Y座標（ローカル座標）
    #[allow(dead_code)]
    pub fn set_position(&mut self, x: u32, y: u32) {
        self.cursor_x = x;
        self.cursor_y = y;
    }

    /// 文字色を設定
    ///
    /// # Arguments
    /// * `color` - 新しい文字色（0xRRGGBB形式）
    #[allow(dead_code)]
    pub fn set_color(&mut self, color: u32) {
        self.color = color;
    }

    /// 領域をクリア
    ///
    /// ローカルバッファにClearコマンドを追加します。
    /// 実際の描画はflush()呼び出し時に行われます。
    ///
    /// # Arguments
    /// * `bg_color` - 背景色
    pub fn clear(&mut self, bg_color: u32) {
        // 蓄積中のテキストをコミットしてからクリア
        self.commit_pending_text();
        self.local_commands
            .push(DrawCommand::Clear { color: bg_color });
        self.cursor_x = 0;
        self.cursor_y = 0;
    }

    /// 指定座標に文字列を描画
    ///
    /// カーソル位置は変更せず、明示座標へ直接描画コマンドを追加します。
    pub fn draw_string_at(&mut self, x: u32, y: u32, text: &str) {
        self.commit_pending_text();

        if x >= self.region.width || y >= self.region.height || text.is_empty() {
            return;
        }

        let max_columns = ((self.region.width - x) / 8) as usize;
        if max_columns == 0 {
            return;
        }

        let visible_text = prefix_to_fit(text, max_columns);
        if visible_text.is_empty() {
            return;
        }

        self.local_commands.push(DrawCommand::DrawString {
            x,
            y,
            text: String::from(visible_text),
            color: self.color,
        });
    }

    /// 指定矩形を塗りつぶす
    ///
    /// 矩形は現在の描画領域内にクリップされます。
    pub fn fill_rect(&mut self, x: u32, y: u32, width: u32, height: u32, color: u32) {
        self.commit_pending_text();

        if x >= self.region.width || y >= self.region.height || width == 0 || height == 0 {
            return;
        }

        let clipped_width = width.min(self.region.width - x);
        let clipped_height = height.min(self.region.height - y);
        if clipped_width == 0 || clipped_height == 0 {
            return;
        }

        self.local_commands.push(DrawCommand::FillRect {
            x,
            y,
            width: clipped_width,
            height: clipped_height,
            color,
        });
    }

    /// 指定座標に1文字を描画
    ///
    /// ピクセル座標で直接指定し、カーソル位置は変更しません。
    pub fn draw_char_at(&mut self, x: u32, y: u32, ch: char) {
        self.commit_pending_text();

        if !ch.is_ascii() || x + 8 > self.region.width || y + 8 > self.region.height {
            return;
        }

        self.local_commands.push(DrawCommand::DrawChar {
            x,
            y,
            ch: ch as u8,
            color: self.color,
        });
    }

    /// ローカルバッファのコマンドを共有バッファに一括転送
    ///
    /// この呼び出しでのみ共有バッファのロックを取得します。
    /// 1フレームの描画の最後に呼び出してください。
    pub fn flush(&mut self) {
        // 蓄積中のテキストをコミット
        self.commit_pending_text();

        if self.local_commands.is_empty() {
            return;
        }

        // 一括転送: drain()を使用してVecの容量を維持（アロケーションフリー）
        {
            let mut buf = self.buffer.lock();
            buf.extend_commands(self.local_commands.drain(..));
        }

        // 可視化フック: flush時にバッファ情報を通知
        notify_flush(&self.buffer);
    }

    /// 蓄積中のテキストをDrawStringコマンドにコミット
    ///
    /// 複数の文字を1つのDrawStringコマンドにバッチ化することで、
    /// コマンド数を大幅に削減します。
    fn commit_pending_text(&mut self) {
        if self.pending_text.is_empty() {
            return;
        }

        // 蓄積中のテキストをDrawStringとして追加
        // mem::take()で所有権を移動し、pending_textを空のStringで置換
        // これによりpending_textの容量は維持される（リアロケーション防止）
        let text = core::mem::take(&mut self.pending_text);
        self.local_commands.push(DrawCommand::DrawString {
            x: self.pending_x,
            y: self.pending_y,
            text,
            color: self.color,
        });
    }
}

// =============================================================================
// 可視化フック関数（featureフラグで有効版/no-op版を切り替え）
// =============================================================================

/// flush時の通知
#[cfg(feature = "visualize-pipeline")]
#[inline(always)]
fn notify_flush(buffer: &SharedBuffer) {
    // タスクIDからバッファインデックスを逆引き
    if let Some(buffer_index) = crate::pipeline_visualization::get_buffer_index_for_current_task() {
        let buf = buffer.lock();
        let commands = buf.commands();
        let command_count = commands.len();
        let mut command_types: [Option<&'static str>; 5] = [None; 5];
        for (i, cmd) in commands.iter().enumerate().take(5) {
            command_types[i] = Some(match cmd {
                DrawCommand::Clear { .. } => "Clear",
                DrawCommand::FillRect { .. } => "FillRect",
                DrawCommand::DrawString { .. } => "String",
                DrawCommand::DrawChar { .. } => "Char",
            });
        }
        drop(buf); // ロック解放してからフック呼び出し
        crate::pipeline_visualization::on_flush_hook(buffer_index, command_count, command_types);
    }
}

#[cfg(not(feature = "visualize-pipeline"))]
#[inline(always)]
fn notify_flush(_buffer: &SharedBuffer) {}

fn prefix_to_fit(text: &str, max_columns: usize) -> &str {
    if max_columns == 0 {
        return "";
    }

    match text.char_indices().nth(max_columns) {
        Some((idx, _)) => &text[..idx],
        None => text,
    }
}

impl core::fmt::Write for TaskWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        // 最適化: 連続する文字をDrawStringにバッチ化
        for ch in s.bytes() {
            if ch == b'\n' {
                // 改行時: 蓄積中のテキストをコミット
                self.commit_pending_text();
                self.cursor_x = 0;
                self.cursor_y += 10;
            } else {
                // 領域内に収まるかチェック
                if self.cursor_x + 8 > self.region.width {
                    // 行の折り返し: 蓄積中のテキストをコミット
                    self.commit_pending_text();
                    self.cursor_x = 0;
                    self.cursor_y += 10;
                }

                // 縦方向のオーバーフロー処理
                if self.cursor_y + 8 > self.region.height {
                    // 蓄積中のテキストをコミットしてからクリア
                    self.commit_pending_text();
                    self.local_commands
                        .push(DrawCommand::Clear { color: 0x00000000 });
                    self.cursor_y = 0;
                }

                // 新しい行の開始位置を記録
                if self.pending_text.is_empty() {
                    self.pending_x = self.cursor_x;
                    self.pending_y = self.cursor_y;
                }

                // 文字を蓄積（1バイトのASCII文字として）
                self.pending_text.push(ch as char);
                self.cursor_x += 8;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::buffer::WriterBuffer;

    fn test_buffer(region: Region) -> SharedBuffer {
        Arc::new(crate::sync::BlockingMutex::new(WriterBuffer::new(region)))
    }

    #[test_case]
    fn test_draw_string_at_commits_pending_text_before_explicit_draw() {
        let buffer = test_buffer(Region::new(0, 0, 64, 32));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), 0x00FF_FFFF);

        let _ = core::fmt::Write::write_str(&mut writer, "ab");
        writer.draw_string_at(16, 10, "cd");
        writer.flush();

        let commands = buffer.lock().commands().to_vec();
        assert_eq!(commands.len(), 2);
        assert!(matches!(
            &commands[0],
            DrawCommand::DrawString {
                x: 0,
                y: 0,
                text,
                color: 0x00FF_FFFF,
            } if text == "ab"
        ));
        assert!(matches!(
            &commands[1],
            DrawCommand::DrawString {
                x: 16,
                y: 10,
                text,
                color: 0x00FF_FFFF,
            } if text == "cd"
        ));
    }

    #[test_case]
    fn test_draw_string_at_clips_to_region_width() {
        let buffer = test_buffer(Region::new(0, 0, 32, 32));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), 0x00FF_FFFF);

        writer.draw_string_at(16, 0, "abcd");
        writer.flush();

        let commands = buffer.lock().commands().to_vec();
        assert_eq!(commands.len(), 1);
        assert!(matches!(
            &commands[0],
            DrawCommand::DrawString {
                x: 16,
                y: 0,
                text,
                color: 0x00FF_FFFF,
            } if text == "ab"
        ));
    }

    #[test_case]
    fn test_fill_rect_clips_to_region_and_commits_pending_text() {
        let buffer = test_buffer(Region::new(0, 0, 20, 12));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), 0x00FF_FFFF);

        let _ = core::fmt::Write::write_str(&mut writer, "xy");
        writer.fill_rect(12, 8, 20, 10, 0x0012_3456);
        writer.flush();

        let commands = buffer.lock().commands().to_vec();
        assert_eq!(commands.len(), 2);
        assert!(matches!(
            &commands[0],
            DrawCommand::DrawString {
                x: 0,
                y: 0,
                text,
                color: 0x00FF_FFFF,
            } if text == "xy"
        ));
        assert!(matches!(
            &commands[1],
            DrawCommand::FillRect {
                x: 12,
                y: 8,
                width: 8,
                height: 4,
                color: 0x0012_3456,
            }
        ));
    }

    #[test_case]
    fn test_draw_char_at_emits_draw_char_and_commits_pending_text() {
        let buffer = test_buffer(Region::new(0, 0, 32, 16));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), 0x00FF_FFFF);

        let _ = core::fmt::Write::write_str(&mut writer, "ab");
        writer.draw_char_at(16, 0, 'c');
        writer.flush();

        let commands = buffer.lock().commands().to_vec();
        assert_eq!(commands.len(), 2);
        assert!(matches!(
            &commands[0],
            DrawCommand::DrawString {
                x: 0,
                y: 0,
                text,
                color: 0x00FF_FFFF,
            } if text == "ab"
        ));
        assert!(matches!(
            &commands[1],
            DrawCommand::DrawChar {
                x: 16,
                y: 0,
                ch: b'c',
                color: 0x00FF_FFFF,
            }
        ));
    }

    #[test_case]
    fn test_draw_char_at_ignores_non_ascii() {
        let buffer = test_buffer(Region::new(0, 0, 32, 16));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), 0x00FF_FFFF);

        writer.draw_char_at(0, 0, 'é');
        writer.flush();

        assert!(buffer.lock().commands().is_empty());
    }

    #[test_case]
    fn test_draw_char_at_ignores_out_of_bounds() {
        let buffer = test_buffer(Region::new(0, 0, 16, 16));
        let mut writer = TaskWriter::new(Arc::clone(&buffer), 0x00FF_FFFF);

        writer.draw_char_at(9, 0, 'a');
        writer.draw_char_at(0, 9, 'a');
        writer.flush();

        assert!(buffer.lock().commands().is_empty());
    }
}
