//! xHCI interrupt registration and handler.

use crate::{apic, idt, usb};

pub const XHCI_INTERRUPT_VECTOR: u8 = 0x50;

#[unsafe(naked)]
extern "C" fn xhci_interrupt_handler() {
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
        handler_inner = sym xhci_interrupt_handler_inner,
    )
}

extern "C" fn xhci_interrupt_handler_inner() {
    let _ = usb::with_xhci_controller_irq(|controller| controller.handle_interrupt());
    apic::send_eoi();
}

pub fn register_interrupt() {
    idt::set_idt_entry(
        XHCI_INTERRUPT_VECTOR,
        xhci_interrupt_handler as *const () as usize,
    );
}
