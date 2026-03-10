use alloc::collections::VecDeque;

use crate::dma::DmaBuffer;
use crate::usb::standard::SetupPacket;
use crate::usb::device::{UsbDeviceHandle, UsbDeviceInfo, UsbSpeed};

use super::context::{ContextLayout, DeviceContextBuffer};
use super::event::{CompletionCode, PENDING_EVENT_CAPACITY};
use super::memory::XhciMemoryError;
use super::registers::{self, portsc};
use super::ring::{ProducerRing, RingError};
use super::trb::{Trb, setup_transfer_type, trb_type};
use super::XhciController;

const DOORBELL_TARGET_MASK: u32 = 0xff;
const PORTSC_CHANGE_BITS: u32 = portsc::CSC
    | portsc::PEC
    | portsc::WRC
    | portsc::OCC
    | portsc::PRC
    | portsc::PLC
    | portsc::CEC;
const PORTSC_PRESERVE_BITS: u32 = portsc::PP;

pub(crate) const CONTROL_ENDPOINT_ID: u8 = 1;

pub(crate) struct InterruptTransferTd {
    trb: Trb,
}

impl InterruptTransferTd {
    pub fn new(data_buffer_phys_addr: u64, transfer_length: u32) -> Self {
        Self {
            trb: interrupt_transfer_trb(data_buffer_phys_addr, transfer_length),
        }
    }

    pub fn enqueue(self, ring: &mut ProducerRing) -> Result<u64, RingError> {
        ring.enqueue(self.trb)
    }

    #[cfg(test)]
    pub const fn trb(&self) -> &Trb {
        &self.trb
    }
}

pub(crate) struct ControlTransferTd {
    trbs: [Trb; 3],
    trb_count: usize,
}

impl ControlTransferTd {
    pub fn new(setup: SetupPacket, data_buffer_phys_addr: Option<u64>) -> Self {
        let mut setup_trb = Trb::default();
        setup_trb.set_parameter(setup.to_u64());
        setup_trb.set_transfer_length(8);
        setup_trb.set_immediate_data(true);
        setup_trb.set_trb_type(trb_type::SETUP_STAGE);
        setup_trb.set_chain_bit(true);
        setup_trb.set_setup_transfer_type(match (setup.length != 0, setup.direction_in()) {
            (false, _) => setup_transfer_type::NO_DATA_STAGE,
            (true, false) => setup_transfer_type::OUT_DATA_STAGE,
            (true, true) => setup_transfer_type::IN_DATA_STAGE,
        });

        let mut status_trb = Trb::default();
        status_trb.set_trb_type(trb_type::STATUS_STAGE);
        status_trb.set_ioc(true);

        if setup.length == 0 {
            status_trb.set_direction_in(true);
            return Self {
                trbs: [setup_trb, status_trb, Trb::default()],
                trb_count: 2,
            };
        }

        let mut data_trb = Trb::default();
        data_trb.set_parameter(data_buffer_phys_addr.unwrap_or(0));
        data_trb.set_transfer_length(setup.length as u32);
        data_trb.set_trb_type(trb_type::DATA_STAGE);
        data_trb.set_direction_in(setup.direction_in());
        data_trb.set_chain_bit(true);

        status_trb.set_direction_in(!setup.direction_in());

        Self {
            trbs: [setup_trb, data_trb, status_trb],
            trb_count: 3,
        }
    }

    pub fn enqueue(self, ring: &mut ProducerRing) -> Result<u64, RingError> {
        let mut completion_trb_pointer = 0;
        for index in 0..self.trb_count {
            let trb_pointer = ring.enqueue(self.trbs[index])?;
            completion_trb_pointer = trb_pointer;
        }
        Ok(completion_trb_pointer)
    }

    #[cfg(test)]
    pub const fn trbs(&self) -> &[Trb; 3] {
        &self.trbs
    }

    #[cfg(test)]
    pub const fn trb_count(&self) -> usize {
        self.trb_count
    }
}

