//! xHCI (USB 3.x) コントローラドライバ

extern crate alloc;

pub mod controller;
pub mod context;
pub mod dma;
pub mod device;
pub mod event;
pub mod interrupt;
pub mod memory;
pub mod registers;
pub mod ring;
pub mod trb;

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::ptr::{addr_of, read_volatile};

use crate::pci::{self, PciDevice};
use crate::{info, msi, paging};
use controller::XhciControllerResources;
use memory::{
    EventRingSegmentDescriptor, EventRingSegmentTable, ScratchpadSet, XhciControlMemoryConfig,
};
use msi::MsiError;
use ring::{ConsumerRing, ProducerRing};

const XHCI_CLASS_CODE: u8 = 0x0C; // Serial Bus Controller
const XHCI_SUBCLASS: u8 = 0x03; // USB Controller
const XHCI_PROG_IF: u8 = 0x30; // xHCI
const XHCI_MMIO_SIZE: u64 = 64 * 1024;
const COMMAND_RING_TRB_COUNT: usize = 256;
const EVENT_RING_TRB_COUNT: usize = 256;
const ERST_SEGMENT_COUNT: usize = 1;

#[derive(Debug)]
pub enum XhciError {
    ControllerNotFound,
    InvalidBar,
    BarNotMemory,
    MmioMappingFailed,
    Interrupt(MsiError),
    UnsupportedPageSize { raw: u32 },
    Memory(memory::XhciMemoryError),
    Ring(ring::RingError),
    Init(controller::XhciControllerInitError),
}

impl From<memory::XhciMemoryError> for XhciError {
    fn from(err: memory::XhciMemoryError) -> Self {
        Self::Memory(err)
    }
}

impl From<ring::RingError> for XhciError {
    fn from(err: ring::RingError) -> Self {
        Self::Ring(err)
    }
}

impl From<MsiError> for XhciError {
    fn from(err: MsiError) -> Self {
        Self::Interrupt(err)
    }
}

