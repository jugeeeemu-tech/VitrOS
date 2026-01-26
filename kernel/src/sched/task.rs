//! タスク構造体と関連型の定義
//!
//! このモジュールはタスクの基本的な構造体、状態、優先度を定義します。

use alloc::boxed::Box;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::paging::KERNEL_VIRTUAL_BASE;

use super::context::Context;

/// タスク操作のエラー型
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskError {
    /// 無効な優先度（アイドルタスク以下）
    InvalidPriority,
    /// スタック割り当て失敗
    StackAllocationFailed,
    /// 無効なスタックアドレス
    InvalidStackAddress,
    /// コンテキスト初期化失敗
    ContextInitFailed,
    /// タスクキューが満杯
    QueueFull,
}

impl core::fmt::Display for TaskError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            TaskError::InvalidPriority => {
                write!(
                    f,
                    "Invalid rt_priority for Realtime task (must be >= {})",
                    rt_priority::MIN
                )
            }
            TaskError::StackAllocationFailed => write!(f, "Failed to allocate task stack"),
            TaskError::InvalidStackAddress => write!(f, "Invalid stack address"),
            TaskError::ContextInitFailed => write!(f, "Failed to initialize task context"),
            TaskError::QueueFull => write!(f, "Task queue is full"),
        }
    }
}

/// タスクID
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskId(u64);

impl TaskId {
    /// 新しいタスクIDを生成
    pub fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        TaskId(id)
    }

    /// u64からTaskIdを作成（可視化モード用）
    #[cfg(feature = "visualize-pipeline")]
    pub fn from_u64(id: u64) -> Self {
        TaskId(id)
    }

    /// タスクIDの値を取得
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

/// Nice値の型（Linuxスタイル）
///
/// -20（最高優先度）〜 +19（最低優先度）の範囲で、
/// Normalクラスのタスクの相対的な優先度を表現します。
/// nice値が低いほど、より多くのCPU時間が割り当てられます。
pub type Nice = i8;

/// Nice値の定数
pub mod nice {
    /// 最高優先度（通常はroot権限が必要）
    pub const MIN: i8 = -20;
    /// デフォルト優先度
    pub const DEFAULT: i8 = 0;
    /// 最低優先度
    pub const MAX: i8 = 19;
}

/// Realtimeクラス用の優先度型
///
/// 1（最低）〜 99（最高）の範囲で、Realtimeクラスのタスクの優先度を表現します。
pub type RtPriority = u8;

/// Realtime優先度の定数
pub mod rt_priority {
    /// 最低優先度
    pub const MIN: u8 = 1;
    /// デフォルト優先度
    #[allow(dead_code)]
    pub const DEFAULT: u8 = 50;
    /// 最高優先度
    pub const MAX: u8 = 99;
}

/// スケジューリングクラス
///
/// タスクの優先度クラスを表します。上位クラスのキューが空になるまで、
/// 下位クラスのタスクは実行されません。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulingClass {
    /// リアルタイムクラス（最高優先度）
    /// Compositor、マウス描画など即座に応答が必要なタスク用
    Realtime = 2,
    /// 通常クラス（CFS方式）
    /// 一般的なアプリケーションタスク用
    Normal = 1,
    /// アイドルクラス（最低優先度）
    /// 他に実行可能タスクがない場合のみ実行
    Idle = 0,
}

/// Nice値から重みを計算（Linuxスタイル）
///
/// nice値が低い（優先度が高い）タスクほど大きな重みを持ち、vruntimeの増加が遅くなります。
/// - nice 0（デフォルト）の重みは1024
/// - nice値が1下がる（優先度上昇）ごとに約1.25倍の重みを持つ
/// - nice値が1上がる（優先度低下）ごとに約0.8倍の重みになる
///
/// # Arguments
/// * `nice` - Nice値（-20〜+19）
///
/// # Returns
/// スケジューリング用の重み
///
/// # Note
/// 範囲外のnice値はクランプされます。
pub fn nice_to_weight(nice: Nice) -> u32 {
    // Linux kernel の sched_prio_to_weight テーブルと同等
    // インデックス0 = nice -20（最高優先度）、インデックス39 = nice +19（最低優先度）
    const PRIO_TO_WEIGHT: [u32; 40] = [
        88761, 71755, 56483, 46273, 36291, 29154, 23254, 18705, 14949, 11916, 9548, 7620, 6100,
        4904, 3906, 3121, 2501, 1991, 1586, 1277, 1024, 820, 655, 526, 423, 335, 272, 215, 172,
        137, 110, 87, 70, 56, 45, 36, 29, 23, 18, 15,
    ];

    // nice値を0-39のインデックスに変換（圧縮なし、直接対応）
    // nice -20 -> index 0 (重み88761)
    // nice 0   -> index 20 (重み1024)
    // nice +19 -> index 39 (重み15)
    let clamped = nice.clamp(nice::MIN, nice::MAX);
    let index = (clamped - nice::MIN) as usize; // -20 -> 0, +19 -> 39

    PRIO_TO_WEIGHT[index]
}

