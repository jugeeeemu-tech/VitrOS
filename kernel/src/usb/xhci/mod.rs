//! xHCI (USB 3.x) コントローラドライバ

use crate::info;
use crate::paging;
use crate::pci::{self, PciDevice};

const XHCI_CLASS_CODE: u8 = 0x0C; // Serial Bus Controller
const XHCI_SUBCLASS: u8 = 0x03; // USB Controller
const XHCI_PROG_IF: u8 = 0x30; // xHCI

#[derive(Debug)]
pub enum XhciError {
    ControllerNotFound,
    InvalidBar,
    BarNotMemory,
    MmioMappingFailed,
}

pub struct XhciController {
    pub device: PciDevice,
    pub mmio_phys_base: u64,
    pub mmio_virt_base: u64,
    pub mmio_size: u64,
}

fn find_xhci_controller() -> Option<PciDevice> {
    pci::find_device(|dev| {
        dev.class_code == XHCI_CLASS_CODE
            && dev.subclass == XHCI_SUBCLASS
            && dev.prog_if == XHCI_PROG_IF
    })
}

pub fn init() -> Result<XhciController, XhciError> {
    let device = find_xhci_controller().ok_or(XhciError::ControllerNotFound)?;

    info!(
        "[xHCI] Controller found: [{:02X}:{:02X}.{}] {:04X}:{:04X}",
        device.bus, device.device, device.function, device.vendor_id, device.device_id
    );

    let bar0 = device.read_bar(0).ok_or(XhciError::InvalidBar)?;
    if !bar0.is_memory {
        return Err(XhciError::BarNotMemory);
    }

    let mmio_phys_base = bar0.base_address;
    let mmio_size = 64 * 1024; // 64KB

    info!(
        "[xHCI] BAR0: phys=0x{:X}, size=0x{:X}, 64bit={}, prefetchable={}",
        mmio_phys_base, mmio_size, bar0.is_64bit, bar0.prefetchable
    );

    let mmio_virt_base =
        paging::map_mmio(mmio_phys_base, mmio_size).map_err(|_| XhciError::MmioMappingFailed)?;

    info!(
        "[xHCI] MMIO mapped: phys=0x{:X} -> virt=0x{:X}",
        mmio_phys_base, mmio_virt_base
    );

    Ok(XhciController {
        device,
        mmio_phys_base,
        mmio_virt_base,
        mmio_size,
    })
}
