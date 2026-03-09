//! xHCI control DMA memory structures.
//!
//! This module builds DMA-backed control structures only.

use alloc::vec::Vec;
use core::mem::size_of;
use core::num::NonZeroU16;
use core::slice;

use crate::dma::{DmaBuffer, DmaError};

use super::dma::XhciDmaProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XhciControlMemoryConfig {
    pub max_slots: u8,
    pub scratchpad_buffer_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XhciMemoryError {
    Dma(DmaError),
    SizeOverflow,
    EmptyEventRingSegments,
    ZeroScratchpadCount,
}

impl core::fmt::Display for XhciMemoryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            XhciMemoryError::Dma(err) => write!(f, "xHCI DMA allocation failed: {}", err),
            XhciMemoryError::SizeOverflow => {
                write!(f, "xHCI memory structure size calculation overflowed")
            }
            XhciMemoryError::EmptyEventRingSegments => {
                write!(f, "event ring segment table requires at least one segment")
            }
            XhciMemoryError::ZeroScratchpadCount => {
                write!(f, "scratchpad set requires at least one buffer")
            }
        }
    }
}

impl From<DmaError> for XhciMemoryError {
    fn from(err: DmaError) -> Self {
        Self::Dma(err)
    }
}

pub struct Dcbaa {
    buffer: DmaBuffer,
    entry_count: usize,
}

impl Dcbaa {
    pub fn new(
        dma: &XhciDmaProfile,
        config: &XhciControlMemoryConfig,
    ) -> Result<Self, XhciMemoryError> {
        let entry_count = usize::from(config.max_slots)
            .checked_add(1)
            .ok_or(XhciMemoryError::SizeOverflow)?;
        let size = checked_byte_len(entry_count, size_of::<u64>())?;
        let buffer = dma.allocate_dcbaa(size)?;

        Ok(Self {
            buffer,
            entry_count,
        })
    }

    pub fn phys_addr(&self) -> u64 {
        self.buffer.phys_addr()
    }

    pub fn entry_count(&self) -> usize {
        self.entry_count
    }

    pub fn set_scratchpad_array(&mut self, scratchpad: Option<&ScratchpadSet>) {
        let addr = scratchpad.map_or(0, ScratchpadSet::array_phys_addr);
        self.entries_mut()[0] = addr;
    }

    fn entries(&self) -> &[u64] {
        // SAFETY: DCBAA is allocated with 64-byte alignment and stores exactly
        // `entry_count` u64 entries in this owned buffer.
        unsafe { slice::from_raw_parts(self.buffer.as_ptr().cast::<u64>(), self.entry_count) }
    }

    fn entries_mut(&mut self) -> &mut [u64] {
        // SAFETY: DCBAA is an owned DMA buffer with exclusive `&mut self`
        // access, so writing `entry_count` u64 entries is valid.
        unsafe { slice::from_raw_parts_mut(self.buffer.as_mut_ptr_t::<u64>(), self.entry_count) }
    }
}

pub struct ScratchpadSet {
    array: DmaBuffer,
    buffers: Vec<DmaBuffer>,
}

impl ScratchpadSet {
    pub fn new(dma: &XhciDmaProfile, count: usize) -> Result<Self, XhciMemoryError> {
        if count == 0 {
            return Err(XhciMemoryError::ZeroScratchpadCount);
        }

        let array_size = checked_byte_len(count, size_of::<u64>())?;
        let mut array = dma.allocate_scratchpad_array(array_size)?;
        let mut buffers = Vec::new();
        buffers
            .try_reserve_exact(count)
            .map_err(|_| XhciMemoryError::SizeOverflow)?;

        for index in 0..count {
            let buffer = dma.allocate_scratchpad_buffer()?;
            let phys_addr = buffer.phys_addr();
            Self::array_entries_mut(&mut array, count)[index] = phys_addr;
            buffers.push(buffer);
        }

        Ok(Self { array, buffers })
    }

    pub fn array_phys_addr(&self) -> u64 {
        self.array.phys_addr()
    }

    pub fn buffer_count(&self) -> usize {
        self.buffers.len()
    }

    pub fn buffer_phys_addr(&self, index: usize) -> Option<u64> {
        self.buffers.get(index).map(DmaBuffer::phys_addr)
    }

