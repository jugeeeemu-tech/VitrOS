//! HPET (High Precision Event Timer) 実装
//!
//! HPETはPITより高精度なタイマーで、APIC Timerのキャリブレーションに最適です。
//! 周波数がACPIテーブルで定義されているため、キャリブレーション不要です。

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::paging::KERNEL_VIRTUAL_BASE;

/// HPETが利用可能かどうか
static HPET_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// HPETのベースアドレス（仮想アドレス）
static HPET_BASE: AtomicU64 = AtomicU64::new(0);

/// HPETの周波数（Hz）
static HPET_FREQUENCY: AtomicU64 = AtomicU64::new(0);

/// HPETのカウント周期（フェムト秒/カウント）
static HPET_PERIOD_FS: AtomicU64 = AtomicU64::new(0);

/// HPET初期化時のカウンタ値（経過時間計算の基準点）
static HPET_START_COUNTER: AtomicU64 = AtomicU64::new(0);

/// HPETレジスタオフセット
mod registers {
    /// General Capabilities and ID Register
    pub const GENERAL_CAP_ID: u64 = 0x000;
    /// General Configuration Register
    pub const GENERAL_CONFIG: u64 = 0x010;
    /// Main Counter Value Register
    pub const MAIN_COUNTER: u64 = 0x0F0;
}

/// HPETレジスタからの読み込み（64bit）
unsafe fn read_hpet_reg(offset: u64) -> u64 {
    let base = HPET_BASE.load(Ordering::SeqCst);
    if base == 0 {
        return 0;
    }
    let addr = (base + offset) as *const u64;
    unsafe { read_volatile(addr) }
}

/// HPETレジスタへの書き込み（64bit）
unsafe fn write_hpet_reg(offset: u64, value: u64) {
    let base = HPET_BASE.load(Ordering::SeqCst);
    if base == 0 {
        return;
    }
    let addr = (base + offset) as *mut u64;
    unsafe { write_volatile(addr, value) }
}

/// ACPIからHPETを初期化
///
/// # Arguments
/// * `base_phys_addr` - HPETレジスタの物理ベースアドレス
///
/// # Errors
/// * `PagingError` - MMIOマッピングに失敗した場合
pub fn init(base_phys_addr: u64) -> Result<(), crate::paging::PagingError> {
    // HPET MMIO領域をUC属性でマッピング
    crate::paging::map_mmio(base_phys_addr, crate::paging::PAGE_SIZE as u64)?;

    // 物理アドレスを仮想アドレスに変換
    let base_virt = KERNEL_VIRTUAL_BASE + base_phys_addr;
    HPET_BASE.store(base_virt, Ordering::SeqCst);

    // SAFETY:
    // - HPETのベースアドレスはACPIテーブルから取得した有効なMMIOアドレス
    // - 上記の map_mmio() によりUC属性でページテーブルにマッピング済み
    // - read_volatile/write_volatile により、コンパイラの最適化を防ぎ
    //   MMIOレジスタへの正確なアクセスを保証
    unsafe {
        // General Capabilities and ID レジスタを読み込み
        let cap_id = read_hpet_reg(registers::GENERAL_CAP_ID);

        // bits 63:32 = Counter Clock Period in femtoseconds
        let period_fs = cap_id >> 32;
        HPET_PERIOD_FS.store(period_fs, Ordering::SeqCst);

        // 周波数を計算: freq = 10^15 / period_fs (Hz)
        // period_fs は通常 ~10,000,000 fs (= 10ns = 100MHz)
        let frequency = if period_fs > 0 {
            1_000_000_000_000_000u64 / period_fs
        } else {
            0
        };
        HPET_FREQUENCY.store(frequency, Ordering::SeqCst);

        // HPETを有効化
        // bit 0 = ENABLE_CNF (Overall Enable)
        let config = read_hpet_reg(registers::GENERAL_CONFIG);
        write_hpet_reg(registers::GENERAL_CONFIG, config | 1);

        // 初期カウンタ値を保存（経過時間計算の基準点）
        let start_counter = read_hpet_reg(registers::MAIN_COUNTER);
        HPET_START_COUNTER.store(start_counter, Ordering::SeqCst);

        HPET_AVAILABLE.store(true, Ordering::SeqCst);

        crate::info!(
            "HPET initialized: base=0x{:X}, period={}fs, freq={}MHz",
            base_phys_addr,
            period_fs,
            frequency / 1_000_000
        );
    }

    Ok(())
}

/// HPETが利用可能かどうか
pub fn is_available() -> bool {
    HPET_AVAILABLE.load(Ordering::SeqCst)
}

/// HPETの周波数を取得（Hz）
#[allow(dead_code)]
pub fn frequency() -> u64 {
    HPET_FREQUENCY.load(Ordering::SeqCst)
}

/// HPETのメインカウンタを読み取る
pub fn read_counter() -> u64 {
    // SAFETY: HPETが初期化されていれば、メインカウンタの読み取りは安全
    unsafe { read_hpet_reg(registers::MAIN_COUNTER) }
}

/// 指定ナノ秒間待機（HPETを使用）
///
/// # Arguments
/// * `ns` - 待機時間（ナノ秒）
pub fn delay_ns(ns: u64) {
    if !is_available() {
        return;
    }

    let period_fs = HPET_PERIOD_FS.load(Ordering::SeqCst);
    if period_fs == 0 {
        return;
    }

    // 必要なカウント数を計算
    // ns * 10^6 = fs なので、 counts = ns * 10^6 / period_fs
    let target_counts = (ns * 1_000_000) / period_fs;

    let start = read_counter();
    while read_counter().wrapping_sub(start) < target_counts {
        core::hint::spin_loop();
    }
}

/// 指定マイクロ秒間待機
#[allow(dead_code)]
pub fn delay_us(us: u64) {
    delay_ns(us * 1_000);
}

/// 指定ミリ秒間待機
pub fn delay_ms(ms: u64) {
    delay_ns(ms * 1_000_000);
}

// ============================================================================
// 経過時間取得API
// ============================================================================

/// HPET初期化からの経過カウント数を取得
fn elapsed_counts() -> u64 {
    let current = read_counter();
    let start = HPET_START_COUNTER.load(Ordering::SeqCst);
    current.wrapping_sub(start)
}

/// HPET初期化からの経過時間を取得（ナノ秒）
///
/// HPETが利用不可の場合は0を返します。
pub fn elapsed_ns() -> u64 {
    if !is_available() {
        return 0;
    }

    let period_fs = HPET_PERIOD_FS.load(Ordering::SeqCst);
    if period_fs == 0 {
        return 0;
    }

    let counts = elapsed_counts();
    // counts * period_fs = フェムト秒
    // フェムト秒 / 10^6 = ナノ秒
    // オーバーフロー対策: 先に割ってから掛ける
    // period_fs は通常 ~10,000,000 fs なので、counts * period_fs がオーバーフローしやすい
    // counts / (10^6 / period_fs) = counts * period_fs / 10^6
    (counts / 1_000_000) * period_fs + (counts % 1_000_000) * period_fs / 1_000_000
}

/// HPET初期化からの経過時間を取得（マイクロ秒）
#[allow(dead_code)]
pub fn elapsed_us() -> u64 {
    elapsed_ns() / 1_000
}

/// HPET初期化からの経過時間を取得（ミリ秒）
pub fn elapsed_ms() -> u64 {
    elapsed_ns() / 1_000_000
}

/// HPET初期化からの経過時間を取得（秒）
pub fn elapsed_secs() -> u64 {
    elapsed_ns() / 1_000_000_000
}
