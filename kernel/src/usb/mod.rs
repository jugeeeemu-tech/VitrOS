//! USB サブシステム

pub mod xhci;

use crate::info;
use lazy_static::lazy_static;
use spin::Mutex;

lazy_static! {
    static ref XHCI_CONTROLLER: Mutex<Option<xhci::XhciController>> = Mutex::new(None);
}

pub fn init() {
    info!("Initializing USB subsystem...");

    match xhci::init() {
        Ok(controller) => {
            *XHCI_CONTROLLER.lock() = Some(controller);
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
