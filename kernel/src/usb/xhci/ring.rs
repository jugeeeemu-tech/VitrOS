//! xHCI ring buffer primitives.

use core::fmt;
use core::mem::size_of;
use core::num::NonZeroU16;
use core::slice;

use crate::dma::{DmaBuffer, DmaError};

use super::dma::XhciDmaProfile;
use super::trb::{Trb, trb_type};

const TRB_SIZE: usize = size_of::<Trb>();
const PRODUCER_RING_SNAPSHOT_SLOTS: usize = 32;
const CONSUMER_RING_SNAPSHOT_SLOTS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingError {
    InvalidCapacity,
    SizeOverflow,
    RingFull,
    InvalidTrbPointer,
    Dma(DmaError),
}

impl fmt::Display for RingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RingError::InvalidCapacity => write!(f, "invalid xHCI ring capacity"),
            RingError::SizeOverflow => write!(f, "xHCI ring size calculation overflowed"),
            RingError::RingFull => write!(f, "xHCI ring is full"),
            RingError::InvalidTrbPointer => write!(f, "TRB pointer is not owned by this ring"),
            RingError::Dma(err) => write!(f, "xHCI ring DMA allocation failed: {}", err),
        }
    }
}

impl From<DmaError> for RingError {
    fn from(err: DmaError) -> Self {
        Self::Dma(err)
    }
}

struct RingStorage {
    buffer: DmaBuffer,
    trb_count: usize,
}

impl RingStorage {
    fn new(trb_count: usize, dma: &XhciDmaProfile) -> Result<Self, RingError> {
        let size = trb_count
            .checked_mul(TRB_SIZE)
            .ok_or(RingError::SizeOverflow)?;
        let buffer = dma.allocate_ring(size).map_err(RingError::Dma)?;

        Ok(Self { buffer, trb_count })
    }

    fn len(&self) -> usize {
        self.trb_count
    }

    fn phys_addr(&self) -> u64 {
        self.buffer.phys_addr()
    }

    fn trb_phys_addr(&self, index: usize) -> u64 {
        debug_assert!(index < self.trb_count);
        self.phys_addr() + (index as u64 * TRB_SIZE as u64)
    }

    fn read(&self, index: usize) -> Trb {
        debug_assert!(index < self.trb_count);
        self.as_slice()[index]
    }

    fn write(&mut self, index: usize, trb: Trb) {
        debug_assert!(index < self.trb_count);
        self.as_mut_slice()[index] = trb;
    }

    fn as_slice(&self) -> &[Trb] {
        // SAFETY: xHCI ring allocations are 64-byte aligned by the DMA facade,
        // which is sufficient for `Trb` alignment. The owned DMA buffer stores
        // exactly `trb_count` contiguous TRBs for the lifetime of `self`.
        unsafe { slice::from_raw_parts(self.buffer.as_ptr().cast::<Trb>(), self.trb_count) }
    }

    fn as_mut_slice(&mut self) -> &mut [Trb] {
        // SAFETY: the DMA buffer is exclusively borrowed via `&mut self`, and
        // contains exactly `trb_count` contiguous TRB slots.
        unsafe { slice::from_raw_parts_mut(self.buffer.as_mut_ptr_t::<Trb>(), self.trb_count) }
    }
}

