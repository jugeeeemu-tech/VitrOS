//! タイマーデバイスの抽象化

/// ハードウェアタイマーデバイスの抽象化
pub trait TimerDevice {
    fn is_available(&self) -> bool;
    fn frequency(&self) -> u64;
    fn delay_ns(&self, ns: u64);

    #[inline]
    fn delay_us(&self, us: u64) {
        self.delay_ns(us * 1_000);
    }

    #[inline]
    fn delay_ms(&self, ms: u64) {
        self.delay_ns(ms * 1_000_000);
    }
}

/// 経過時間追跡機能
pub trait ElapsedTimer: TimerDevice {
    fn elapsed_ns(&self) -> u64;

    #[inline]
    fn elapsed_us(&self) -> u64 {
        self.elapsed_ns() / 1_000
    }

    #[inline]
    fn elapsed_ms(&self) -> u64 {
        self.elapsed_ns() / 1_000_000
    }
}
