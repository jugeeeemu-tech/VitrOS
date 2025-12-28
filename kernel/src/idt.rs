//! Interrupt Descriptor Table (IDT) 実装
//!
//! x86_64アーキテクチャの割り込み処理を管理するIDTを実装します。

use core::arch::asm;
use lazy_static::lazy_static;
use spin::Mutex;
use je4os_common::{println, info};

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

// =============================================================================
// 例外ハンドラ実装
// =============================================================================

/// Divide Error (#DE, ベクタ0) ハンドラ
/// ゼロ除算または除算結果がオーバーフローした場合に発生
#[unsafe(naked)]
extern "C" fn divide_error_handler() {
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
        "call {handler_inner}",

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

        handler_inner = sym divide_error_handler_inner,
    )
}

extern "C" fn divide_error_handler_inner() {
    println!("\n\n");
    println!("========================================");
    println!("EXCEPTION: Divide Error (#DE)");
    println!("========================================");
    println!("ゼロ除算または除算結果のオーバーフローが発生しました。");
    println!("");

    // 停止
    loop {
        unsafe { asm!("hlt") };
    }
}

/// Debug Exception (#DB, ベクタ1) ハンドラ
/// デバッグレジスタによるブレークポイントやシングルステップで発生
#[unsafe(naked)]
extern "C" fn debug_exception_handler() {
    core::arch::naked_asm!(
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",

        "call {handler_inner}",

        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rax",

        "iretq",

        handler_inner = sym debug_exception_handler_inner,
    )
}

extern "C" fn debug_exception_handler_inner() {
    println!("\n\n");
    println!("========================================");
    println!("EXCEPTION: Debug Exception (#DB)");
    println!("========================================");
    println!("デバッグ例外が発生しました。");
    println!("");

    loop {
        unsafe { asm!("hlt") };
    }
}

/// Breakpoint (#BP, ベクタ3) ハンドラ
/// INT3命令（0xCC）によって発生
#[unsafe(naked)]
extern "C" fn breakpoint_handler() {
    core::arch::naked_asm!(
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",

        "call {handler_inner}",

        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rax",

        "iretq",

        handler_inner = sym breakpoint_handler_inner,
    )
}

extern "C" fn breakpoint_handler_inner() {
    println!("\n\n");
    println!("========================================");
    println!("EXCEPTION: Breakpoint (#BP)");
    println!("========================================");
    println!("ブレークポイント例外が発生しました。");
    println!("");

    // ブレークポイントは通常、続行可能
    println!("デバッガが接続されていれば、ここで制御が移ります。");
}

/// Invalid Opcode (#UD, ベクタ6) ハンドラ
/// 無効な命令やサポートされていない命令を実行しようとした場合に発生
#[unsafe(naked)]
extern "C" fn invalid_opcode_handler() {
    core::arch::naked_asm!(
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",

        "call {handler_inner}",

        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rax",

        "iretq",

        handler_inner = sym invalid_opcode_handler_inner,
    )
}

extern "C" fn invalid_opcode_handler_inner() {
    println!("\n\n");
    println!("========================================");
    println!("EXCEPTION: Invalid Opcode (#UD)");
    println!("========================================");
    println!("無効な命令を実行しようとしました。");
    println!("");

    loop {
        unsafe { asm!("hlt") };
    }
}

// =============================================================================
// エラーコード付き例外ハンドラ実装
// =============================================================================

/// Double Fault (#DF, ベクタ8) ハンドラ
/// 例外ハンドラ内で別の例外が発生した場合に発生（重大なエラー）
#[unsafe(naked)]
extern "C" fn double_fault_handler() {
    core::arch::naked_asm!(
        // エラーコードをRDIレジスタに移動（System V ABIの第1引数）
        "pop rdi",

        // レジスタを保存
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        // rdi は既にエラーコードが入っているので保存しない
        "push r8",
        "push r9",
        "push r10",
        "push r11",

        // 実際のハンドラを呼び出し（RDIにエラーコード）
        "call {handler_inner}",

        // レジスタを復元（復帰しないが形式上）
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rax",

        // Double Faultは通常復帰できないが、念のためiretq
        "iretq",

        handler_inner = sym double_fault_handler_inner,
    )
}

extern "C" fn double_fault_handler_inner(error_code: u64) {
    println!("\n\n");
    println!("========================================");
    println!("FATAL: Double Fault (#DF)");
    println!("========================================");
    println!("例外ハンドラ内で別の例外が発生しました。");
    println!("エラーコード: 0x{:X}", error_code);
    println!("");
    println!("システムは重大なエラー状態にあります。");
    println!("");

    // 永久停止
    loop {
        unsafe { asm!("cli; hlt") };
    }
}

