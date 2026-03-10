//! xHCI event definitions and software event queue.

use alloc::vec::Vec;

use super::trb::{Trb, trb_type};

const COMPLETION_CODE_SHIFT: u32 = 24;
const COMPLETION_CODE_MASK: u32 = 0xff;
const TRANSFER_LENGTH_MASK: u32 = 0x00ff_ffff;
const ENDPOINT_ID_SHIFT: u32 = 16;
const ENDPOINT_ID_MASK: u32 = 0x1f;
const SLOT_ID_SHIFT: u32 = 24;
const SLOT_ID_MASK: u32 = 0xff;
const PORT_ID_SHIFT: u32 = 24;
const PORT_ID_MASK: u64 = 0xff;

pub(crate) const PENDING_EVENT_CAPACITY: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionCode {
    Success,
    DataBufferError,
    BabbleDetected,
    UsbTransactionError,
    TrbError,
    StallError,
    ShortPacket,
    Unknown(u8),
}

impl CompletionCode {
    #[inline]
    pub const fn from_raw(value: u8) -> Self {
        match value {
            1 => Self::Success,
            2 => Self::DataBufferError,
            3 => Self::BabbleDetected,
            4 => Self::UsbTransactionError,
            5 => Self::TrbError,
            6 => Self::StallError,
            13 => Self::ShortPacket,
            other => Self::Unknown(other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    CommandCompletion {
        trb_pointer: u64,
        completion_code: CompletionCode,
        slot_id: u8,
    },
    PortStatusChange {
        port_id: u8,
    },
    TransferEvent {
        trb_pointer: u64,
        completion_code: CompletionCode,
        transfer_length: u32,
        slot_id: u8,
        endpoint_id: u8,
    },
    Unknown {
        trb_type: u8,
        raw: Trb,
    },
}

impl Event {
    #[inline]
    pub fn from_trb(trb: Trb) -> Self {
        match trb.trb_type() {
            trb_type::COMMAND_COMPLETION_EVENT => Self::CommandCompletion {
                trb_pointer: trb.parameter(),
                completion_code: completion_code_from_status(trb.status()),
                slot_id: slot_id_from_control(trb.control()),
            },
            trb_type::PORT_STATUS_CHANGE_EVENT => Self::PortStatusChange {
                port_id: port_id_from_parameter(trb.parameter()),
            },
            trb_type::TRANSFER_EVENT => Self::TransferEvent {
                trb_pointer: trb.parameter(),
                completion_code: completion_code_from_status(trb.status()),
                transfer_length: transfer_length_from_status(trb.status()),
                slot_id: slot_id_from_control(trb.control()),
                endpoint_id: endpoint_id_from_control(trb.control()),
            },
            other => Self::Unknown {
                trb_type: other,
                raw: trb,
            },
        }
    }
}

pub(crate) type PendingEventQueue = EventQueue<PENDING_EVENT_CAPACITY>;

pub(crate) struct EventQueue<const N: usize> {
    entries: Vec<Option<Event>>,
    head: usize,
    tail: usize,
    len: usize,
    overflowed: bool,
}

impl<const N: usize> EventQueue<N> {
    pub fn new() -> Self {
        let mut entries = Vec::with_capacity(N);
        entries.resize(N, None);

        Self {
            entries,
            head: 0,
            tail: 0,
            len: 0,
            overflowed: false,
        }
    }

    pub fn push(&mut self, event: Event) -> bool {
        if self.len == N {
            self.overflowed = true;
            return false;
        }

        self.entries[self.tail] = Some(event);
        self.tail = (self.tail + 1) % N;
        self.len += 1;
        true
    }

    pub fn pop(&mut self) -> Option<Event> {
        if self.len == 0 {
            return None;
        }

        let event = self.entries[self.head].take();
        self.head = (self.head + 1) % N;
        self.len -= 1;
        event
    }

    pub fn take_overflow_flag(&mut self) -> bool {
        let overflowed = self.overflowed;
        self.overflowed = false;
        overflowed
    }
}

impl<const N: usize> Default for EventQueue<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn completion_code_from_status(status: u32) -> CompletionCode {
    CompletionCode::from_raw(((status >> COMPLETION_CODE_SHIFT) & COMPLETION_CODE_MASK) as u8)
}

#[inline]
fn transfer_length_from_status(status: u32) -> u32 {
    status & TRANSFER_LENGTH_MASK
}

#[inline]
fn endpoint_id_from_control(control: u32) -> u8 {
    ((control >> ENDPOINT_ID_SHIFT) & ENDPOINT_ID_MASK) as u8
}

#[inline]
fn slot_id_from_control(control: u32) -> u8 {
    ((control >> SLOT_ID_SHIFT) & SLOT_ID_MASK) as u8
}

#[inline]
fn port_id_from_parameter(parameter: u64) -> u8 {
    ((parameter >> PORT_ID_SHIFT) & PORT_ID_MASK) as u8
}

#[cfg(test)]
mod tests {
    use super::{CompletionCode, Event, EventQueue};
    use crate::usb::xhci::trb::{Trb, trb_type};

    fn trb_with_type(trb_type_value: u8) -> Trb {
        let mut trb = Trb::default();
        trb.set_trb_type(trb_type_value);
        trb
    }

    #[test_case]
    fn test_parse_command_completion_event() {
        let mut trb = trb_with_type(trb_type::COMMAND_COMPLETION_EVENT);
        trb.set_parameter(0x1234_5000);
        trb.set_status(1u32 << 24);
        trb.set_control((3u32 << 24) | (u32::from(trb_type::COMMAND_COMPLETION_EVENT) << 10));

        assert_eq!(
            Event::from_trb(trb),
            Event::CommandCompletion {
                trb_pointer: 0x1234_5000,
                completion_code: CompletionCode::Success,
                slot_id: 3,
            }
        );
    }

    #[test_case]
    fn test_parse_port_status_change_event() {
        let mut trb = trb_with_type(trb_type::PORT_STATUS_CHANGE_EVENT);
        trb.set_parameter(7u64 << 24);

        assert_eq!(Event::from_trb(trb), Event::PortStatusChange { port_id: 7 });
    }

    #[test_case]
    fn test_parse_transfer_event() {
        let mut trb = trb_with_type(trb_type::TRANSFER_EVENT);
        trb.set_parameter(0xCAFE_BABE_1234_5000);
        trb.set_status((13u32 << 24) | 0x12_3456);
        trb.set_control((5u32 << 24) | (2u32 << 16) | (u32::from(trb_type::TRANSFER_EVENT) << 10));

        assert_eq!(
            Event::from_trb(trb),
            Event::TransferEvent {
                trb_pointer: 0xCAFE_BABE_1234_5000,
                completion_code: CompletionCode::ShortPacket,
                transfer_length: 0x12_3456,
                slot_id: 5,
                endpoint_id: 2,
            }
        );
    }

    #[test_case]
    fn test_parse_unknown_event() {
        let mut trb = trb_with_type(63);
        trb.set_parameter(0xDEAD_BEEF);
        trb.set_status(0xABCD_EF01);
        trb.set_control((9u32 << 24) | (63u32 << 10) | 1);

        assert_eq!(
            Event::from_trb(trb),
            Event::Unknown {
                trb_type: 63,
                raw: trb
            }
        );
    }

    #[test_case]
    fn test_event_queue_preserves_push_pop_order() {
        let mut queue = EventQueue::<4>::new();
        let first = Event::PortStatusChange { port_id: 1 };
        let second = Event::PortStatusChange { port_id: 2 };
        let third = Event::PortStatusChange { port_id: 3 };

        assert!(queue.push(first));
        assert!(queue.push(second));
        assert!(queue.push(third));

        assert_eq!(queue.pop(), Some(first));
        assert_eq!(queue.pop(), Some(second));
        assert_eq!(queue.pop(), Some(third));
        assert_eq!(queue.pop(), None);
    }

    #[test_case]
    fn test_event_queue_wraps_around() {
        let mut queue = EventQueue::<3>::new();
        let first = Event::PortStatusChange { port_id: 1 };
        let second = Event::PortStatusChange { port_id: 2 };
        let third = Event::PortStatusChange { port_id: 3 };

        assert!(queue.push(first));
        assert!(queue.push(second));
        assert_eq!(queue.pop(), Some(first));
        assert!(queue.push(third));

        assert_eq!(queue.pop(), Some(second));
        assert_eq!(queue.pop(), Some(third));
        assert_eq!(queue.pop(), None);
    }

    #[test_case]
    fn test_event_queue_overflow_flag_is_sticky_until_taken() {
        let mut queue = EventQueue::<2>::new();

        assert!(queue.push(Event::PortStatusChange { port_id: 1 }));
        assert!(queue.push(Event::PortStatusChange { port_id: 2 }));
        assert!(!queue.push(Event::PortStatusChange { port_id: 3 }));

        assert!(queue.take_overflow_flag());
        assert!(!queue.take_overflow_flag());
    }
}
