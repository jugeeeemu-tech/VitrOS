//! USB サブシステム

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

pub mod descriptor;
pub mod device;
pub mod standard;
pub mod xhci;

use crate::dma::DmaError;
use crate::sched;
use crate::sync::wait_queue::WaitQueue;
use crate::{info, io::without_interrupts, warn};
use descriptor::{ConfigurationDescriptorHeader, DescriptorParseError, DeviceDescriptor};
use device::{UsbDeviceHandle, UsbDeviceInfo};
use lazy_static::lazy_static;
use spin::Mutex;
use standard::{SetupPacket, descriptor_type};
use xhci::context::{DeviceContextBuffer, InputContextBuffer};
use xhci::device::{
    CONTROL_ENDPOINT_ID, CommandCompletionRecord, ControlTransferTd, PortState, SlotRuntime,
    TransferCompletionRecord,
};
use xhci::event::CompletionCode;
use xhci::ring::{ProducerRing, RingError};

lazy_static! {
    static ref XHCI_CONTROLLER: Mutex<Option<xhci::XhciController>> = Mutex::new(None);
    static ref DEVICE_REGISTRY: Mutex<Vec<UsbDeviceInfo>> = Mutex::new(Vec::new());
}

static USB_WORKER_WAIT: WaitQueue = WaitQueue::new();
static USB_WORKER_SIGNAL: AtomicBool = AtomicBool::new(false);

const USB_WORKER_NICE: i8 = sched::nice::DEFAULT - 5;
const USB_ENUMERATION_TIMEOUT_MS: u64 = 100;
const USB_EP0_RING_TRB_COUNT: usize = 32;

#[derive(Debug)]
enum EnumerationError {
    ControllerUnavailable,
    InvalidPort,
    InvalidPortSpeed(u8),
    PortDisconnected,
    Timeout(&'static str),
    Ring(RingError),
    Dma(DmaError),
    Memory(xhci::memory::XhciMemoryError),
    Descriptor(DescriptorParseError),
    CommandFailed(CompletionCode),
    TransferFailed(CompletionCode),
}

impl core::fmt::Display for EnumerationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ControllerUnavailable => write!(f, "xHCI controller unavailable"),
            Self::InvalidPort => write!(f, "invalid xHCI port"),
            Self::InvalidPortSpeed(speed_id) => write!(f, "invalid USB port speed id {}", speed_id),
            Self::PortDisconnected => write!(f, "USB port disconnected"),
            Self::Timeout(step) => write!(f, "timeout while waiting for {}", step),
            Self::Ring(err) => write!(f, "ring error: {}", err),
            Self::Dma(err) => write!(f, "DMA error: {}", err),
            Self::Memory(err) => write!(f, "memory error: {}", err),
            Self::Descriptor(err) => write!(f, "descriptor parse error: {:?}", err),
            Self::CommandFailed(code) => write!(f, "command completion failed: {:?}", code),
            Self::TransferFailed(code) => write!(f, "transfer completion failed: {:?}", code),
        }
    }
}

impl From<RingError> for EnumerationError {
    fn from(err: RingError) -> Self {
        Self::Ring(err)
    }
}

impl From<DmaError> for EnumerationError {
    fn from(err: DmaError) -> Self {
        Self::Dma(err)
    }
}

impl From<xhci::memory::XhciMemoryError> for EnumerationError {
    fn from(err: xhci::memory::XhciMemoryError) -> Self {
        Self::Memory(err)
    }
}

impl From<DescriptorParseError> for EnumerationError {
    fn from(err: DescriptorParseError) -> Self {
        Self::Descriptor(err)
    }
}