/// General Protection Fault (#GP, ベクタ13) ハンドラ
/// セグメント違反、特権レベル違反、無効なメモリアクセスなどで発生
#[unsafe(naked)]
extern "C" fn general_protection_fault_handler() {
    core::arch::naked_asm!(
        // エラーコードをRDIレジスタに移動
        "pop rdi",

        // レジスタを保存
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",

        // 実際のハンドラを呼び出し
        "call {handler_inner}",

        // レジスタを復元
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rax",

        "iretq",

        handler_inner = sym general_protection_fault_handler_inner,
    )
}

extern "C" fn general_protection_fault_handler_inner(error_code: u64) {
    println!("\n\n");
    println!("========================================");
    println!("EXCEPTION: General Protection Fault (#GP)");
    println!("========================================");
    println!("セグメント違反または特権レベル違反が発生しました。");
    println!("エラーコード: 0x{:X}", error_code);

    // エラーコードの詳細を解析
    if error_code != 0 {
        let external = (error_code & 0x01) != 0;
        let table = (error_code >> 1) & 0x03;
        let index = (error_code >> 3) & 0x1FFF;

        println!("");
        println!("エラーコード詳細:");
        println!("  - External: {}", if external { "Yes" } else { "No" });
        println!("  - Table: {}", match table {
            0 => "GDT",
            1 => "IDT",
            2 => "LDT",
            3 => "IDT",
            _ => "Unknown",
        });
        println!("  - Index: 0x{:X}", index);
    }
    println!("");

    loop {
        unsafe { asm!("hlt") };
    }
}

/// Page Fault (#PF, ベクタ14) ハンドラ
/// 無効なページアクセス、権限違反、ページ未マップなどで発生
#[unsafe(naked)]
extern "C" fn page_fault_handler() {
    core::arch::naked_asm!(
        // エラーコードをRDIレジスタに移動
        "pop rdi",

        // レジスタを保存
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",

        // 実際のハンドラを呼び出し
        "call {handler_inner}",

        // レジスタを復元
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rax",

        "iretq",

        handler_inner = sym page_fault_handler_inner,
    )
}

extern "C" fn page_fault_handler_inner(error_code: u64) {
    // CR2レジスタから違反アドレスを取得
    let fault_addr: u64;
    unsafe {
        asm!("mov {}, cr2", out(reg) fault_addr, options(nomem, nostack));
    }

    println!("\n\n");
    println!("========================================");
    println!("EXCEPTION: Page Fault (#PF)");
    println!("========================================");
    println!("無効なメモリアクセスが発生しました。");
    println!("違反アドレス: 0x{:016X}", fault_addr);
    println!("エラーコード: 0x{:X}", error_code);

    // エラーコードの詳細を解析
    println!("");
    println!("エラーコード詳細:");
    println!("  - Present: {}", if error_code & 0x01 != 0 { "Yes (権限違反)" } else { "No (ページ未マップ)" });
    println!("  - Write: {}", if error_code & 0x02 != 0 { "Yes (書き込み)" } else { "No (読み込み)" });
    println!("  - User: {}", if error_code & 0x04 != 0 { "Yes (ユーザーモード)" } else { "No (カーネルモード)" });
    println!("  - Reserved: {}", if error_code & 0x08 != 0 { "Yes" } else { "No" });
    println!("  - Instruction: {}", if error_code & 0x10 != 0 { "Yes (命令フェッチ)" } else { "No (データアクセス)" });
    println!("");

    loop {
        unsafe { asm!("hlt") };
    }
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
    // 例外ハンドラを登録
    set_idt_entry(0, divide_error_handler as usize);        // #DE: Divide Error
    set_idt_entry(1, debug_exception_handler as usize);     // #DB: Debug Exception
    set_idt_entry(3, breakpoint_handler as usize);          // #BP: Breakpoint
    set_idt_entry(6, invalid_opcode_handler as usize);      // #UD: Invalid Opcode
    set_idt_entry(8, double_fault_handler as usize);        // #DF: Double Fault
    set_idt_entry(13, general_protection_fault_handler as usize); // #GP: General Protection Fault
    set_idt_entry(14, page_fault_handler as usize);         // #PF: Page Fault

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

    info!("IDT initialized with exception handlers");
}
