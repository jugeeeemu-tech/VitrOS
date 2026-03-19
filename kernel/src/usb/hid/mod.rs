#[cfg(feature = "visualize-input")]
use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::input::{self, KeyEvent, KeyEventKind};
use crate::usb::device::{UsbDeviceHandle, UsbDeviceInfo};
use crate::usb::standard::SetupPacket;
use crate::usb::xhci::context::InputContextBuffer;
use crate::usb::xhci::device::{
    InterruptEndpointRuntime, InterruptTransferTd, endpoint_address_to_dci,
};
use crate::usb::xhci::event::CompletionCode;
use crate::usb::xhci::ring::{ProducerRing, RingError};
use crate::{info, warn};
use lazy_static::lazy_static;
use spin::Mutex;

#[cfg(feature = "visualize-input")]
use crate::usb::xhci::ring::{ConsumerRingSnapshot, ProducerRingSnapshot};

use super::{
    EnumerationError, USB_INTERRUPT_RING_TRB_COUNT, ensure_command_success,
    submit_control_transfer_for_handle, wait_for_command_completion, with_controller,
    with_xhci_controller,
};

mod keyboard;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HidKeyboard {
    handle: UsbDeviceHandle,
    slot_id: u8,
    endpoint_id: u8,
    last_report: [u8; 8],
}

#[derive(Debug)]
pub(super) enum HidError {
    Usb(EnumerationError),
    Ring(RingError),
    Dma(crate::dma::DmaError),
}

impl core::fmt::Display for HidError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Usb(err) => write!(f, "USB error: {}", err),
            Self::Ring(err) => write!(f, "ring error: {}", err),
            Self::Dma(err) => write!(f, "DMA error: {}", err),
        }
    }
}

impl From<EnumerationError> for HidError {
    fn from(value: EnumerationError) -> Self {
        Self::Usb(value)
    }
}

impl From<RingError> for HidError {
    fn from(value: RingError) -> Self {
        Self::Ring(value)
    }
}

impl From<crate::dma::DmaError> for HidError {
    fn from(value: crate::dma::DmaError) -> Self {
        Self::Dma(value)
    }
}

lazy_static! {
    static ref KEYBOARDS: Mutex<Vec<HidKeyboard>> = Mutex::new(Vec::new());
    static ref LAST_SERVED_KEYBOARD: Mutex<Option<UsbDeviceHandle>> = Mutex::new(None);
}

#[cfg(feature = "visualize-input")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyboardInputPathSnapshot {
    pub last_report: [u8; 8],
    pub interrupt_ring: ProducerRingSnapshot,
    pub report_buffer_len: usize,
    pub pending_trb_pointer: Option<u64>,
    pub event_ring: ConsumerRingSnapshot,
    pub event_overflowed: bool,
}

