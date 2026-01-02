//! タスク管理とマルチタスキング
//!
//! このモジュールはタスクの作成、管理、スケジューリングを担当します。
//!
//! # モジュール構成
//! - `task`: タスク構造体、状態、優先度の定義
//! - `context`: CPUコンテキストとコンテキストスイッチ
//! - `scheduler`: スケジューラとキュー管理
//! - `blocking`: タスクのブロッキングとスリープ機能

mod blocking;
mod context;
mod scheduler;
mod task;

// 公開API: タスク関連
pub use task::Nice;
pub use task::RtPriority;
pub use task::SchedulingClass;
pub use task::Task;
pub use task::TaskError;
pub use task::TaskId;
pub use task::TaskState;
pub use task::nice;
pub use task::rt_priority;

// 公開API: コンテキスト関連
pub use context::Context;

// 公開API: スケジューラ関連
pub use scheduler::add_task;
pub use scheduler::check_resched_on_interrupt_exit;
pub use scheduler::current_task_id;
pub use scheduler::init;
pub use scheduler::schedule;
pub use scheduler::set_current_task;
pub use scheduler::set_need_resched;
pub use scheduler::try_add_task;
pub use scheduler::update_current_task_vruntime;
pub use scheduler::yield_now;

// 公開API: ブロッキング関連
pub use blocking::block_current_task;
pub use blocking::is_interrupt_context;
pub use blocking::sleep_ms;
pub use blocking::unblock_task;
