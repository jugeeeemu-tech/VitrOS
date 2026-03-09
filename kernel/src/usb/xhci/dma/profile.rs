use crate::dma::{DmaBuffer, DmaError};

use super::preset;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XhciDmaProfile {
    max_address: u64,
    page_size: usize,
}

impl XhciDmaProfile {
    pub const fn new(supports_64bit_addressing: bool, page_size: usize) -> Self {
        let max_address = if supports_64bit_addressing {
            u64::MAX
        } else {
            u32::MAX as u64
        };

        Self {
            max_address,
            page_size,
        }
    }

    pub fn allocate_ring(&self, size: usize) -> Result<DmaBuffer, DmaError> {
        DmaBuffer::allocate(size, preset::ring(self))
    }

    pub fn allocate_context(&self, size: usize) -> Result<DmaBuffer, DmaError> {
        DmaBuffer::allocate(size, preset::context(self))
    }

    pub fn allocate_dcbaa(&self, size: usize) -> Result<DmaBuffer, DmaError> {
        DmaBuffer::allocate(size, preset::dcbaa(self))
    }

    pub fn allocate_scratchpad_array(&self, size: usize) -> Result<DmaBuffer, DmaError> {
        DmaBuffer::allocate(size, preset::scratchpad_array(self))
    }

    pub fn allocate_scratchpad_buffer(&self) -> Result<DmaBuffer, DmaError> {
        DmaBuffer::allocate(self.page_size, preset::scratchpad_buffer(self))
    }

    pub fn allocate_erst(&self, size: usize) -> Result<DmaBuffer, DmaError> {
        DmaBuffer::allocate(size, preset::erst(self))
    }

    pub fn allocate_data_buffer(&self, size: usize) -> Result<DmaBuffer, DmaError> {
        DmaBuffer::allocate(size, preset::data_buffer(self))
    }

    pub(super) const fn max_address(&self) -> u64 {
        self.max_address
    }

    pub(super) const fn page_size(&self) -> usize {
        self.page_size
    }
}

#[cfg(test)]
mod tests {
    use super::XhciDmaProfile;
    use crate::dma::{DmaBuffer, DmaError};
    use crate::{frame_allocator, paging};
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

    fn assert_zeroed(buf: &DmaBuffer) {
        assert!(buf.as_slice().iter().all(|byte| *byte == 0));
    }

    fn assert_within_boundary(buf: &DmaBuffer, boundary: u64) {
        let start = buf.phys_addr();
        let end = start + buf.allocated_len() as u64 - 1;
        assert_eq!(start / boundary, end / boundary);
    }

    #[test_case]
    fn test_allocate_ring_guarantees_alignment_boundary_and_zeroed() {
        setup_allocator(&[MemoryRegion {
            start: 0xF000,
            size: 0x50000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let profile = XhciDmaProfile::new(true, paging::PAGE_SIZE);
        let buf = profile.allocate_ring(0x3000).expect("allocate failed");

        assert_eq!(buf.phys_addr() & 0x3F, 0);
        assert_within_boundary(&buf, 0x1_0000);
        assert_zeroed(&buf);
    }

    #[test_case]
    fn test_allocate_context_guarantees_page_boundary() {
        setup_allocator(&[MemoryRegion {
            start: 0x12000,
            size: 0x40000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let profile = XhciDmaProfile::new(true, paging::PAGE_SIZE);
        let buf = profile.allocate_context(0x400).expect("allocate failed");

        assert_eq!(buf.phys_addr() & 0x3F, 0);
        assert_within_boundary(&buf, paging::PAGE_SIZE as u64);
        assert_zeroed(&buf);
    }

    #[test_case]
    fn test_allocate_dcbaa_guarantees_page_boundary() {
        setup_allocator(&[MemoryRegion {
            start: 0x22000,
            size: 0x40000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let profile = XhciDmaProfile::new(true, paging::PAGE_SIZE);
        let buf = profile.allocate_dcbaa(0x800).expect("allocate failed");

        assert_eq!(buf.phys_addr() & 0x3F, 0);
        assert_within_boundary(&buf, paging::PAGE_SIZE as u64);
        assert_zeroed(&buf);
    }

    #[test_case]
    fn test_allocate_scratchpad_array_guarantees_alignment_and_zeroed() {
        setup_allocator(&[MemoryRegion {
            start: 0x200000,
            size: 0x20000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let profile = XhciDmaProfile::new(true, paging::PAGE_SIZE);
        let buf = profile
            .allocate_scratchpad_array(0x100)
            .expect("allocate failed");

        assert_eq!(buf.phys_addr() & 0x3F, 0);
        assert_zeroed(&buf);
    }

    #[test_case]
    fn test_allocate_scratchpad_buffer_guarantees_page_size_alignment_and_zeroed() {
        setup_allocator(&[MemoryRegion {
            start: 0x300000,
            size: 0x20000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let profile = XhciDmaProfile::new(true, paging::PAGE_SIZE);
        let buf = profile
            .allocate_scratchpad_buffer()
            .expect("allocate failed");

        assert_eq!(buf.len(), paging::PAGE_SIZE);
        assert_eq!(buf.phys_addr() & (paging::PAGE_SIZE as u64 - 1), 0);
        assert_zeroed(&buf);
    }

    #[test_case]
    fn test_allocate_erst_guarantees_alignment_and_zeroed() {
        setup_allocator(&[MemoryRegion {
            start: 0x400000,
            size: 0x20000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let profile = XhciDmaProfile::new(true, paging::PAGE_SIZE);
        let buf = profile.allocate_erst(0x100).expect("allocate failed");

        assert_eq!(buf.phys_addr() & 0x3F, 0);
        assert_zeroed(&buf);
    }

    #[test_case]
    fn test_allocate_data_buffer_guarantees_boundary_and_zeroed() {
        setup_allocator(&[MemoryRegion {
            start: 0x4F000,
            size: 0x50000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let profile = XhciDmaProfile::new(true, paging::PAGE_SIZE);
        let buf = profile
            .allocate_data_buffer(0x3000)
            .expect("allocate failed");

        assert_within_boundary(&buf, 0x1_0000);
        assert_zeroed(&buf);
    }

    #[test_case]
    fn test_allocate_ring_respects_32bit_dma_limit() {
        setup_allocator(&[MemoryRegion {
            start: 0x1_0000_0000,
            size: 0x20000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let profile = XhciDmaProfile::new(false, paging::PAGE_SIZE);
        assert!(matches!(
            profile.allocate_ring(0x1000),
            Err(DmaError::OutOfMemory)
        ));
    }
}
