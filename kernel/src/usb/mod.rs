//! USB サブシステム

pub mod xhci;

use crate::info;

pub fn init() {
    info!("Initializing USB subsystem...");

    match xhci::init() {
        Ok(_) => info!("USB: xHCI controller initialized"),
        Err(e) => info!("USB: No xHCI controller found: {:?}", e),
    }
}
