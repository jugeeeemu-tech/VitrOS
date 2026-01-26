//! 描画バッファと描画コマンド

use super::region::Region;
use crate::sync::BlockingMutex;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// 描画コマンドの列挙型
///
/// 生ピクセルではなく高レベルコマンドを格納することで、
/// メモリ効率を高め、Compositorが最適化を適用可能にします。
#[allow(dead_code)]
#[derive(Clone)]
pub enum DrawCommand {
    /// 文字を描画 (x, y は Region 内のローカル座標)
    DrawChar { x: u32, y: u32, ch: u8, color: u32 },
    /// 文字列を描画
    DrawString {
        x: u32,
        y: u32,
        text: String,
        color: u32,
    },
    /// 矩形を塗りつぶし
    FillRect {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        color: u32,
    },
    /// 領域全体をクリア
    Clear { color: u32 },
}

/// 描画コマンドを格納するバッファ
pub struct WriterBuffer {
    /// 描画コマンドのキュー
    commands: Vec<DrawCommand>,
    /// バッファが変更されたかのフラグ
    dirty: bool,
    /// このバッファの描画領域
    region: Region,
}

impl WriterBuffer {
    /// 新しいWriterBufferを作成
    ///
    /// # Arguments
    /// * `region` - このバッファの描画領域
    pub fn new(region: Region) -> Self {
        Self {
            commands: Vec::with_capacity(64), // 初期容量64コマンド
            dirty: false,
            region,
        }
    }

    /// コマンドを追加
    ///
    /// # Arguments
    /// * `cmd` - 追加する描画コマンド
    #[allow(dead_code)]
    pub fn push_command(&mut self, cmd: DrawCommand) {
        self.commands.push(cmd);
        self.dirty = true;
    }

    /// 複数のコマンドを一括で追加（アロケーションフリー）
    ///
    /// # Arguments
    /// * `commands` - 追加する描画コマンドのイテレータ
    pub fn extend_commands<I: IntoIterator<Item = DrawCommand>>(&mut self, commands: I) {
        let old_len = self.commands.len();
        self.commands.extend(commands);
        if self.commands.len() > old_len {
            self.dirty = true;
        }
    }

    /// コマンドのスライス参照を取得（アロケーションなし）
    ///
    /// # Returns
    /// 蓄積された描画コマンドへのスライス参照
    #[inline]
    pub fn commands(&self) -> &[DrawCommand] {
        &self.commands
    }

    /// コマンドをクリアしてダーティフラグをリセット
    ///
    /// Vecの容量は維持されるため、再アロケーションは発生しません。
    #[inline]
    pub fn clear_commands(&mut self) {
        self.commands.clear();
        self.dirty = false;
    }

    /// ダーティかどうか
    ///
    /// # Returns
    /// バッファに未描画のコマンドがあればtrue
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// 領域を取得
    ///
    /// # Returns
    /// このバッファの描画領域
    pub fn region(&self) -> Region {
        self.region
    }
}

/// 共有可能なバッファハンドル
///
/// Arc<BlockingMutex<WriterBuffer>>の型エイリアス。
/// TaskWriterとCompositorの間でバッファを共有するために使用します。
pub type SharedBuffer = Arc<BlockingMutex<WriterBuffer>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_writer_buffer_new() {
        let region = Region::new(0, 0, 100, 50);
        let buffer = WriterBuffer::new(region);
        // 初期状態ではdirty=false
        assert!(!buffer.is_dirty());
        assert!(buffer.commands().is_empty());
    }

    #[test_case]
    fn test_writer_buffer_push_command() {
        let region = Region::new(0, 0, 100, 50);
        let mut buffer = WriterBuffer::new(region);

        buffer.push_command(DrawCommand::Clear { color: 0xFFFFFF });

        // コマンド追加でdirty=true
        assert!(buffer.is_dirty());
        assert_eq!(buffer.commands().len(), 1);
    }

    #[test_case]
    fn test_writer_buffer_clear_commands() {
        let region = Region::new(0, 0, 100, 50);
        let mut buffer = WriterBuffer::new(region);

        buffer.push_command(DrawCommand::Clear { color: 0xFFFFFF });
        assert!(buffer.is_dirty());

        buffer.clear_commands();

        // クリアでdirty=false
        assert!(!buffer.is_dirty());
        assert!(buffer.commands().is_empty());
    }

    #[test_case]
    fn test_writer_buffer_region() {
        let region = Region::new(10, 20, 100, 50);
        let buffer = WriterBuffer::new(region);

        let stored_region = buffer.region();
        assert_eq!(stored_region.x, 10);
        assert_eq!(stored_region.y, 20);
        assert_eq!(stored_region.width, 100);
        assert_eq!(stored_region.height, 50);
    }
}