pub(super) fn attach_device(info: &UsbDeviceInfo) -> Result<(), HidError> {
    let Some(descriptor) = keyboard::find_boot_keyboard(info) else {
        return Ok(());
    };

    if KEYBOARDS
        .lock()
        .iter()
        .any(|keyboard| keyboard.handle == info.handle)
    {
        return Ok(());
    }

    submit_control_transfer_for_handle(
        info.handle,
        SetupPacket::set_configuration(descriptor.configuration_value),
        None,
    )?;
    submit_control_transfer_for_handle(
        info.handle,
        SetupPacket::set_protocol(descriptor.interface_number, 0),
        None,
    )?;

    let slot_id = with_controller(|controller| controller.slot_id_for_handle(info.handle))?
        .ok_or(EnumerationError::SlotRuntimeUnavailable(info.handle))?;
    let (dma, layout) =
        with_controller(|controller| (controller.dma_profile(), controller.context_layout()))?;
    let endpoint_id = endpoint_address_to_dci(descriptor.endpoint_address);
    let transfer_ring = ProducerRing::new(USB_INTERRUPT_RING_TRB_COUNT, &dma)?;
    let report_buffer = dma.allocate_data_buffer(8)?;
    let mut input_context = InputContextBuffer::new(&dma, layout)?;
    input_context.set_configure_interrupt_endpoint(
        info.port_id,
        info.speed,
        endpoint_id,
        endpoint_id,
        descriptor.max_packet_size,
        descriptor.endpoint_interval,
        transfer_ring.phys_addr(),
        transfer_ring.cycle_state(),
        8,
    );

    let configure_trb_pointer = with_controller(|controller| {
        controller.submit_configure_endpoint_command(slot_id, input_context.phys_addr())
    })??;
    let completion = wait_for_command_completion(configure_trb_pointer)?;
    ensure_command_success(completion.completion_code)?;

    with_controller(|controller| {
        let slot = controller
            .slot_runtime_mut_by_handle(info.handle)
            .ok_or(EnumerationError::SlotRuntimeUnavailable(info.handle))?;
        slot.active_configuration = Some(descriptor.configuration_value);
        slot.interrupt_in = Some(InterruptEndpointRuntime {
            endpoint_address: descriptor.endpoint_address,
            endpoint_id,
            interface_number: descriptor.interface_number,
            max_packet_size: descriptor.max_packet_size,
            interval: descriptor.endpoint_interval,
            report_len: 8,
            ring: transfer_ring,
            report_buffer,
            pending_trb_pointer: None,
        });
        Ok::<_, EnumerationError>(())
    })??;

    if let Err(err) = arm_keyboard_transfer(info.handle) {
        clear_keyboard_runtime(info.handle);
        return Err(err);
    }
    KEYBOARDS.lock().push(HidKeyboard {
        handle: info.handle,
        slot_id,
        endpoint_id,
        last_report: [0; 8],
    });
    info!(
        "[HID] Keyboard initialized: slot={}, interface={}, endpoint=0x{:02X}",
        slot_id, descriptor.interface_number, descriptor.endpoint_address
    );
    Ok(())
}

pub(super) fn detach_device(handle: UsbDeviceHandle) {
    KEYBOARDS
        .lock()
        .retain(|keyboard| keyboard.handle != handle);
    let mut last = LAST_SERVED_KEYBOARD.lock();
    if *last == Some(handle) {
        *last = None;
    }
}

pub(super) fn service_keyboards() -> bool {
    let snapshot = KEYBOARDS.lock().clone();
    let mut did_work = false;

    for keyboard in snapshot {
        match service_keyboard(keyboard) {
            Ok(worked) => {
                did_work |= worked;
            }
            Err(err) => {
                warn!(
                    "[HID] Detaching keyboard handle {} after service error: {}",
                    keyboard.handle.as_u64(),
                    err
                );
                detach_device(keyboard.handle);
                clear_keyboard_runtime(keyboard.handle);
            }
        }
    }

    did_work
}

