//! Local APIC (Advanced Programmable Interrupt Controller) 実装
//!
//! Intel SDM Vol 3A Chapter 10 に基づく実装

use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU32, Ordering};

use crate::hpet;
use crate::paging::KERNEL_VIRTUAL_BASE;
use crate::pit;

/// APIC操作のエラー型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApicError {
    /// タイマーがキャリブレーションされていない
    NotCalibrated,
    /// キャリブレーションに失敗
    CalibrationFailed,
    /// 初期化に失敗
    InitFailed,
    /// 無効な周波数（0など）
    InvalidFrequency,
}

impl core::fmt::Display for ApicError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            ApicError::NotCalibrated => write!(f, "APIC Timer not calibrated"),
            ApicError::CalibrationFailed => write!(f, "APIC Timer calibration failed"),
            ApicError::InitFailed => write!(f, "APIC initialization failed"),
            ApicError::InvalidFrequency => write!(f, "Invalid APIC timer frequency"),
        }
    }
}

/// Local APICのベースアドレス（高位仮想アドレス）
/// 物理アドレス 0xFEE00000 を高位仮想アドレス経由でアクセス
const APIC_BASE: u64 = KERNEL_VIRTUAL_BASE + 0xFEE00000;

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
///
/// # Safety
/// - APIC_BASEが有効なLocal APICメモリマップドレジスタのベースアドレスであること
/// - offsetが有効なAPICレジスタオフセットであること
/// - APICが有効化されていること（enable_apic()呼び出し後）
unsafe fn write_apic_register(offset: u32, value: u32) {
    let addr = (APIC_BASE + offset as u64) as *mut u32;
    // SAFETY: 呼び出し元が上記の安全性要件を満たすことを保証する。
    // APICレジスタはメモリマップドI/Oであり、write_volatileで書き込む必要がある。
    unsafe {
        write_volatile(addr, value);
    }
}

/// Local APICレジスタからの読み込み
///
/// # Safety
/// - APIC_BASEが有効なLocal APICメモリマップドレジスタのベースアドレスであること
/// - offsetが有効なAPICレジスタオフセットであること
unsafe fn read_apic_register(offset: u32) -> u32 {
    let addr = (APIC_BASE + offset as u64) as *const u32;
    // SAFETY: 呼び出し元が上記の安全性要件を満たすことを保証する。
    // APICレジスタはメモリマップドI/Oであり、read_volatileで読み込む必要がある。
    unsafe { read_volatile(addr) }
}

