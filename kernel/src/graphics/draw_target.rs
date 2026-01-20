//! 描画ターゲットの抽象化
//!
//! このモジュールは、グラフィックス描画先を抽象化するためのトレイトを定義します。
//! フレームバッファ、オフスクリーンバッファ、ダブルバッファリングなど
//! 様々な描画先を統一的に扱うことができます。
//!
//! # トレイト
//!
//! - [`DrawTarget`] - 基本的な描画操作（矩形塗りつぶし、文字描画）
//! - [`DirtyTrackingTarget`] - ダーティ領域追跡による部分更新の最適化

use super::Region;

/// 描画ターゲットの抽象化トレイト
///
/// フレームバッファやオフスクリーンバッファなど、描画先となる
/// サーフェスが実装すべきインターフェースを定義します。
///
/// # Note
///
/// `base_addr()`は既存の描画関数との互換性のために提供されています。
/// 可能な限り`fill_rect()`, `draw_char()`, `draw_string()`を使用してください。
/// 直接メモリアクセスはunsafeであり、将来のAPI変更で動作しなくなる可能性があります。
#[allow(dead_code)]
pub trait DrawTarget {
    /// フレームバッファのベースアドレスを返す
    ///
    /// # Note
    ///
    /// このメソッドは既存コードとの互換性のために提供されています。
    /// 新規コードでは`fill_rect()`などの高レベルAPIを使用してください。
    fn base_addr(&self) -> u64;

    /// 描画領域の幅をピクセル単位で返す
    fn width(&self) -> u32;

    /// 描画領域の高さをピクセル単位で返す
    fn height(&self) -> u32;

    /// 1行あたりのピクセル数（ストライド）を返す
    ///
    /// デフォルトでは`width()`と同じ値を返します。
    /// パディングがある場合はオーバーライドしてください。
    fn stride(&self) -> u32 {
        self.width()
    }

    /// 指定された矩形領域を単色で塗りつぶす
    fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, color: u32);

    /// 指定された位置に文字を描画する
    fn draw_char(&mut self, x: u32, y: u32, ch: u8, color: u32);

    /// 指定された位置に文字列を描画する
    fn draw_string(&mut self, x: u32, y: u32, s: &str, color: u32);
}

/// ダーティ領域追跡機能を持つ描画ターゲット
///
/// [`DrawTarget`]を拡張し、変更された領域（ダーティ領域）を追跡する機能を追加します。
/// ダブルバッファリングや部分画面更新の最適化に使用できます。
#[allow(dead_code)]
pub trait DirtyTrackingTarget: DrawTarget {
    /// 指定された領域をダーティとしてマークする
    fn mark_dirty(&mut self, region: &Region);

    /// 全領域をダーティとしてマークする
    fn mark_all_dirty(&mut self);

    /// ダーティ領域を取得し、クリアする
    ///
    /// ダーティ領域がない場合は`None`を返します。
    fn take_dirty_rect(&mut self) -> Option<Region>;
}