fn service_keyboard(keyboard_state: HidKeyboard) -> Result<bool, HidError> {
    if arm_keyboard_transfer(keyboard_state.handle)? {
        return Ok(true);
    }

    let Some(pending_trb_pointer) = with_xhci_controller(|controller| {
        controller
            .slot_runtime_by_handle(keyboard_state.handle)
            .and_then(|slot| {
                let runtime = slot.interrupt_in.as_ref()?;
                runtime.pending_trb_pointer
            })
    })
    .flatten() else {
        return Ok(false);
    };

    let Some(completion) = with_xhci_controller(|controller| {
        controller.take_matching_transfer_completion(
            pending_trb_pointer,
            keyboard_state.slot_id,
            keyboard_state.endpoint_id,
        )
    })
    .flatten() else {
        return Ok(false);
    };

    remember_last_serviced_keyboard(keyboard_state.handle);

    let (report, report_len) = with_controller(|controller| {
        let slot = controller
            .slot_runtime_mut_by_handle(keyboard_state.handle)
            .ok_or(EnumerationError::SlotRuntimeUnavailable(
                keyboard_state.handle,
            ))?;
        let runtime =
            slot.interrupt_in
                .as_mut()
                .ok_or(EnumerationError::SlotRuntimeUnavailable(
                    keyboard_state.handle,
                ))?;
        runtime.pending_trb_pointer = None;
        runtime.ring.complete_through(completion.trb_pointer)?;

        let mut report = [0u8; 8];
        let report_len = runtime.report_len.min(report.len());
        report[..report_len].copy_from_slice(&runtime.report_buffer.as_slice()[..report_len]);
        Ok::<_, EnumerationError>((report, report_len))
    })??;

    match completion.completion_code {
        CompletionCode::Success | CompletionCode::ShortPacket => {}
        other => {
            warn!(
                "[HID] Transfer failed for slot {} endpoint {}: {:?}",
                keyboard_state.slot_id, keyboard_state.endpoint_id, other
            );
            #[cfg(feature = "visualize-input")]
            crate::input_trace::record_transfer_failure(
                keyboard_state.slot_id,
                keyboard_state.endpoint_id,
                completion.trb_pointer,
                other,
                completion.transfer_length,
            );
            arm_keyboard_transfer(keyboard_state.handle)?;
            return Ok(true);
        }
    }

    let bytes_transferred = report_len
        .saturating_sub(completion.transfer_length as usize)
        .min(report_len);
    #[cfg(feature = "visualize-input")]
    crate::input_trace::record_report_ready(report, bytes_transferred as u8);
    if bytes_transferred < report_len {
        warn!(
            "[HID] Ignoring short keyboard report on slot {}: {} bytes",
            keyboard_state.slot_id, bytes_transferred
        );
    } else {
        process_report(keyboard_state.handle, report)?;
    }

    arm_keyboard_transfer(keyboard_state.handle)?;
    Ok(true)
}

fn process_report(handle: UsbDeviceHandle, report: [u8; 8]) -> Result<(), HidError> {
    let mut keyboards = KEYBOARDS.lock();
    let Some(keyboard) = keyboards
        .iter_mut()
        .find(|keyboard| keyboard.handle == handle)
    else {
        return Ok(());
    };

    if let Err(err) = keyboard::dispatch_report(&mut keyboard.last_report, &report, |event| {
        log_key_event(event);
        input::push_key_event(event);
    }) {
        warn!(
            "[HID] Ignoring malformed keyboard report for handle {}: {:?}",
            handle.as_u64(),
            err
        );
    }

    Ok(())
}

fn arm_keyboard_transfer(handle: UsbDeviceHandle) -> Result<bool, HidError> {
    let should_ring = with_controller(|controller| {
        #[cfg(feature = "visualize-input")]
        let (slot_id, endpoint_id, completion_trb_pointer, should_ring) = {
            let slot = controller
                .slot_runtime_mut_by_handle(handle)
                .ok_or(EnumerationError::SlotRuntimeUnavailable(handle))?;
            let slot_id = slot.slot_id;
            let runtime = slot
                .interrupt_in
                .as_mut()
                .ok_or(EnumerationError::SlotRuntimeUnavailable(handle))?;

            if runtime.pending_trb_pointer.is_some() {
                (slot_id, runtime.endpoint_id, None, false)
            } else {
                let td = InterruptTransferTd::new(
                    runtime.report_buffer.phys_addr(),
                    runtime.report_len as u32,
                );
                let completion_trb_pointer = td.enqueue(&mut runtime.ring)?;
                runtime.pending_trb_pointer = Some(completion_trb_pointer);
                #[cfg(feature = "visualize-input")]
                crate::input_trace::record_transfer_queued(
                    slot_id,
                    runtime.endpoint_id,
                    completion_trb_pointer,
                );
                (
                    slot_id,
                    runtime.endpoint_id,
                    Some(completion_trb_pointer),
                    true,
                )
            }
        };

        #[cfg(not(feature = "visualize-input"))]
        let (slot_id, endpoint_id, should_ring) = {
            let slot = controller
                .slot_runtime_mut_by_handle(handle)
                .ok_or(EnumerationError::SlotRuntimeUnavailable(handle))?;
            let slot_id = slot.slot_id;
            let runtime = slot
                .interrupt_in
                .as_mut()
                .ok_or(EnumerationError::SlotRuntimeUnavailable(handle))?;

            if runtime.pending_trb_pointer.is_some() {
                (slot_id, runtime.endpoint_id, false)
            } else {
                let td = InterruptTransferTd::new(
                    runtime.report_buffer.phys_addr(),
                    runtime.report_len as u32,
                );
                let completion_trb_pointer = td.enqueue(&mut runtime.ring)?;
                runtime.pending_trb_pointer = Some(completion_trb_pointer);
                (slot_id, runtime.endpoint_id, true)
            }
        };

        if should_ring {
            controller.ring_device_doorbell(slot_id, endpoint_id);
            #[cfg(feature = "visualize-input")]
            if let Some(trb_pointer) = completion_trb_pointer {
                crate::input_trace::record_transfer_doorbell(slot_id, endpoint_id, trb_pointer);
            }
        }

        Ok::<_, EnumerationError>(should_ring)
    })??;
    Ok(should_ring)
}

