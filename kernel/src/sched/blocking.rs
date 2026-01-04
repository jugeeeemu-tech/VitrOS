//! タスクのブロッキングとスリープ機能
//!
//! このモジュールはタスクのブロック/アンブロックとスリープ機能を提供します。

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::collections::BTreeSet;
use lazy_static::lazy_static;
use spin::Mutex;

use crate::io::without_interrupts;

use super::scheduler::{CURRENT_TASK, current_task_id, schedule};
use super::task::{Task, TaskId, TaskState};

lazy_static! {
    /// ブロック中のタスク (TaskId -> Task)
    /// ブロッキング同期プリミティブで待機中のタスクを管理
    pub(super) static ref BLOCKED_TASKS: Mutex<BTreeMap<u64, Box<Task>>> = Mutex::new(BTreeMap::new());

    /// 起床保留中のタスクID集合
    ///
    /// Lost Wakeup問題を防ぐために使用。
    /// unblock_task()が呼ばれた時にタスクがまだBLOCKED_TASKSにいない場合、
    /// このセットにIDを追加し、block_current_task()でチェックする。
    /// schedule()でもBLOCKED_TASKSへの追加前にチェックされる。
    pub(super) static ref WAKEUP_PENDING: Mutex<BTreeSet<u64>> = Mutex::new(BTreeSet::new());
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
    // SAFETY: PUSHFQ/POP命令でRFLAGSを読み取る。
    // これらの命令はメモリアクセスを伴わず安全。
    unsafe {
        core::arch::asm!("pushfq; pop {}", out(reg) rflags, options(nomem, nostack));
    }
    // IF=0 なら割り込み無効＝割り込みコンテキストの可能性
    // より正確には、割り込み無効化されている＝ブロックすべきでない
    (rflags & 0x200) == 0
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
            super::scheduler::enqueue_to_appropriate_queue(task, sched_class);
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
        super::scheduler::yield_now();
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