/// MSR (Model Specific Register) の読み込み
///
/// # Safety
/// - msrが有効なMSRアドレスであること
/// - Ring 0で実行されること
unsafe fn read_msr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: 呼び出し元が有効なMSRアドレスを指定することを保証する。
    // RDMSR命令はRing 0でのみ実行可能であり、カーネルモードで動作している。
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
///
/// # Safety
/// - msrが有効な書き込み可能MSRアドレスであること
/// - valueがそのMSRに対して有効な値であること
/// - Ring 0で実行されること
unsafe fn write_msr(msr: u32, value: u64) {
    let low = (value & 0xFFFFFFFF) as u32;
    let high = ((value >> 32) & 0xFFFFFFFF) as u32;
    // SAFETY: 呼び出し元が有効なMSRアドレスと値を指定することを保証する。
    // WRMSR命令はRing 0でのみ実行可能であり、カーネルモードで動作している。
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
    // SAFETY: IA32_APIC_BASE MSR (0x1B) はx86_64アーキテクチャで定義された
    // 標準的なMSRであり、APICの有効化に使用される。
    // Spurious Interrupt Vector Registerへの書き込みも、APICが
    // メモリマップされた標準アドレス(0xFEE00000)に存在する前提で安全。
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

/// HPETを使って1回のAPIC Timer測定を実行
///
/// # Safety
/// APICが有効化されていること、HPETが初期化済みであること
unsafe fn measure_apic_ticks_hpet(ms: u64) -> u32 {
    unsafe {
        // Timer Divide Configuration Register を設定
        // 0x3 = Divide by 16
        write_apic_register(registers::TIMER_DIVIDE_CONFIG, 0x3);

        // Timer LVT Register を設定（マスク状態）
        let masked = 1 << 16;
        write_apic_register(registers::TIMER_LVT, masked);

        // APIC Timerを最大値で開始（One-shot mode）
        write_apic_register(registers::TIMER_INITIAL_COUNT, 0xFFFFFFFF);

        // HPETで指定ミリ秒待つ（高精度）
        hpet::delay_ms(ms);

        // 現在のカウント値を読み取る
        let current_count = read_apic_register(registers::TIMER_CURRENT_COUNT);

        // タイマーを停止
        write_apic_register(registers::TIMER_INITIAL_COUNT, 0);

        // カウントダウンした量を返す
        0xFFFFFFFF - current_count
    }
}

/// PITを使って1回のAPIC Timer測定を実行
///
/// # Safety
/// APICが有効化されていること
unsafe fn measure_apic_ticks_pit(ms: u32) -> u32 {
    unsafe {
        // Timer Divide Configuration Register を設定
        // 0x3 = Divide by 16
        write_apic_register(registers::TIMER_DIVIDE_CONFIG, 0x3);

        // Timer LVT Register を設定（マスク状態）
        let masked = 1 << 16;
        write_apic_register(registers::TIMER_LVT, masked);

        // APIC Timerを最大値で開始（One-shot mode）
        write_apic_register(registers::TIMER_INITIAL_COUNT, 0xFFFFFFFF);

        // PITで指定ミリ秒待つ
        pit::sleep_ms(ms);

        // 現在のカウント値を読み取る
        let current_count = read_apic_register(registers::TIMER_CURRENT_COUNT);

        // タイマーを停止
        write_apic_register(registers::TIMER_INITIAL_COUNT, 0);

        // カウントダウンした量を返す
        0xFFFFFFFF - current_count
    }
}

/// APIC Timerをキャリブレーション
///
/// HPET（利用可能な場合）またはPITを使ってAPIC Timerの周波数を測定します。
/// HPETは高精度なので1回測定、PITは5回測定して中央値を採用します。
/// この関数は割り込みが無効な状態で呼び出す必要があります。
///
/// # Errors
/// * `ApicError::CalibrationFailed` - キャリブレーションに失敗した場合（周波数が0など）
pub fn calibrate_timer() -> Result<(), ApicError> {
    let ticks_per_second = if hpet::is_available() {
        // HPETが利用可能: 高精度なので1回測定で十分
        const CALIBRATION_MS: u64 = 50;

        crate::info!("Calibrating APIC Timer using HPET...");

        // SAFETY: enable_apic()呼び出し後であることが前提
        let ticks = unsafe { measure_apic_ticks_hpet(CALIBRATION_MS) };
        let ticks_per_second = ticks * (1000 / CALIBRATION_MS as u32);

        crate::info!(
            "APIC Timer calibrated (HPET): {} Hz ({} ticks in {}ms)",
            ticks_per_second,
            ticks,
            CALIBRATION_MS
        );

        ticks_per_second
    } else {
        // PITを使用: 精度向上のため5回測定して中央値
        const MEASUREMENTS: usize = 5;
        const CALIBRATION_MS: u32 = 50;

        crate::info!("Calibrating APIC Timer using PIT ({} measurements)...", MEASUREMENTS);

        let mut measurements = [0u32; MEASUREMENTS];

        for measurement in measurements.iter_mut() {
            // SAFETY: enable_apic()呼び出し後であることが前提
            *measurement = unsafe { measure_apic_ticks_pit(CALIBRATION_MS) };
        }

        // ソートして中央値を取る（外れ値の影響を排除）
        measurements.sort_unstable();
        let median_ticks = measurements[MEASUREMENTS / 2];
        let multiplier = 1000 / CALIBRATION_MS;
        let ticks_per_second = median_ticks * multiplier;

        crate::info!(
            "APIC Timer calibrated (PIT): {} Hz (median: {} ticks in {}ms)",
            ticks_per_second,
            median_ticks,
            CALIBRATION_MS
        );
        crate::info!("  measurements: {:?}", measurements.map(|t| t * multiplier));

        ticks_per_second
    };

    // バス周波数を保存（分周比16を考慮した実効周波数）
    APIC_TIMER_FREQUENCY.store(ticks_per_second, Ordering::SeqCst);

    // 周波数が0の場合はエラー
    if ticks_per_second == 0 {
        return Err(ApicError::CalibrationFailed);
    }

    Ok(())
}

/// Local APIC Timerを初期化
///
/// # Arguments
/// * `frequency_hz` - タイマー割り込みの周波数 (Hz)
///
/// # Errors
/// * `ApicError::NotCalibrated` - タイマーがキャリブレーションされていない場合
/// * `ApicError::InvalidFrequency` - 周波数が0の場合
pub fn init_timer(frequency_hz: u32) -> Result<(), ApicError> {
    if frequency_hz == 0 {
        return Err(ApicError::InvalidFrequency);
    }

    // SAFETY: APICレジスタへのアクセスは、enable_apic()でAPICが有効化され、
    // calibrate_timer()でキャリブレーションが完了した後に行われる。
    // すべてのレジスタオフセットはIntel SDMで定義された有効な値。
    unsafe {
        // キャリブレーション結果を取得
        let apic_freq = APIC_TIMER_FREQUENCY.load(Ordering::SeqCst);
        if apic_freq == 0 {
            return Err(ApicError::NotCalibrated);
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

        Ok(())
    }
}

/// End of Interrupt (EOI) を送信
/// 割り込みハンドラの最後に呼び出す必要があります
pub fn send_eoi() {
    // SAFETY: EOIレジスタへの書き込みは、APICが有効化されていれば常に安全。
    // この関数は割り込みハンドラから呼ばれ、APICは初期化時に有効化済み。
    unsafe {
        write_apic_register(registers::EOI, 0);
    }
}

/// レガシーPIC（8259 PIC）を無効化
/// APICを使う場合、古いPICとの競合を避けるために無効化が必要
fn disable_legacy_pic() {
    // SAFETY: I/Oポート0x21と0xA1は8259 PICのIMRポートとして定義されている。
    // 0xFFを書き込むことで全ての割り込みをマスクする標準的な操作。
    // Ring 0で実行されることが前提。
    unsafe {
        // マスターPICとスレーブPICの両方のIMR（Interrupt Mask Register）に
        // 0xFFを書き込んで、すべての割り込みをマスク
        asm!(
            "mov al, 0xFF",
            "out 0x21, al",  // マスターPIC IMR
            "out 0xA1, al",  // スレーブPIC IMR
            out("al") _,
            options(nomem, nostack, preserves_flags)
        );
    }
}

/// Local APICを初期化
pub fn init() {
    // まずレガシーPICを無効化
    disable_legacy_pic();
    enable_apic();
    // タイマーは別途 init_timer() で初期化
}
