//! 汎用DMAバッファ基盤
//!
//! デバイス非依存で利用できる、物理連続DMAバッファを提供する。

use core::{fmt, ptr, slice};

use crate::frame_allocator::{self, ContiguousConstraints, FrameAllocError};
use crate::paging;

/// DMA確保時の制約
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaConstraints {
    pub alignment: usize,
    pub boundary: usize,
    pub max_address: u64,
    pub contiguous: bool,
    pub zeroed: bool,
}

/// DMA操作のエラー
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaError {
    InvalidConstraint,
    Unsupported,
    OutOfMemory,
    ConstraintViolation,
}

impl fmt::Display for DmaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DmaError::InvalidConstraint => write!(f, "invalid DMA constraints"),
            DmaError::Unsupported => write!(f, "unsupported DMA configuration"),
            DmaError::OutOfMemory => write!(f, "DMA allocation out of memory"),
            DmaError::ConstraintViolation => write!(f, "DMA constraint violation"),
        }
    }
}

/// 物理連続DMAバッファ
pub struct DmaBuffer {
    phys_addr: u64,
    virt_addr: u64,
    len: usize,
    allocated_len: usize,
}

impl DmaBuffer {
    /// DMAバッファを確保する
    pub fn allocate(size: usize, constraints: DmaConstraints) -> Result<Self, DmaError> {
        if size == 0 {
            return Err(DmaError::InvalidConstraint);
        }
        if !constraints.contiguous {
            return Err(DmaError::Unsupported);
        }
        if constraints.alignment == 0 || !constraints.alignment.is_power_of_two() {
            return Err(DmaError::InvalidConstraint);
        }
        if constraints.boundary != 0 && !constraints.boundary.is_power_of_two() {
            return Err(DmaError::InvalidConstraint);
        }

        let size_u64 = u64::try_from(size).map_err(|_| DmaError::InvalidConstraint)?;
        let allocated_len_u64 =
            align_up(size_u64, paging::PAGE_SIZE as u64).ok_or(DmaError::InvalidConstraint)?;
        if allocated_len_u64 == 0 {
            return Err(DmaError::InvalidConstraint);
        }

        let min_required_max = allocated_len_u64
            .checked_sub(1)
            .ok_or(DmaError::InvalidConstraint)?;
        if constraints.max_address < min_required_max {
            return Err(DmaError::InvalidConstraint);
        }

        let alignment =
            u64::try_from(constraints.alignment).map_err(|_| DmaError::InvalidConstraint)?;
        let boundary =
            u64::try_from(constraints.boundary).map_err(|_| DmaError::InvalidConstraint)?;
        let contiguous_constraints = ContiguousConstraints {
            alignment,
            boundary,
            max_address: constraints.max_address,
        };

        let (phys_addr, allocated_len) =
            frame_allocator::alloc_contiguous(size, contiguous_constraints)
                .map_err(map_alloc_error)?;

        let virt_addr = match paging::phys_to_virt(phys_addr) {
            Ok(v) => v,
            Err(_) => {
                let _ = frame_allocator::free_contiguous(phys_addr, allocated_len);
                return Err(DmaError::ConstraintViolation);
            }
        };

        if constraints.zeroed {
            // SAFETY: 確保直後の専有バッファ領域のみを初期化する。
            unsafe {
                ptr::write_bytes(virt_addr as *mut u8, 0, allocated_len);
            }
        }

        Ok(Self {
            phys_addr,
            virt_addr,
            len: size,
            allocated_len,
        })
    }

    pub fn phys_addr(&self) -> u64 {
        self.phys_addr
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn allocated_len(&self) -> usize {
        self.allocated_len
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.virt_addr as *const u8
    }

    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.virt_addr as *mut u8
    }

