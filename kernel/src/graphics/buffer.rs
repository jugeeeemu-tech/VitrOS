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
    /// 所有タスクのID（可視化モード用）
    #[cfg(feature = "visualize-pipeline")]
    owner_task_id: Option<u64>,
    /// 可視化用のバッファインデックス
    #[cfg(feature = "visualize-pipeline")]
    vis_buffer_index: Option<usize>,
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
            #[cfg(feature = "visualize-pipeline")]
            owner_task_id: None,
            #[cfg(feature = "visualize-pipeline")]
            vis_buffer_index: None,
        }
    }

    /// 所有タスクIDを設定（可視化モード用）
    #[cfg(feature = "visualize-pipeline")]
    pub fn set_owner_task_id(&mut self, task_id: u64) {
        self.owner_task_id = Some(task_id);
    }

    /// 所有タスクIDを取得（可視化モード用）
    #[cfg(feature = "visualize-pipeline")]
    pub fn owner_task_id(&self) -> Option<u64> {
        self.owner_task_id
    }

    /// 可視化用バッファインデックスを設定
    #[cfg(feature = "visualize-pipeline")]
    pub fn set_vis_buffer_index(&mut self, index: usize) {
        self.vis_buffer_index = Some(index);
    }

    /// 可視化用バッファインデックスを取得
    #[cfg(feature = "visualize-pipeline")]
    pub fn vis_buffer_index(&self) -> Option<usize> {
        self.vis_buffer_index
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

/// SharedBuffer用の同期フラッシュ拡張トレイト（可視化モード用）
#[cfg(feature = "visualize-pipeline")]
pub trait SyncFlushExt {
    /// コマンド追加後、Compositorによる処理完了を待機
    ///
    /// タスクはブロック状態に入り、Compositorがコマンドを処理して
    /// unblock_taskを呼ぶまで待機します。
    fn sync_flush(&self);
}

#[cfg(feature = "visualize-pipeline")]
impl SyncFlushExt for SharedBuffer {
    fn sync_flush(&self) {
        // ロックを解放した状態でブロック（Compositorがロックを取得できるように）
        crate::sched::block_current_task();
    }
}
