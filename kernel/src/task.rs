//! タスク管理とマルチタスキング
//!
//! このモジュールはタスクの作成、管理、スケジューリングを担当します。

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
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

        // 4. fxsave領域を確保（512バイト、16バイトアライメント）
        rsp -= FXSAVE_SIZE;
        rsp = (rsp / FXSAVE_ALIGN) * FXSAVE_ALIGN; // 16バイトアライメント

        // バリデーション: 最終的なrspが有効か
        if rsp == 0 || rsp % FXSAVE_ALIGN != 0 {
            return Err(TaskError::InvalidStackAddress);
        }

        // fxsave領域をゼロクリア（初期状態）
        unsafe {
            core::ptr::write_bytes(rsp as *mut u8, 0, FXSAVE_SIZE as usize);
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
    /// スケジューリング用の重み（優先度から計算）
    /// 値が大きいほど、vruntimeの増加が遅くなり、より頻繁に実行される
    weight: u32,
    /// 仮想実行時間（CFS風スケジューリング）
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

        // 優先度から重みを計算
        let weight = priority_to_weight(priority);

        Ok(Self {
            id: TaskId::new(),
            name,
            priority,
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

/// 初回起動時に使用するダミーコンテキスト
/// 現在のタスクが存在しない場合、このコンテキストに「保存」する（実際には捨てられる）
static mut DUMMY_CONTEXT: Context = Context { rsp: 0 };

// グローバルタスクキュー
lazy_static! {
    /// 実行可能なタスクのツリー (Ready状態のタスク)
    /// キー: (vruntime, task_id) - vruntimeでソートされ、同じvruntimeの場合はtask_idで区別
    /// 値: タスク
    static ref TASK_QUEUE: Mutex<BTreeMap<(u64, u64), Box<Task>>> = Mutex::new(BTreeMap::new());

    /// 現在実行中のタスク
    static ref CURRENT_TASK: Mutex<Option<Box<Task>>> = Mutex::new(None);
}

/// タスク管理システムの初期化
pub fn init() {
    vitros_common::info!("Task system initialized");
}

/// 新しいタスクをタスクキューに追加（エラーハンドリング版）
///
/// # Arguments
/// * `task` - 追加するタスク
///
/// # Errors
/// * `TaskError::QueueFull` - タスクキューが満杯の場合（現在は常に成功）
pub fn try_add_task(task: Task) -> Result<(), TaskError> {
    let mut tree = TASK_QUEUE.lock();

    let task_id = task.id().as_u64();
    let vruntime = task.vruntime();
    // 名前を所有型として取得（借用を終わらせるため）
    let name = alloc::format!("{}", task.name());

    // BTreeMapに追加: キーは(vruntime, task_id)
    let boxed_task = Box::new(task);
    tree.insert((vruntime, task_id), boxed_task);

    vitros_common::info!("Task added to queue: ID={}, name={}", task_id, name);
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
pub fn set_current_task(task: Task) {
    let mut current = CURRENT_TASK.lock();
    *current = Some(Box::new(task));
}

/// 現在実行中のタスクのvruntimeを更新
///
/// タイマー割り込みハンドラから呼び出されます。
///
/// # Arguments
/// * `delta` - 実際の実行時間（ナノ秒単位）
pub fn update_current_task_vruntime(delta: u64) {
    let mut current = CURRENT_TASK.lock();
    if let Some(task) = current.as_mut() {
        task.update_vruntime(delta);
    }
}

/// スケジューリングが必要であることを示すフラグをセット
///
/// タイマー割り込みハンドラから呼び出されます。
/// 実際のスケジューリングは割り込み復帰時に行われます。
pub fn set_need_resched() {
    NEED_RESCHED.store(true, Ordering::Release);
}

/// 割り込み復帰時にスケジューリングをチェック
///
/// need_reschedフラグがセットされていれば、スケジューラを呼び出します。
/// この関数は割り込みハンドラの復帰処理から呼び出されます。
///
/// 注意: schedule()は割り込み無効の状態で実行されます。
/// 割り込みはコンテキストスイッチ後に再有効化されます。
pub fn check_resched_on_interrupt_exit() {
    if NEED_RESCHED.swap(false, Ordering::Acquire) {
        // 割り込みは無効のままschedule()を呼び出す
        // これにより、schedule()実行中に再度タイマー割り込みが入ることを防ぐ
        schedule();
    }
}

/// 次に実行するタスクを選択してコンテキストスイッチ
///
/// CFS風のスケジューリングを行います。
/// - vruntimeが最も小さいタスクを選択（BTreeMapの最左端）
/// - 同じvruntimeの場合は、task_idで区別（自動的にソート済み）
/// - 現在のタスクがある場合は、それをツリーに戻します
///
/// RFLAGSの保存・復元はswitch_context()内部で自動的に行われます。
pub fn schedule() {
    let mut tree = TASK_QUEUE.lock();
    let mut current = CURRENT_TASK.lock();

    // ツリーが空の場合は何もしない
    if tree.is_empty() {
        drop(tree);
        drop(current);
        return;
    }

    // vruntimeが最も小さいタスクを取得（O(log n)）
    // BTreeMapのfirst_entry()は最小のキーを持つエントリを返す
    let mut next_task = tree.pop_first().unwrap().1;

    next_task.set_state(TaskState::Running);

    let new_context_ptr = next_task.context() as *const Context;

    // 古いタスクのコンテキストを保存する準備
    let old_context_ptr = if let Some(mut old_task) = current.take() {
        // 実行中だった場合は準備完了状態に変更
        if old_task.state() == TaskState::Running {
            old_task.set_state(TaskState::Ready);
        }

        // 古いタスクのコンテキストへのポインタを取得
        // （Box内のTaskは移動しても同じアドレスに留まる）
        let old_ctx_ptr = old_task.context_mut() as *mut Context;

        // 終了していないタスクのみツリーに戻す
        // vruntimeが更新されているので、新しいキーで挿入
        if old_task.state() != TaskState::Terminated {
            let key = (old_task.vruntime(), old_task.id().as_u64());
            tree.insert(key, old_task);
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
    drop(tree);
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
        // RFLAGSを保存
        "pushfq",
        // fxsave用の領域を確保（512バイト、16バイトアライメント）
        "sub rsp, 512",
        "and rsp, ~0xF", // 16バイトアライメント
        // FPU/SSE状態を保存
        "fxsave [rsp]",
        // 現在のrspをold_contextに保存
        // old_context->rsp = rsp
        "mov [rdi], rsp", // offset 0: rsp
        // ========== 新しいコンテキストを復元 ==========
        // new_context->rspを読み込み
        "mov rsp, [rsi]", // offset 0: rsp
        // FPU/SSE状態を復元
        "fxrstor [rsp]",
        // fxsave領域をスキップ
        "add rsp, 512",
        // RFLAGSを復元
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
