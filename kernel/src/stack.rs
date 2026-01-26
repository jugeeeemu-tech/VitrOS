//! カーネルスタック管理モジュール
//!
//! リンカスクリプトで定義されたスタック領域へのアクセスを提供。
//! テスト環境ではスタブ実装を提供（QEMUスタックを使用）。

use crate::paging::PAGE_SIZE;

// リンカスクリプトで定義されたシンボル（paging.rsと同じパターン）
#[cfg(not(test))]
unsafe extern "C" {
    static __stack_top: u8;
    static __stack_bottom: u8;
    static __stack_guard: u8;
}

/// スタックトップアドレスを取得
#[cfg(not(test))]
pub fn stack_top() -> u64 {
    core::ptr::addr_of!(__stack_top) as u64
}

/// ガードページアドレスを取得
#[cfg(not(test))]
pub fn guard_page_address() -> Option<u64> {
    Some(core::ptr::addr_of!(__stack_guard) as u64)
}

/// 指定されたアドレスがガードページ範囲内かを判定
pub fn is_guard_page_fault(fault_addr: u64) -> bool {
    match guard_page_address() {
        Some(guard_addr) => fault_addr >= guard_addr && fault_addr < guard_addr + PAGE_SIZE as u64,
        None => false,
    }
}

// テスト環境用スタブ
#[cfg(test)]
pub fn stack_top() -> u64 {
    0
}

#[cfg(test)]
pub fn guard_page_address() -> Option<u64> {
    None
}