/// タスクの状態
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    /// 実行中（CPUを使用中）
    Running,
    /// 実行可能（CPUを待機中）
    Ready,
    /// ブロック中（I/O待ちなど）
    Blocked,
    /// 終了
    Terminated,
}

/// タスクスタック
const STACK_SIZE: usize = 16384; // 16KB

#[repr(align(16))]
pub(super) struct TaskStack([u8; STACK_SIZE]);

impl TaskStack {
    pub(super) const fn new() -> Self {
        Self([0; STACK_SIZE])
    }

    /// スタックの最上位アドレスを取得（仮想アドレス）
    pub(super) fn top(&self) -> u64 {
        let base = self.0.as_ptr() as u64;
        let physical_top = base + STACK_SIZE as u64;

        // ヒープは物理アドレスで割り当てられるため、カーネル仮想アドレスに変換
        // カーネルは KERNEL_VIRTUAL_BASE (0xFFFF800000000000) 以降で動作
        if physical_top >= KERNEL_VIRTUAL_BASE {
            // 既に仮想アドレス
            physical_top
        } else {
            // 物理アドレスを仮想アドレスに変換
            KERNEL_VIRTUAL_BASE + physical_top
        }
    }
}

/// タスク制御ブロック (Task Control Block)
pub struct Task {
    /// タスクID
    id: TaskId,
    /// タスク名（デバッグ用）
    name: &'static str,
    /// スケジューリングクラス（Realtime, Normal, Idle）
    sched_class: SchedulingClass,
    /// Normalクラス用のnice値（-20〜+19）
    /// nice値が低いほど、より多くのCPU時間が割り当てられる
    #[allow(dead_code)]
    nice: Nice,
    /// Realtimeクラス用の優先度（1-99）
    /// rt_priorityが高いほど、優先的に実行される
    rt_priority: RtPriority,
    /// スケジューリング用の重み（nice値から計算、Normalクラスで使用）
    /// 値が大きいほど、vruntimeの増加が遅くなり、より頻繁に実行される
    weight: u32,
    /// 仮想実行時間（CFS風スケジューリング、Normalクラスで使用）
    /// この値が小さいタスクが優先的に実行される
    vruntime: u64,
    /// CPUコンテキスト
    context: Context,
    /// タスクの状態
    state: TaskState,
    /// タスク専用スタック（ヒープに割り当て）
    #[allow(dead_code)]
    stack: Box<TaskStack>,
}

impl Task {
    /// Normalクラスのタスクを作成
    ///
    /// # Arguments
    /// * `name` - タスク名
    /// * `nice` - Nice値（-20〜+19、小さいほど高優先度）
    /// * `entry_point` - エントリポイント関数のアドレス
    ///
    /// # Errors
    /// * `TaskError::StackAllocationFailed` - スタック割り当てに失敗した場合
    /// * `TaskError::ContextInitFailed` - コンテキスト初期化に失敗した場合
    ///
    /// # Note
    /// nice値は自動的に有効範囲（-20〜+19）にクランプされます。
    pub fn new(
        name: &'static str,
        nice: Nice,
        entry_point: extern "C" fn() -> !,
    ) -> Result<Self, TaskError> {
        // スタックをヒープに割り当て
        let stack = Box::new(TaskStack::new());
        let stack_top = stack.top();

        let context = Context::new(entry_point as u64, stack_top)?;

        // nice値から重みを計算
        let clamped_nice = nice.clamp(nice::MIN, nice::MAX);
        let weight = nice_to_weight(clamped_nice);

        Ok(Self {
            id: TaskId::new(),
            name,
            sched_class: SchedulingClass::Normal,
            nice: clamped_nice,
            rt_priority: 0, // Normalクラスでは使用しない
            weight,
            vruntime: 0, // 初期値は0
            context,
            state: TaskState::Ready,
            stack,
        })
    }

