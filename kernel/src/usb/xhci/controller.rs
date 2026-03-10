//! xHCI controller initialization sequence.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use crate::{hpet, info, pit};

use super::{
    XhciController,
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
        self.write_iman(INTERRUPTER_INDEX, iman | registers::iman::IE);

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
            self.write_iman(INTERRUPTER_INDEX, iman & !registers::iman::IE);
        }

        let usbcmd = self.read_usbcmd();
        self.write_usbcmd(usbcmd & !(registers::usbcmd::INTE | registers::usbcmd::RUN_STOP));
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

    fn resources(&self) -> &XhciControllerResources {
        self.resources
            .as_ref()
            .expect("xHCI controller resources must be retained after init")
    }

    fn operational_registers(&self) -> *mut OperationalRegisters {
        self.op_virt_base as *mut OperationalRegisters
    }

    fn runtime_registers(&self) -> *mut RuntimeRegisters {
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

pub(super) const fn config_with_max_slots(current: u32, max_slots: u8) -> u32 {
    (current & !registers::config::MAX_SLOTS_EN_MASK)
        | (max_slots as u32 & registers::config::MAX_SLOTS_EN_MASK)
}

pub(super) const fn command_ring_control_value(ring_phys_addr: u64, cycle_state: bool) -> u64 {
    ring_phys_addr | if cycle_state { registers::crcr::RCS } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::{command_ring_control_value, config_with_max_slots};
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
}
