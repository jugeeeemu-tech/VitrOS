//! xHCI controller initialization sequence.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use crate::{hpet, info, pit};

use super::{
    XhciController,
    event::{CompletionCode, Event},
    memory::{Dcbaa, EventRingSegmentTable, ScratchpadSet},
    registers::{self, InterrupterRegisterSet, OperationalRegisters, RuntimeRegisters},
    ring::{ConsumerRing, ProducerRing},
};

const INTERRUPTER_INDEX: usize = 0;
const POLL_INTERVAL_US: u64 = 10;
const TIMEOUT_MS: u64 = 100;
const TIMEOUT_US: u64 = TIMEOUT_MS * 1000;

pub struct XhciControllerResources {
    pub dcbaa: Dcbaa,
    pub scratchpad: Option<ScratchpadSet>,
    pub command_ring: ProducerRing,
    pub event_ring: ConsumerRing,
    pub erst: EventRingSegmentTable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XhciControllerInitError {
    Timeout { step: &'static str, timeout_ms: u64 },
    ZeroMaxSlots,
    NoInterrupters,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InterruptAck {
    UsbStatus(u32),
    Iman(u32),
}

impl XhciController {
    pub fn init(
        &mut self,
        resources: XhciControllerResources,
    ) -> Result<(), XhciControllerInitError> {
        if registers::hcsparams1::max_slots(self.hcsparams1) == 0 {
            return Err(XhciControllerInitError::ZeroMaxSlots);
        }
        if registers::hcsparams1::max_intrs(self.hcsparams1) == 0 {
            return Err(XhciControllerInitError::NoInterrupters);
        }

        self.resources = Some(resources);

        let result = (|| {
            self.stop_controller()?;
            self.reset_controller()?;
            self.configure_controller()?;
            self.setup_command_ring()?;
            self.setup_event_ring()?;
            self.enable_interrupts()?;
            self.start_controller()?;
            Ok(())
        })();

        if result.is_err() {
            self.best_effort_quiesce();
        }

        result
    }

    pub fn handle_interrupt(&mut self) {
        let usbsts = self.read_usbsts();
        let iman = self.read_iman(INTERRUPTER_INDEX);

        for action in interrupt_ack_actions(usbsts, iman) {
            match action {
                Some(InterruptAck::UsbStatus(value)) => self.write_usbsts(value),
                Some(InterruptAck::Iman(value)) => self.write_iman(INTERRUPTER_INDEX, value),
                None => {}
            }
        }

        #[cfg(feature = "visualize-input")]
        crate::input_trace::record_interrupt_notify();

        self.process_events();
    }

    fn stop_controller(&mut self) -> Result<(), XhciControllerInitError> {
        info!("[xHCI] Stopping controller...");
        let usbcmd = self.read_usbcmd();
        self.write_usbcmd(usbcmd & !registers::usbcmd::RUN_STOP);
        self.poll_until("stop_controller", || {
            (self.read_usbsts() & registers::usbsts::HCH) != 0
        })
    }

    fn reset_controller(&mut self) -> Result<(), XhciControllerInitError> {
        info!("[xHCI] Resetting controller...");
        let usbcmd = self.read_usbcmd();
        self.write_usbcmd(usbcmd | registers::usbcmd::HCRST);
        self.poll_until("reset_controller", || {
            (self.read_usbcmd() & registers::usbcmd::HCRST) == 0
                && (self.read_usbsts() & registers::usbsts::CNR) == 0
        })
    }

    fn configure_controller(&mut self) -> Result<(), XhciControllerInitError> {
        let max_slots = registers::hcsparams1::max_slots(self.hcsparams1);
        if max_slots == 0 {
            return Err(XhciControllerInitError::ZeroMaxSlots);
        }

        info!(
            "[xHCI] Configuring: max_slots={}, max_ports={}, context_size={}, scratchpad_buffers={}, page_size={}",
            max_slots,
            registers::hcsparams1::max_ports(self.hcsparams1),
            registers::hccparams1::context_size(self.hccparams1),
            registers::hcsparams2::max_scratchpad_buffers(self.hcsparams2),
            self.page_size
        );

        let config = config_with_max_slots(self.read_config(), max_slots);
        self.write_config(config);
        self.write_dcbaap(self.resources().dcbaa.phys_addr());
        Ok(())
    }

    fn setup_command_ring(&mut self) -> Result<(), XhciControllerInitError> {
        let crcr = command_ring_control_value(
            self.resources().command_ring.phys_addr(),
            self.resources().command_ring.cycle_state(),
        );
        self.write_crcr(crcr);
        info!("[xHCI] Command Ring configured");
        Ok(())
    }

    fn setup_event_ring(&mut self) -> Result<(), XhciControllerInitError> {
        if registers::hcsparams1::max_intrs(self.hcsparams1) == 0 {
            return Err(XhciControllerInitError::NoInterrupters);
        }

        let erst = self.resources().erst.segment_count() as u32;
        let erdp = self.resources().event_ring.dequeue_pointer();
        let erstba = self.resources().erst.phys_addr();

        self.write_erstsz(INTERRUPTER_INDEX, erst);
        self.write_erdp(INTERRUPTER_INDEX, erdp);
        self.write_erstba(INTERRUPTER_INDEX, erstba);
        info!("[xHCI] Event Ring configured");
        Ok(())
    }

    fn enable_interrupts(&mut self) -> Result<(), XhciControllerInitError> {
        if registers::hcsparams1::max_intrs(self.hcsparams1) == 0 {
            return Err(XhciControllerInitError::NoInterrupters);
        }

        let iman = self.read_iman(INTERRUPTER_INDEX);
        self.write_iman(INTERRUPTER_INDEX, iman_interrupt_enable_value(iman));

        let usbcmd = self.read_usbcmd();
        self.write_usbcmd(usbcmd | registers::usbcmd::INTE);
        Ok(())
    }

    fn start_controller(&mut self) -> Result<(), XhciControllerInitError> {
        let usbcmd = self.read_usbcmd();
        self.write_usbcmd(usbcmd | registers::usbcmd::RUN_STOP);
        self.poll_until("start_controller", || {
            (self.read_usbsts() & registers::usbsts::HCH) == 0
        })?;
        info!("[xHCI] Controller started successfully");
        Ok(())
    }

    fn best_effort_quiesce(&mut self) {
        if registers::hcsparams1::max_intrs(self.hcsparams1) > 0 {
            let iman = self.read_iman(INTERRUPTER_INDEX);
            self.write_iman(INTERRUPTER_INDEX, iman_interrupt_disable_value(iman));
        }

        let usbcmd = self.read_usbcmd();
        self.write_usbcmd(usbcmd & !(registers::usbcmd::INTE | registers::usbcmd::RUN_STOP));
    }

    fn process_events(&mut self) {
        while let Some(trb) = { self.resources_mut().event_ring.dequeue() } {
            self.handle_event(Event::from_trb(trb));
        }

        let erdp = erdp_dequeue_pointer_value(self.resources().event_ring.dequeue_pointer());
        self.write_erdp(INTERRUPTER_INDEX, erdp);
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::CommandCompletion {
                trb_pointer,
                completion_code,
                slot_id,
            } => {
                if let Err(err) = self
                    .resources_mut()
                    .command_ring
                    .complete_through(trb_pointer)
                {
                    crate::warn!(
                        "[xHCI] Failed to reclaim command TRB 0x{:X}: {}",
                        trb_pointer,
                        err
                    );
                }

                if !completion_is_nonfatal(completion_code) {
                    crate::warn!(
                        "[xHCI] Command completion failed: slot={}, trb=0x{:X}, code={:?}",
                        slot_id,
                        trb_pointer,
                        completion_code
                    );
                }

                self.record_command_completion(trb_pointer, completion_code, slot_id);
            }
            Event::PortStatusChange { port_id } => {
                info!("[xHCI] Port {} status changed", port_id);
                self.record_port_change(port_id);
            }
            Event::TransferEvent {
                trb_pointer,
                completion_code,
                transfer_length,
                slot_id,
                endpoint_id,
            } => {
                #[cfg(feature = "visualize-input")]
                {
                    crate::input_trace::record_transfer_event(
                        slot_id,
                        endpoint_id,
                        trb_pointer,
                        completion_code,
                        transfer_length,
                    );
                    crate::input_trace::record_event_ring_os_read(slot_id, endpoint_id);
                }

                if !completion_is_nonfatal(completion_code) {
                    crate::warn!(
                        "[xHCI] Transfer event failed: slot={}, ep={}, trb=0x{:X}, remaining={}, code={:?}",
                        slot_id,
                        endpoint_id,
                        trb_pointer,
                        transfer_length,
                        completion_code
                    );
                }

                self.record_transfer_completion(
                    trb_pointer,
                    completion_code,
                    transfer_length,
                    slot_id,
                    endpoint_id,
                );
            }
            Event::Unknown { trb_type, .. } => {
                crate::warn!("[xHCI] Unhandled event TRB type {}", trb_type);
            }
        }
    }

    fn poll_until<F>(
        &self,
        step: &'static str,
        mut predicate: F,
    ) -> Result<(), XhciControllerInitError>
    where
        F: FnMut() -> bool,
    {
        if hpet::is_available() {
            let start = hpet::elapsed_us();
            loop {
                if predicate() {
                    return Ok(());
                }
                if hpet::elapsed_us().wrapping_sub(start) >= TIMEOUT_US {
                    return Err(XhciControllerInitError::Timeout {
                        step,
                        timeout_ms: TIMEOUT_MS,
                    });
                }
                hpet::delay_us(POLL_INTERVAL_US);
            }
        }

        for _ in 0..(TIMEOUT_US / POLL_INTERVAL_US) {
            if predicate() {
                return Ok(());
            }
            pit::udelay(POLL_INTERVAL_US as u32);
        }

        if predicate() {
            return Ok(());
        }

        Err(XhciControllerInitError::Timeout {
            step,
            timeout_ms: TIMEOUT_MS,
        })
    }

    pub(crate) fn resources(&self) -> &XhciControllerResources {
        self.resources
            .as_ref()
            .expect("xHCI controller resources must be retained after init")
    }

    pub(crate) fn resources_mut(&mut self) -> &mut XhciControllerResources {
        self.resources
            .as_mut()
            .expect("xHCI controller resources must be retained after init")
    }

    pub(crate) fn operational_registers(&self) -> *mut OperationalRegisters {
        self.op_virt_base as *mut OperationalRegisters
    }

    pub(crate) fn runtime_registers(&self) -> *mut RuntimeRegisters {
        self.runtime_virt_base as *mut RuntimeRegisters
    }

    fn interrupter_registers(&self, index: usize) -> *mut InterrupterRegisterSet {
        let runtime = self.runtime_registers();
        // SAFETY: runtime_virt_base points to mapped xHCI runtime registers and the caller
        // bounds-checks interrupter availability through HCSPARAMS1 before using index 0.
        unsafe { addr_of_mut!((*runtime).ir[index]) }
    }

    fn read_usbcmd(&self) -> u32 {
        let regs = self.operational_registers();
        // SAFETY: operational registers are MMIO mapped for the lifetime of the controller.
        unsafe { read_volatile(addr_of!((*regs).usbcmd)) }
    }

    fn write_usbcmd(&mut self, value: u32) {
        let regs = self.operational_registers();
        // SAFETY: operational registers are MMIO mapped and this writes a single USBCMD register.
        unsafe { write_volatile(addr_of_mut!((*regs).usbcmd), value) };
    }

    fn read_usbsts(&self) -> u32 {
        let regs = self.operational_registers();
        // SAFETY: operational registers are MMIO mapped for the lifetime of the controller.
        unsafe { read_volatile(addr_of!((*regs).usbsts)) }
    }

    fn write_usbsts(&mut self, value: u32) {
        let regs = self.operational_registers();
        // SAFETY: operational registers are MMIO mapped and USBSTS is acknowledged with W1C bits.
        unsafe { write_volatile(addr_of_mut!((*regs).usbsts), value) };
    }

    fn read_config(&self) -> u32 {
        let regs = self.operational_registers();
        // SAFETY: operational registers are MMIO mapped for the lifetime of the controller.
        unsafe { read_volatile(addr_of!((*regs).config)) }
    }

    fn write_config(&mut self, value: u32) {
        let regs = self.operational_registers();
        // SAFETY: operational registers are MMIO mapped and this writes CONFIG atomically.
        unsafe { write_volatile(addr_of_mut!((*regs).config), value) };
    }

    fn write_crcr(&mut self, value: u64) {
        let regs = self.operational_registers();
        // SAFETY: operational registers are MMIO mapped and CRCR accepts a 64-bit ring pointer.
        unsafe { write_volatile(addr_of_mut!((*regs).crcr), value) };
    }

    fn write_dcbaap(&mut self, value: u64) {
        let regs = self.operational_registers();
        // SAFETY: operational registers are MMIO mapped and DCBAAP accepts a 64-bit pointer.
        unsafe { write_volatile(addr_of_mut!((*regs).dcbaap), value) };
    }

    fn read_iman(&self, index: usize) -> u32 {
        let regs = self.interrupter_registers(index);
        // SAFETY: the selected interrupter register set is within mapped runtime registers.
        unsafe { read_volatile(addr_of!((*regs).iman)) }
    }

    fn write_iman(&mut self, index: usize, value: u32) {
        let regs = self.interrupter_registers(index);
        // SAFETY: the selected interrupter register set is within mapped runtime registers.
        unsafe { write_volatile(addr_of_mut!((*regs).iman), value) };
    }

    fn write_erstsz(&mut self, index: usize, value: u32) {
        let regs = self.interrupter_registers(index);
        // SAFETY: the selected interrupter register set is within mapped runtime registers.
        unsafe { write_volatile(addr_of_mut!((*regs).erstsz), value) };
    }

    fn write_erstba(&mut self, index: usize, value: u64) {
        let regs = self.interrupter_registers(index);
        // SAFETY: the selected interrupter register set is within mapped runtime registers.
        unsafe { write_volatile(addr_of_mut!((*regs).erstba), value) };
    }

    fn write_erdp(&mut self, index: usize, value: u64) {
        let regs = self.interrupter_registers(index);
        // SAFETY: the selected interrupter register set is within mapped runtime registers.
        unsafe { write_volatile(addr_of_mut!((*regs).erdp), value) };
    }
}

fn completion_is_nonfatal(code: CompletionCode) -> bool {
    matches!(code, CompletionCode::Success | CompletionCode::ShortPacket)
}

fn interrupt_ack_actions(usbsts: u32, iman: u32) -> [Option<InterruptAck>; 2] {
    [usbsts_eint_clear_action(usbsts), iman_ip_clear_action(iman)]
}

fn usbsts_eint_clear_action(usbsts: u32) -> Option<InterruptAck> {
    if (usbsts & registers::usbsts::EINT) != 0 {
        Some(InterruptAck::UsbStatus(registers::usbsts::EINT))
    } else {
        None
    }
}

fn iman_ip_clear_action(iman: u32) -> Option<InterruptAck> {
    if (iman & registers::iman::IP) != 0 {
        Some(InterruptAck::Iman(iman_interrupt_pending_clear_value(iman)))
    } else {
        None
    }
}

const fn iman_interrupt_enable_value(current: u32) -> u32 {
    registers::iman::IE | (current & registers::iman::IP)
}

const fn iman_interrupt_disable_value(current: u32) -> u32 {
    current & registers::iman::IP
}

const fn iman_interrupt_pending_clear_value(current: u32) -> u32 {
    (current & registers::iman::IE) | registers::iman::IP
}

const fn erdp_dequeue_pointer_value(dequeue_pointer: u64) -> u64 {
    dequeue_pointer | registers::erdp::EHB
}

pub(super) const fn config_with_max_slots(current: u32, max_slots: u8) -> u32 {
    (current & !registers::config::MAX_SLOTS_EN_MASK)
        | (max_slots as u32 & registers::config::MAX_SLOTS_EN_MASK)
}

pub(super) const fn command_ring_control_value(ring_phys_addr: u64, cycle_state: bool) -> u64 {
    ring_phys_addr | if cycle_state { registers::crcr::RCS } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::{
        InterruptAck, command_ring_control_value, config_with_max_slots,
        erdp_dequeue_pointer_value, iman_interrupt_disable_value, iman_interrupt_enable_value,
        iman_interrupt_pending_clear_value, interrupt_ack_actions,
    };
    use crate::usb::xhci::registers;

    #[test_case]
    fn test_config_with_max_slots_preserves_upper_bits() {
        let current = 0xABCD_1200;
        assert_eq!(config_with_max_slots(current, 0x34), 0xABCD_1234);
    }

    #[test_case]
    fn test_command_ring_control_value_sets_cycle_bit_only_when_requested() {
        assert_eq!(
            command_ring_control_value(0x1234_5000, true),
            0x1234_5000 | registers::crcr::RCS
        );
        assert_eq!(command_ring_control_value(0x1234_5000, false), 0x1234_5000);
    }

    #[test_case]
    fn test_interrupt_ack_actions_clear_usbsts_before_iman() {
        let actions = interrupt_ack_actions(
            registers::usbsts::EINT,
            registers::iman::IE | registers::iman::IP,
        );

        assert_eq!(
            actions[0],
            Some(InterruptAck::UsbStatus(registers::usbsts::EINT))
        );
        assert_eq!(
            actions[1],
            Some(InterruptAck::Iman(
                registers::iman::IE | registers::iman::IP
            ))
        );
    }

    #[test_case]
    fn test_iman_interrupt_pending_clear_value_preserves_ie_only() {
        assert_eq!(
            iman_interrupt_pending_clear_value(
                registers::iman::IE | registers::iman::IP | 0x8000_0000
            ),
            registers::iman::IE | registers::iman::IP
        );
    }

    #[test_case]
    fn test_iman_interrupt_enable_and_disable_values_do_not_echo_reserved_bits() {
        assert_eq!(
            iman_interrupt_enable_value(registers::iman::IP | 0x8000_0000),
            registers::iman::IE | registers::iman::IP
        );
        assert_eq!(
            iman_interrupt_disable_value(registers::iman::IE | registers::iman::IP | 0x8000_0000),
            registers::iman::IP
        );
    }

    #[test_case]
    fn test_erdp_dequeue_pointer_value_sets_ehb() {
        assert_eq!(
            erdp_dequeue_pointer_value(0x1234_5000),
            0x1234_5000 | registers::erdp::EHB
        );
    }
}