#[cfg(feature = "visualize-input")]
pub(crate) fn active_keyboard_present() -> bool {
    !KEYBOARDS.lock().is_empty()
}

#[cfg(feature = "visualize-input")]
pub(crate) fn snapshot_keyboard_input_path() -> Option<Box<KeyboardInputPathSnapshot>> {
    let keyboard = selected_keyboard_state()?;
    with_xhci_controller(|controller| {
        let slot = controller.slot_runtime_by_handle(keyboard.handle)?;
        let interrupt = slot.interrupt_in.as_ref()?;
        Some(Box::new(KeyboardInputPathSnapshot {
            last_report: keyboard.last_report,
            interrupt_ring: interrupt.ring.snapshot(),
            report_buffer_len: interrupt.report_buffer.len(),
            pending_trb_pointer: interrupt.pending_trb_pointer,
            event_ring: controller.resources().event_ring.snapshot(),
            event_overflowed: controller.event_overflowed(),
        }))
    })
    .flatten()
}

#[cfg(feature = "visualize-input")]
pub(crate) fn keyboard_input_path_available() -> bool {
    let Some(keyboard) = selected_keyboard_state() else {
        return false;
    };

    with_xhci_controller(|controller| {
        controller
            .slot_runtime_by_handle(keyboard.handle)
            .and_then(|slot| slot.interrupt_in.as_ref())
            .is_some()
    })
    .unwrap_or(false)
}

fn clear_keyboard_runtime(handle: UsbDeviceHandle) {
    let _ = with_xhci_controller(|controller| {
        if let Some(slot) = controller.slot_runtime_mut_by_handle(handle) {
            slot.interrupt_in = None;
        }
    });
}

fn log_key_event(event: KeyEvent) {
    match event.kind {
        KeyEventKind::Press => info!("[HID] Key pressed: {:?}", event.code),
        KeyEventKind::Release => info!("[HID] Key released: {:?}", event.code),
    }
}

fn remember_last_serviced_keyboard(handle: UsbDeviceHandle) {
    *LAST_SERVED_KEYBOARD.lock() = Some(handle);
}

#[cfg(feature = "visualize-input")]
fn selected_keyboard_state() -> Option<HidKeyboard> {
    let preferred = *LAST_SERVED_KEYBOARD.lock();
    let keyboards = KEYBOARDS.lock();
    if let Some(handle) = preferred {
        if let Some(keyboard) = keyboards.iter().find(|keyboard| keyboard.handle == handle) {
            return Some(*keyboard);
        }
    }
    keyboards.last().copied()
}
