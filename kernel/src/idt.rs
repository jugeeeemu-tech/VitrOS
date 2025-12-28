//! Interrupt Descriptor Table (IDT) 実装
//!
//! x86_64アーキテクチャの割り込み処理を管理するIDTを実装します。

use core::arch::asm;
use lazy_static::lazy_static;
use spin::Mutex;

use crate::apic;
use crate::gdt;
use crate::timer;
use crate::paging::KERNEL_VIRTUAL_BASE;

/// 現在高位アドレス空間で実行されているかチェック
fn is_higher_half() -> bool {
    let rip: u64;
    unsafe {
        asm!("lea {}, [rip]", out(reg) rip, options(nomem, nostack));
    }
    rip >= KERNEL_VIRTUAL_BASE
}

/// IDTエントリ（割り込みゲートディスクリプタ）
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct IdtEntry {
    offset_low: u16,     // オフセット下位16ビット
    selector: u16,       // コードセグメントセレクタ
    ist: u8,             // Interrupt Stack Table (0 = 使用しない)
    attributes: u8,      // タイプとアトリビュート
    offset_middle: u16,  // オフセット中位16ビット
    offset_high: u32,    // オフセット上位32ビット
    reserved: u32,       // 予約領域（0）
}

impl IdtEntry {
    /// 空のIDTエントリを作成
    const fn null() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            attributes: 0,
            offset_middle: 0,
            offset_high: 0,
            reserved: 0,
        }
    }

    /// 割り込みゲートを作成
    ///
    /// # Arguments
    /// * `handler` - 割り込みハンドラ関数のアドレス
    /// * `selector` - コードセグメントセレクタ（通常はカーネルコードセグメント）
    /// * `dpl` - Descriptor Privilege Level (0 = カーネル, 3 = ユーザー)
    const fn new(handler: usize, selector: u16, dpl: u8) -> Self {
        Self {
            offset_low: (handler & 0xFFFF) as u16,
            selector,
            ist: 0,
            // Present (bit 7) | DPL (bits 5-6) | Gate Type (0xE = Interrupt Gate)
            attributes: 0x80 | ((dpl & 0b11) << 5) | 0x0E,
            offset_middle: ((handler >> 16) & 0xFFFF) as u16,
            offset_high: ((handler >> 32) & 0xFFFFFFFF) as u32,
            reserved: 0,
        }
    }
}

/// IDT（Interrupt Descriptor Table）
/// x86_64では最大256個の割り込みベクタ
#[repr(C, align(16))]
struct Idt {
    entries: [IdtEntry; 256],
}

impl Idt {
    /// 新しいIDTを作成（すべてのエントリを空で初期化）
    const fn new() -> Self {
        Self {
            entries: [IdtEntry::null(); 256],
        }
    }
}

/// IDTR（IDT Register）用の構造体
#[repr(C, packed)]
struct Idtr {
    limit: u16,
    base: u64,
}

// グローバルIDTインスタンス
lazy_static! {
    static ref IDT: Mutex<Idt> = Mutex::new(Idt::new());
}

/// デフォルト割り込みハンドラ（何もしない）
#[allow(dead_code)]
#[unsafe(naked)]
extern "C" fn default_handler() {
    core::arch::naked_asm!(
        "iretq"
    )
}

/// タイマー割り込みハンドラ
#[unsafe(naked)]
extern "C" fn timer_interrupt_handler() {
    core::arch::naked_asm!(
        // レジスタを保存
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",

        // 実際のハンドラを呼び出し
        "call {timer_handler_inner}",

        // レジスタを復元
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rax",

        // 割り込みから復帰
        "iretq",

        timer_handler_inner = sym timer_handler_inner,
    )
}

/// タイマー割り込みハンドラの実装
extern "C" fn timer_handler_inner() {
    // tick数をインクリメント
    let tick = timer::increment_tick();

    // 期限切れタイマーをチェック（ペンディングキューに移動するだけ）
    timer::check_timers();

    // EOI (End of Interrupt) を送信
    apic::send_eoi();
}

/// IDTエントリを設定
fn set_idt_entry(vector: u8, handler: usize) {
    let mut idt = IDT.lock();

    // カーネルが高位アドレスでリンクされているため、ハンドラアドレスは既に高位
    idt.entries[vector as usize] = IdtEntry::new(
        handler,
        gdt::selector::KERNEL_CODE,
        0, // DPL = 0 (カーネルレベル)
    );
}

/// IDTを初期化してロード
pub fn init() {
    // タイマー割り込みハンドラを登録
    set_idt_entry(apic::TIMER_INTERRUPT_VECTOR, timer_interrupt_handler as usize);

    unsafe {
        // IDTのアドレスを取得（カーネルが高位アドレスでリンクされているため既に高位）
        let idt = IDT.lock();
        let idt_addr = &*idt as *const Idt as u64;

        let idtr = Idtr {
            limit: (core::mem::size_of::<Idt>() - 1) as u16,
            base: idt_addr,
        };

        // LIDT命令でIDTをロード
        asm!(
            "lidt [{}]",
            in(reg) &idtr,
            options(readonly, nostack, preserves_flags)
        );
    }
}
