//! スケジューラとタスクキュー管理
//!
//! このモジュールはマルチレベルキュースケジューリングとタスク管理を担当します。

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

use crate::io::without_interrupts;

use super::blocking::BLOCKED_TASKS;
use super::context::{Context, switch_context};
use super::task::{SchedulingClass, Task, TaskError, TaskId, TaskState, rt_priority};

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
    pub(super) static ref CURRENT_TASK: Mutex<Option<Box<Task>>> = Mutex::new(None);
}

/// タスク管理システムの初期化
pub fn init() {
    crate::info!("Task system initialized");
}

/// タスクを適切なキューに追加（単一キューロック版）
///
/// schedule()の最適化用。必要なキューのみをロックしてエンキューします。
/// これにより、ロック保持時間を最小化します。
///
/// # Safety Contract
/// この関数は割り込み無効状態（cli実行後）でのみ呼び出すこと。
/// schedule()の内部ヘルパーとして設計されており、割り込み有効状態で
/// 呼び出すとデッドロックの可能性があります。
#[inline]
fn enqueue_task_single(task: Box<Task>) {
    match task.sched_class() {
        SchedulingClass::Realtime => {
            let key = (rt_priority::MAX - task.rt_priority(), task.id().as_u64());
            let mut rt_queue = RT_QUEUE.lock();
            rt_queue.insert(key, task);
        }
        SchedulingClass::Normal => {
            let key = (task.vruntime(), task.id().as_u64());
            let mut cfs_queue = CFS_QUEUE.lock();
            cfs_queue.insert(key, task);
        }
        SchedulingClass::Idle => {
            let mut idle_queue = IDLE_QUEUE.lock();
            idle_queue.push_back(task);
        }
    }
}