pub fn init() {
    info!("Initializing USB subsystem...");

    match xhci::init() {
        Ok(controller) => {
            let worker_task = match sched::Task::new("UsbWorker", USB_WORKER_NICE, usb_worker_task)
            {
                Ok(task) => task,
                Err(_) => {
                    info!("USB: worker task creation failed");
                    return;
                }
            };

            without_interrupts(|| {
                *XHCI_CONTROLLER.lock() = Some(controller);
                DEVICE_REGISTRY.lock().clear();
            });
            sched::add_task(worker_task);
            signal_xhci_worker();
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

pub fn snapshot_devices() -> Vec<UsbDeviceInfo> {
    without_interrupts(|| DEVICE_REGISTRY.lock().clone())
}

pub(crate) fn signal_xhci_worker() {
    USB_WORKER_SIGNAL.store(true, Ordering::Release);
    USB_WORKER_WAIT.wake_one();
}

extern "C" fn usb_worker_task() -> ! {
    info!("[USB] Worker started");

    let mut pending_ports = VecDeque::new();
    let mut rescan_all_ports = true;

    loop {
        drain_port_notifications(&mut pending_ports, &mut rescan_all_ports);
        if rescan_all_ports {
            enqueue_all_ports(&mut pending_ports);
            rescan_all_ports = false;
        }

        let mut did_work = false;
        while let Some(port_id) = pending_ports.pop_front() {
            process_port(port_id);
            did_work = true;
            drain_port_notifications(&mut pending_ports, &mut rescan_all_ports);
        }

        if !did_work && !rescan_all_ports {
            wait_for_worker_signal();
        }
    }
}

fn drain_port_notifications(pending_ports: &mut VecDeque<u8>, rescan_all_ports: &mut bool) {
    loop {
        let next_port = with_xhci_controller(|controller| controller.take_next_port_change()).flatten();
        let Some(port_id) = next_port else {
            break;
        };
        pending_ports.push_back(port_id);
    }

    if with_xhci_controller(|controller| controller.take_overflow_flag()).unwrap_or(false) {
        *rescan_all_ports = true;
    }
}

fn enqueue_all_ports(pending_ports: &mut VecDeque<u8>) {
    let Some(max_ports) = with_xhci_controller(|controller| controller.max_ports()) else {
        return;
    };

    for port_id in 1..=max_ports {
        pending_ports.push_back(port_id);
    }
}

fn process_port(port_id: u8) {
    let Some((state, status)) = with_xhci_controller(|controller| {
        let state = controller.port_state(port_id).unwrap_or(PortState::Disconnected);
        controller.port_status(port_id).map(|status| (state, status))
    })
    .flatten()
    else {
        return;
    };

    if !status.connected() {
        let _ = with_xhci_controller(|controller| {
            controller.acknowledge_port_changes(port_id, status.change_bits())
        });
        match state {
            PortState::Addressed { .. } => detach_port(port_id),
            PortState::Enumerating => {
                let _ = with_xhci_controller(|controller| {
                    controller.set_port_state(port_id, PortState::Disconnected)
                });
            }
            PortState::Disconnected => {}
        }
        return;
    }

    match state {
        PortState::Disconnected => {
            if let Err(err) = enumerate_port(port_id, status) {
                warn!("[USB] Enumeration failed on port {}: {}", port_id, err);
            }
        }
        PortState::Enumerating => {
            let _ = with_xhci_controller(|controller| {
                controller.set_port_state(port_id, PortState::Disconnected)
            });
            if let Err(err) = enumerate_port(port_id, status) {
                warn!("[USB] Enumeration retry failed on port {}: {}", port_id, err);
            }
        }
        PortState::Addressed { .. } => {
            let _ = with_xhci_controller(|controller| {
                controller.acknowledge_port_changes(port_id, status.change_bits())
            });
        }
    }
}

fn detach_port(port_id: u8) {
    let Some(slot_runtime) =
        with_xhci_controller(|controller| controller.take_slot_runtime_for_port(port_id)).flatten()
    else {
        return;
    };

    remove_device(slot_runtime.handle);
    info!("[xHCI] Device disconnected from port {}", port_id);

    if let Ok(completion) = submit_disable_slot(slot_runtime.slot_id) {
        if completion.completion_code != CompletionCode::Success {
            warn!(
                "[xHCI] Disable Slot failed for port {} slot {}: {:?}",
                port_id, slot_runtime.slot_id, completion.completion_code
            );
        }
    }

    let _ = with_xhci_controller(|controller| controller.clear_device_context(slot_runtime.slot_id));
}

fn enumerate_port(
    port_id: u8,
    initial_status: xhci::device::PortStatus,
) -> Result<(), EnumerationError> {
    let mut cleanup_slot_id = None;
    let result = (|| {
        let speed = initial_status
            .speed()
            .ok_or(EnumerationError::InvalidPortSpeed(initial_status.speed_id()))?;

        info!("[xHCI] Device connected on port {}", port_id);
        info!(
            "[xHCI] Port {} speed: {} (id={})",
            port_id,
            speed.as_str(),
            speed.port_speed_id()
        );

        with_controller(|controller| {
            if !controller.set_port_state(port_id, PortState::Enumerating) {
                return false;
            }
            controller.start_port_reset(port_id)
        })?
        .then_some(())
        .ok_or(EnumerationError::InvalidPort)?;

        wait_for_port_reset_change(port_id)?;
        info!("[xHCI] Port reset complete");

        let enable_completion = submit_enable_slot()?;
        ensure_command_success(enable_completion.completion_code)?;
        let slot_id = enable_completion.slot_id;
        cleanup_slot_id = Some(slot_id);
        info!("[xHCI] Slot {} enabled", slot_id);

        let (dma, layout) =
            with_controller(|controller| (controller.dma_profile(), controller.context_layout()))?;
        let device_context = DeviceContextBuffer::new(&dma, layout)?;
        let mut ep0_ring = ProducerRing::new(USB_EP0_RING_TRB_COUNT, &dma)?;
        let mut input_context = InputContextBuffer::new(&dma, layout)?;
        let mut ep0_max_packet_size = speed.default_ep0_max_packet_size();

        input_context.set_address_device_context(
            port_id,
            speed,
            ep0_max_packet_size,
            ep0_ring.phys_addr(),
            ep0_ring.cycle_state(),
        );

        with_controller(|controller| {
            controller.install_device_context(slot_id, device_context.phys_addr())
        })??;
        let address_completion = with_controller(|controller| {
            controller.submit_address_device_command(slot_id, input_context.phys_addr())
        })??;
        let address_completion = wait_for_command_completion(address_completion)?;
        ensure_command_success(address_completion.completion_code)?;

        let address = device_context.usb_device_address();
        info!("[xHCI] Device addressed: slot={}, address={}", slot_id, address);

        let descriptor_prefix = read_descriptor_bytes(
            slot_id,
            &mut ep0_ring,
            &dma,
            SetupPacket::get_descriptor(descriptor_type::DEVICE, 0, 8),
            8,
        )?;
        let reported_mps0 = u16::from(DeviceDescriptor::parse_max_packet_size0(&descriptor_prefix)?);
        if reported_mps0 != ep0_max_packet_size {
            ep0_max_packet_size = reported_mps0;
            let mut evaluate_context = InputContextBuffer::new(&dma, layout)?;
            evaluate_context.set_evaluate_context_for_ep0(
                ep0_max_packet_size,
                ep0_ring.phys_addr(),
                ep0_ring.cycle_state(),
            );
            let evaluate_completion = with_controller(|controller| {
                controller.submit_evaluate_context_command(slot_id, evaluate_context.phys_addr())
            })??;
            let evaluate_completion = wait_for_command_completion(evaluate_completion)?;
            ensure_command_success(evaluate_completion.completion_code)?;
        }

        let device_descriptor = DeviceDescriptor::parse(&read_descriptor_bytes(
            slot_id,
            &mut ep0_ring,
            &dma,
            SetupPacket::get_descriptor(descriptor_type::DEVICE, 0, 18),
            18,
        )?)?;

        let configuration_header = ConfigurationDescriptorHeader::parse(&read_descriptor_bytes(
            slot_id,
            &mut ep0_ring,
            &dma,
            SetupPacket::get_descriptor(descriptor_type::CONFIGURATION, 0, 9),
            9,
        )?)?;
        let configuration = descriptor::parse_configuration(&read_descriptor_bytes(
            slot_id,
            &mut ep0_ring,
            &dma,
            SetupPacket::get_descriptor(
                descriptor_type::CONFIGURATION,
                0,
                configuration_header.total_length,
            ),
            usize::from(configuration_header.total_length),
        )?)?;

        let handle = UsbDeviceHandle::allocate();
        let mut configurations = Vec::new();
        configurations.push(configuration);
        let info = UsbDeviceInfo {
            handle,
            port_id,
            address,
            speed,
            vendor_id: device_descriptor.vendor_id,
            product_id: device_descriptor.product_id,
            configurations,
        };

        let slot_runtime = SlotRuntime {
            handle,
            slot_id,
            port_id,
            speed,
            address,
            info: info.clone(),
            device_context,
            ep0_ring,
        };

        with_controller(|controller| controller.publish_slot_runtime(slot_runtime))?;
        cleanup_slot_id = None;
        publish_device(info.clone());
        info!(
            "[USB] Device: VID=0x{:04X}, PID=0x{:04X}",
            info.vendor_id, info.product_id
        );
        Ok(())
    })();

    if result.is_err() {
        cleanup_failed_enumeration(port_id, cleanup_slot_id);
    }

    result
}

fn read_descriptor_bytes(
    slot_id: u8,
    ep0_ring: &mut ProducerRing,
    dma: &xhci::dma::XhciDmaProfile,
    setup: SetupPacket,
    requested_length: usize,
) -> Result<Vec<u8>, EnumerationError> {
    let mut buffer = dma.allocate_data_buffer(requested_length.max(1))?;
    let bytes_transferred =
        submit_control_transfer(slot_id, ep0_ring, setup, Some(&mut buffer))?.min(requested_length);
    Ok(buffer.as_slice()[..bytes_transferred].to_vec())
}

fn submit_control_transfer(
    slot_id: u8,
    ep0_ring: &mut ProducerRing,
    setup: SetupPacket,
    data_buffer: Option<&mut crate::dma::DmaBuffer>,
) -> Result<usize, EnumerationError> {
    let td = ControlTransferTd::new(setup, data_buffer.as_ref().map(|buffer| buffer.phys_addr()));
    let completion_trb_pointer = td.enqueue(ep0_ring)?;
    with_controller(|controller| controller.ring_device_doorbell(slot_id, CONTROL_ENDPOINT_ID))?;

    let transfer = wait_for_transfer_completion(completion_trb_pointer, slot_id, CONTROL_ENDPOINT_ID)?;
    ep0_ring.complete_through(transfer.trb_pointer)?;
    ensure_transfer_success(transfer.completion_code)?;

    Ok((setup.length as usize).saturating_sub(transfer.transfer_length as usize))
}

fn wait_for_port_reset_change(port_id: u8) -> Result<(), EnumerationError> {
    for _ in 0..USB_ENUMERATION_TIMEOUT_MS {
        let status = with_controller(|controller| controller.port_status(port_id))?
            .ok_or(EnumerationError::InvalidPort)?;
        if !status.connected() {
            return Err(EnumerationError::PortDisconnected);
        }
        if status.port_reset_change() {
            let _ = with_xhci_controller(|controller| {
                controller.acknowledge_port_changes(port_id, status.change_bits())
            });
            return Ok(());
        }
        wait_for_worker_signal_or_tick();
    }

    Err(EnumerationError::Timeout("port reset"))
}

fn submit_enable_slot() -> Result<CommandCompletionRecord, EnumerationError> {
    let trb_pointer = with_controller(|controller| controller.submit_enable_slot_command())??;
    let completion = wait_for_command_completion(trb_pointer)?;
    Ok(completion)
}

fn cleanup_failed_enumeration(port_id: u8, slot_id: Option<u8>) {
    let _ = with_xhci_controller(|controller| controller.set_port_state(port_id, PortState::Disconnected));
    let Some(slot_id) = slot_id else {
        return;
    };

    match submit_disable_slot(slot_id) {
        Ok(completion) if completion.completion_code == CompletionCode::Success => {}
        Ok(completion) => {
            warn!(
                "[xHCI] Disable Slot failed during cleanup: slot={}, code={:?}",
                slot_id, completion.completion_code
            );
        }
        Err(err) => {
            warn!(
                "[xHCI] Disable Slot timed out during cleanup: slot={}, err={:?}",
                slot_id, err
            );
        }
    }

    let _ = with_xhci_controller(|controller| controller.clear_device_context(slot_id));
}

fn submit_disable_slot(slot_id: u8) -> Result<CommandCompletionRecord, EnumerationError> {
    let trb_pointer = with_controller(|controller| controller.submit_disable_slot_command(slot_id))??;
    wait_for_command_completion(trb_pointer)
}

fn wait_for_command_completion(trb_pointer: u64) -> Result<CommandCompletionRecord, EnumerationError> {
    for _ in 0..USB_ENUMERATION_TIMEOUT_MS {
        if let Some(record) =
            with_xhci_controller(|controller| controller.take_matching_command_completion(trb_pointer))
                .flatten()
        {
            return Ok(record);
        }
        wait_for_worker_signal_or_tick();
    }

    Err(EnumerationError::Timeout("command completion"))
}

fn wait_for_transfer_completion(
    trb_pointer: u64,
    slot_id: u8,
    endpoint_id: u8,
) -> Result<TransferCompletionRecord, EnumerationError> {
    for _ in 0..USB_ENUMERATION_TIMEOUT_MS {
        if let Some(record) = with_xhci_controller(|controller| {
            controller.take_matching_transfer_completion(trb_pointer, slot_id, endpoint_id)
        })
        .flatten()
        {
            return Ok(record);
        }
        wait_for_worker_signal_or_tick();
    }

    Err(EnumerationError::Timeout("transfer completion"))
}

fn wait_for_worker_signal_or_tick() {
    if !USB_WORKER_SIGNAL.swap(false, Ordering::AcqRel) {
        sched::sleep_ms(1);
    }
}

fn wait_for_worker_signal() {
    loop {
        if USB_WORKER_SIGNAL.swap(false, Ordering::AcqRel) {
            return;
        }
        USB_WORKER_WAIT.wait();
    }
}

fn ensure_command_success(completion_code: CompletionCode) -> Result<(), EnumerationError> {
    match completion_code {
        CompletionCode::Success => Ok(()),
        other => Err(EnumerationError::CommandFailed(other)),
    }
}

fn ensure_transfer_success(completion_code: CompletionCode) -> Result<(), EnumerationError> {
    match completion_code {
        CompletionCode::Success | CompletionCode::ShortPacket => Ok(()),
        other => Err(EnumerationError::TransferFailed(other)),
    }
}

fn publish_device(info: UsbDeviceInfo) {
    without_interrupts(|| {
        let mut registry = DEVICE_REGISTRY.lock();
        registry.retain(|entry| entry.handle != info.handle);
        registry.push(info);
    });
}

fn remove_device(handle: UsbDeviceHandle) {
    without_interrupts(|| {
        DEVICE_REGISTRY.lock().retain(|entry| entry.handle != handle);
    });
}

fn with_controller<R>(
    f: impl FnOnce(&mut xhci::XhciController) -> R,
) -> Result<R, EnumerationError> {
    with_xhci_controller(f).ok_or(EnumerationError::ControllerUnavailable)
}
