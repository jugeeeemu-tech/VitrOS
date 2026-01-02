//! タスク管理とマルチタスキング
//!
//! このモジュールはタスクの作成、管理、スケジューリングを担当します。

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::collections::BTreeSet;
use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

use crate::paging::KERNEL_VIRTUAL_BASE;

/// タスク操作のエラー型
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
                write!(f, "Invalid task priority (must be >= {})", priority::MIN)
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

    /// タスクIDの値を取得
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

/// タスク優先度の定数
pub mod priority {
    /// アイドルタスクの優先度（最低）
    pub const IDLE: u8 = 0;
    /// 通常タスクの最低優先度
    pub const MIN: u8 = 1;
    /// デフォルト優先度
    pub const DEFAULT: u8 = 10;
    /// 最高優先度
    pub const MAX: u8 = 255;
    /// リアルタイムクラスの下限（この値以上はリアルタイムクラス）
    pub const RT_MIN: u8 = 100;
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

/// 優先度からスケジューリングクラスを判定
///
/// # Arguments
/// * `priority` - タスクの優先度 (0-255)
///
/// # Returns
/// 対応するスケジューリングクラス
pub fn priority_to_class(priority: u8) -> SchedulingClass {
    if priority == priority::IDLE {
        SchedulingClass::Idle
    } else if priority >= priority::RT_MIN {
        SchedulingClass::Realtime
    } else {
        SchedulingClass::Normal
    }
}

/// f32の整数べき乗を計算（no_std環境用）
///
/// バイナリべき乗法を使用して効率的に計算します。
///
/// # Arguments
/// * `base` - 底
/// * `exp` - 指数（整数）
///
/// # Returns
/// base^exp
fn pow_f32(base: f32, exp: i32) -> f32 {
    if exp == 0 {
        return 1.0;
    }

    let abs_exp = exp.abs() as u32;
    let mut result = 1.0;
    let mut current_base = base;
    let mut current_exp = abs_exp;

    // バイナリべき乗法: O(log n)
    while current_exp > 0 {
        if current_exp & 1 == 1 {
            result *= current_base;
        }
        current_base *= current_base;
        current_exp >>= 1;
    }

    // 負の指数の場合は逆数を返す
    if exp < 0 { 1.0 / result } else { result }
}

/// 優先度から重みを計算
///
/// 優先度が高いタスクほど大きな重みを持ち、vruntimeの増加が遅くなります。
/// - 基準優先度（DEFAULT=10）の重みは1024
/// - 優先度が1上がるごとに約1.25倍の重みを持つ
///
/// # Arguments
/// * `priority` - タスクの優先度（0-255）
///
/// # Returns
/// スケジューリング用の重み
fn priority_to_weight(priority: u8) -> u32 {
    const PRIO_TO_WEIGHT: [u32; 40] = [
        88761, 71755, 56483, 46273, 36291, 29154, 23254, 18705, 14949, 11916, 9548, 7620, 6100,
        4904, 3906, 3121, 2501, 1991, 1586, 1277, 1024, 820, 655, 526, 423, 335, 272, 215, 172,
        137, 110, 87, 70, 56, 45, 36, 29, 23, 18, 15,
    ];

    // 0-255 の優先度を 0-39 のインデックスに変換
    // priority 0 (低) -> index 39 (重み15)
    // priority 255 (高) -> index 0 (重み88761)

    // 単純に範囲を圧縮する
    let index = 39 - (priority as usize * 40 / 256).min(39);

    PRIO_TO_WEIGHT[index]
}

/// タスクの状態
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

/// CPUコンテキスト（レジスタ状態）
///
/// Linux方式: すべてのレジスタとFPU/SSE状態をスタックに保存
/// Contextにはスタックポインタのみを保持
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Context {
    // スタックポインタのみ
    // レジスタはすべてスタックに保存される
    pub rsp: u64,
}

impl Context {
    /// 新しいコンテキストを作成（Linux方式）
    ///
    /// スタックに以下の順序でレジスタを配置（switch_context()のpush順序に合わせる）:
    /// 1. 戻りアドレス（entry_point） - 最上位
    /// 2. rbp, rbx, r12, r13, r14, r15（callee-savedレジスタ）
    /// 3. rflags
    /// 4. fxsave領域（512バイト、16バイトアライメント） - 最下位、rspがここを指す
    ///
    /// # Arguments
    /// * `entry_point` - タスクのエントリポイント
    /// * `stack_top` - スタックの最上位アドレス
    ///
    /// # Errors
    /// * `TaskError::InvalidStackAddress` - スタックアドレスが無効（null、アラインメント不正、範囲不正）
    /// * `TaskError::ContextInitFailed` - コンテキスト初期化に失敗
    pub fn new(entry_point: u64, stack_top: u64) -> Result<Self, TaskError> {
        const FXSAVE_SIZE: u64 = 512;
        const FXSAVE_ALIGN: u64 = 16;
        const MIN_REQUIRED_STACK: u64 = 1024; // 最小スタックサイズ

        // バリデーション: スタックトップがnullでないか
        if stack_top == 0 {
            return Err(TaskError::InvalidStackAddress);
        }

        // バリデーション: エントリポイントがnullでないか
        if entry_point == 0 {
            return Err(TaskError::ContextInitFailed);
        }

        // バリデーション: スタックが最低限の容量を持っているか
        if stack_top < MIN_REQUIRED_STACK {
            return Err(TaskError::InvalidStackAddress);
        }

        // スタックポインタの初期位置
        let mut rsp = stack_top;

        // 1. 戻りアドレス（entry_point）- switch_context()のret用
        rsp -= 8;
        if rsp == 0 {
            return Err(TaskError::InvalidStackAddress);
        }
        unsafe {
            *(rsp as *mut u64) = entry_point;
        }

        // 2. callee-savedレジスタ（switch_context()のpush順序に合わせる）
        // rbp（push rbpで積まれる）
        rsp -= 8;
        unsafe {
            *(rsp as *mut u64) = 0;
        }
        // rbx（push rbxで積まれる）
        rsp -= 8;
        unsafe {
            *(rsp as *mut u64) = 0;
        }
        // r12（push r12で積まれる）
        rsp -= 8;
        unsafe {
            *(rsp as *mut u64) = 0;
        }
        // r13（push r13で積まれる）
        rsp -= 8;
        unsafe {
            *(rsp as *mut u64) = 0;
        }
        // r14（push r14で積まれる）
        rsp -= 8;
        unsafe {
            *(rsp as *mut u64) = 0;
        }
        // r15（push r15で積まれる）
        rsp -= 8;
        unsafe {
            *(rsp as *mut u64) = 0;
        }

        // 3. rflags（pushfqで積まれる）- 割り込み有効
        rsp -= 8;
        unsafe {
            *(rsp as *mut u64) = 0x202; // IF (Interrupt Flag) を有効化
        }

        // 4. fxsave領域を確保（512バイト）
        // switch_contextと同じアラインメント処理を適用
        let rsp_before_fxsave = rsp; // アラインメント前のRSPを保存
        rsp -= FXSAVE_SIZE;
        rsp = (rsp / FXSAVE_ALIGN) * FXSAVE_ALIGN; // 16バイトアラインに切り下げ

        // バリデーション: 最終的なrspが有効かつ16バイトアラインされているか
        if rsp == 0 || rsp % FXSAVE_ALIGN != 0 {
            return Err(TaskError::InvalidStackAddress);
        }

        // fxsave領域をゼロクリア（初期状態）
        unsafe {
            core::ptr::write_bytes(rsp as *mut u8, 0, FXSAVE_SIZE as usize);
            // アラインメント前のRSPを保存（switch_contextと同じ位置）
            *((rsp + 504) as *mut u64) = rsp_before_fxsave;
        }

        Ok(Self { rsp })
    }