pub struct ProducerRing {
    storage: RingStorage,
    enqueue_index: usize,
    reclaim_index: usize,
    outstanding: usize,
    producer_cycle_state: bool,
    usable_capacity: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducerRingSnapshot {
    pub phys_addr: u64,
    pub capacity: usize,
    pub enqueue_index: usize,
    pub reclaim_index: usize,
    pub outstanding: usize,
    pub producer_cycle_state: bool,
    pub slot_count: usize,
    pub slots: [RingSlotSnapshot; PRODUCER_RING_SNAPSHOT_SLOTS],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingSlotSnapshot {
    pub index: u16,
    pub occupied: bool,
    pub trb_type: u8,
    pub cycle_bit: bool,
    pub is_enqueue: bool,
    pub is_reclaim: bool,
    pub is_dequeue: bool,
    pub is_recent: bool,
    pub is_error: bool,
    pub is_link: bool,
}

impl RingSlotSnapshot {
    pub const fn empty(index: u16) -> Self {
        Self {
            index,
            occupied: false,
            trb_type: 0,
            cycle_bit: false,
            is_enqueue: false,
            is_reclaim: false,
            is_dequeue: false,
            is_recent: false,
            is_error: false,
            is_link: false,
        }
    }
}

impl ProducerRing {
    pub fn new(capacity: usize, dma: &XhciDmaProfile) -> Result<Self, RingError> {
        if capacity < 2 {
            return Err(RingError::InvalidCapacity);
        }

        let usable_capacity = capacity - 1;
        let mut ring = Self {
            storage: RingStorage::new(capacity, dma)?,
            enqueue_index: 0,
            reclaim_index: 0,
            outstanding: 0,
            producer_cycle_state: true,
            usable_capacity,
        };
        ring.write_link_trb(ring.producer_cycle_state);

        Ok(ring)
    }

    pub fn enqueue(&mut self, mut trb: Trb) -> Result<u64, RingError> {
        if self.outstanding == self.usable_capacity {
            return Err(RingError::RingFull);
        }

        let index = self.enqueue_index;
        trb.set_cycle_bit(self.producer_cycle_state);
        self.storage.write(index, trb);

        self.enqueue_index += 1;
        self.outstanding += 1;

        if self.enqueue_index == self.usable_capacity {
            self.write_link_trb(self.producer_cycle_state);
            self.enqueue_index = 0;
            self.producer_cycle_state = !self.producer_cycle_state;
        }

        Ok(self.storage.trb_phys_addr(index))
    }

    pub fn complete_through(&mut self, trb_phys_addr: u64) -> Result<(), RingError> {
        if self.outstanding == 0 {
            return Err(RingError::InvalidTrbPointer);
        }

        let target_index = self
            .data_index_for_phys_addr(trb_phys_addr)
            .ok_or(RingError::InvalidTrbPointer)?;

        let mut index = self.reclaim_index;
        for release_count in 1..=self.outstanding {
            if index == target_index {
                self.reclaim_index = (target_index + 1) % self.usable_capacity;
                self.outstanding -= release_count;
                return Ok(());
            }
            index = (index + 1) % self.usable_capacity;
        }

        Err(RingError::InvalidTrbPointer)
    }

    pub fn phys_addr(&self) -> u64 {
        self.storage.phys_addr()
    }

    pub fn cycle_state(&self) -> bool {
        self.producer_cycle_state
    }

    pub fn capacity(&self) -> usize {
        self.usable_capacity
    }

    pub fn snapshot(&self) -> ProducerRingSnapshot {
        ProducerRingSnapshot {
            phys_addr: self.phys_addr(),
            capacity: self.capacity(),
            enqueue_index: self.enqueue_index,
            reclaim_index: self.reclaim_index,
            outstanding: self.outstanding,
            producer_cycle_state: self.producer_cycle_state,
            slot_count: self.storage.len().min(PRODUCER_RING_SNAPSHOT_SLOTS),
            slots: producer_slots(self),
        }
    }

    fn link_index(&self) -> usize {
        self.usable_capacity
    }

    fn data_index_for_phys_addr(&self, trb_phys_addr: u64) -> Option<usize> {
        let base = self.phys_addr();
        let offset = trb_phys_addr.checked_sub(base)?;
        if offset % TRB_SIZE as u64 != 0 {
            return None;
        }

        let index = usize::try_from(offset / TRB_SIZE as u64).ok()?;
        if index >= self.usable_capacity {
            return None;
        }

        Some(index)
    }

    fn write_link_trb(&mut self, cycle_state: bool) {
        let mut trb = Trb::default();
        trb.set_parameter(self.phys_addr());
        trb.set_trb_type(trb_type::LINK);
        trb.set_toggle_cycle(true);
        trb.set_cycle_bit(cycle_state);
        self.storage.write(self.link_index(), trb);
    }

    fn is_outstanding_index(&self, index: usize) -> bool {
        if index >= self.usable_capacity {
            return false;
        }

        let mut cursor = self.reclaim_index;
        for _ in 0..self.outstanding {
            if cursor == index {
                return true;
            }
            cursor = (cursor + 1) % self.usable_capacity;
        }
        false
    }
}

pub struct ConsumerRing {
    storage: RingStorage,
    dequeue_index: usize,
    consumer_cycle_state: bool,
    trb_count: NonZeroU16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumerRingSnapshot {
    pub phys_addr: u64,
    pub trb_count: u16,
    pub dequeue_index: usize,
    pub dequeue_pointer: u64,
    pub consumer_cycle_state: bool,
    pub slot_count: usize,
    pub slots: [RingSlotSnapshot; CONSUMER_RING_SNAPSHOT_SLOTS],
}

impl ConsumerRing {
    pub fn new(capacity: usize, dma: &XhciDmaProfile) -> Result<Self, RingError> {
        let trb_count =
            NonZeroU16::new(u16::try_from(capacity).map_err(|_| RingError::InvalidCapacity)?)
                .ok_or(RingError::InvalidCapacity)?;

        Ok(Self {
            storage: RingStorage::new(capacity, dma)?,
            dequeue_index: 0,
            consumer_cycle_state: true,
            trb_count,
        })
    }

    pub fn dequeue(&mut self) -> Option<Trb> {
        let trb = self.storage.read(self.dequeue_index);
        if trb.cycle_bit() != self.consumer_cycle_state {
            return None;
        }

        self.dequeue_index += 1;
        if self.dequeue_index == self.storage.len() {
            self.dequeue_index = 0;
            self.consumer_cycle_state = !self.consumer_cycle_state;
        }

        Some(trb)
    }

    pub fn phys_addr(&self) -> u64 {
        self.storage.phys_addr()
    }

    pub fn dequeue_pointer(&self) -> u64 {
        self.storage.trb_phys_addr(self.dequeue_index)
    }

    pub fn trb_count(&self) -> NonZeroU16 {
        self.trb_count
    }

    pub fn snapshot(&self) -> ConsumerRingSnapshot {
        ConsumerRingSnapshot {
            phys_addr: self.phys_addr(),
            trb_count: self.trb_count.get(),
            dequeue_index: self.dequeue_index,
            dequeue_pointer: self.dequeue_pointer(),
            consumer_cycle_state: self.consumer_cycle_state,
            slot_count: self.storage.len().min(CONSUMER_RING_SNAPSHOT_SLOTS),
            slots: consumer_slots(self),
        }
    }
}

fn producer_slots(ring: &ProducerRing) -> [RingSlotSnapshot; PRODUCER_RING_SNAPSHOT_SLOTS] {
    let mut slots = core::array::from_fn(|index| RingSlotSnapshot::empty(index as u16));
    let count = ring.storage.len().min(PRODUCER_RING_SNAPSHOT_SLOTS);
    for (index, slot) in slots.iter_mut().enumerate().take(count) {
        let trb = ring.storage.read(index);
        let is_link = index == ring.link_index() || trb.trb_type() == trb_type::LINK;
        *slot = RingSlotSnapshot {
            index: index as u16,
            occupied: is_link || ring.is_outstanding_index(index),
            trb_type: trb.trb_type(),
            cycle_bit: trb.cycle_bit(),
            is_enqueue: index == ring.enqueue_index,
            is_reclaim: index == ring.reclaim_index,
            is_dequeue: false,
            is_recent: false,
            is_error: false,
            is_link,
        };
    }
    slots
}

fn consumer_slots(ring: &ConsumerRing) -> [RingSlotSnapshot; CONSUMER_RING_SNAPSHOT_SLOTS] {
    let mut slots = core::array::from_fn(|index| RingSlotSnapshot::empty(index as u16));
    let count = ring.storage.len().min(CONSUMER_RING_SNAPSHOT_SLOTS);
    for (index, slot) in slots.iter_mut().enumerate().take(count) {
        let trb = ring.storage.read(index);
        let expected_cycle = if index < ring.dequeue_index {
            !ring.consumer_cycle_state
        } else {
            ring.consumer_cycle_state
        };
        *slot = RingSlotSnapshot {
            index: index as u16,
            occupied: trb.trb_type() != 0 && trb.cycle_bit() == expected_cycle,
            trb_type: trb.trb_type(),
            cycle_bit: trb.cycle_bit(),
            is_enqueue: false,
            is_reclaim: false,
            is_dequeue: index == ring.dequeue_index,
            is_recent: false,
            is_error: false,
            is_link: false,
        };
    }
    slots
}

#[cfg(test)]
mod tests {
    use super::{ConsumerRing, ProducerRing, RingError};
    use crate::frame_allocator;
    use crate::paging;
    use crate::usb::xhci::dma::XhciDmaProfile;
    use crate::usb::xhci::trb::{Trb, trb_type};
    use core::num::NonZeroU16;
    use vitros_common::boot_info::{BootInfo, MemoryRegion};
    use vitros_common::uefi::EFI_CONVENTIONAL_MEMORY;

    fn boot_info_with_regions(regions: &[MemoryRegion]) -> BootInfo {
        let mut boot_info = BootInfo::new();
        for (i, region) in regions.iter().enumerate() {
            boot_info.memory_map[i] = *region;
        }
        boot_info.memory_map_count = regions.len();
        boot_info
    }

    fn setup_allocator(regions: &[MemoryRegion]) {
        frame_allocator::reset_for_test();
        let boot_info = boot_info_with_regions(regions);
        frame_allocator::init(&boot_info).expect("init failed");
    }

    fn dma_profile() -> XhciDmaProfile {
        XhciDmaProfile::new(true, paging::PAGE_SIZE)
    }

    fn test_region() -> MemoryRegion {
        MemoryRegion {
            start: 0x100000,
            size: 0x200000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }
    }

    fn make_trb(trb_type_value: u8) -> Trb {
        let mut trb = Trb::default();
        trb.set_trb_type(trb_type_value);
        trb
    }

    #[test_case]
    fn test_producer_ring_initializes_link_trb() {
        setup_allocator(&[test_region()]);

        let ring = ProducerRing::new(4, &dma_profile()).expect("producer ring");

        assert_eq!(ring.capacity(), 3);
        assert!(ring.cycle_state());

        let link = ring.storage.read(ring.link_index());
        assert_eq!(link.parameter(), ring.phys_addr());
        assert_eq!(link.trb_type(), trb_type::LINK);
        assert!(link.toggle_cycle());
        assert!(link.cycle_bit());
    }

    #[test_case]
    fn test_producer_ring_enqueue_wrap_and_reclaim() {
        setup_allocator(&[test_region()]);

        let mut ring = ProducerRing::new(5, &dma_profile()).expect("producer ring");
        let base = ring.phys_addr();

        let first = ring
            .enqueue(make_trb(trb_type::ENABLE_SLOT))
            .expect("first");
        let second = ring
            .enqueue(make_trb(trb_type::ADDRESS_DEVICE))
            .expect("second");
        let third = ring
            .enqueue(make_trb(trb_type::CONFIGURE_ENDPOINT))
            .expect("third");
        let fourth = ring.enqueue(make_trb(trb_type::NORMAL)).expect("fourth");

        assert_eq!(first, base);
        assert_eq!(second, base + 16);
        assert_eq!(third, base + 32);
        assert_eq!(fourth, base + 48);
        assert!(!ring.cycle_state());
        assert_eq!(ring.storage.read(ring.link_index()).cycle_bit(), true);

        ring.complete_through(second)
            .expect("reclaim through second");
        let wrapped = ring
            .enqueue(make_trb(trb_type::TRANSFER_EVENT))
            .expect("wrapped enqueue");

        assert_eq!(wrapped, base);
        assert!(!ring.storage.read(0).cycle_bit());
        assert_eq!(ring.storage.read(0).trb_type(), trb_type::TRANSFER_EVENT);
    }

    #[test_case]
    fn test_producer_ring_full_keeps_existing_trbs() {
        setup_allocator(&[test_region()]);

        let mut ring = ProducerRing::new(3, &dma_profile()).expect("producer ring");
        ring.enqueue(make_trb(trb_type::ENABLE_SLOT))
            .expect("first");
        ring.enqueue(make_trb(trb_type::ADDRESS_DEVICE))
            .expect("second");

        assert!(matches!(
            ring.enqueue(make_trb(trb_type::CONFIGURE_ENDPOINT)),
            Err(RingError::RingFull)
        ));
        assert_eq!(ring.storage.read(0).trb_type(), trb_type::ENABLE_SLOT);
        assert_eq!(ring.storage.read(1).trb_type(), trb_type::ADDRESS_DEVICE);
    }

    #[test_case]
    fn test_producer_ring_rejects_invalid_completion_pointer() {
        setup_allocator(&[test_region()]);

        let mut ring = ProducerRing::new(4, &dma_profile()).expect("producer ring");
        let first = ring
            .enqueue(make_trb(trb_type::ENABLE_SLOT))
            .expect("first");

        assert!(matches!(
            ring.complete_through(first + 8),
            Err(RingError::InvalidTrbPointer)
        ));
        assert!(matches!(
            ring.complete_through(ring.storage.trb_phys_addr(ring.link_index())),
            Err(RingError::InvalidTrbPointer)
        ));

        ring.complete_through(first).expect("complete first");
        assert!(matches!(
            ring.complete_through(first),
            Err(RingError::InvalidTrbPointer)
        ));
    }

    #[test_case]
    fn test_producer_ring_rejects_too_small_capacity() {
        setup_allocator(&[test_region()]);

        assert!(matches!(
            ProducerRing::new(1, &dma_profile()),
            Err(RingError::InvalidCapacity)
        ));
    }

    #[test_case]
    fn test_consumer_ring_zeroed_is_empty() {
        setup_allocator(&[test_region()]);

        let mut ring = ConsumerRing::new(4, &dma_profile()).expect("consumer ring");

        assert_eq!(ring.trb_count(), NonZeroU16::new(4).expect("non-zero"));
        assert_eq!(ring.dequeue_pointer(), ring.phys_addr());
        assert!(ring.dequeue().is_none());
    }

    #[test_case]
    fn test_consumer_ring_dequeue_and_wrap() {
        setup_allocator(&[test_region()]);

        let mut ring = ConsumerRing::new(2, &dma_profile()).expect("consumer ring");
        let base = ring.phys_addr();

        let mut first = make_trb(trb_type::TRANSFER_EVENT);
        first.set_cycle_bit(true);
        ring.storage.write(0, first);

        let mut second = make_trb(trb_type::COMMAND_COMPLETION_EVENT);
        second.set_cycle_bit(true);
        ring.storage.write(1, second);

        assert_eq!(ring.dequeue_pointer(), base);
        assert_eq!(
            ring.dequeue().expect("first").trb_type(),
            trb_type::TRANSFER_EVENT
        );
        assert_eq!(ring.dequeue_pointer(), base + 16);
        assert_eq!(
            ring.dequeue().expect("second").trb_type(),
            trb_type::COMMAND_COMPLETION_EVENT
        );
        assert_eq!(ring.dequeue_pointer(), base);
        assert!(!ring.consumer_cycle_state);
        assert!(ring.dequeue().is_none());

        let mut wrapped = make_trb(trb_type::PORT_STATUS_CHANGE_EVENT);
        wrapped.set_cycle_bit(false);
        ring.storage.write(0, wrapped);

        assert_eq!(
            ring.dequeue().expect("wrapped").trb_type(),
            trb_type::PORT_STATUS_CHANGE_EVENT
        );
    }

    #[test_case]
    fn test_consumer_ring_rejects_invalid_capacity() {
        setup_allocator(&[test_region()]);

        assert!(matches!(
            ConsumerRing::new(0, &dma_profile()),
            Err(RingError::InvalidCapacity)
        ));
        assert!(matches!(
            ConsumerRing::new(usize::from(u16::MAX) + 1, &dma_profile()),
            Err(RingError::InvalidCapacity)
        ));
    }

    #[test_case]
    fn test_producer_ring_snapshot_reports_indices_and_outstanding() {
        setup_allocator(&[test_region()]);

        let mut ring = ProducerRing::new(8, &dma_profile()).expect("producer ring");
        let mut first_trb = make_trb(trb_type::NORMAL);
        first_trb.set_transfer_length(8);
        first_trb.set_ioc(true);
        first_trb.set_chain_bit(true);
        let first = ring.enqueue(first_trb).expect("enqueue first");
        let _second = ring
            .enqueue(make_trb(trb_type::ADDRESS_DEVICE))
            .expect("enqueue second");
        ring.complete_through(first).expect("reclaim first");

        let snapshot = ring.snapshot();
        assert_eq!(snapshot.phys_addr, ring.phys_addr());
        assert_eq!(snapshot.capacity, ring.capacity());
        assert_eq!(snapshot.enqueue_index, 2);
        assert_eq!(snapshot.reclaim_index, 1);
        assert_eq!(snapshot.outstanding, 1);
        assert!(snapshot.producer_cycle_state);
        assert_eq!(snapshot.slot_count, 8);
        assert_eq!(snapshot.slots[0].index, 0);
        assert_eq!(snapshot.slots[0].trb_type, trb_type::NORMAL);
        assert!(!snapshot.slots[0].occupied);
        assert!(!snapshot.slots[0].is_enqueue);
        assert!(!snapshot.slots[0].is_reclaim);
        assert_eq!(snapshot.slots[1].index, 1);
        assert_eq!(snapshot.slots[1].trb_type, trb_type::ADDRESS_DEVICE);
        assert!(snapshot.slots[1].occupied);
        assert!(snapshot.slots[1].is_reclaim);
        assert_eq!(snapshot.slots[2].index, 2);
        assert!(snapshot.slots[2].is_enqueue);
    }

    #[test_case]
    fn test_producer_ring_snapshot_includes_link_slot() {
        setup_allocator(&[test_region()]);

        let ring = ProducerRing::new(6, &dma_profile()).expect("producer ring");
        let link = ring.snapshot().slots[5];

        assert_eq!(link.index, 5);
        assert_eq!(link.trb_type, trb_type::LINK);
        assert!(link.occupied);
        assert!(link.is_link);
        assert!(link.cycle_bit);
    }

    #[test_case]
    fn test_consumer_ring_snapshot_reports_pointer_and_cycle_state() {
        setup_allocator(&[test_region()]);

        let mut ring = ConsumerRing::new(4, &dma_profile()).expect("consumer ring");
        let mut first = make_trb(trb_type::TRANSFER_EVENT);
        first.set_parameter(0x1234_5000);
        first.set_status((13u32 << 24) | 7);
        first
            .set_control((5u32 << 24) | (2u32 << 16) | (u32::from(trb_type::TRANSFER_EVENT) << 10));
        first.set_cycle_bit(true);
        ring.storage.write(0, first);

        let mut second = make_trb(trb_type::COMMAND_COMPLETION_EVENT);
        second.set_parameter(0xDEAD_BEEF);
        second.set_status(1u32 << 24);
        second.set_control((3u32 << 24) | (u32::from(trb_type::COMMAND_COMPLETION_EVENT) << 10));
        second.set_cycle_bit(true);
        ring.storage.write(1, second);

        let initial = ring.snapshot();
        assert_eq!(initial.phys_addr, ring.phys_addr());
        assert_eq!(initial.trb_count, 4);
        assert_eq!(initial.dequeue_index, 0);
        assert_eq!(initial.dequeue_pointer, ring.phys_addr());
        assert!(initial.consumer_cycle_state);
        assert_eq!(initial.slot_count, 4);
        assert_eq!(initial.slots[0].index, 0);
        assert_eq!(initial.slots[0].trb_type, trb_type::TRANSFER_EVENT);
        assert!(initial.slots[0].occupied);
        assert!(initial.slots[0].is_dequeue);
        assert_eq!(initial.slots[1].index, 1);
        assert_eq!(
            initial.slots[1].trb_type,
            trb_type::COMMAND_COMPLETION_EVENT
        );
        assert!(initial.slots[1].occupied);

        let _ = ring.dequeue().expect("first");

        let snapshot = ring.snapshot();
        assert_eq!(snapshot.dequeue_index, 1);
        assert_eq!(snapshot.dequeue_pointer, ring.phys_addr() + 16);
        assert!(snapshot.consumer_cycle_state);
        assert!(!snapshot.slots[0].occupied);
        assert!(snapshot.slots[1].is_dequeue);
    }
}
