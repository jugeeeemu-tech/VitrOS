//! PIT (Programmable Interval Timer) 実装
//!
//! 8254 PITチップを使用してタイミング制御を行います。
//! 主にAPIC Timerのキャリブレーションに使用します。

use core::arch::asm;

/// PIT周波数（Hz）
const PIT_FREQUENCY: u32 = 1193182;

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

/// I/Oポートへの書き込み
unsafe fn outb(port: u16, value: u8) {
    unsafe {
        asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nostack, preserves_flags)
        );
    }
}

/// I/Oポートからの読み込み
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nostack, preserves_flags)
        );
    }
    value
}

/// PITを使って指定ミリ秒待機
///
/// # Arguments
/// * `ms` - 待機時間（ミリ秒）
pub fn sleep_ms(ms: u32) {
    unsafe {
        // PITのカウント値を計算（ミリ秒 -> カウント数）
        let count = (PIT_FREQUENCY * ms / 1000) as u16;

        // Channel 0, Rate Generator mode (mode 2), binary counter
        // Command: 0x34 = 0011 0100
        // - Channel 0 (bits 6-7: 00)
        // - Access mode: lobyte/hibyte (bits 4-5: 11)
        // - Operating mode 2: rate generator (bits 1-3: 010)
        // - Binary counter (bit 0: 0)
        outb(ports::COMMAND, 0x34);

        // カウント値を設定（下位バイト、上位バイト）
        outb(ports::CHANNEL_0, (count & 0xFF) as u8);
        outb(ports::CHANNEL_0, ((count >> 8) & 0xFF) as u8);

        // カウントが0になるまで待つ
        // 現在のカウント値を読み取る（latch command）
        outb(ports::COMMAND, 0x00);

        // 初回の読み取り
        let mut last_count = read_current_count();

        // カウントダウンが完了するまで待つ
        loop {
            outb(ports::COMMAND, 0x00); // latch command
            let current_count = read_current_count();

            // カウントが一巡したら（0xFFFF -> 小さい値）終了
            if current_count > last_count {
                break;
            }
            last_count = current_count;
        }
    }
}

/// 現在のPITカウント値を読み取る
unsafe fn read_current_count() -> u16 {
    unsafe {
        let low = inb(ports::CHANNEL_0) as u16;
        let high = inb(ports::CHANNEL_0) as u16;
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
        outb(ports::COMMAND, 0x30);

        // カウント値を設定
        outb(ports::CHANNEL_0, (count & 0xFF) as u8);
        outb(ports::CHANNEL_0, ((count >> 8) & 0xFF) as u8);
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
        outb(ports::COMMAND, 0x30);
        outb(ports::CHANNEL_0, (count & 0xFF) as u8);
        outb(ports::CHANNEL_0, ((count >> 8) & 0xFF) as u8);

        // カウントが0になるまで待つ
        loop {
            outb(ports::COMMAND, 0x00); // latch
            let low = inb(ports::CHANNEL_0) as u16;
            let high = inb(ports::CHANNEL_0) as u16;
            let current = (high << 8) | low;

            if current == 0 {
                break;
            }
        }
    }
}
