// x86_64 I/Oポート操作
use core::arch::asm;

// I/Oポートに1バイト書き込み
#[inline]
pub unsafe fn port_write_u8(port: u16, value: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
    }
}

// I/Oポートから1バイト読み込み
#[inline]
pub unsafe fn port_read_u8(port: u16) -> u8 {
    let value: u8;
    unsafe {
        asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack));
    }
    value
}

// I/Oポートに2バイト書き込み
#[inline]
pub unsafe fn port_write_u16(port: u16, value: u16) {
    unsafe {
        asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack));
    }
}

// I/Oポートから2バイト読み込み
#[inline]
pub unsafe fn port_read_u16(port: u16) -> u16 {
    let value: u16;
    unsafe {
        asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack));
    }
    value
}

// I/Oポートに4バイト書き込み
#[inline]
pub unsafe fn port_write_u32(port: u16, value: u32) {
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack));
    }
}

// I/Oポートから4バイト読み込み
#[inline]
pub unsafe fn port_read_u32(port: u16) -> u32 {
    let value: u32;
    unsafe {
        asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack));
    }
    value
}
