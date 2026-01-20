//! PIT (Programmable Interval Timer) 実装
//!
//! 8254 PITチップを使用してタイミング制御を行います。
//! 主にAPIC Timerのキャリブレーションに使用します。

use crate::io::{port_read_u8, port_write_u8};

/// PIT周波数（Hz）
const PIT_FREQUENCY: u32 = 1193182;

/// ポーリングループの最大イテレーション数（タイムアウト用）
const MAX_POLL_ITERATIONS: u32 = 100_000;

/// PITのI/Oポート
mod ports {
    /// Channel 0 data port (read/write)
    pub const CHANNEL_0: u16 = 0x40;
    /// Channel 2 data port (read/write)
    #[allow(dead_code)]
    pub const CHANNEL_2: u16 = 0x42;
    /// Mode/Command register (write only)
    pub const COMMAND: u16 = 0x43;
}

/// PITを使って指定ミリ秒待機
///
/// # Arguments
/// * `ms` - 待機時間（ミリ秒）
pub fn sleep_ms(ms: u32) {
    // 1msずつ待機することで精度を上げる
    for _ in 0..ms {
        sleep_1ms();
    }
}

/// 1ミリ秒待機（内部関数）
fn sleep_1ms() {
    unsafe {
        // 1ms = 1193 カウント（PIT_FREQUENCY / 1000）
        let count: u16 = (PIT_FREQUENCY / 1000) as u16;

        // Channel 0, Mode 0 (Interrupt on terminal count), binary counter
        // Command: 0x30 = 0011 0000
        // - Channel 0 (bits 6-7: 00)
        // - Access mode: lobyte/hibyte (bits 4-5: 11)
        // - Operating mode 0: interrupt on terminal count (bits 1-3: 000)
        // - Binary counter (bit 0: 0)
        port_write_u8(ports::COMMAND, 0x30);

        // カウント値を設定（下位バイト、上位バイト）
        port_write_u8(ports::CHANNEL_0, (count & 0xFF) as u8);
        port_write_u8(ports::CHANNEL_0, ((count >> 8) & 0xFF) as u8);

        // 初回の読み取り
        port_write_u8(ports::COMMAND, 0x00);
        let mut last_count = read_current_count();

        // カウントダウンが完了するまで待つ（タイムアウト付き）
        let mut iterations = 0u32;
        loop {
            if iterations >= MAX_POLL_ITERATIONS {
                break; // タイムアウト
            }
            iterations += 1;

            port_write_u8(ports::COMMAND, 0x00); // latch command
            let current_count = read_current_count();

            // Mode 0: カウンタが0になるか、再ロードされて大きくなったら終了
            if current_count == 0 || current_count > last_count {
                break;
            }
            last_count = current_count;
        }
    }
}

/// 現在のPITカウント値を読み取る
unsafe fn read_current_count() -> u16 {
    unsafe {
        let low = port_read_u8(ports::CHANNEL_0) as u16;
        let high = port_read_u8(ports::CHANNEL_0) as u16;
        (high << 8) | low
    }
}

/// PITのOne-shot modeで指定カウント後にシグナルを送る
///
/// # Arguments
/// * `count` - カウント数
#[allow(dead_code)]
pub fn oneshot(count: u16) {
    unsafe {
        // Channel 0, Interrupt on terminal count (mode 0), binary counter
        // Command: 0x30 = 0011 0000
        port_write_u8(ports::COMMAND, 0x30);

        // カウント値を設定
        port_write_u8(ports::CHANNEL_0, (count & 0xFF) as u8);
        port_write_u8(ports::CHANNEL_0, ((count >> 8) & 0xFF) as u8);
    }
}

/// PITでマイクロ秒単位の遅延を実現
///
/// # Arguments
/// * `us` - 待機時間（マイクロ秒）
#[allow(dead_code)]
pub fn udelay(us: u32) {
    // 1マイクロ秒 = PIT_FREQUENCY / 1_000_000 カウント
    let count = ((PIT_FREQUENCY as u64 * us as u64) / 1_000_000) as u16;

    unsafe {
        // One-shot mode
        port_write_u8(ports::COMMAND, 0x30);
        port_write_u8(ports::CHANNEL_0, (count & 0xFF) as u8);
        port_write_u8(ports::CHANNEL_0, ((count >> 8) & 0xFF) as u8);

        // カウントが0になるまで待つ（タイムアウト付き）
        let mut iterations = 0u32;
        loop {
            if iterations >= MAX_POLL_ITERATIONS {
                break; // タイムアウト
            }
            iterations += 1;

            port_write_u8(ports::COMMAND, 0x00); // latch
            let low = port_read_u8(ports::CHANNEL_0) as u16;
            let high = port_read_u8(ports::CHANNEL_0) as u16;
            let current = (high << 8) | low;

            if current == 0 {
                break;
            }
        }
    }
}

// ============================================================================
// TimerDevice trait 実装
// ============================================================================

use crate::timer_device::TimerDevice;

/// PIT タイマーデバイス
pub struct Pit;

impl TimerDevice for Pit {
    fn is_available(&self) -> bool {
        true
    }

    fn frequency(&self) -> u64 {
        PIT_FREQUENCY as u64
    }

    fn delay_ns(&self, ns: u64) {
        let us = ((ns + 999) / 1_000) as u32;
        if us > 0 {
            udelay(us);
        }
    }

    fn delay_ms(&self, ms: u64) {
        sleep_ms(ms as u32);
    }
}

/// グローバルPITインスタンス
pub static PIT: Pit = Pit;