    pub fn as_mut_ptr_t<T>(&self) -> *mut T {
        self.as_mut_ptr().cast::<T>()
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: バッファは`self`が生存する間有効で、lenは要求サイズに制限される。
        unsafe { slice::from_raw_parts(self.as_ptr(), self.len) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `&mut self` により排他的アクセスが保証される。
        unsafe { slice::from_raw_parts_mut(self.as_mut_ptr(), self.len) }
    }
}

impl Drop for DmaBuffer {
    fn drop(&mut self) {
        if self.phys_addr == 0 || self.allocated_len == 0 {
            return;
        }
        let _ = frame_allocator::free_contiguous(self.phys_addr, self.allocated_len);
    }
}

fn map_alloc_error(err: FrameAllocError) -> DmaError {
    match err {
        FrameAllocError::InvalidAddress | FrameAllocError::InvalidRange => {
            DmaError::InvalidConstraint
        }
        FrameAllocError::OutOfMemory | FrameAllocError::TooManyRanges => DmaError::OutOfMemory,
        _ => DmaError::ConstraintViolation,
    }
}

#[inline]
fn align_up(value: u64, align: u64) -> Option<u64> {
    let addend = align - 1;
    value.checked_add(addend).map(|v| v & !addend)
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn default_constraints() -> DmaConstraints {
        DmaConstraints {
            alignment: paging::PAGE_SIZE,
            boundary: 0,
            max_address: u64::MAX,
            contiguous: true,
            zeroed: false,
        }
    }

    #[test_case]
    fn test_allocate_alignment_guarantee() {
        setup_allocator(&[MemoryRegion {
            start: 0x12000,
            size: 0x40000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let mut c = default_constraints();
        c.alignment = 0x10000;

        let buf = DmaBuffer::allocate(128, c).expect("allocate failed");
        assert_eq!(buf.phys_addr() & 0xFFFF, 0);
        assert_eq!(buf.len(), 128);
        assert_eq!(buf.allocated_len(), paging::PAGE_SIZE);
    }

    #[test_case]
    fn test_allocate_boundary_guarantee() {
        setup_allocator(&[MemoryRegion {
            start: 0xF000,
            size: 0x50000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let mut c = default_constraints();
        c.boundary = 0x10000;

        let buf = DmaBuffer::allocate(0x3000, c).expect("allocate failed");
        let start = buf.phys_addr();
        let end = start + buf.allocated_len() as u64 - 1;
        assert_eq!(start >> 16, end >> 16);
    }

    #[test_case]
    fn test_allocate_max_address_guarantee() {
        setup_allocator(&[MemoryRegion {
            start: 0x10000,
            size: 0x8000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let mut c = default_constraints();
        c.max_address = 0x12FFF;

        let buf = DmaBuffer::allocate(0x3000, c).expect("allocate failed");
        assert!(buf.phys_addr() + buf.allocated_len() as u64 - 1 <= c.max_address);
    }

    #[test_case]
    fn test_allocate_zeroed_guarantee() {
        setup_allocator(&[MemoryRegion {
            start: 0x200000,
            size: 0x20000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let mut c = default_constraints();
        c.zeroed = true;

        let buf = DmaBuffer::allocate(0x1234, c).expect("allocate failed");
        assert_eq!(buf.as_slice().len(), 0x1234);
        assert!(buf.as_slice().iter().all(|b| *b == 0));
    }

    #[test_case]
    fn test_allocate_unsupported_contiguous_false() {
        setup_allocator(&[MemoryRegion {
            start: 0x300000,
            size: 0x10000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let mut c = default_constraints();
        c.contiguous = false;
        assert!(matches!(
            DmaBuffer::allocate(0x1000, c),
            Err(DmaError::Unsupported)
        ));
    }

    #[test_case]
    fn test_allocate_invalid_constraints() {
        setup_allocator(&[MemoryRegion {
            start: 0x300000,
            size: 0x10000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let mut c = default_constraints();
        c.alignment = 3;
        assert!(matches!(
            DmaBuffer::allocate(0x1000, c),
            Err(DmaError::InvalidConstraint)
        ));

        let mut c = default_constraints();
        c.boundary = 3;
        assert!(matches!(
            DmaBuffer::allocate(0x1000, c),
            Err(DmaError::InvalidConstraint)
        ));

        let mut c = default_constraints();
        c.max_address = 0x0FFE;
        assert!(matches!(
            DmaBuffer::allocate(0x1000, c),
            Err(DmaError::InvalidConstraint)
        ));
    }

    #[test_case]
    fn test_allocate_out_of_memory() {
        setup_allocator(&[MemoryRegion {
            start: 0x400000,
            size: 0x2000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let c = default_constraints();
        assert!(matches!(
            DmaBuffer::allocate(0x3000, c),
            Err(DmaError::OutOfMemory)
        ));
    }

    #[test_case]
    fn test_drop_releases_memory() {
        setup_allocator(&[MemoryRegion {
            start: 0x500000,
            size: 0x10000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);

        let c = default_constraints();
        let first_phys = {
            let mut buf = DmaBuffer::allocate(0x3000, c).expect("first allocate failed");
            buf.as_mut_slice()[0] = 0xAB;
            buf.phys_addr()
        };

        let buf = DmaBuffer::allocate(0x3000, c).expect("second allocate failed");
        assert_eq!(buf.phys_addr(), first_phys);
    }
}