    /// Realtimeクラスのタスクを作成
    ///
    /// # Arguments
    /// * `name` - タスク名
    /// * `rt_priority` - Realtime優先度（1-99、大きいほど高優先度）
    /// * `entry_point` - エントリポイント関数のアドレス
    ///
    /// # Errors
    /// * `TaskError::InvalidPriority` - rt_priorityが0の場合
    /// * `TaskError::StackAllocationFailed` - スタック割り当てに失敗した場合
    /// * `TaskError::ContextInitFailed` - コンテキスト初期化に失敗した場合
    pub fn new_realtime(
        name: &'static str,
        rt_priority: RtPriority,
        entry_point: extern "C" fn() -> !,
    ) -> Result<Self, TaskError> {
        // rt_priority 0は無効（Normalクラスと区別するため）
        if rt_priority < rt_priority::MIN {
            return Err(TaskError::InvalidPriority);
        }

        // スタックをヒープに割り当て
        let stack = Box::new(TaskStack::new());
        let stack_top = stack.top();

        let context = Context::new(entry_point as u64, stack_top)?;

        // Realtimeクラスではweightとvruntimeは使用しない
        Ok(Self {
            id: TaskId::new(),
            name,
            sched_class: SchedulingClass::Realtime,
            nice: 0, // Realtimeクラスでは使用しない
            rt_priority: rt_priority.min(rt_priority::MAX),
            weight: 0,   // Realtimeクラスでは使用しない
            vruntime: 0, // Realtimeクラスでは使用しない
            context,
            state: TaskState::Ready,
            stack,
        })
    }

    /// アイドルタスク専用の作成関数
    ///
    /// # Arguments
    /// * `name` - タスク名
    /// * `entry_point` - エントリポイント関数のアドレス
    ///
    /// # Errors
    /// * `TaskError::StackAllocationFailed` - スタック割り当てに失敗した場合
    /// * `TaskError::ContextInitFailed` - コンテキスト初期化に失敗した場合
    pub fn new_idle(
        name: &'static str,
        entry_point: extern "C" fn() -> !,
    ) -> Result<Self, TaskError> {
        // スタックをヒープに割り当て
        let stack = Box::new(TaskStack::new());
        let stack_top = stack.top();

        let context = Context::new(entry_point as u64, stack_top)?;

        Ok(Self {
            id: TaskId::new(),
            name,
            sched_class: SchedulingClass::Idle,
            nice: nice::MAX, // Idleは最低優先度相当
            rt_priority: 0,
            weight: nice_to_weight(nice::MAX), // 参考値
            vruntime: 0,
            context,
            state: TaskState::Ready,
            stack,
        })
    }

    /// タスクIDを取得
    pub fn id(&self) -> TaskId {
        self.id
    }

    /// タスク名を取得
    pub fn name(&self) -> &str {
        self.name
    }

    /// Nice値を取得（Normalクラス用）
    #[allow(dead_code)]
    pub fn nice(&self) -> Nice {
        self.nice
    }

    /// Realtime優先度を取得（Realtimeクラス用）
    pub fn rt_priority(&self) -> RtPriority {
        self.rt_priority
    }

    /// スケジューリングクラスを取得
    pub fn sched_class(&self) -> SchedulingClass {
        self.sched_class
    }

    /// タスクの重みを取得
    #[allow(dead_code)]
    pub fn weight(&self) -> u32 {
        self.weight
    }

    /// タスクの仮想実行時間を取得
    pub fn vruntime(&self) -> u64 {
        self.vruntime
    }

    /// タスクの仮想実行時間を更新
    ///
    /// # Arguments
    /// * `delta` - 実際の実行時間（ナノ秒単位）
    pub fn update_vruntime(&mut self, delta: u64) {
        // vruntime += delta * BASE_WEIGHT / weight
        // 優先度が高い（weightが大きい）ほど、vruntimeの増加が遅い
        const BASE_WEIGHT: u64 = 1024;

        if self.weight == 0 {
            return;
        }

        let increment = (delta * BASE_WEIGHT) / (self.weight as u64);
        self.vruntime = self.vruntime.saturating_add(increment);
    }

    /// タスクの状態を取得
    pub fn state(&self) -> TaskState {
        self.state
    }

    /// タスクの状態を設定
    pub fn set_state(&mut self, state: TaskState) {
        self.state = state;
    }

    /// コンテキストへの参照を取得
    pub fn context(&self) -> &Context {
        &self.context
    }

    /// コンテキストへの可変参照を取得
    pub fn context_mut(&mut self) -> &mut Context {
        &mut self.context
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_nice_to_weight_default() {
        // nice 0（デフォルト）は重み1024
        assert_eq!(nice_to_weight(0), 1024);
    }

    #[test_case]
    fn test_nice_to_weight_min() {
        // nice -20（最高優先度）は重み88761
        assert_eq!(nice_to_weight(-20), 88761);
    }

    #[test_case]
    fn test_nice_to_weight_max() {
        // nice +19（最低優先度）は重み15
        assert_eq!(nice_to_weight(19), 15);
    }

    #[test_case]
    fn test_nice_to_weight_clamping() {
        // 範囲外のnice値はクランプされる
        // nice -30 -> クランプされて -20 -> 重み88761
        assert_eq!(nice_to_weight(-30), nice_to_weight(-20));
        // nice +30 -> クランプされて +19 -> 重み15
        assert_eq!(nice_to_weight(30), nice_to_weight(19));
    }
}
