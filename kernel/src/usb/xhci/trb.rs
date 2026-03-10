//! xHCI Transfer Request Block definitions.

const CYCLE_BIT: u32 = 1 << 0;
const TOGGLE_CYCLE_BIT: u32 = 1 << 1;
const CHAIN_BIT: u32 = 1 << 4;
const INTERRUPT_ON_COMPLETION_BIT: u32 = 1 << 5;
const IMMEDIATE_DATA_BIT: u32 = 1 << 6;
const ADDRESS_DEVICE_BSR_BIT: u32 = 1 << 9;
const TRB_TYPE_SHIFT: u32 = 10;
const TRB_TYPE_MASK: u32 = 0x3f << TRB_TYPE_SHIFT;
const DIRECTION_IN_BIT: u32 = 1 << 16;
const SETUP_TRANSFER_TYPE_SHIFT: u32 = 16;
const SETUP_TRANSFER_TYPE_MASK: u32 = 0x3 << SETUP_TRANSFER_TYPE_SHIFT;
const SLOT_ID_SHIFT: u32 = 24;
const SLOT_ID_MASK: u32 = 0xff << SLOT_ID_SHIFT;
const TRANSFER_LENGTH_MASK: u32 = 0x1ffff;

pub mod trb_type {
    pub const NORMAL: u8 = 1;
    pub const SETUP_STAGE: u8 = 2;
    pub const DATA_STAGE: u8 = 3;
    pub const STATUS_STAGE: u8 = 4;
    pub const LINK: u8 = 6;
    pub const ENABLE_SLOT: u8 = 9;
    pub const DISABLE_SLOT: u8 = 10;
    pub const ADDRESS_DEVICE: u8 = 11;
    pub const CONFIGURE_ENDPOINT: u8 = 12;
    pub const EVALUATE_CONTEXT: u8 = 13;
    pub const TRANSFER_EVENT: u8 = 32;
    pub const COMMAND_COMPLETION_EVENT: u8 = 33;
    pub const PORT_STATUS_CHANGE_EVENT: u8 = 34;
}

pub mod setup_transfer_type {
    pub const NO_DATA_STAGE: u8 = 0;
    pub const OUT_DATA_STAGE: u8 = 2;
    pub const IN_DATA_STAGE: u8 = 3;
}

#[repr(C, align(16))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Trb {
    parameter: u64,
    status: u32,
    control: u32,
}

impl Trb {
    pub fn parameter(&self) -> u64 {
        self.parameter
    }

    pub fn set_parameter(&mut self, value: u64) {
        self.parameter = value;
    }

    pub fn status(&self) -> u32 {
        self.status
    }

    pub fn set_status(&mut self, value: u32) {
        self.status = value;
    }

    pub fn control(&self) -> u32 {
        self.control
    }

    pub fn set_control(&mut self, value: u32) {
        self.control = value;
    }

    pub fn cycle_bit(&self) -> bool {
        (self.control & CYCLE_BIT) != 0
    }

    pub fn set_cycle_bit(&mut self, value: bool) {
        self.update_control_bit(CYCLE_BIT, value);
    }

    pub fn toggle_cycle(&self) -> bool {
        (self.control & TOGGLE_CYCLE_BIT) != 0
    }

    pub fn set_toggle_cycle(&mut self, value: bool) {
        self.update_control_bit(TOGGLE_CYCLE_BIT, value);
    }

    pub fn chain_bit(&self) -> bool {
        (self.control & CHAIN_BIT) != 0
    }

    pub fn set_chain_bit(&mut self, value: bool) {
        self.update_control_bit(CHAIN_BIT, value);
    }

    pub fn ioc(&self) -> bool {
        (self.control & INTERRUPT_ON_COMPLETION_BIT) != 0
    }

    pub fn set_ioc(&mut self, value: bool) {
        self.update_control_bit(INTERRUPT_ON_COMPLETION_BIT, value);
    }

    pub fn immediate_data(&self) -> bool {
        (self.control & IMMEDIATE_DATA_BIT) != 0
    }

    pub fn set_immediate_data(&mut self, value: bool) {
        self.update_control_bit(IMMEDIATE_DATA_BIT, value);
    }

    pub fn direction_in(&self) -> bool {
        (self.control & DIRECTION_IN_BIT) != 0
    }

    pub fn set_direction_in(&mut self, value: bool) {
        self.update_control_bit(DIRECTION_IN_BIT, value);
    }

    pub fn address_device_bsr(&self) -> bool {
        (self.control & ADDRESS_DEVICE_BSR_BIT) != 0
    }

    pub fn set_address_device_bsr(&mut self, value: bool) {
        self.update_control_bit(ADDRESS_DEVICE_BSR_BIT, value);
    }

    pub fn trb_type(&self) -> u8 {
        ((self.control & TRB_TYPE_MASK) >> TRB_TYPE_SHIFT) as u8
    }

    pub fn set_trb_type(&mut self, value: u8) {
        self.control =
            (self.control & !TRB_TYPE_MASK) | (u32::from(value & 0x3f) << TRB_TYPE_SHIFT);
    }

