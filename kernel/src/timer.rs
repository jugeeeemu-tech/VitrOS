//! タイマー管理モジュール
//!
//! Linuxの hrtimer/timerfd に似た、タイマーキューとコールバック機構を提供します。
//! 割り込みハンドラでは期限切れタイマーの検出のみを行い、
//! 実際のコールバック実行はメインループで行うことで割り込み無効時間を最小化します。

use alloc::boxed::Box;
use alloc::collections::{BinaryHeap, VecDeque};
use core::cmp::Ordering;
use core::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use lazy_static::lazy_static;
use spin::Mutex;

/// グローバルタイマーカウンタ（tick数）
static TICK_COUNT: AtomicU64 = AtomicU64::new(0);

/// タイマーIDカウンタ
static TIMER_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// タイマー周波数（Hz）
static TIMER_FREQUENCY_HZ: AtomicU64 = AtomicU64::new(0);

/// タイマーコールバック型
pub type TimerCallback = Box<dyn FnOnce() + Send + 'static>;

/// タイマー構造体
pub struct Timer {
    /// タイマーID
    id: u64,
    /// 期限切れ時刻（tick数）
    expires_at: u64,
    /// コールバック関数
    callback: Option<TimerCallback>,
}

impl Timer {
    /// 新しいタイマーを作成
    ///
    /// # Arguments
    /// * `delay_ticks` - 現在時刻からの遅延（tick数）
    /// * `callback` - 期限切れ時に実行するコールバック
    pub fn new(delay_ticks: u64, callback: TimerCallback) -> Self {
        let id = TIMER_ID_COUNTER.fetch_add(1, AtomicOrdering::SeqCst);
        let expires_at = current_tick() + delay_ticks;
        Self {
            id,
            expires_at,
            callback: Some(callback),
        }
    }
}

// BinaryHeap用の順序付け（期限が早い方が優先度高い）
impl Ord for Timer {
    fn cmp(&self, other: &Self) -> Ordering {
        // 逆順にして、期限が早い方が先に来るようにする
        other.expires_at.cmp(&self.expires_at)
    }
}

impl PartialOrd for Timer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Eq for Timer {}

impl PartialEq for Timer {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

// タイマーキュー（期限でソートされた優先度付きキュー）
lazy_static! {
    static ref TIMER_QUEUE: Mutex<BinaryHeap<Timer>> = Mutex::new(BinaryHeap::new());
}

// ペンディングキュー（割り込みハンドラから期限切れタイマーを受け取る）
lazy_static! {
    static ref PENDING_QUEUE: Mutex<VecDeque<Timer>> = Mutex::new(VecDeque::new());
}

/// タイマーシステムを初期化
///
/// # Arguments
/// * `frequency_hz` - タイマー周波数（Hz）
pub fn init(frequency_hz: u64) {
    TIMER_FREQUENCY_HZ.store(frequency_hz, AtomicOrdering::SeqCst);
}

/// 現在のtick数を取得
pub fn current_tick() -> u64 {
    TICK_COUNT.load(AtomicOrdering::SeqCst)
}

/// tick数をインクリメント（APIC Timer割り込みハンドラから呼ばれる）
///
/// # Returns
/// インクリメント後のtick数
pub fn increment_tick() -> u64 {
    TICK_COUNT.fetch_add(1, AtomicOrdering::SeqCst) + 1
}

/// タイマーをキューに登録
///
/// # Arguments
/// * `delay_ticks` - 現在時刻からの遅延（tick数）
/// * `callback` - 期限切れ時に実行するコールバック
///
/// # Returns
/// タイマーID
pub fn register_timer(delay_ticks: u64, callback: TimerCallback) -> u64 {
    let timer = Timer::new(delay_ticks, callback);
    let id = timer.id;

    // 割り込みを無効化してからロックを取得（デッドロック回避）
    let flags = unsafe {
        let flags: u64;
        core::arch::asm!(
            "pushfq",
            "pop {}",
            "cli",
            out(reg) flags,
            options(nomem, nostack)
        );
        flags
    };

    let mut queue = TIMER_QUEUE.lock();
    queue.push(timer);
    drop(queue);

    // 割り込みを復元
    unsafe {
        if flags & 0x200 != 0 {
            core::arch::asm!("sti", options(nomem, nostack));
        }
    }

    id
}

/// 期限切れタイマーを検出してペンディングキューに移動（割り込みハンドラから呼ばれる）
///
/// この関数は割り込みコンテキストで実行されるため、最小限の処理のみを行います。
/// 実際のコールバック実行は process_pending_timers() で行われます。
pub fn check_timers() {
    let current = current_tick();
    let mut queue = TIMER_QUEUE.lock();
    let mut pending = PENDING_QUEUE.lock();

    // 期限切れのタイマーをペンディングキューに移動
    while let Some(timer) = queue.peek() {
        if timer.expires_at <= current {
            if let Some(timer) = queue.pop() {
                pending.push_back(timer);
            }
        } else {
            // まだ期限切れではない
            break;
        }
    }
}

/// ペンディングキューのタイマーを処理（メインループから呼ばれる）
///
/// この関数は通常コンテキストで実行されるため、コールバック実行中も割り込みを受け付けられます。
pub fn process_pending_timers() {
    loop {
        // ペンディングキューから1つ取り出す
        let timer = {
            let mut pending = PENDING_QUEUE.lock();
            pending.pop_front()
        };

        match timer {
            Some(mut timer) => {
                // コールバックを実行（割り込み有効状態で実行される）
                if let Some(callback) = timer.callback.take() {
                    callback();
                }
            }
            None => {
                // キューが空になった
                break;
            }
        }
    }
}

/// ミリ秒をtick数に変換
///
/// # Arguments
/// * `ms` - ミリ秒
#[allow(dead_code)]
pub fn ms_to_ticks(ms: u64) -> u64 {
    let frequency = TIMER_FREQUENCY_HZ.load(AtomicOrdering::SeqCst);
    (ms * frequency) / 1000
}

/// 秒をtick数に変換
///
/// # Arguments
/// * `seconds` - 秒
pub fn seconds_to_ticks(seconds: u64) -> u64 {
    let frequency = TIMER_FREQUENCY_HZ.load(AtomicOrdering::SeqCst);
    seconds * frequency
}
