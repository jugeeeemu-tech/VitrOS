//! Local APIC (Advanced Programmable Interrupt Controller) 実装
//!
//! Intel SDM Vol 3A Chapter 10 に基づく実装

use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU32, Ordering};

use crate::pit;

/// Local APICのベースアドレス（デフォルト）
const APIC_BASE: u64 = 0xFEE00000;

/// Local APICレジスタのオフセット
mod registers {
    /// Spurious Interrupt Vector Register
    pub const SPURIOUS_INTERRUPT_VECTOR: u32 = 0xF0;
    /// End of Interrupt Register
    pub const EOI: u32 = 0xB0;
    /// Timer LVT (Local Vector Table) Register
    pub const TIMER_LVT: u32 = 0x320;
    /// Timer Divide Configuration Register
    pub const TIMER_DIVIDE_CONFIG: u32 = 0x3E0;
    /// Timer Initial Count Register
    pub const TIMER_INITIAL_COUNT: u32 = 0x380;
    /// Timer Current Count Register
    pub const TIMER_CURRENT_COUNT: u32 = 0x390;
}

/// Local APICレジスタへの書き込み
unsafe fn write_apic_register(offset: u32, value: u32) {
    let addr = (APIC_BASE + offset as u64) as *mut u32;
    unsafe {
        write_volatile(addr, value);
    }
}

/// Local APICレジスタからの読み込み
unsafe fn read_apic_register(offset: u32) -> u32 {
    let addr = (APIC_BASE + offset as u64) as *const u32;
    unsafe {
        read_volatile(addr)
    }
}

/// MSR (Model Specific Register) の読み込み
unsafe fn read_msr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nostack, preserves_flags)
        );
    }
    ((high as u64) << 32) | (low as u64)
}

/// MSR (Model Specific Register) への書き込み
unsafe fn write_msr(msr: u32, value: u64) {
    let low = (value & 0xFFFFFFFF) as u32;
    let high = ((value >> 32) & 0xFFFFFFFF) as u32;
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") low,
            in("edx") high,
            options(nostack, preserves_flags)
        );
    }
}

/// Local APICを有効化
pub fn enable_apic() {
    unsafe {
        // IA32_APIC_BASE MSR (0x1B) を読み込み
        const IA32_APIC_BASE_MSR: u32 = 0x1B;
        let mut apic_base = read_msr(IA32_APIC_BASE_MSR);

        // APIC Enable bit (bit 11) をセット
        apic_base |= 1 << 11;

        // MSRに書き戻し
        write_msr(IA32_APIC_BASE_MSR, apic_base);

        // Spurious Interrupt Vector Registerを設定してAPICを有効化
        // bit 8: APIC Software Enable/Disable
        // bits 0-7: Spurious Vector (通常は0xFF)
        write_apic_register(registers::SPURIOUS_INTERRUPT_VECTOR, 0x1FF);
    }
}

/// タイマー割り込みベクタ番号
pub const TIMER_INTERRUPT_VECTOR: u8 = 32;

/// キャリブレーションされたAPIC Timerのバス周波数（Hz）
/// 分周比を考慮した実効周波数
static APIC_TIMER_FREQUENCY: AtomicU32 = AtomicU32::new(0);

/// APIC Timerをキャリブレーション
///
/// PITを使ってAPIC Timerの実際の周波数を測定します。
/// この関数は割り込みが無効な状態で呼び出す必要があります。
pub fn calibrate_timer() {
    unsafe {
        // Timer Divide Configuration Register を設定
        // 0x3 = Divide by 16
        write_apic_register(registers::TIMER_DIVIDE_CONFIG, 0x3);

        // Timer LVT Register を設定（マスク状態）
        // bit 16: Mask (1 = Masked)
        let masked = 1 << 16;
        write_apic_register(registers::TIMER_LVT, masked);

        // APIC Timerを最大値で開始（One-shot mode）
        write_apic_register(registers::TIMER_INITIAL_COUNT, 0xFFFFFFFF);

        // PITで10ms待つ
        pit::sleep_ms(10);

        // 現在のカウント値を読み取る
        let current_count = read_apic_register(registers::TIMER_CURRENT_COUNT);

        // タイマーを停止
        write_apic_register(registers::TIMER_INITIAL_COUNT, 0);

        // 10msでカウントダウンした量を計算
        let ticks_in_10ms = 0xFFFFFFFF - current_count;

        // 1秒間のtick数を計算（10ms * 100 = 1秒）
        let ticks_per_second = ticks_in_10ms * 100;

        // バス周波数を保存（分周比16を考慮した実効周波数）
        APIC_TIMER_FREQUENCY.store(ticks_per_second, Ordering::SeqCst);

        crate::info!(
            "APIC Timer calibrated: {} Hz ({} ticks in 10ms)",
            ticks_per_second,
            ticks_in_10ms
        );
    }
}

/// Local APIC Timerを初期化
///
/// # Arguments
/// * `frequency_hz` - タイマー割り込みの周波数 (Hz)
pub fn init_timer(frequency_hz: u32) {
    unsafe {
        // キャリブレーション結果を取得
        let apic_freq = APIC_TIMER_FREQUENCY.load(Ordering::SeqCst);
        if apic_freq == 0 {
            panic!("APIC Timer not calibrated! Call calibrate_timer() first.");
        }

        // Timer Divide Configuration Register を設定
        // 0x3 = Divide by 16
        write_apic_register(registers::TIMER_DIVIDE_CONFIG, 0x3);

        // Timer LVT Register を設定
        // bit 17: Timer Mode (0 = One-shot, 1 = Periodic)
        // bit 16: Mask (0 = Not masked)
        // bits 0-7: Vector number
        let timer_mode_periodic = 1 << 17;
        let not_masked = 0 << 16;
        let lvt_value = timer_mode_periodic | not_masked | (TIMER_INTERRUPT_VECTOR as u32);
        write_apic_register(registers::TIMER_LVT, lvt_value);

        // Initial Count Register を設定
        // キャリブレーション結果を使って正確な値を計算
        let initial_count = apic_freq / frequency_hz;
        write_apic_register(registers::TIMER_INITIAL_COUNT, initial_count);

        crate::info!(
            "APIC Timer initialized: {} Hz (initial count: {})",
            frequency_hz,
            initial_count
        );
    }
}

/// End of Interrupt (EOI) を送信
/// 割り込みハンドラの最後に呼び出す必要があります
pub fn send_eoi() {
    unsafe {
        write_apic_register(registers::EOI, 0);
    }
}

/// Local APICを初期化
pub fn init() {
    enable_apic();
    // タイマーは別途 init_timer() で初期化
}
