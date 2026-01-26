//! ブロッキングMutex
//!
//! スピンロックではなく、タスクをブロックすることで排他制御を行うMutex

use super::wait_queue::WaitQueue;
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// ブロッキングMutex
///
/// ロック取得時にスピンせず、タスクをブロックすることで
/// CPU時間を無駄にしない排他制御を実現します。
///
/// # Safety
/// 内部でロックにより排他アクセスを保証します。
pub struct BlockingMutex<T: ?Sized> {
    /// ロック状態（true = ロック中）
    locked: AtomicBool,
    /// 待機キュー
    wait_queue: WaitQueue,
    /// 保護対象データ
    data: UnsafeCell<T>,
}

// Safety: 内部でロックにより排他アクセスを保証
unsafe impl<T: ?Sized + Send> Send for BlockingMutex<T> {}
unsafe impl<T: ?Sized + Send> Sync for BlockingMutex<T> {}

impl<T> BlockingMutex<T> {
    /// 新しいBlockingMutexを作成
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            wait_queue: WaitQueue::new(),
            data: UnsafeCell::new(value),
        }
    }

    /// ロックを取得
    ///
    /// 他のタスクがロックを保持している場合、現在のタスクをブロックします。
    /// 割り込みコンテキストではスピンにフォールバックします。
    ///
    /// # Returns
    /// ロックガード
    pub fn lock(&self) -> MutexGuard<'_, T> {
        loop {
            // 楽観的にロック取得を試みる
            if self
                .locked
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return MutexGuard { mutex: self };
            }

            // 取得失敗時、割り込みコンテキストでないことを確認
            if crate::sched::is_interrupt_context() {
                // 割り込み中はスピンにフォールバック
                while self.locked.load(Ordering::Relaxed) {
                    core::hint::spin_loop();
                }
            } else {
                // 通常コンテキストではブロック
                self.wait_queue.wait();
            }
        }
    }

    /// try_lock: ノンブロッキングでロック取得を試みる
    ///
    /// ロックが取得できない場合、即座にNoneを返します。
    ///
    /// # Returns
    /// ロックが取得できた場合はSome(MutexGuard)、できなかった場合はNone
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        if self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(MutexGuard { mutex: self })
        } else {
            None
        }
    }
}

/// Mutexガード（RAII）
///
/// Drop時にロックを自動的に解放し、待機中のタスクを起床させます。
pub struct MutexGuard<'a, T: ?Sized> {
    mutex: &'a BlockingMutex<T>,
}

impl<'a, T: ?Sized> Deref for MutexGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<'a, T: ?Sized> DerefMut for MutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<'a, T: ?Sized> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        // ロックを解放
        self.mutex.locked.store(false, Ordering::Release);
        // 待機中のタスクを1つ起床
        self.mutex.wait_queue.wake_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_mutex_new() {
        let mutex = BlockingMutex::new(42);
        // 初期状態でロック可能
        assert!(mutex.try_lock().is_some());
    }

    #[test_case]
    fn test_mutex_try_lock_success() {
        let mutex = BlockingMutex::new(42);
        let guard = mutex.try_lock();
        assert!(guard.is_some());
        assert_eq!(*guard.unwrap(), 42);
    }

    #[test_case]
    fn test_mutex_try_lock_fail_when_locked() {
        let mutex = BlockingMutex::new(42);
        let _guard = mutex.try_lock().unwrap();
        // 既にロック済みなのでNoneが返る
        assert!(mutex.try_lock().is_none());
    }

    #[test_case]
    fn test_mutex_guard_deref() {
        let mutex = BlockingMutex::new(42);
        let guard = mutex.try_lock().unwrap();
        // Deref で不変参照を取得
        let value: &i32 = &*guard;
        assert_eq!(*value, 42);
    }

    #[test_case]
    fn test_mutex_guard_deref_mut() {
        let mutex = BlockingMutex::new(42);
        let mut guard = mutex.try_lock().unwrap();
        // DerefMut で可変参照を取得し、値を変更
        *guard = 100;
        assert_eq!(*guard, 100);
    }

    #[test_case]
    fn test_mutex_unlock_on_drop() {
        let mutex = BlockingMutex::new(42);
        {
            let _guard = mutex.try_lock().unwrap();
            // ここではロック中
            assert!(mutex.try_lock().is_none());
        }
        // guardがdropされたのでロック解除
        assert!(mutex.try_lock().is_some());
    }
}