#[allow(dead_code)]
pub(crate) struct InterruptEndpointRuntime {
    pub endpoint_address: u8,
    pub endpoint_id: u8,
    pub interface_number: u8,
    pub max_packet_size: u16,
    pub interval: u8,
    pub report_len: usize,
    pub ring: ProducerRing,
    pub report_buffer: DmaBuffer,
    pub pending_trb_pointer: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortState {
    Disconnected,
    Enumerating,
    Addressed { handle: UsbDeviceHandle, slot_id: u8 },
}

#[allow(dead_code)]
pub(crate) struct SlotRuntime {
    pub handle: UsbDeviceHandle,
    pub slot_id: u8,
    pub port_id: u8,
    pub speed: UsbSpeed,
    pub address: u8,
    pub info: UsbDeviceInfo,
    pub device_context: DeviceContextBuffer,
    pub ep0_ring: ProducerRing,
    pub active_configuration: Option<u8>,
    pub interrupt_in: Option<InterruptEndpointRuntime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommandCompletionRecord {
    pub trb_pointer: u64,
    pub completion_code: CompletionCode,
    pub slot_id: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransferCompletionRecord {
    pub trb_pointer: u64,
    pub completion_code: CompletionCode,
    pub transfer_length: u32,
    pub slot_id: u8,
    pub endpoint_id: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PortStatus {
    raw: u32,
}

impl PortStatus {
    pub const fn new(raw: u32) -> Self {
        Self { raw }
    }

    pub const fn connected(self) -> bool {
        (self.raw & portsc::CCS) != 0
    }

    pub const fn port_reset_change(self) -> bool {
        (self.raw & portsc::PRC) != 0
    }

    pub const fn speed_id(self) -> u8 {
        registers::portsc::port_speed(self.raw)
    }

    pub const fn speed(self) -> Option<UsbSpeed> {
        UsbSpeed::from_port_speed_id(self.speed_id())
    }

    pub const fn change_bits(self) -> u32 {
        self.raw & PORTSC_CHANGE_BITS
    }
}

impl XhciController {
    pub(crate) fn context_layout(&self) -> ContextLayout {
        ContextLayout::new(registers::hccparams1::context_size(self.hccparams1))
            .expect("xHCI context size must be 32 or 64 bytes")
    }

    pub(crate) fn max_ports(&self) -> u8 {
        registers::hcsparams1::max_ports(self.hcsparams1)
    }

    pub(crate) fn dma_profile(&self) -> super::dma::XhciDmaProfile {
        self.dma
    }

    pub(crate) fn port_state(&self, port_id: u8) -> Option<PortState> {
        self.port_states.get(port_id as usize).copied()
    }

    pub(crate) fn set_port_state(&mut self, port_id: u8, state: PortState) -> bool {
        let Some(entry) = self.port_states.get_mut(port_id as usize) else {
            return false;
        };
        *entry = state;
        true
    }

    pub(crate) fn take_next_port_change(&mut self) -> Option<u8> {
        self.pending_port_changes.pop_front()
    }

    pub(crate) fn take_overflow_flag(&mut self) -> bool {
        let overflowed = self.event_overflowed;
        self.event_overflowed = false;
        overflowed
    }

    pub(crate) fn take_matching_command_completion(
        &mut self,
        trb_pointer: u64,
    ) -> Option<CommandCompletionRecord> {
        take_matching(
            &mut self.pending_command_completions,
            |record| record.trb_pointer == trb_pointer,
        )
    }

    pub(crate) fn take_matching_transfer_completion(
        &mut self,
        trb_pointer: u64,
        slot_id: u8,
        endpoint_id: u8,
    ) -> Option<TransferCompletionRecord> {
        take_matching(&mut self.pending_transfer_events, |record| {
            record.trb_pointer == trb_pointer
                && record.slot_id == slot_id
                && record.endpoint_id == endpoint_id
        })
    }

    pub(crate) fn publish_slot_runtime(&mut self, slot: SlotRuntime) {
        let port_id = slot.port_id;
        let slot_id = slot.slot_id as usize;
        let handle = slot.handle;

        if slot_id < self.slots.len() {
            self.slots[slot_id] = Some(slot);
        }
        if (port_id as usize) < self.port_states.len() {
            self.port_states[port_id as usize] = PortState::Addressed {
                handle,
                slot_id: slot_id as u8,
            };
        }
    }

    pub(crate) fn take_slot_runtime_for_port(&mut self, port_id: u8) -> Option<SlotRuntime> {
        let state = self.port_state(port_id)?;
        let PortState::Addressed { slot_id, .. } = state else {
            return None;
        };

        self.port_states[port_id as usize] = PortState::Disconnected;
        self.slots.get_mut(slot_id as usize)?.take()
    }

    pub(crate) fn slot_id_for_handle(&self, handle: UsbDeviceHandle) -> Option<u8> {
        self.slots.iter().enumerate().find_map(|(slot_id, slot)| {
            let slot = slot.as_ref()?;
            (slot.handle == handle).then_some(slot_id as u8)
        })
    }

    pub(crate) fn slot_runtime_by_handle(&self, handle: UsbDeviceHandle) -> Option<&SlotRuntime> {
        self.slots.iter().flatten().find(|slot| slot.handle == handle)
    }

    pub(crate) fn slot_runtime_mut_by_handle(
        &mut self,
        handle: UsbDeviceHandle,
    ) -> Option<&mut SlotRuntime> {
        self.slots.iter_mut().flatten().find(|slot| slot.handle == handle)
    }

    pub(crate) fn submit_enable_slot_command(&mut self) -> Result<u64, RingError> {
        let mut trb = Trb::default();
        trb.set_trb_type(trb_type::ENABLE_SLOT);

        let trb_pointer = self.resources_mut().command_ring.enqueue(trb)?;
        self.ring_command_doorbell();
        Ok(trb_pointer)
    }

    pub(crate) fn submit_disable_slot_command(&mut self, slot_id: u8) -> Result<u64, RingError> {
        let mut trb = Trb::default();
        trb.set_trb_type(trb_type::DISABLE_SLOT);
        trb.set_slot_id(slot_id);

        let trb_pointer = self.resources_mut().command_ring.enqueue(trb)?;
        self.ring_command_doorbell();
        Ok(trb_pointer)
    }

    pub(crate) fn submit_address_device_command(
        &mut self,
        slot_id: u8,
        input_context_phys_addr: u64,
    ) -> Result<u64, RingError> {
        let mut trb = Trb::default();
        trb.set_parameter(input_context_phys_addr);
        trb.set_trb_type(trb_type::ADDRESS_DEVICE);
        trb.set_slot_id(slot_id);
        trb.set_address_device_bsr(false);

        let trb_pointer = self.resources_mut().command_ring.enqueue(trb)?;
        self.ring_command_doorbell();
        Ok(trb_pointer)
    }

    pub(crate) fn submit_evaluate_context_command(
        &mut self,
        slot_id: u8,
        input_context_phys_addr: u64,
    ) -> Result<u64, RingError> {
        let trb = evaluate_context_command_trb(slot_id, input_context_phys_addr);

        let trb_pointer = self.resources_mut().command_ring.enqueue(trb)?;
        self.ring_command_doorbell();
        Ok(trb_pointer)
    }

    pub(crate) fn submit_configure_endpoint_command(
        &mut self,
        slot_id: u8,
        input_context_phys_addr: u64,
    ) -> Result<u64, RingError> {
        let trb = configure_endpoint_command_trb(slot_id, input_context_phys_addr);

        let trb_pointer = self.resources_mut().command_ring.enqueue(trb)?;
        self.ring_command_doorbell();
        Ok(trb_pointer)
    }

    pub(crate) fn install_device_context(
        &mut self,
        slot_id: u8,
        phys_addr: u64,
    ) -> Result<(), XhciMemoryError> {
        self.resources_mut()
            .dcbaa
            .set_device_context(slot_id, phys_addr)
    }

    pub(crate) fn clear_device_context(&mut self, slot_id: u8) -> Result<(), XhciMemoryError> {
        self.resources_mut().dcbaa.clear_device_context(slot_id)
    }

    pub(crate) fn record_port_change(&mut self, port_id: u8) {
        push_bounded(
            &mut self.pending_port_changes,
            port_id,
            &mut self.event_overflowed,
        );
    }

    pub(crate) fn record_command_completion(
        &mut self,
        trb_pointer: u64,
        completion_code: CompletionCode,
        slot_id: u8,
    ) {
        push_bounded(
            &mut self.pending_command_completions,
            CommandCompletionRecord {
                trb_pointer,
                completion_code,
                slot_id,
            },
            &mut self.event_overflowed,
        );
    }

    pub(crate) fn record_transfer_completion(
        &mut self,
        trb_pointer: u64,
        completion_code: CompletionCode,
        transfer_length: u32,
        slot_id: u8,
        endpoint_id: u8,
    ) {
        push_bounded(
            &mut self.pending_transfer_events,
            TransferCompletionRecord {
                trb_pointer,
                completion_code,
                transfer_length,
                slot_id,
                endpoint_id,
            },
            &mut self.event_overflowed,
        );
    }

    pub(crate) fn port_status(&self, port_id: u8) -> Option<PortStatus> {
        self.read_portsc(port_id).map(PortStatus::new)
    }

    pub(crate) fn acknowledge_port_changes(&mut self, port_id: u8, bits: u32) -> bool {
        if self.read_portsc(port_id).is_none() {
            return false;
        }

        self.write_portsc(port_id, 0, bits & PORTSC_CHANGE_BITS);
        true
    }

    pub(crate) fn start_port_reset(&mut self, port_id: u8) -> bool {
        if self.read_portsc(port_id).is_none() {
            return false;
        }

        self.write_portsc(port_id, portsc::PR, PORTSC_CHANGE_BITS);
        true
    }

    pub(crate) fn read_portsc(&self, port_id: u8) -> Option<u32> {
        if port_id == 0 || port_id > self.max_ports() {
            return None;
        }

        let regs = self.operational_registers();
        let port_index = (port_id - 1) as usize;
        // SAFETY: the operational register block is MMIO mapped and `port_index`
        // has been bounds-checked against the controller's reported max ports.
        Some(unsafe { core::ptr::read_volatile(&(*regs).ports[port_index].portsc) })
    }

    pub(crate) fn write_portsc(&mut self, port_id: u8, set_bits: u32, clear_change_bits: u32) {
        let Some(current) = self.read_portsc(port_id) else {
            return;
        };

        let value = (current & PORTSC_PRESERVE_BITS)
            | set_bits
            | (clear_change_bits & PORTSC_CHANGE_BITS);
        let regs = self.operational_registers();
        let port_index = (port_id - 1) as usize;

        // SAFETY: the operational register block is MMIO mapped and `port_index`
        // has been bounds-checked through `read_portsc`.
        unsafe {
            core::ptr::write_volatile(&mut (*regs).ports[port_index].portsc, value);
        }
    }

    pub(crate) fn ring_command_doorbell(&mut self) {
        self.write_doorbell(0, 0);
    }

    pub(crate) fn ring_device_doorbell(&mut self, slot_id: u8, endpoint_id: u8) {
        self.write_doorbell(slot_id, endpoint_id as u32 & DOORBELL_TARGET_MASK);
    }

    fn write_doorbell(&mut self, slot_id: u8, value: u32) {
        let index = slot_id as usize;
        let base = self.doorbell_virt_base as *mut registers::DoorbellRegister;
        // SAFETY: DBOFF points to an MMIO array of doorbell registers. Index 0 is
        // the command ring, slot doorbells start at index 1 and `slot_id` is
        // validated by callers before use.
        unsafe {
            core::ptr::write_volatile(&mut (*base.add(index)).db, value);
        }
    }
}

pub(crate) fn endpoint_address_to_dci(address: u8) -> u8 {
    let endpoint_number = address & 0x0f;
    if endpoint_number == 0 {
        return CONTROL_ENDPOINT_ID;
    }

    endpoint_number.saturating_mul(2) + u8::from((address & 0x80) != 0)
}

fn evaluate_context_command_trb(slot_id: u8, input_context_phys_addr: u64) -> Trb {
    let mut trb = Trb::default();
    trb.set_parameter(input_context_phys_addr);
    trb.set_trb_type(trb_type::EVALUATE_CONTEXT);
    trb.set_slot_id(slot_id);
    trb
}

fn configure_endpoint_command_trb(slot_id: u8, input_context_phys_addr: u64) -> Trb {
    let mut trb = Trb::default();
    trb.set_parameter(input_context_phys_addr);
    trb.set_trb_type(trb_type::CONFIGURE_ENDPOINT);
    trb.set_slot_id(slot_id);
    trb
}

fn interrupt_transfer_trb(data_buffer_phys_addr: u64, transfer_length: u32) -> Trb {
    let mut trb = Trb::default();
    trb.set_parameter(data_buffer_phys_addr);
    trb.set_transfer_length(transfer_length);
    trb.set_trb_type(trb_type::NORMAL);
    trb.set_ioc(true);
    trb
}

fn push_bounded<T>(queue: &mut VecDeque<T>, value: T, overflowed: &mut bool) {
    if queue.len() >= PENDING_EVENT_CAPACITY {
        *overflowed = true;
        return;
    }

    queue.push_back(value);
}

fn take_matching<T>(
    queue: &mut VecDeque<T>,
    predicate: impl Fn(&T) -> bool,
) -> Option<T> {
    let index = queue.iter().position(predicate)?;
    queue.remove(index)
}

#[cfg(test)]
mod tests {
    use alloc::collections::VecDeque;

    use super::{
        CommandCompletionRecord, ControlTransferTd, InterruptTransferTd, PortState, PortStatus,
        TransferCompletionRecord, configure_endpoint_command_trb, endpoint_address_to_dci,
    };
    use crate::usb::device::{UsbDeviceHandle, UsbSpeed};
    use crate::usb::standard::{SetupPacket, descriptor_type};
    use crate::usb::xhci::event::CompletionCode;
    use crate::usb::xhci::registers::portsc;
    use crate::usb::xhci::trb::{setup_transfer_type, trb_type};

    #[test_case]
    fn test_port_status_extracts_speed_and_change_bits() {
        let status = PortStatus::new(portsc::CCS | portsc::PRC | (3u32 << 10));
        assert!(status.connected());
        assert!(status.port_reset_change());
        assert_eq!(status.speed(), Some(UsbSpeed::High));
        assert_eq!(status.change_bits(), portsc::PRC);
    }

    #[test_case]
    fn test_take_matching_from_completion_queue() {
        let mut queue = VecDeque::new();
        queue.push_back(CommandCompletionRecord {
            trb_pointer: 1,
            completion_code: CompletionCode::Success,
            slot_id: 2,
        });
        queue.push_back(CommandCompletionRecord {
            trb_pointer: 2,
            completion_code: CompletionCode::Success,
            slot_id: 3,
        });

        let record = super::take_matching(&mut queue, |entry| entry.trb_pointer == 2)
            .expect("matching completion");
        assert_eq!(record.slot_id, 3);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].trb_pointer, 1);
    }

    #[test_case]
    fn test_transfer_completion_record_is_equatable() {
        let record = TransferCompletionRecord {
            trb_pointer: 0x1234,
            completion_code: CompletionCode::ShortPacket,
            transfer_length: 8,
            slot_id: 1,
            endpoint_id: 1,
        };

        assert_eq!(record.endpoint_id, 1);
        assert_eq!(record.completion_code, CompletionCode::ShortPacket);
    }

    #[test_case]
    fn test_port_state_addressed_carries_handle_and_slot() {
        let handle = UsbDeviceHandle::allocate();
        let state = PortState::Addressed { handle, slot_id: 7 };

        assert_eq!(state, PortState::Addressed { handle, slot_id: 7 });
    }

    #[test_case]
    fn test_control_transfer_td_builds_setup_data_status_sequence_for_get_descriptor() {
        let td = ControlTransferTd::new(
            SetupPacket::get_descriptor(descriptor_type::DEVICE, 0, 18),
            Some(0x1234_5000),
        );

        assert_eq!(td.trb_count(), 3);
        assert_eq!(td.trbs()[0].trb_type(), trb_type::SETUP_STAGE);
        assert!(td.trbs()[0].immediate_data());
        assert_eq!(
            td.trbs()[0].setup_transfer_type(),
            setup_transfer_type::IN_DATA_STAGE
        );
        assert_eq!(td.trbs()[1].trb_type(), trb_type::DATA_STAGE);
        assert!(td.trbs()[1].direction_in());
        assert_eq!(td.trbs()[1].parameter(), 0x1234_5000);
        assert_eq!(td.trbs()[2].trb_type(), trb_type::STATUS_STAGE);
        assert!(td.trbs()[2].ioc());
        assert!(!td.trbs()[2].direction_in());
    }

    #[test_case]
    fn test_endpoint_address_to_dci_mapping() {
        assert_eq!(endpoint_address_to_dci(0x00), 1);
        assert_eq!(endpoint_address_to_dci(0x81), 3);
        assert_eq!(endpoint_address_to_dci(0x02), 4);
        assert_eq!(endpoint_address_to_dci(0x83), 7);
    }

    #[test_case]
    fn test_configure_endpoint_command_trb_sets_slot_and_type() {
        let trb = configure_endpoint_command_trb(4, 0xCAFE_BABE);
        assert_eq!(trb.parameter(), 0xCAFE_BABE);
        assert_eq!(trb.trb_type(), trb_type::CONFIGURE_ENDPOINT);
        assert_eq!(trb.slot_id(), 4);
    }

    #[test_case]
    fn test_interrupt_transfer_td_builds_normal_trb() {
        let td = InterruptTransferTd::new(0x1234_5000, 8);
        assert_eq!(td.trb().parameter(), 0x1234_5000);
        assert_eq!(td.trb().transfer_length(), 8);
        assert_eq!(td.trb().trb_type(), trb_type::NORMAL);
        assert!(td.trb().ioc());
    }
}