    /// 空のコンテキストを作成
    pub const fn empty() -> Self {
        Self { rsp: 0 }
    }
}

/// タスクスタック
const STACK_SIZE: usize = 16384; // 16KB

#[repr(align(16))]
struct TaskStack([u8; STACK_SIZE]);

impl TaskStack {
    const fn new() -> Self {
        Self([0; STACK_SIZE])
    }

    /// スタックの最上位アドレスを取得（仮想アドレス）
    fn top(&self) -> u64 {
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
    /// タスク優先度（0-255、数値が大きいほど優先度が高い）
    priority: u8,
    /// スケジューリングクラス（Realtime, Normal, Idle）
    sched_class: SchedulingClass,
    /// スケジューリング用の重み（優先度から計算、Normalクラスで使用）
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
    stack: Box<TaskStack>,
}

impl Task {
    /// 新しいタスクを作成
    ///
    /// # Arguments
    /// * `name` - タスク名
    /// * `priority` - タスク優先度（priority::MIN以上、priority::MAX以下）
    /// * `entry_point` - エントリポイント関数のアドレス
    ///
    /// # Errors
    /// * `TaskError::InvalidPriority` - 優先度がpriority::MINより小さい場合
    /// * `TaskError::StackAllocationFailed` - スタック割り当てに失敗した場合
    /// * `TaskError::ContextInitFailed` - コンテキスト初期化に失敗した場合
    pub fn new(
        name: &'static str,
        priority: u8,
        entry_point: extern "C" fn() -> !,
    ) -> Result<Self, TaskError> {
        // 優先度の検証：アイドルタスク以下の優先度は許可しない
        if priority < priority::MIN {
            return Err(TaskError::InvalidPriority);
        }

        // スタックをヒープに割り当て
        let stack = Box::new(TaskStack::new());
        let stack_top = stack.top();

        let context = Context::new(entry_point as u64, stack_top)?;

        // 優先度から重みとスケジューリングクラスを計算
        let weight = priority_to_weight(priority);
        let sched_class = priority_to_class(priority);

        Ok(Self {
            id: TaskId::new(),
            name,
            priority,
            sched_class,
            weight,
            vruntime: 0, // 初期値は0
            context,
            state: TaskState::Ready,
            stack,
        })
    }

