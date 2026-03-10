//! Kernel heap window manager.
//!
//! Scope 2-1b:
//! - virtual address range management in heap window
//! - physical frame supply + map transaction
//! - rollback on OOM / map failure

use crate::frame_allocator::{self, FrameAllocError};
use crate::io::without_interrupts;
use crate::paging::{
    self, KERNEL_HEAP_WINDOW_END, KERNEL_HEAP_WINDOW_START, PAGE_SIZE, PageTableFlags, PagingError,
};
use crate::{info, warn};
use spin::Mutex;

const PAGE_SIZE_U64: u64 = PAGE_SIZE as u64;
const MAX_HEAP_WINDOW_RANGES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapWindowAllocation {
    pub virt_start: u64,
    pub page_count: usize,
    pub size_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollbackOrigin {
    FrameOutOfMemory,
    MappingFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeapWindowError {
    NotInitialized,
    AlreadyInitialized,
    InvalidRequest,
    VirtualAddressExhausted {
        requested_pages: usize,
        free_pages: usize,
    },
    FrameOutOfMemory {
        requested_pages: usize,
        mapped_pages: usize,
    },
    MappingFailed {
        virt_start: u64,
        failed_page_index: usize,
        cause: PagingError,
    },
    RollbackFailed {
        original: RollbackOrigin,
        first_unmap_error: Option<PagingError>,
        first_free_error: Option<FrameAllocError>,
        range_reclaim_failed: bool,
        rolled_back_pages: usize,
    },
    RangeTableFull,
}

impl core::fmt::Display for HeapWindowError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HeapWindowError::NotInitialized => write!(f, "Heap window manager is not initialized"),
            HeapWindowError::AlreadyInitialized => {
                write!(f, "Heap window manager is already initialized")
            }
            HeapWindowError::InvalidRequest => write!(f, "Invalid heap window supply request"),
            HeapWindowError::VirtualAddressExhausted {
                requested_pages,
                free_pages,
            } => write!(
                f,
                "Heap window virtual address exhausted (requested pages: {}, free pages: {})",
                requested_pages, free_pages
            ),
            HeapWindowError::FrameOutOfMemory {
                requested_pages,
                mapped_pages,
            } => write!(
                f,
                "Frame out of memory during heap window supply (requested pages: {}, mapped pages: {})",
                requested_pages, mapped_pages
            ),
            HeapWindowError::MappingFailed {
                virt_start,
                failed_page_index,
                cause,
            } => write!(
                f,
                "Heap window mapping failed at 0x{:X} page {}: {}",
                virt_start, failed_page_index, cause
            ),
            HeapWindowError::RollbackFailed {
                original,
                first_unmap_error,
                first_free_error,
                range_reclaim_failed,
                rolled_back_pages,
            } => write!(
                f,
                "Heap window rollback failed (origin: {:?}, unmap={:?}, free={:?}, range_reclaim_failed={}, rolled_back_pages={})",
                original,
                first_unmap_error,
                first_free_error,
                range_reclaim_failed,
                rolled_back_pages
            ),
            HeapWindowError::RangeTableFull => write!(f, "Heap window free-range table is full"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VaRange {
    start: u64,
    end: u64,
}

impl VaRange {
    const fn empty() -> Self {
        Self { start: 0, end: 0 }
    }

    #[inline]
    fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

struct HeapWindowManager {
    initialized: bool,
    free_ranges: [VaRange; MAX_HEAP_WINDOW_RANGES],
    free_count: usize,
}

impl HeapWindowManager {
    const fn new() -> Self {
        Self {
            initialized: false,
            free_ranges: [VaRange::empty(); MAX_HEAP_WINDOW_RANGES],
            free_count: 0,
        }
    }

    fn init(&mut self) -> Result<(), HeapWindowError> {
        if self.initialized {
            return Err(HeapWindowError::AlreadyInitialized);
        }

        self.initialized = true;
        self.free_count = 1;
        self.free_ranges[0] = VaRange {
            start: KERNEL_HEAP_WINDOW_START,
            end: KERNEL_HEAP_WINDOW_END,
        };
        for range in self.free_ranges.iter_mut().skip(1) {
            *range = VaRange::empty();
        }

        Ok(())
    }

    #[cfg(test)]
    fn reset_for_test(&mut self) {
        self.initialized = false;
        self.free_count = 0;
        for range in &mut self.free_ranges {
            *range = VaRange::empty();
        }
    }

    fn allocate_range(&mut self, size: u64) -> Result<u64, HeapWindowError> {
        if !self.initialized {
            return Err(HeapWindowError::NotInitialized);
        }
        if size == 0 || size & (PAGE_SIZE_U64 - 1) != 0 {
            return Err(HeapWindowError::InvalidRequest);
        }

        for i in 0..self.free_count {
            let range = self.free_ranges[i];
            if range.len() < size {
                continue;
            }

            let alloc_start = range.start;
            self.free_ranges[i].start = alloc_start + size;
            if self.free_ranges[i].start == self.free_ranges[i].end {
                self.remove_range(i);
            }

            return Ok(alloc_start);
        }

        Err(HeapWindowError::VirtualAddressExhausted {
            requested_pages: (size / PAGE_SIZE_U64) as usize,
            free_pages: self.total_free_pages(),
        })
    }

    fn reclaim_range(&mut self, start: u64, size: u64) -> Result<(), HeapWindowError> {
        if size == 0 || start & (PAGE_SIZE_U64 - 1) != 0 || size & (PAGE_SIZE_U64 - 1) != 0 {
            return Err(HeapWindowError::InvalidRequest);
        }

        let end = start
            .checked_add(size)
            .ok_or(HeapWindowError::InvalidRequest)?;
        if start < KERNEL_HEAP_WINDOW_START || end > KERNEL_HEAP_WINDOW_END {
            return Err(HeapWindowError::InvalidRequest);
        }

        self.insert_merged_range(start, end)
    }

    fn total_free_pages(&self) -> usize {
        let mut pages = 0usize;
        for range in self.free_ranges.iter().take(self.free_count) {
            pages += (range.len() / PAGE_SIZE_U64) as usize;
        }
        pages
    }

    fn insert_merged_range(&mut self, start: u64, end: u64) -> Result<(), HeapWindowError> {
        let mut new_start = start;
        let mut new_end = end;
        let mut idx = 0usize;

        while idx < self.free_count && self.free_ranges[idx].end < new_start {
            idx += 1;
        }

        while idx < self.free_count && self.free_ranges[idx].start <= new_end {
            let range = self.free_ranges[idx];
            new_start = new_start.min(range.start);
            new_end = new_end.max(range.end);
            self.remove_range(idx);
        }

        if self.free_count >= MAX_HEAP_WINDOW_RANGES {
            return Err(HeapWindowError::RangeTableFull);
        }

        for shift_idx in (idx..self.free_count).rev() {
            self.free_ranges[shift_idx + 1] = self.free_ranges[shift_idx];
        }
        self.free_ranges[idx] = VaRange {
            start: new_start,
            end: new_end,
        };
        self.free_count += 1;
        Ok(())
    }

    fn remove_range(&mut self, idx: usize) {
        for i in idx..self.free_count.saturating_sub(1) {
            self.free_ranges[i] = self.free_ranges[i + 1];
        }
        if self.free_count > 0 {
            self.free_count -= 1;
            self.free_ranges[self.free_count] = VaRange::empty();
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RollbackReport {
    first_unmap_error: Option<PagingError>,
    first_free_error: Option<FrameAllocError>,
    rolled_back_pages: usize,
}

impl RollbackReport {
    const fn empty() -> Self {
        Self {
            first_unmap_error: None,
            first_free_error: None,
            rolled_back_pages: 0,
        }
    }

    fn has_failure(&self) -> bool {
        self.first_unmap_error.is_some() || self.first_free_error.is_some()
    }
}

static HEAP_WINDOW_MANAGER: Mutex<HeapWindowManager> = Mutex::new(HeapWindowManager::new());

fn page_count_for_request(min_bytes: usize) -> Result<usize, HeapWindowError> {
    if min_bytes == 0 {
        return Err(HeapWindowError::InvalidRequest);
    }
    let rounded = min_bytes
        .checked_add(PAGE_SIZE - 1)
        .ok_or(HeapWindowError::InvalidRequest)?;
    Ok(rounded / PAGE_SIZE)
}

fn checked_size_bytes(page_count: usize) -> Result<(usize, u64), HeapWindowError> {
    let size_bytes = page_count
        .checked_mul(PAGE_SIZE)
        .ok_or(HeapWindowError::InvalidRequest)?;
    let size_u64 = u64::try_from(size_bytes).map_err(|_| HeapWindowError::InvalidRequest)?;
    Ok((size_bytes, size_u64))
}

fn rollback_mapped_pages(virt_start: u64, mapped_pages: usize) -> RollbackReport {
    let mut report = RollbackReport::empty();

    for page_index in (0..mapped_pages).rev() {
        let virt_addr = virt_start + (page_index as u64 * PAGE_SIZE_U64);
        match paging::unmap_kernel_page_at(virt_addr) {
            Ok(frame_phys) => match frame_allocator::free_frame(frame_phys) {
                Ok(()) => report.rolled_back_pages += 1,
                Err(free_err) => {
                    if report.first_free_error.is_none() {
                        report.first_free_error = Some(free_err);
                    }
                }
            },
            Err(unmap_err) => {
                if report.first_unmap_error.is_none() {
                    report.first_unmap_error = Some(unmap_err);
                }
            }
        }
    }

    report
}

pub fn init_manager() -> Result<(), HeapWindowError> {
    without_interrupts(|| {
        let mut manager = HEAP_WINDOW_MANAGER.lock();
        manager.init()
    })
}

pub fn supply_pages(min_bytes: usize) -> Result<HeapWindowAllocation, HeapWindowError> {
    let requested_pages = page_count_for_request(min_bytes)?;
    let (size_bytes, requested_size_u64) = checked_size_bytes(requested_pages)?;

    without_interrupts(|| {
        let mut manager = HEAP_WINDOW_MANAGER.lock();
        let virt_start = manager.allocate_range(requested_size_u64)?;
        let mut mapped_pages = 0usize;

        for page_index in 0..requested_pages {
            let virt_addr = virt_start + (page_index as u64 * PAGE_SIZE_U64);

            let frame_phys = match frame_allocator::alloc_frame() {
                Ok(frame) => frame,
                Err(_) => {
                    warn!(
                        "Heap window supply OOM: requested_pages={}, mapped_pages={}",
                        requested_pages, mapped_pages
                    );
                    let rollback = rollback_mapped_pages(virt_start, mapped_pages);
                    let range_reclaim_failed = manager
                        .reclaim_range(virt_start, requested_size_u64)
                        .is_err();

                    if rollback.has_failure() || range_reclaim_failed {
                        return Err(HeapWindowError::RollbackFailed {
                            original: RollbackOrigin::FrameOutOfMemory,
                            first_unmap_error: rollback.first_unmap_error,
                            first_free_error: rollback.first_free_error,
                            range_reclaim_failed,
                            rolled_back_pages: rollback.rolled_back_pages,
                        });
                    }

                    return Err(HeapWindowError::FrameOutOfMemory {
                        requested_pages,
                        mapped_pages,
                    });
                }
            };

            if let Err(map_err) =
                paging::map_kernel_page_at(virt_addr, frame_phys, PageTableFlags::NoExecute as u64)
            {
                warn!(
                    "Heap window supply map failed: page_index={}, virt=0x{:X}, err={:?}",
                    page_index, virt_addr, map_err
                );

                let mut rollback = rollback_mapped_pages(virt_start, mapped_pages);
                if let Err(free_err) = frame_allocator::free_frame(frame_phys)
                    && rollback.first_free_error.is_none()
                {
                    rollback.first_free_error = Some(free_err);
                }

                let range_reclaim_failed = manager
                    .reclaim_range(virt_start, requested_size_u64)
                    .is_err();
                if rollback.has_failure() || range_reclaim_failed {
                    return Err(HeapWindowError::RollbackFailed {
                        original: RollbackOrigin::MappingFailure,
                        first_unmap_error: rollback.first_unmap_error,
                        first_free_error: rollback.first_free_error,
                        range_reclaim_failed,
                        rolled_back_pages: rollback.rolled_back_pages,
                    });
                }

                return Err(HeapWindowError::MappingFailed {
                    virt_start,
                    failed_page_index: page_index,
                    cause: map_err,
                });
            }

            mapped_pages += 1;
        }

        info!(
            "Heap window supply success: virt=0x{:X}, pages={}, bytes={}",
            virt_start, requested_pages, size_bytes
        );
        Ok(HeapWindowAllocation {
            virt_start,
            page_count: requested_pages,
            size_bytes,
        })
    })
}

#[cfg(test)]
pub(crate) fn test_reset_manager_for_test() {
    without_interrupts(|| {
        let mut manager = HEAP_WINDOW_MANAGER.lock();
        manager.reset_for_test();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use vitros_common::boot_info::{BootInfo, MemoryRegion};
    use vitros_common::uefi::EFI_CONVENTIONAL_MEMORY;

    fn setup_test_env(region_size: u64) {
        crate::frame_allocator::reset_for_test();
        test_reset_manager_for_test();

        let mut boot_info = BootInfo::new();
        boot_info.memory_map[0] = MemoryRegion {
            start: 0x1000,
            size: region_size,
            region_type: EFI_CONVENTIONAL_MEMORY,
        };
        boot_info.memory_map_count = 1;
        boot_info.max_physical_address = region_size;

        crate::frame_allocator::init(&boot_info).expect("frame allocator init failed");
        crate::paging::test_reset_page_tables_for_test();
        crate::paging::test_clear_map_kernel_page_failpoint();
    }

    #[test_case]
    fn test_supply_heap_window_pages_keeps_contiguous_state() {
        setup_test_env(0x80_0000);
        init_manager().expect("manager init failed");

        let first = supply_pages(PAGE_SIZE * 3).expect("first supply failed");
        let second = supply_pages(PAGE_SIZE * 2).expect("second supply failed");

        assert_eq!(first.virt_start, KERNEL_HEAP_WINDOW_START);
        assert_eq!(first.page_count, 3);
        assert_eq!(
            second.virt_start,
            KERNEL_HEAP_WINDOW_START + (3 * PAGE_SIZE) as u64
        );
        assert_eq!(second.page_count, 2);
    }

    #[test_case]
    fn test_supply_heap_window_pages_map_failure_rollback() {
        setup_test_env(0x80_0000);
        init_manager().expect("manager init failed");

        let before_pages = crate::frame_allocator::debug_free_pages();
        crate::paging::test_set_map_kernel_page_fail_after(2);

        let err = supply_pages(PAGE_SIZE * 4).expect_err("supply should fail");
        match err {
            HeapWindowError::MappingFailed {
                virt_start,
                failed_page_index,
                cause,
            } => {
                assert_eq!(virt_start, KERNEL_HEAP_WINDOW_START);
                assert_eq!(failed_page_index, 2);
                assert_eq!(cause, PagingError::FrameAllocationFailed);
            }
            _ => panic!("unexpected error: {:?}", err),
        }

        let after_pages = crate::frame_allocator::debug_free_pages();
        assert_eq!(
            after_pages, before_pages,
            "frame allocator pages must be restored after rollback"
        );

        for i in 0..4usize {
            let va = KERNEL_HEAP_WINDOW_START + (i as u64 * PAGE_SIZE_U64);
            let result = crate::io::without_interrupts(|| paging::unmap_kernel_page_at(va));
            assert_eq!(result, Err(PagingError::MappingNotPresent));
        }

        crate::paging::test_clear_map_kernel_page_failpoint();
    }

    #[test_case]
    fn test_supply_heap_window_pages_oom_rollback() {
        setup_test_env(0x40_000);
        init_manager().expect("manager init failed");

        let before_pages = crate::frame_allocator::debug_free_pages();
        let requested_pages = before_pages + 8;
        let requested_bytes = requested_pages * PAGE_SIZE;

        let err = supply_pages(requested_bytes).expect_err("supply should run out of memory");
        let mapped_pages = match err {
            HeapWindowError::FrameOutOfMemory {
                requested_pages: actual_requested,
                mapped_pages,
            } => {
                assert_eq!(actual_requested, requested_pages);
                mapped_pages
            }
            _ => panic!("unexpected error: {:?}", err),
        };
        assert!(
            mapped_pages < requested_pages,
            "must fail before mapping all requested pages"
        );

        let after_pages = crate::frame_allocator::debug_free_pages();
        assert_eq!(
            after_pages, before_pages,
            "frame allocator pages must be restored after OOM rollback"
        );

        for i in 0..8usize {
            let va = KERNEL_HEAP_WINDOW_START + (i as u64 * PAGE_SIZE_U64);
            let result = crate::io::without_interrupts(|| paging::unmap_kernel_page_at(va));
            assert_eq!(result, Err(PagingError::MappingNotPresent));
        }
    }

    #[test_case]
    fn test_supply_heap_window_pages_error_surface() {
        setup_test_env(0x80_0000);

        assert_eq!(supply_pages(0), Err(HeapWindowError::InvalidRequest));
        assert_eq!(
            supply_pages(PAGE_SIZE),
            Err(HeapWindowError::NotInitialized)
        );

        init_manager().expect("manager init failed");
        assert_eq!(init_manager(), Err(HeapWindowError::AlreadyInitialized));

        {
            let mut manager = HEAP_WINDOW_MANAGER.lock();
            manager.initialized = true;
            manager.free_count = 1;
            manager.free_ranges[0] = VaRange {
                start: KERNEL_HEAP_WINDOW_START,
                end: KERNEL_HEAP_WINDOW_START + PAGE_SIZE_U64,
            };
            for range in manager.free_ranges.iter_mut().skip(1) {
                *range = VaRange::empty();
            }
        }

        let err = supply_pages(PAGE_SIZE * 2).expect_err("virtual address should be exhausted");
        match err {
            HeapWindowError::VirtualAddressExhausted {
                requested_pages,
                free_pages,
            } => {
                assert_eq!(requested_pages, 2);
                assert_eq!(free_pages, 1);
            }
            _ => panic!("unexpected error: {:?}", err),
        }
    }
}
