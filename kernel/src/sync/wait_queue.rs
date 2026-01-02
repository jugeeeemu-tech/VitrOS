//! Wait Queue - ブロックされたタスクを管理するキュー
//!
//! # 設計方針
//! シングルCPU環境でのデッドロック防止のため、スピンロック保持中は
//! 割り込みを無効化します。これにより、ロック保持中にプリエンプションが
//! 発生して別タスクが同じロックを取得しようとする問題を防ぎます。

use crate::task::TaskId;
use alloc::collections::VecDeque;
use spin::Mutex as SpinMutex;

/// ブロックされたタスクを管理するキュー
pub struct WaitQueue {
    /// 待機中のタスクIDリスト
    waiters: SpinMutex<VecDeque<TaskId>>,
}

/// 割り込みを無効化してクロージャを実行
///
/// クロージャ実行後、元の割り込み状態を復元します。
fn without_interrupts<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    let rflags: u64;
    unsafe {
        core::arch::asm!("pushfq; pop {}; cli", out(reg) rflags, options(nomem, nostack));
    }

    let result = f();

    if rflags & 0x200 != 0 {
        unsafe {
            core::arch::asm!("sti", options(nomem, nostack));
        }
    }

    result
}

impl WaitQueue {
    /// 新しいWaitQueueを作成
    pub const fn new() -> Self {
        Self {
            waiters: SpinMutex::new(VecDeque::new()),
        }
    }

    /// 現在のタスクを待機キューに追加してブロック
    ///
    /// この関数はタスクをBlockedに設定してスケジュールします。
    ///
    /// # 実装詳細
    /// スピンロック保持中は割り込みを無効化し、シングルCPU環境での
    /// デッドロックを防止します。
    pub fn wait(&self) {
        let task_id = crate::task::current_task_id();

        // スピンロック保持中は割り込みを無効化
        // これにより、ロック保持中のプリエンプションを防ぐ
        without_interrupts(|| {
            let mut waiters = self.waiters.lock();
            waiters.push_back(task_id);
        });

        // waitersロック解放後にブロック
        // block_current_task()は内部で適切にロックを管理する
        crate::task::block_current_task();
    }

    /// 1つのタスクを起床させる
    ///
    /// # Returns
    /// 起床させたタスクがあればtrue、キューが空ならfalse
    ///
    /// # 実装詳細
    /// スピンロック保持中は割り込みを無効化し、シングルCPU環境での
    /// デッドロックを防止します。unblock_task()はロック解放後に呼び出します。
    pub fn wake_one(&self) -> bool {
        // スピンロック操作を割り込み無効で実行
        let task_id = without_interrupts(|| {
            let mut waiters = self.waiters.lock();
            waiters.pop_front()
        });

        if let Some(id) = task_id {
            // ロック解放後にunblock_task()を呼び出す
            crate::task::unblock_task(id);
            true
        } else {
            false
        }
    }

    /// すべてのタスクを起床させる
    ///
    /// # 実装詳細
    /// 各タスクの起床処理を個別に行い、ロック保持時間を最小化します。
    pub fn wake_all(&self) {
        loop {
            // 1つずつタスクIDを取得（割り込み無効で）
            let task_id = without_interrupts(|| {
                let mut waiters = self.waiters.lock();
                waiters.pop_front()
            });

            if let Some(id) = task_id {
                // ロック解放後にunblock_task()を呼び出す
                crate::task::unblock_task(id);
            } else {
                break;
            }
        }
    }
}