    pub fn setup_transfer_type(&self) -> u8 {
        ((self.control & SETUP_TRANSFER_TYPE_MASK) >> SETUP_TRANSFER_TYPE_SHIFT) as u8
    }

    pub fn set_setup_transfer_type(&mut self, value: u8) {
        self.control = (self.control & !SETUP_TRANSFER_TYPE_MASK)
            | (u32::from(value & 0x03) << SETUP_TRANSFER_TYPE_SHIFT);
    }

    pub fn slot_id(&self) -> u8 {
        ((self.control & SLOT_ID_MASK) >> SLOT_ID_SHIFT) as u8
    }

    pub fn set_slot_id(&mut self, value: u8) {
        self.control = (self.control & !SLOT_ID_MASK) | (u32::from(value) << SLOT_ID_SHIFT);
    }

    pub fn transfer_length(&self) -> u32 {
        self.status & TRANSFER_LENGTH_MASK
    }

    pub fn set_transfer_length(&mut self, value: u32) {
        self.status = (self.status & !TRANSFER_LENGTH_MASK) | (value & TRANSFER_LENGTH_MASK);
    }

    fn update_control_bit(&mut self, mask: u32, value: bool) {
        if value {
            self.control |= mask;
        } else {
            self.control &= !mask;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CHAIN_BIT, CYCLE_BIT, DIRECTION_IN_BIT, IMMEDIATE_DATA_BIT, INTERRUPT_ON_COMPLETION_BIT,
        TOGGLE_CYCLE_BIT, TRB_TYPE_MASK, TRB_TYPE_SHIFT, Trb, setup_transfer_type, trb_type,
    };
    use core::mem::{align_of, size_of};

    #[test_case]
    fn test_trb_layout() {
        assert_eq!(size_of::<Trb>(), 16);
        assert_eq!(align_of::<Trb>(), 16);
    }

    #[test_case]
    fn test_trb_default_is_zeroed() {
        let trb = Trb::default();
        assert_eq!(trb.parameter(), 0);
        assert_eq!(trb.status(), 0);
        assert_eq!(trb.control(), 0);
    }

    #[test_case]
    fn test_trb_field_round_trip() {
        let mut trb = Trb::default();
        trb.set_parameter(0x1122_3344_5566_7788);
        trb.set_status(0xAABB_CCDD);
        trb.set_control(0x1234_5678);

        assert_eq!(trb.parameter(), 0x1122_3344_5566_7788);
        assert_eq!(trb.status(), 0xAABB_CCDD);
        assert_eq!(trb.control(), 0x1234_5678);
    }

    #[test_case]
    fn test_trb_control_bit_helpers() {
        let mut trb = Trb::default();
        trb.set_cycle_bit(true);
        trb.set_toggle_cycle(true);
        trb.set_chain_bit(true);
        trb.set_ioc(true);
        trb.set_immediate_data(true);
        trb.set_direction_in(true);
        trb.set_trb_type(0x21);

        assert!(trb.cycle_bit());
        assert!(trb.toggle_cycle());
        assert!(trb.chain_bit());
        assert!(trb.ioc());
        assert!(trb.immediate_data());
        assert!(trb.direction_in());
        assert_eq!(trb.trb_type(), 0x21);
        assert_eq!(trb.control() & CYCLE_BIT, CYCLE_BIT);
        assert_eq!(trb.control() & TOGGLE_CYCLE_BIT, TOGGLE_CYCLE_BIT);
        assert_eq!(trb.control() & CHAIN_BIT, CHAIN_BIT);
        assert_eq!(
            trb.control() & INTERRUPT_ON_COMPLETION_BIT,
            INTERRUPT_ON_COMPLETION_BIT
        );
        assert_eq!(trb.control() & IMMEDIATE_DATA_BIT, IMMEDIATE_DATA_BIT);
        assert_eq!(trb.control() & DIRECTION_IN_BIT, DIRECTION_IN_BIT);
        assert_eq!(trb.control() & TRB_TYPE_MASK, 0x21u32 << TRB_TYPE_SHIFT);

        trb.set_cycle_bit(false);
        trb.set_toggle_cycle(false);
        trb.set_chain_bit(false);
        trb.set_ioc(false);
        trb.set_immediate_data(false);
        trb.set_direction_in(false);
        trb.set_trb_type(0);

        assert!(!trb.cycle_bit());
        assert!(!trb.toggle_cycle());
        assert!(!trb.chain_bit());
        assert!(!trb.ioc());
        assert!(!trb.immediate_data());
        assert!(!trb.direction_in());
        assert_eq!(trb.trb_type(), 0);
    }

    #[test_case]
    fn test_trb_slot_id_setup_type_and_transfer_length_helpers() {
        let mut trb = Trb::default();
        trb.set_trb_type(trb_type::SETUP_STAGE);
        trb.set_slot_id(7);
        trb.set_setup_transfer_type(setup_transfer_type::IN_DATA_STAGE);
        trb.set_transfer_length(8);

        assert_eq!(trb.trb_type(), trb_type::SETUP_STAGE);
        assert_eq!(trb.slot_id(), 7);
        assert_eq!(
            trb.setup_transfer_type(),
            setup_transfer_type::IN_DATA_STAGE
        );
        assert_eq!(trb.transfer_length(), 8);
    }
}
