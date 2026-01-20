//! Per-task Writer

use super::buffer::{DrawCommand, SharedBuffer};
use super::region::Region;
use alloc::string::String;
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
