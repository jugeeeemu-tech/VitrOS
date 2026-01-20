//! Compositor Observer トレイト
//!
//! Compositorの各フェーズを監視するオブザーバーパターンを実装。
//! ジェネリクス + ZST（ゼロサイズ型）によるゼロコスト抽象化を実現。

use super::buffer::{DrawCommand, SharedBuffer};
use super::region::Region;

/// Compositorの各フェーズを監視するオブザーバートレイト
///
/// デフォルト実装により、各メソッドは何もしないno-op動作となる。
/// `#[inline(always)]`によりコンパイル時に最適化され、
/// NoOpObserverを使用する場合はゼロコストで呼び出しが消える。
///
/// # 使用例
///
/// ```ignore
/// struct MyObserver;
///
/// impl CompositorObserver for MyObserver {
///     fn on_frame_start(&mut self, buffers: &[SharedBuffer], width: u32, height: u32) -> bool {
///         // カスタム処理
///         false
///     }
/// }
/// ```
pub trait CompositorObserver {
    /// バッファ登録時に呼ばれる
    ///
    /// # Arguments
    /// * `buffer_index` - 登録されたバッファのインデックス
    /// * `buffer` - 登録されたバッファへの参照
    #[inline(always)]
    fn on_buffer_registered(&mut self, _buffer_index: usize, _buffer: &SharedBuffer) {}

    /// フレーム処理開始時に呼ばれる
    ///
    /// # Arguments
    /// * `buffers` - 全バッファのスライス
    /// * `width` - 画面幅
    /// * `height` - 画面高さ
    ///
    /// # Returns
    /// `true`を返すと通常のレンダリング処理をスキップする（可視化モード用）
    #[inline(always)]
    fn on_frame_start(&mut self, _buffers: &[SharedBuffer], _width: u32, _height: u32) -> bool {
        false
    }

    /// コマンド処理時に呼ばれる
    ///
    /// # Arguments
    /// * `buffer_idx` - 処理中のバッファインデックス
    /// * `region` - 描画領域
    /// * `cmd` - 描画コマンド
    #[inline(always)]
    fn on_command_processed(&mut self, _buffer_idx: usize, _region: &Region, _cmd: &DrawCommand) {}

    /// Blit完了時に呼ばれる
    #[inline(always)]
    fn on_blit_complete(&mut self) {}
}

/// No-op オブザーバー（ZST - メモリ消費ゼロ）
///
/// 何もしないデフォルトのオブザーバー実装。
/// ゼロサイズ型（ZST）であるため、Compositorのサイズに影響しない。
/// コンパイル時の最適化により、オブザーバー呼び出しは完全に消える。
///
/// # サイズ保証
///
/// ```ignore
/// assert_eq!(core::mem::size_of::<NoOpObserver>(), 0);
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOpObserver;

impl CompositorObserver for NoOpObserver {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_observer_is_zst() {
        assert_eq!(core::mem::size_of::<NoOpObserver>(), 0);
    }
}
