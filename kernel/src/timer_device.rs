//! タイマーデバイスの抽象化
//!
//! このモジュールは、ハードウェアタイマーデバイス（PIT、APIC Timer、HPETなど）を
//! 抽象化するためのトレイトを定義します。
//!
//! # トレイト
//!
//! - [`TimerDevice`] - 基本的なタイマー機能（遅延、周波数取得）
//! - [`ElapsedTimer`] - 経過時間追跡機能を追加

/// ハードウェアタイマーデバイスの抽象化トレイト
///
/// タイマーデバイスが提供すべき基本機能を定義します。
/// PIT、APIC Timer、HPETなど様々なタイマーハードウェアを
/// 統一的なインターフェースで扱うことができます。
///
/// # 例
///
/// ```ignore
/// impl TimerDevice for MyTimer {
///     fn is_available(&self) -> bool { true }
///     fn frequency(&self) -> u64 { 1_000_000 }
///     fn delay_ns(&self, ns: u64) { /* ... */ }
/// }
/// ```
pub trait TimerDevice {
    /// このタイマーデバイスが利用可能かどうかを返す
    fn is_available(&self) -> bool;

    /// タイマーの動作周波数をHz単位で返す
    #[allow(dead_code)]
    fn frequency(&self) -> u64;

    /// 指定されたナノ秒だけ待機する
    fn delay_ns(&self, ns: u64);

    /// 指定されたマイクロ秒だけ待機する
    #[allow(dead_code)]
    #[inline]
    fn delay_us(&self, us: u64) {
        self.delay_ns(us.saturating_mul(1_000));
    }

    /// 指定されたミリ秒だけ待機する
    #[inline]
    fn delay_ms(&self, ms: u64) {
        self.delay_ns(ms.saturating_mul(1_000_000));
    }
}

/// 経過時間追跡機能を持つタイマーデバイス
///
/// [`TimerDevice`]を拡張し、経過時間を計測する機能を追加します。
/// パフォーマンス計測やプロファイリングに使用できます。
#[allow(dead_code)]
pub trait ElapsedTimer: TimerDevice {
    /// 計測開始からの経過時間をナノ秒で返す
    fn elapsed_ns(&self) -> u64;

    /// 計測開始からの経過時間をマイクロ秒で返す
    #[inline]
    fn elapsed_us(&self) -> u64 {
        self.elapsed_ns() / 1_000
    }

    /// 計測開始からの経過時間をミリ秒で返す
    #[inline]
    fn elapsed_ms(&self) -> u64 {
        self.elapsed_ns() / 1_000_000
    }
}
