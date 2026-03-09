//! xHCI (USB 3.x) コントローラドライバ

pub mod dma;
pub mod registers;

use core::ptr::{addr_of, read_volatile};

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
    UnsupportedPageSize,
}

pub struct XhciController {
    pub device: PciDevice,
    pub mmio_phys_base: u64,
    pub mmio_virt_base: u64,
    pub mmio_size: u64,
    pub dma: dma::XhciDmaProfile,
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

    let cap_regs = mmio_virt_base as *const registers::CapabilityRegisters;
    let caplength = unsafe {
        // SAFETY: xHCI MMIO space is mapped and CAPLENGTH is a read-only register field.
        read_volatile(addr_of!((*cap_regs).caplength))
    };
    let hccparams1 = unsafe {
        // SAFETY: xHCI MMIO space is mapped and HCCPARAMS1 is a read-only register field.
        read_volatile(addr_of!((*cap_regs).hccparams1))
    };

    let op_regs = (mmio_virt_base + u64::from(caplength)) as *const registers::OperationalRegisters;
    let pagesize = unsafe {
        // SAFETY: Operational registers begin at MMIO base + CAPLENGTH and PAGESIZE is read-only here.
        read_volatile(addr_of!((*op_regs).pagesize))
    };
    if !registers::pagesize::supports_4k(pagesize) {
        return Err(XhciError::UnsupportedPageSize);
    }

    let dma = dma::XhciDmaProfile::new(registers::hccparams1::ac64(hccparams1), paging::PAGE_SIZE);

    Ok(XhciController {
        device,
        mmio_phys_base,
        mmio_virt_base,
        mmio_size,
        dma,
    })
}