    fn array_entries(&self) -> &[u64] {
        Self::array_entries_from(&self.array, self.buffers.len())
    }

    fn array_entries_from(array: &DmaBuffer, count: usize) -> &[u64] {
        // SAFETY: the scratchpad array buffer is allocated with 64-byte
        // alignment and stores `count` u64 physical addresses.
        unsafe { slice::from_raw_parts(array.as_ptr().cast::<u64>(), count) }
    }

    fn array_entries_mut(array: &mut DmaBuffer, count: usize) -> &mut [u64] {
        // SAFETY: the scratchpad array buffer is exclusively owned here and
        // contains `count` u64 physical address slots.
        unsafe { slice::from_raw_parts_mut(array.as_mut_ptr_t::<u64>(), count) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventRingSegmentDescriptor {
    pub base_addr: u64,
    pub trb_count: NonZeroU16,
}

pub struct EventRingSegmentTable {
    buffer: DmaBuffer,
    segment_count: usize,
}

impl EventRingSegmentTable {
    pub fn new(
        dma: &XhciDmaProfile,
        segments: &[EventRingSegmentDescriptor],
    ) -> Result<Self, XhciMemoryError> {
        if segments.is_empty() {
            return Err(XhciMemoryError::EmptyEventRingSegments);
        }

        let segment_count = segments.len();
        let size = checked_byte_len(segment_count, size_of::<EventRingSegmentTableEntry>())?;
        let mut buffer = dma.allocate_erst(size)?;

        for (entry, segment) in Self::entries_mut_from(&mut buffer, segment_count)
            .iter_mut()
            .zip(segments.iter())
        {
            *entry = EventRingSegmentTableEntry {
                ring_segment_base_address: segment.base_addr,
                ring_segment_size: segment.trb_count.get(),
                _reserved: [0; 6],
            };
        }

        Ok(Self {
            buffer,
            segment_count,
        })
    }

    pub fn phys_addr(&self) -> u64 {
        self.buffer.phys_addr()
    }

    pub fn segment_count(&self) -> usize {
        self.segment_count
    }

    fn entries(&self) -> &[EventRingSegmentTableEntry] {
        Self::entries_from(&self.buffer, self.segment_count)
    }

    fn entries_from(buffer: &DmaBuffer, segment_count: usize) -> &[EventRingSegmentTableEntry] {
        // SAFETY: the ERST buffer is 64-byte aligned and initialized as exactly
        // `segment_count` ERST entries inside this owned allocation.
        unsafe {
            slice::from_raw_parts(
                buffer.as_ptr().cast::<EventRingSegmentTableEntry>(),
                segment_count,
            )
        }
    }

    fn entries_mut_from(
        buffer: &mut DmaBuffer,
        segment_count: usize,
    ) -> &mut [EventRingSegmentTableEntry] {
        // SAFETY: the ERST buffer is exclusively owned here and stores exactly
        // `segment_count` ERST entries.
        unsafe {
            slice::from_raw_parts_mut(
                buffer.as_mut_ptr_t::<EventRingSegmentTableEntry>(),
                segment_count,
            )
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EventRingSegmentTableEntry {
    ring_segment_base_address: u64,
    ring_segment_size: u16,
    _reserved: [u8; 6],
}

fn checked_byte_len(count: usize, element_size: usize) -> Result<usize, XhciMemoryError> {
    count
        .checked_mul(element_size)
        .ok_or(XhciMemoryError::SizeOverflow)
}

#[cfg(test)]
mod tests {
    use super::{
        Dcbaa, EventRingSegmentDescriptor, EventRingSegmentTable, EventRingSegmentTableEntry,
        ScratchpadSet, XhciControlMemoryConfig, XhciMemoryError,
    };
    use crate::frame_allocator;
    use crate::paging;
    use crate::usb::xhci::dma::XhciDmaProfile;
    use core::mem::size_of;
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

    #[test_case]
    fn test_dcbaa_allocates_max_slots_plus_one_entries_zeroed() {
        setup_allocator(&[MemoryRegion {
            start: 0x12000,
            size: 0x80000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let config = XhciControlMemoryConfig {
            max_slots: 31,
            scratchpad_buffer_count: 0,
        };
        let dcbaa = Dcbaa::new(&dma_profile(), &config).expect("dcbaa");

        assert_eq!(dcbaa.entry_count(), 32);
        assert_eq!(dcbaa.entries().len(), 32);
        assert!(dcbaa.entries().iter().all(|entry| *entry == 0));
    }

    #[test_case]
    fn test_dcbaa_sets_and_clears_scratchpad_array_pointer_only_at_entry_zero() {
        setup_allocator(&[MemoryRegion {
            start: 0x200000,
            size: 0x80000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let config = XhciControlMemoryConfig {
            max_slots: 7,
            scratchpad_buffer_count: 2,
        };
        let mut dcbaa = Dcbaa::new(&dma_profile(), &config).expect("dcbaa");
        let scratchpad = ScratchpadSet::new(&dma_profile(), 2).expect("scratchpad");

        dcbaa.set_scratchpad_array(Some(&scratchpad));
        assert_eq!(dcbaa.entries()[0], scratchpad.array_phys_addr());
        assert!(dcbaa.entries()[1..].iter().all(|entry| *entry == 0));

        dcbaa.set_scratchpad_array(None);
        assert!(dcbaa.entries().iter().all(|entry| *entry == 0));
    }

    #[test_case]
    fn test_scratchpad_set_populates_array_and_buffers() {
        setup_allocator(&[MemoryRegion {
            start: 0x300000,
            size: 0x200000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let scratchpad = ScratchpadSet::new(&dma_profile(), 4).expect("scratchpad");

        assert_eq!(scratchpad.buffer_count(), 4);
        for index in 0..scratchpad.buffer_count() {
            assert_eq!(
                scratchpad.array_entries()[index],
                scratchpad
                    .buffer_phys_addr(index)
                    .expect("buffer phys addr")
            );
        }
    }

    #[test_case]
    fn test_scratchpad_set_rejects_zero_count() {
        setup_allocator(&[MemoryRegion {
            start: 0x400000,
            size: 0x20000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        assert!(matches!(
            ScratchpadSet::new(&dma_profile(), 0),
            Err(XhciMemoryError::ZeroScratchpadCount)
        ));
    }

    #[test_case]
    fn test_erst_builds_single_and_multiple_segments() {
        setup_allocator(&[MemoryRegion {
            start: 0x500000,
            size: 0x200000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let single = [EventRingSegmentDescriptor {
            base_addr: 0x1234_0000,
            trb_count: NonZeroU16::new(16).expect("non-zero"),
        }];
        let single_erst = EventRingSegmentTable::new(&dma_profile(), &single).expect("single");
        assert_eq!(single_erst.segment_count(), 1);
        assert_eq!(
            single_erst.entries(),
            &[EventRingSegmentTableEntry {
                ring_segment_base_address: 0x1234_0000,
                ring_segment_size: 16,
                _reserved: [0; 6],
            }]
        );

        let segments = [
            EventRingSegmentDescriptor {
                base_addr: 0x1234_0000,
                trb_count: NonZeroU16::new(64).expect("non-zero"),
            },
            EventRingSegmentDescriptor {
                base_addr: 0x5678_0000,
                trb_count: NonZeroU16::new(128).expect("non-zero"),
            },
        ];
        let erst = EventRingSegmentTable::new(&dma_profile(), &segments).expect("erst");

        assert_eq!(erst.segment_count(), 2);
        assert_eq!(size_of::<EventRingSegmentTableEntry>(), 16);
        assert_eq!(erst.entries()[0].ring_segment_base_address, 0x1234_0000);
        assert_eq!(erst.entries()[0].ring_segment_size, 64);
        assert_eq!(erst.entries()[1].ring_segment_base_address, 0x5678_0000);
        assert_eq!(erst.entries()[1].ring_segment_size, 128);
    }

    #[test_case]
    fn test_erst_rejects_empty_segments() {
        setup_allocator(&[MemoryRegion {
            start: 0x700000,
            size: 0x20000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        assert!(matches!(
            EventRingSegmentTable::new(&dma_profile(), &[]),
            Err(XhciMemoryError::EmptyEventRingSegments)
        ));
    }
}