impl From<controller::XhciControllerInitError> for XhciError {
    fn from(err: controller::XhciControllerInitError) -> Self {
        Self::Init(err)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InterruptMode {
    Msi,
    Msix,
}

impl InterruptMode {
    fn disable(self, device: &PciDevice) {
        let _ = match self {
            Self::Msi => msi::disable_msi(device),
            Self::Msix => msi::disable_msix(device),
        };
    }
}

pub struct XhciController {
    pub device: PciDevice,
    pub mmio_phys_base: u64,
    pub mmio_virt_base: u64,
    pub mmio_size: u64,
    op_virt_base: u64,
    pub runtime_virt_base: u64,
    pub doorbell_virt_base: u64,
    hcsparams1: u32,
    hcsparams2: u32,
    hccparams1: u32,
    page_size: usize,
    pub dma: dma::XhciDmaProfile,
    pending_port_changes: VecDeque<u8>,
    pending_command_completions: VecDeque<device::CommandCompletionRecord>,
    pending_transfer_events: VecDeque<device::TransferCompletionRecord>,
    event_overflowed: bool,
    port_states: Vec<device::PortState>,
    slots: Vec<Option<device::SlotRuntime>>,
    resources: Option<XhciControllerResources>,
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
    let mmio_size = XHCI_MMIO_SIZE;

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
    let hcsparams1 = unsafe {
        // SAFETY: xHCI MMIO space is mapped and HCSPARAMS1 is a read-only register field.
        read_volatile(addr_of!((*cap_regs).hcsparams1))
    };
    let hcsparams2 = unsafe {
        // SAFETY: xHCI MMIO space is mapped and HCSPARAMS2 is a read-only register field.
        read_volatile(addr_of!((*cap_regs).hcsparams2))
    };
    let hccparams1 = unsafe {
        // SAFETY: xHCI MMIO space is mapped and HCCPARAMS1 is a read-only register field.
        read_volatile(addr_of!((*cap_regs).hccparams1))
    };
    let dboff = unsafe {
        // SAFETY: xHCI MMIO space is mapped and DBOFF is a read-only register field.
        read_volatile(addr_of!((*cap_regs).dboff))
    };
    let rtsoff = unsafe {
        // SAFETY: xHCI MMIO space is mapped and RTSOFF is a read-only register field.
        read_volatile(addr_of!((*cap_regs).rtsoff))
    };

    let op_virt_base = mmio_virt_base + u64::from(caplength);
    let op_regs = op_virt_base as *const registers::OperationalRegisters;
    let pagesize_raw = unsafe {
        // SAFETY: Operational registers begin at MMIO base + CAPLENGTH and PAGESIZE is read-only here.
        read_volatile(addr_of!((*op_regs).pagesize))
    };
    let page_size = registers::pagesize::smallest_supported_page_size(pagesize_raw)
        .ok_or(XhciError::UnsupportedPageSize { raw: pagesize_raw })?;

    let dma = dma::XhciDmaProfile::new(registers::hccparams1::ac64(hccparams1), page_size);
    let runtime_virt_base = mmio_virt_base + registers::offsets::runtime_offset(rtsoff);
    let doorbell_virt_base = mmio_virt_base + registers::offsets::doorbell_offset(dboff);
    let max_ports = usize::from(registers::hcsparams1::max_ports(hcsparams1));
    let max_slots = usize::from(registers::hcsparams1::max_slots(hcsparams1));
    let mut port_states = Vec::new();
    port_states.resize(max_ports.saturating_add(1), device::PortState::Disconnected);
    let mut slots = Vec::new();
    slots.resize_with(max_slots.saturating_add(1), || None);

    let mut controller = XhciController {
        device,
        mmio_phys_base,
        mmio_virt_base,
        mmio_size,
        op_virt_base,
        runtime_virt_base,
        doorbell_virt_base,
        hcsparams1,
        hcsparams2,
        hccparams1,
        page_size,
        dma,
        pending_port_changes: VecDeque::new(),
        pending_command_completions: VecDeque::new(),
        pending_transfer_events: VecDeque::new(),
        event_overflowed: false,
        port_states,
        slots,
        resources: None,
    };

    let resources = build_resources(&controller)?;
    interrupt::register_interrupt();
    let interrupt_mode = configure_interrupt_mode(&controller.device)?;

    if let Err(err) = controller.init(resources) {
        interrupt_mode.disable(&controller.device);
        return Err(XhciError::Init(err));
    }

    Ok(controller)
}

fn configure_interrupt_mode(device: &PciDevice) -> Result<InterruptMode, XhciError> {
    match msi::configure_msi(device, interrupt::XHCI_INTERRUPT_VECTOR) {
        Ok(_) => Ok(InterruptMode::Msi),
        Err(MsiError::NotSupported) => {
            msi::configure_msix(device, &[interrupt::XHCI_INTERRUPT_VECTOR])?;
            Ok(InterruptMode::Msix)
        }
        Err(err) => Err(XhciError::Interrupt(err)),
    }
}

fn build_resources(controller: &XhciController) -> Result<XhciControllerResources, XhciError> {
    let max_slots = registers::hcsparams1::max_slots(controller.hcsparams1);
    let scratchpad_count = registers::hcsparams2::max_scratchpad_buffers(controller.hcsparams2);
    let config = XhciControlMemoryConfig {
        max_slots,
        scratchpad_buffer_count: scratchpad_count,
    };

    let mut dcbaa = memory::Dcbaa::new(&controller.dma, &config)?;
    let scratchpad = if scratchpad_count == 0 {
        None
    } else {
        Some(ScratchpadSet::new(&controller.dma, scratchpad_count)?)
    };
    dcbaa.set_scratchpad_array(scratchpad.as_ref());

    let command_ring = ProducerRing::new(COMMAND_RING_TRB_COUNT, &controller.dma)?;
    let event_ring = ConsumerRing::new(EVENT_RING_TRB_COUNT, &controller.dma)?;
    let segment = EventRingSegmentDescriptor {
        base_addr: event_ring.phys_addr(),
        trb_count: event_ring.trb_count(),
    };
    let segments = [segment; ERST_SEGMENT_COUNT];
    let erst = EventRingSegmentTable::new(&controller.dma, &segments)?;

    Ok(XhciControllerResources {
        dcbaa,
        scratchpad,
        command_ring,
        event_ring,
        erst,
    })
}
