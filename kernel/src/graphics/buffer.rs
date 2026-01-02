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
    pub fn push_command(&mut self, cmd: DrawCommand) {
        self.commands.push(cmd);
        self.dirty = true;
    }

    /// ダーティフラグをクリアしてコマンドを取得
    ///
    /// # Returns
    /// 蓄積された描画コマンドのVec
    pub fn take_commands(&mut self) -> Vec<DrawCommand> {
        self.dirty = false;
        core::mem::take(&mut self.commands)
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