    /// アイドルタスク専用の作成関数（優先度チェックをスキップ）
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

        // アイドルタスクの重みを計算
        let weight = priority_to_weight(priority::IDLE);

        Ok(Self {
            id: TaskId::new(),
            name,
            priority: priority::IDLE,
            sched_class: SchedulingClass::Idle, // アイドルタスクは常にIdleクラス
            weight,
            vruntime: 0, // 初期値は0
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

    /// タスク優先度を取得
    pub fn priority(&self) -> u8 {
        self.priority
    }

    /// スケジューリングクラスを取得
    pub fn sched_class(&self) -> SchedulingClass {
        self.sched_class
    }

    /// タスクの重みを取得
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

/// スケジューリングが必要かどうかを示すフラグ
/// 割り込みハンドラがこのフラグをセットし、割り込み復帰時にチェックされる
static NEED_RESCHED: AtomicBool = AtomicBool::new(false);

/// 現在のタスクの蓄積実行時間（ナノ秒）
/// タイマー割り込みで加算され、schedule()でvruntimeに反映される
/// これにより、ロックを取得せずに実行時間を記録できる
static ACCUMULATED_RUNTIME: AtomicU64 = AtomicU64::new(0);

/// 初回起動時に使用するダミーコンテキスト
/// 現在のタスクが存在しない場合、このコンテキストに「保存」する（実際には捨てられる）
static mut DUMMY_CONTEXT: Context = Context { rsp: 0 };

// グローバルタスクキュー（マルチレベル）
lazy_static! {
    /// リアルタイムキュー (Realtimeクラスのタスク)
    /// キー: (255 - priority, task_id) - 優先度が高い順にソート
    /// 値: タスク
    static ref RT_QUEUE: Mutex<BTreeMap<(u8, u64), Box<Task>>> = Mutex::new(BTreeMap::new());

    /// 通常キュー (Normalクラスのタスク、CFS方式)
    /// キー: (vruntime, task_id) - vruntimeでソートされ、同じvruntimeの場合はtask_idで区別
    /// 値: タスク
    static ref CFS_QUEUE: Mutex<BTreeMap<(u64, u64), Box<Task>>> = Mutex::new(BTreeMap::new());

    /// アイドルキュー (Idleクラスのタスク)
    /// FIFO順で管理
    static ref IDLE_QUEUE: Mutex<VecDeque<Box<Task>>> = Mutex::new(VecDeque::new());

    /// 現在実行中のタスク
    static ref CURRENT_TASK: Mutex<Option<Box<Task>>> = Mutex::new(None);

    /// ブロック中のタスク (TaskId -> Task)
    /// ブロッキング同期プリミティブで待機中のタスクを管理
    static ref BLOCKED_TASKS: Mutex<BTreeMap<u64, Box<Task>>> = Mutex::new(BTreeMap::new());

    /// 起床保留中のタスクID集合
    ///
    /// Lost Wakeup問題を防ぐために使用。
    /// unblock_task()が呼ばれた時にタスクがまだBLOCKED_TASKSにいない場合、
    /// このセットにIDを追加し、block_current_task()でチェックする。
    static ref WAKEUP_PENDING: Mutex<BTreeSet<u64>> = Mutex::new(BTreeSet::new());
}

/// タスク管理システムの初期化
pub fn init() {
    crate::info!("Task system initialized");
}

/// タスクを適切なキューに追加（内部ヘルパー関数）
///
/// スケジューリングクラスに応じて、RT_QUEUE、CFS_QUEUE、IDLE_QUEUEのいずれかに追加します。
///
/// # Arguments
/// * `task` - 追加するタスク
/// * `rt_queue` - リアルタイムキューのロック済みガード
/// * `cfs_queue` - CFSキューのロック済みガード
/// * `idle_queue` - アイドルキューのロック済みガード
fn enqueue_task(
    task: Box<Task>,
    rt_queue: &mut BTreeMap<(u8, u64), Box<Task>>,
    cfs_queue: &mut BTreeMap<(u64, u64), Box<Task>>,
    idle_queue: &mut VecDeque<Box<Task>>,
) {
    match task.sched_class() {
        SchedulingClass::Realtime => {
            // リアルタイムキュー: (255 - priority, task_id) で優先度高い順にソート
            let key = (255 - task.priority(), task.id().as_u64());
            rt_queue.insert(key, task);
        }
        SchedulingClass::Normal => {
            // CFSキュー: (vruntime, task_id) でソート
            let key = (task.vruntime(), task.id().as_u64());
            cfs_queue.insert(key, task);
        }
        SchedulingClass::Idle => {
            // アイドルキュー: FIFO
            idle_queue.push_back(task);
        }
    }
}

/// 新しいタスクをタスクキューに追加（エラーハンドリング版）
///
/// # Arguments
/// * `task` - 追加するタスク
///
/// # Errors
/// * `TaskError::QueueFull` - タスクキューが満杯の場合（現在は常に成功）
///
/// # Note
/// 割り込みを無効化してからロックを取得し、デッドロックを防ぎます。
/// スケジューリングクラスに応じて、適切なキュー（RT/CFS/IDLE）に追加します。
pub fn try_add_task(task: Task) -> Result<(), TaskError> {
    let task_id = task.id().as_u64();
    let sched_class = task.sched_class();
    // 名前を所有型として取得（借用を終わらせるため）
    let name = alloc::format!("{}", task.name());

    without_interrupts(|| {
        let boxed_task = Box::new(task);

        // スケジューリングクラスに応じて適切なキューに追加
        match sched_class {
            SchedulingClass::Realtime => {
                let mut rt = RT_QUEUE.lock();
                let key = (255 - boxed_task.priority(), task_id);
                rt.insert(key, boxed_task);
            }
            SchedulingClass::Normal => {
                let mut cfs = CFS_QUEUE.lock();
                let key = (boxed_task.vruntime(), task_id);
                cfs.insert(key, boxed_task);
            }
            SchedulingClass::Idle => {
                let mut idle = IDLE_QUEUE.lock();
                idle.push_back(boxed_task);
            }
        }
    });

    crate::info!(
        "Task added to queue: ID={}, name={}, class={:?}",
        task_id,
        name,
        sched_class
    );
    Ok(())
}

/// 新しいタスクをタスクキューに追加（後方互換性のため残す）
///
/// # Arguments
/// * `task` - 追加するタスク
///
/// # Panics
/// タスク追加に失敗した場合（現在は発生しない）
pub fn add_task(task: Task) {
    try_add_task(task).expect("Failed to add task to queue");
}

/// 現在のタスクが自発的にCPUを手放す
///
/// 現在のタスクを準備完了状態にして、次のタスクに切り替えます。
/// タスク内から呼び出すことで、協調的マルチタスキングを実現します。
pub fn yield_now() {
    schedule();
}

/// 現在実行中のタスクを設定
///
/// カーネル初期化時に、kernel_main_innerをタスクとして登録するために使用します。
///
/// # Arguments
/// * `task` - 現在のタスクとして設定するタスク
///
/// # Note
/// 割り込みを無効化してからロックを取得し、デッドロックを防ぎます。
pub fn set_current_task(task: Task) {
    without_interrupts(|| {
        let mut current = CURRENT_TASK.lock();
        *current = Some(Box::new(task));
    });
}

/// 現在実行中のタスクの実行時間を蓄積
///
/// タイマー割り込みハンドラから呼び出されます。
/// 実際のvruntime更新はschedule()内で行われます。
///
/// # Arguments
/// * `delta` - 実際の実行時間（ナノ秒単位）
///
/// # Design
/// ロックを取得せずにアトミック変数に蓄積することで、
/// デッドロックを回避しつつ、確実に実行時間を記録します。
/// schedule()が呼ばれた時に、蓄積された時間がvruntimeに反映されます。
pub fn update_current_task_vruntime(delta: u64) {
    ACCUMULATED_RUNTIME.fetch_add(delta, Ordering::Relaxed);
}

/// スケジューリングが必要であることを示すフラグをセット
///
/// タイマー割り込みハンドラから呼び出されます。
/// 実際のスケジューリングは割り込み復帰時に行われます。
pub fn set_need_resched() {
    NEED_RESCHED.store(true, Ordering::Release);
}

/// 割り込み復帰時にスケジューリングとsoftirq処理をチェック
///
/// 1. need_reschedフラグがセットされていれば、スケジューラを呼び出します。
/// 2. softirqフラグがセットされていれば、タイマーコールバックを処理します。
///
/// この関数は割り込みハンドラの復帰処理から呼び出されます。
///
/// # Design (Linux風 softirq)
/// - schedule()は割り込み無効の状態で実行されます
/// - schedule()後、switch_context()でIFフラグが強制セットされ割り込みが有効になります
/// - softirq処理は割り込み有効状態で実行され、コールバック内でブロック可能です
/// - 処理中に新たな割り込みが発生しても、do_softirq()の再入は防止されます
pub fn check_resched_on_interrupt_exit() {
    // 1. スケジューリングチェック
    if NEED_RESCHED.swap(false, Ordering::Acquire) {
        // 割り込みは無効のままschedule()を呼び出す
        // これにより、schedule()実行中に再度タイマー割り込みが入ることを防ぐ
        schedule();
    }

    // 2. softirq処理（タイマーコールバック実行）
    // - schedule()が呼ばれた場合: switch_context()でIF強制セット済み
    // - schedule()が呼ばれなかった場合: まだ割り込み無効
    // どちらの場合も sti で有効化してから処理（既に有効でも無害）
    if crate::timer::softirq_pending() {
        unsafe {
            core::arch::asm!("sti", options(nomem, nostack));
        }
        crate::timer::do_softirq();
        // iretqで元のRFLAGSが復元されるのでcliは不要
    }
}

/// 現在のタスクIDを取得
///
/// # Returns
/// 現在実行中のタスクのID。タスクが存在しない場合は新しいIDを生成
///
/// # Note
/// 割り込みを無効化してからロックを取得し、デッドロックを防ぎます。
pub fn current_task_id() -> TaskId {
    without_interrupts(|| {
        let current = CURRENT_TASK.lock();
        current.as_ref().map(|t| t.id()).unwrap_or_else(TaskId::new)
    })
}

/// 現在のタスクをブロック状態にしてスケジュール
///
/// この関数は同期プリミティブ（BlockingMutex等）から呼び出されます。
/// 現在のタスクをBlocked状態に設定し、BLOCKED_TASKSに移動してスケジューラを呼び出します。
///
/// # Lost Wakeup防止
/// ブロック前にWAKEUP_PENDINGをチェックし、既に起床シグナルが
/// 発行されていればブロックをスキップします。これにより、
/// WaitQueue::wait()とwake_one()の間の競合状態を防ぎます。
///
/// # アトミック性保証
/// WAKEUP_PENDINGのチェックとBlocked状態設定を同一のクリティカルセクション内で
/// 実行し、その間に起床シグナルが失われることを防ぎます。
/// ロック順序: WAKEUP_PENDING → CURRENT_TASK（デッドロック防止）
///
/// # Note
/// 割り込みを無効化してからロックを取得し、デッドロックを防ぎます。
pub fn block_current_task() {
    // Lost Wakeup防止: WAKEUP_PENDINGチェックとBlocked設定をアトミックに実行
    let should_block = without_interrupts(|| {
        // ロック順序: WAKEUP_PENDING → CURRENT_TASK
        // この順序を維持することでデッドロックを防ぐ
        let mut wakeup_pending = WAKEUP_PENDING.lock();
        let mut current = CURRENT_TASK.lock();

        if let Some(task) = current.as_mut() {
            let id = task.id().as_u64();

            // WAKEUP_PENDINGをチェック（両方のロックを保持したまま）
            if wakeup_pending.remove(&id) {
                // 既に起床シグナルが発行されている（Lost Wakeup検出）
                // ブロックせずに即座にリターン
                return false;
            }

            // WAKEUP_PENDINGにないので、通常通りBlocked状態に設定
            // （まだ両方のロックを保持している）
            task.set_state(TaskState::Blocked);
            true
        } else {
            false
        }
    });

    if should_block {
        // schedule()は内部で割り込みを無効化する
        schedule();
    }
}

/// 指定タスクをアンブロック（Ready状態に戻す）
///
/// BLOCKED_TASKSから取り出して、スケジューリングクラスに応じたキューに追加します。
/// タスクがまだBLOCKED_TASKSに登録されていない場合（Lost Wakeup防止）、
/// WAKEUP_PENDINGセットに追加し、block_current_task()で検出できるようにします。
///
/// # Arguments
/// * `task_id` - アンブロックするタスクのID
///
/// # Note
/// 割り込みを無効化してからロックを取得し、デッドロックを防ぎます。
pub fn unblock_task(task_id: TaskId) {
    without_interrupts(|| {
        let mut blocked_tasks = BLOCKED_TASKS.lock();

        if let Some(mut task) = blocked_tasks.remove(&task_id.as_u64()) {
            // Ready状態に戻す
            task.set_state(TaskState::Ready);
            let sched_class = task.sched_class();
            drop(blocked_tasks); // ロックを早期に解放

            // スケジューリングクラスに応じて適切なキューに追加
            match sched_class {
                SchedulingClass::Realtime => {
                    let mut rt = RT_QUEUE.lock();
                    let key = (255 - task.priority(), task.id().as_u64());
                    rt.insert(key, task);
                }
                SchedulingClass::Normal => {
                    let mut cfs = CFS_QUEUE.lock();
                    let key = (task.vruntime(), task.id().as_u64());
                    cfs.insert(key, task);
                }
                SchedulingClass::Idle => {
                    let mut idle = IDLE_QUEUE.lock();
                    idle.push_back(task);
                }
            }
        } else {
            // タスクがBLOCKED_TASKSにない場合、まだblock_current_task()が
            // 完了していない可能性がある（Lost Wakeup問題）。
            // WAKEUP_PENDINGに追加して、block_current_task()で検出できるようにする。
            drop(blocked_tasks);
            let mut wakeup_pending = WAKEUP_PENDING.lock();
            wakeup_pending.insert(task_id.as_u64());
        }
    });
}

/// 割り込みコンテキスト内かどうかを判定
///
/// RFLAGSのIFフラグ（bit 9）をチェックします。
/// IF=0（割り込み無効）の場合、割り込みコンテキストの可能性が高いと判断します。
///
/// # Returns
/// 割り込みコンテキスト内ならtrue、通常コンテキストならfalse
pub fn is_interrupt_context() -> bool {
    let rflags: u64;
    unsafe {
        core::arch::asm!("pushfq; pop {}", out(reg) rflags, options(nomem, nostack));
    }
    // IF=0 なら割り込み無効＝割り込みコンテキストの可能性
    // より正確には、割り込み無効化されている＝ブロックすべきでない
    (rflags & 0x200) == 0
}

/// 割り込みを無効化してクロージャを実行
///
/// クロージャ実行後、元の割り込み状態を復元します。
/// 割り込みハンドラからアクセスされる可能性のあるロックを取得する際に使用します。
///
/// # Arguments
/// * `f` - 割り込み無効状態で実行するクロージャ
///
/// # Returns
/// クロージャの戻り値
fn without_interrupts<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    let rflags: u64;
    unsafe {
        // RFLAGSを保存して割り込みを無効化
        core::arch::asm!("pushfq; pop {}; cli", out(reg) rflags, options(nomem, nostack));
    }

    let result = f();

    // 元々割り込みが有効だった場合のみ再有効化
    if rflags & 0x200 != 0 {
        unsafe {
            core::arch::asm!("sti", options(nomem, nostack));
        }
    }

    result
}

/// 次に実行するタスクを選択してコンテキストスイッチ
///
/// マルチレベルキュースケジューリングを行います。
/// - 優先順位: Realtime > Normal (CFS) > Idle
/// - 上位クラスのキューが空になるまで、下位クラスのタスクは実行されません
/// - Realtimeクラス内では優先度順、Normalクラス内ではvruntime順
///
/// RFLAGSの保存・復元はswitch_context()内部で自動的に行われます。
/// switch_context()でRFLAGSのIFフラグが強制セットされるため、
/// タスク復帰時は必ず割り込み有効状態になります。
///
/// # Note
/// この関数は割り込みを無効化してからロックを取得します。
/// これにより、タイマー割り込みハンドラとのデッドロックを防ぎます。
pub fn schedule() {
    // 割り込みを無効化
    // これにより、ロック保持中にタイマー割り込みが発生してデッドロックすることを防ぐ
    unsafe {
        core::arch::asm!("cli", options(nomem, nostack));
    }

    // 3つのキューをロック（固定順序でデッドロック防止）
    let mut rt_queue = RT_QUEUE.lock();
    let mut cfs_queue = CFS_QUEUE.lock();
    let mut idle_queue = IDLE_QUEUE.lock();
    let mut current = CURRENT_TASK.lock();

    // 次に実行するタスクを選択（優先順位: Realtime > Normal > Idle）
    let next_task = if let Some(entry) = rt_queue.pop_first() {
        // 1. リアルタイムキューから取得（優先度が最も高いタスク）
        Some(entry.1)
    } else if let Some(entry) = cfs_queue.pop_first() {
        // 2. CFSキューから取得（vruntimeが最も小さいタスク）
        Some(entry.1)
    } else if let Some(task) = idle_queue.pop_front() {
        // 3. アイドルキューから取得（FIFO）
        Some(task)
    } else {
        // 実行可能なタスクがない
        None
    };

    // タスクがない場合は何もしない
    let Some(mut next_task) = next_task else {
        drop(rt_queue);
        drop(cfs_queue);
        drop(idle_queue);
        drop(current);
        // 割り込みを再有効化
        unsafe {
            core::arch::asm!("sti", options(nomem, nostack));
        }
        return;
    };

    next_task.set_state(TaskState::Running);

    let new_context_ptr = next_task.context() as *const Context;

    // 古いタスクのコンテキストを保存する準備
    let old_context_ptr = if let Some(mut old_task) = current.take() {
        // 蓄積された実行時間でvruntimeを更新（Normalクラスのみ有効）
        let accumulated = ACCUMULATED_RUNTIME.swap(0, Ordering::Relaxed);
        if accumulated > 0 && old_task.sched_class() == SchedulingClass::Normal {
            old_task.update_vruntime(accumulated);
        }

        // 実行中だった場合は準備完了状態に変更
        if old_task.state() == TaskState::Running {
            old_task.set_state(TaskState::Ready);
        }

        // 古いタスクのコンテキストへのポインタを取得
        // （Box内のTaskは移動しても同じアドレスに留まる）
        let old_ctx_ptr = old_task.context_mut() as *mut Context;

        // タスクの状態に応じて適切な場所に戻す
        match old_task.state() {
            TaskState::Terminated => {
                // 終了したタスクは破棄
            }
            TaskState::Blocked => {
                // ブロック中のタスクはBLOCKED_TASKSに移動
                let task_id = old_task.id().as_u64();
                // キューのロックを一時解放
                drop(rt_queue);
                drop(cfs_queue);
                drop(idle_queue);
                let mut blocked = BLOCKED_TASKS.lock();
                blocked.insert(task_id, old_task);
                drop(blocked);
                // 再度ロック取得
                rt_queue = RT_QUEUE.lock();
                cfs_queue = CFS_QUEUE.lock();
                idle_queue = IDLE_QUEUE.lock();
            }
            _ => {
                // Ready状態のタスクはクラスに応じたキューに戻す
                enqueue_task(old_task, &mut rt_queue, &mut cfs_queue, &mut idle_queue);
            }
        }

        old_ctx_ptr
    } else {
        // 現在のタスクがない場合（初回起動時）はstaticなダミーコンテキストを使用
        &raw mut DUMMY_CONTEXT as *mut Context
    };

    // 新しいタスクを現在のタスクに設定
    *current = Some(next_task);

    // ロックを解放してからコンテキストスイッチ
    // （コンテキストスイッチ中にロックを保持していると、戻ってきた時に問題が起きる）
    drop(rt_queue);
    drop(cfs_queue);
    drop(idle_queue);
    drop(current);

    // コンテキストスイッチを実行
    // old_context_ptrに現在の状態を保存し、new_context_ptrの状態を復元
    // RFLAGSの保存・復元もswitch_context()内部で自動的に処理される
    unsafe {
        switch_context(old_context_ptr, new_context_ptr);
    }

    // ここに戻ってくるのは、このタスクが再度スケジュールされた時
}

/// コンテキストスイッチを実行（Linux方式）
///
/// すべてのレジスタとFPU/SSE状態をスタックに保存/復元します。
///
/// # Safety
/// この関数は低レベルのアセンブリ操作を行うため、正しいコンテキスト構造体へのポインタを渡す必要があります。
///
/// # Arguments
/// * `old_context` - 現在のコンテキストを保存する先（rspのみ）
/// * `new_context` - 切り替え先のコンテキスト（rspのみ）
///
/// # Note
/// 保存されるRFLAGSは割り込み有効フラグ(IF)が強制的にセットされます。
/// これにより、タスク復帰時に必ず割り込み有効状態になることが保証されます。
#[unsafe(naked)]
pub unsafe extern "C" fn switch_context(old_context: *mut Context, new_context: *const Context) {
    core::arch::naked_asm!(
        // ========== 現在のコンテキストを保存 ==========
        // callee-savedレジスタをスタックに保存
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        // RFLAGSを保存し、IFフラグを強制的にセット
        // これにより、タスク復帰時に必ず割り込みが有効になる
        "pushfq",
        "or qword ptr [rsp], 0x200", // IF (bit 9) を強制的に1にする
        // fxsave用の領域を確保し、16バイトアラインを保証
        // call命令で8バイトプッシュされているため、アラインメント調整が必要
        "mov r11, rsp", // アラインメント前のRSPを保存
        "sub rsp, 512",
        "and rsp, -16", // 16バイトアラインに切り下げ
        // FPU/SSE状態を保存
        "fxsave [rsp]",
        // アラインメント前のRSPをスタックに保存（復元時に必要）
        "mov [rsp + 504], r11", // fxsave領域の末尾近くに保存
        // 現在のrspをold_contextに保存
        "mov [rdi], rsp",
        // ========== 新しいコンテキストを復元 ==========
        // new_context->rspを読み込み
        "mov rsp, [rsi]",
        // FPU/SSE状態を復元
        "fxrstor [rsp]",
        // アラインメント前のRSPを復元
        "mov rsp, [rsp + 504]",
        // RFLAGSを復元（IFは保存時に強制セット済みなので、割り込み有効で復帰）
        "popfq",
        // callee-savedレジスタを復元（保存と逆順）
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        // リターン（スタックトップの戻りアドレスに戻る）
        "ret",
    )
}

/// 指定したミリ秒数だけ現在のタスクをスリープさせる
///
/// Linux の `schedule_timeout()` に倣った実装です。
/// タイマーを登録し、タスクをブロック状態にしてスケジューラに譲ります。
/// タイマー期限切れ時にコールバックでタスクを起床させます。
///
/// # Arguments
/// * `ms` - スリープ時間（ミリ秒）
///
/// # Note
/// - タイマー周波数が100Hz（10ms周期）のため、10ms未満の精度は保証されない
/// - 0msの場合は yield_now() と同等の動作（他タスクに実行機会を与える）
/// - 割り込みコンテキストからは呼び出し不可
///
/// # Panics
/// 割り込みコンテキストから呼び出された場合（デバッグビルドのみ）
pub fn sleep_ms(ms: u64) {
    // 安全性チェック: 割り込みコンテキストではブロック不可
    debug_assert!(
        !is_interrupt_context(),
        "sleep_ms() cannot be called from interrupt context"
    );

    // 0ms の場合は yield して即座にリターン
    if ms == 0 {
        yield_now();
        return;
    }

    // 現在のタスクIDを取得（TaskId は Copy なのでクロージャにキャプチャ可能）
    let task_id = current_task_id();

    // ミリ秒をtick数に変換（最小1tickを保証）
    let ticks = crate::timer::ms_to_ticks(ms).max(1);

    // タイマーを登録: 期限切れ時に unblock_task を呼び出す
    crate::timer::register_timer(
        ticks,
        Box::new(move || {
            unblock_task(task_id);
        }),
    );

    // タスクをブロック状態にしてスケジュール
    // タイマーが起床するまで他のタスクが実行される
    block_current_task();
}
