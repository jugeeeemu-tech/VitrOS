//! USB サブシステム

pub mod xhci;

use crate::{info, io::without_interrupts};
use lazy_static::lazy_static;
use spin::Mutex;

lazy_static! {
    static ref XHCI_CONTROLLER: Mutex<Option<xhci::XhciController>> = Mutex::new(None);
}

pub fn init() {
    info!("Initializing USB subsystem...");

    match xhci::init() {
        Ok(controller) => {
            without_interrupts(|| {
                *XHCI_CONTROLLER.lock() = Some(controller);
            });
            info!("USB: xHCI controller initialized");
        }
        Err(xhci::XhciError::ControllerNotFound) => {
            info!("USB: No xHCI controller found");
        }
        Err(e) => {
            info!("USB: xHCI controller initialization failed: {:?}", e);
        }
    }
}

pub fn with_xhci_controller<R>(f: impl FnOnce(&mut xhci::XhciController) -> R) -> Option<R> {
    without_interrupts(|| {
        let mut guard = XHCI_CONTROLLER.lock();
        let controller = guard.as_mut()?;
        Some(f(controller))
    })
}

pub fn with_xhci_controller_irq<R>(f: impl FnOnce(&mut xhci::XhciController) -> R) -> Option<R> {
    let mut guard = XHCI_CONTROLLER.lock();
    let controller = guard.as_mut()?;
    Some(f(controller))
}
