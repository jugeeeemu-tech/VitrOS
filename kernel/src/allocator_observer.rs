//! Allocator Observer トレイト
//!
//! メモリアロケータのイベントを監視するオブザーバーパターンを実装。
//! SlabAllocatorは`#[global_allocator]`で使用され、`const fn new()`で
//! 初期化が必要なため、ジェネリクス化ではなく条件付きコンパイル +
//! フック関数アプローチを採用。

/// アロケータオブザーバートレイト
///
/// アロケータの割り当て・解放イベントを監視します。
/// デフォルト実装により、各メソッドは何もしないno-op動作となります。
///
/// # 使用例
///
/// ```ignore
/// struct MyAllocatorObserver;
///
/// impl AllocatorObserver for MyAllocatorObserver {
///     fn on_allocate(&self, class_idx: usize, ptr: *mut u8) {
///         // 割り当てを記録
///     }
/// }
/// ```
pub trait AllocatorObserver: Send + Sync {
    /// メモリ割り当て時に呼ばれる
    ///
    /// # Arguments
    /// * `class_idx` - サイズクラスのインデックス
    /// * `ptr` - 割り当てられたポインタ
    fn on_allocate(&self, _class_idx: usize, _ptr: *mut u8) {}

    /// メモリ解放時に呼ばれる
    ///
    /// # Arguments
    /// * `class_idx` - サイズクラスのインデックス
    /// * `ptr` - 解放されるポインタ
    fn on_deallocate(&self, _class_idx: usize, _ptr: *mut u8) {}

    /// 指定サイズクラスの空きブロック数を取得
    ///
    /// # Arguments
    /// * `class_idx` - サイズクラスのインデックス
    ///
    /// # Returns
    /// 空きブロック数
    fn count_free_blocks(&self, _class_idx: usize) -> usize {
        0
    }

    /// 大きなサイズ用領域の使用状況を取得
    ///
    /// # Returns
    /// (使用量, 総容量) のタプル
    fn large_alloc_usage(&self) -> (usize, usize) {
        (0, 0)
    }
}

/// No-op アロケータオブザーバー（ZST - メモリ消費ゼロ）
///
/// 何もしないデフォルトのオブザーバー実装。
/// ゼロサイズ型（ZST）であるため、メモリを消費しません。
///
/// # サイズ保証
///
/// ```ignore
/// assert_eq!(core::mem::size_of::<NoOpAllocatorObserver>(), 0);
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOpAllocatorObserver;

impl AllocatorObserver for NoOpAllocatorObserver {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_allocator_observer_is_zst() {
        assert_eq!(core::mem::size_of::<NoOpAllocatorObserver>(), 0);
    }
}