/// タスクを適切なキューに追加（blocking.rsから呼び出される）
pub(super) fn enqueue_to_appropriate_queue(task: Box<Task>, sched_class: SchedulingClass) {
    match sched_class {
        SchedulingClass::Realtime => {
            let mut rt = RT_QUEUE.lock();
            let key = (rt_priority::MAX - task.rt_priority(), task.id().as_u64());
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
                let key = (rt_priority::MAX - boxed_task.rt_priority(), task_id);
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

/// 割り込み復帰時にsoftirq処理とスケジューリングをチェック
///
/// 1. softirqフラグがセットされていれば、タイマーコールバックを処理します。
/// 2. need_reschedフラグがセットされていれば、スケジューラを呼び出します。
///
/// この関数は割り込みハンドラの復帰処理から呼び出されます。
///
/// # Design (Linux風 softirq)
/// - softirq処理を先に行うことで、sleep_msで待機中のタスクが即座にunblockされます
/// - unblockされたタスクはschedule()で即座にスケジューリング対象になります
/// - これにより、sleep_msの遅延が最小化されます（+1 tick遅延を回避）
/// - softirq処理は割り込み有効状態で実行され、コールバック内でブロック可能です
/// - schedule()は割り込み無効の状態で実行されます
/// - 処理中に新たな割り込みが発生しても、do_softirq()の再入は防止されます
///
/// # Note: Heavy Callback Latency
/// do_softirq()でコールバックが逐次実行されるため、重いコールバックがあると
/// schedule()の呼び出しが遅延し、sleep_msの精度が悪化する可能性がある。
/// 詳細は timer.rs の process_pending_timers() のTODOコメントを参照。
pub fn check_resched_on_interrupt_exit() {
    // 1. softirq処理（タイマーコールバック実行）
    // schedule()の前に実行することで、unblockされたタスクが即座にスケジューリング対象になる
    if crate::timer::softirq_pending() {
        // SAFETY: STI命令は割り込みフラグを有効化するのみで安全。
        unsafe {
            core::arch::asm!("sti", options(nomem, nostack));
        }
        crate::timer::do_softirq();
        // schedule()は割り込み無効状態で実行するためcliで無効化
        // SAFETY: CLI命令は割り込みフラグを無効化するのみで安全。
        unsafe {
            core::arch::asm!("cli", options(nomem, nostack));
        }
    }

    // 2. スケジューリングチェック
    // softirq処理中（IN_SOFTIRQ == true）の場合はスケジューリングをスキップ
    // これにより、do_softirq()が完了するまでコンテキストスイッチを防ぐ
    // ネストした割り込みでschedule()が呼ばれると、do_softirq()が完了せず
    // IN_SOFTIRQが永続的にtrueのまま残り、全softirq処理がスキップされる問題を防ぐ
    if crate::timer::in_softirq() {
        return;
    }

    // softirq処理でunblockされたタスクも含めてスケジューリング
    if NEED_RESCHED.swap(false, Ordering::Acquire) {
        // 割り込みは無効のままschedule()を呼び出す
        // これにより、schedule()実行中に再度タイマー割り込みが入ることを防ぐ
        schedule();
    }
    // iretqで元のRFLAGSが復元される
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
/// # ロック順序（段階的取得）
/// 1. RT_QUEUE → 即解放
/// 2. CFS_QUEUE → 即解放
/// 3. IDLE_QUEUE → 即解放
/// 4. CURRENT_TASK → 処理後解放
/// 5. BLOCKED_TASKS または 各キュー（単一）
///
/// # 前提条件
/// この関数は内部で cli を実行するため、割り込み有効状態で呼び出すこと。
/// シングルコア環境を前提としており、cli による割り込み無効化が
/// フェーズ間のレース条件を防いでいます。
///
/// # Note
/// この関数は割り込みを無効化してからロックを取得します。
/// これにより、タイマー割り込みハンドラとのデッドロックを防ぎます。
pub fn schedule() {
    // SAFETY: cli はRFLAGSの割り込みフラグを無効化するのみで、メモリ安全性に影響しない。
    // カーネルモードで実行されることが前提であり、ユーザーモードからの呼び出しはCPUが拒否する。
    // 割り込み無効化により、フェーズ間でのレース条件を防ぎ、データ整合性を保証する。
    unsafe {
        core::arch::asm!("cli", options(nomem, nostack));
    }

    // ===== フェーズ1: 次タスクの選択（段階的ロック取得） =====
    // 優先度順にキューをチェックし、見つかったらすぐにロック解放
    // これにより、複数のキューを同時にロックする必要がなくなる
    let next_task = {
        // 1. リアルタイムキューをチェック（最優先）
        let mut rt_queue = RT_QUEUE.lock();
        if let Some(entry) = rt_queue.pop_first() {
            drop(rt_queue);
            Some(entry.1)
        } else {
            drop(rt_queue);
            // 2. CFSキューをチェック
            let mut cfs_queue = CFS_QUEUE.lock();
            if let Some(entry) = cfs_queue.pop_first() {
                drop(cfs_queue);
                Some(entry.1)
            } else {
                drop(cfs_queue);
                // 3. アイドルキューをチェック
                let mut idle_queue = IDLE_QUEUE.lock();
                let task = idle_queue.pop_front();
                drop(idle_queue);
                task
            }
        }
    };

    // タスクがない場合は早期リターン
    let Some(mut next_task) = next_task else {
        // SAFETY: sti は割り込みフラグを有効化するのみで、メモリ安全性に影響しない。
        // cli で無効化した割り込みを復元する。
        unsafe {
            core::arch::asm!("sti", options(nomem, nostack));
        }
        return;
    };

    next_task.set_state(TaskState::Running);
    let new_context_ptr = next_task.context() as *const Context;

    // ===== フェーズ2: 現在のタスクの処理（CURRENT_TASKのみロック） =====
    let old_context_ptr = {
        let mut current = CURRENT_TASK.lock();
        if let Some(mut old_task) = current.take() {
            // 蓄積された実行時間でvruntimeを更新（Normalクラスのみ有効）
            // accumulatedが0でも最小値(1)を加算して、同じタスクが連続選択されることを防ぐ
            let accumulated = ACCUMULATED_RUNTIME.swap(0, Ordering::Relaxed);
            if old_task.sched_class() == SchedulingClass::Normal {
                let delta = if accumulated > 0 { accumulated } else { 1 };
                old_task.update_vruntime(delta);
            }

            // 実行中だった場合は準備完了状態に変更
            if old_task.state() == TaskState::Running {
                old_task.set_state(TaskState::Ready);
            }

            // 古いタスクのコンテキストへのポインタを取得
            // （Box内のTaskは移動しても同じアドレスに留まる）
            let old_ctx_ptr = old_task.context_mut() as *mut Context;
            let state = old_task.state();

            // 新しいタスクを現在のタスクに設定
            *current = Some(next_task);
            drop(current); // CURRENT_TASKのロック解放

            // ===== フェーズ3: 古いタスクを適切な場所に移動（単一キューロック） =====
            // 各キューを個別にロックすることで、ロック競合を最小化
            match state {
                TaskState::Terminated => {
                    // 終了したタスクは破棄
                }
                TaskState::Blocked => {
                    // ブロック中のタスクはBLOCKED_TASKSに移動
                    let task_id = old_task.id().as_u64();
                    let mut blocked = BLOCKED_TASKS.lock();
                    blocked.insert(task_id, old_task);
                    // blockedのロックはスコープ終了で自動解放
                }
                _ => {
                    // Ready状態のタスクは適切なキューにエンキュー（単一キューロック版）
                    enqueue_task_single(old_task);
                }
            }

            old_ctx_ptr
        } else {
            // 現在のタスクがない場合（初回起動時）
            // 新しいタスクを現在のタスクに設定
            *current = Some(next_task);
            drop(current);
            // staticなダミーコンテキストを使用
            // SAFETY: DUMMY_CONTEXTへの可変参照は、初回スケジュール時にのみ使用される。
            // その後は使用されないため、競合は発生しない。
            &raw mut DUMMY_CONTEXT as *mut Context
        }
    };

    // コンテキストスイッチを実行
    // old_context_ptrに現在の状態を保存し、new_context_ptrの状態を復元
    // RFLAGSの保存・復元もswitch_context()内部で自動的に処理される
    // SAFETY: old_context_ptrとnew_context_ptrは、それぞれ有効なContext構造体を指す。
    // old_context_ptrは現在実行中のタスクまたはDUMMY_CONTEXT、
    // new_context_ptrはキューから取得した次のタスクのコンテキスト。
    unsafe {
        switch_context(old_context_ptr, new_context_ptr);
    }

    // ここに戻ってくるのは、このタスクが再度スケジュールされた時
}
