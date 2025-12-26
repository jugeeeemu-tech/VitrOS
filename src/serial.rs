// シリアルポート（COM1）ドライバ
use crate::io::{port_read_u8, port_write_u8};
use core::fmt;

const COM1: u16 = 0x3F8;

pub struct SerialPort {
    base: u16,
}

impl SerialPort {
    pub const fn new(base: u16) -> Self {
        Self { base }
    }

    // シリアルポートの初期化
    pub fn init(&self) {
        unsafe {
            port_write_u8(self.base + 1, 0x00); // 割り込み無効化
            port_write_u8(self.base + 3, 0x80); // DLABを有効化
            port_write_u8(self.base + 0, 0x03); // ボーレート: 38400 bps (Lo)
            port_write_u8(self.base + 1, 0x00); // ボーレート: 38400 bps (Hi)
            port_write_u8(self.base + 3, 0x03); // 8ビット、パリティなし、1ストップビット
            port_write_u8(self.base + 2, 0xC7); // FIFOを有効化、14バイトしきい値
            port_write_u8(self.base + 4, 0x0B); // IRQ有効、RTS/DSR設定
        }
    }

    // 送信準備完了を待つ
    fn wait_for_transmit(&self) {
        unsafe {
            while (port_read_u8(self.base + 5) & 0x20) == 0 {
                core::hint::spin_loop();
            }
        }
    }

    // 1バイト送信
    pub fn write_byte(&self, byte: u8) {
        self.wait_for_transmit();
        unsafe {
            port_write_u8(self.base, byte);
        }
    }

    // 文字列送信
    pub fn write_str(&self, s: &str) {
        for byte in s.bytes() {
            self.write_byte(byte);
        }
    }
}

// SerialPort に対して fmt::Write を実装
impl fmt::Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        SerialPort::write_str(self, s);
        Ok(())
    }
}

// シリアルポートを初期化（1回だけ呼ぶ）
pub fn init() {
    SerialPort::new(COM1).init();
}

// print系マクロの内部実装
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use fmt::Write;
    let mut serial = SerialPort::new(COM1);
    let _ = serial.write_fmt(args);
}

// print!マクロ
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        $crate::serial::_print(format_args!($($arg)*));
    }};
}

// println!マクロ
#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let mut serial = $crate::serial::SerialPort::new(0x3F8);
        let _ = writeln!(serial, $($arg)*);
    }};
}

// info!マクロ（白色表示）
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let mut serial = $crate::serial::SerialPort::new(0x3F8);
        let _ = writeln!(serial, "[INFO] {}", format_args!($($arg)*));
    }};
}

// warn!マクロ（黄色表示）
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let mut serial = $crate::serial::SerialPort::new(0x3F8);
        let _ = writeln!(serial, "\x1b[33m[WARN]\x1b[0m {}", format_args!($($arg)*));
    }};
}

// error!マクロ（赤色表示）
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let mut serial = $crate::serial::SerialPort::new(0x3F8);
        let _ = writeln!(serial, "\x1b[31m[ERROR]\x1b[0m {}", format_args!($($arg)*));
    }};
}
