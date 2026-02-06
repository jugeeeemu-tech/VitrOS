//! 物理メモリアロケータ（4KBページフレーム管理）
//!
//! UEFIメモリマップから `EFI_CONVENTIONAL_MEMORY` 領域を取り込み、
//! 4KBフレーム単位で割り当て/解放を行う。

use crate::info;
use crate::io::without_interrupts;
use core::fmt;
use vitros_common::boot_info::{BootInfo, MAX_MEMORY_REGIONS, MemoryRegion};
use vitros_common::uefi::EFI_CONVENTIONAL_MEMORY;

const PAGE_SIZE: u64 = 4096;
const MAX_FREE_RANGES: usize = MAX_MEMORY_REGIONS * 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameAllocError {
    NotInitialized,
    AlreadyInitialized,
    InvalidAddress,
    InvalidRange,
    OutOfMemory,
    OutOfMemoryBelowLimit,
    AddressNotManaged,
    DoubleFree,
    TooManyRanges,
    NoUsableMemory,
}

impl fmt::Display for FrameAllocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameAllocError::NotInitialized => write!(f, "Frame allocator is not initialized"),
            FrameAllocError::AlreadyInitialized => {
                write!(f, "Frame allocator is already initialized")
            }
            FrameAllocError::InvalidAddress => write!(f, "Invalid physical address"),
            FrameAllocError::InvalidRange => write!(f, "Invalid range"),
            FrameAllocError::OutOfMemory => write!(f, "Out of physical memory"),
            FrameAllocError::OutOfMemoryBelowLimit => {
                write!(f, "Out of physical memory below limit")
            }
            FrameAllocError::AddressNotManaged => write!(f, "Address is outside managed ranges"),
            FrameAllocError::DoubleFree => write!(f, "Double free detected"),
            FrameAllocError::TooManyRanges => write!(f, "Too many free ranges"),
            FrameAllocError::NoUsableMemory => write!(f, "No usable memory regions found"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
struct PhysRange {
    start: u64,
    end: u64,
}

impl PhysRange {
    const fn empty() -> Self {
        Self { start: 0, end: 0 }
    }

    #[inline]
    fn is_valid(&self) -> bool {
        self.start < self.end
    }

    #[inline]
    fn page_count(&self) -> usize {
        ((self.end - self.start) / PAGE_SIZE) as usize
    }
}

#[derive(Clone, Copy)]
struct FrameAllocator {
    initialized: bool,
    free_ranges: [PhysRange; MAX_FREE_RANGES],
    free_count: usize,
    managed_ranges: [PhysRange; MAX_MEMORY_REGIONS],
    managed_count: usize,
    total_pages: usize,
    free_pages: usize,
}

impl FrameAllocator {
    const fn new() -> Self {
        Self {
            initialized: false,
            free_ranges: [PhysRange::empty(); MAX_FREE_RANGES],
            free_count: 0,
            managed_ranges: [PhysRange::empty(); MAX_MEMORY_REGIONS],
            managed_count: 0,
            total_pages: 0,
            free_pages: 0,
        }
    }

    fn reset(&mut self) {
        self.initialized = false;
        self.free_count = 0;
        self.managed_count = 0;
        self.total_pages = 0;
        self.free_pages = 0;

        for range in &mut self.free_ranges {
            *range = PhysRange::empty();
        }
        for range in &mut self.managed_ranges {
            *range = PhysRange::empty();
        }
    }

    fn init(&mut self, boot_info: &BootInfo) -> Result<(), FrameAllocError> {
        if self.initialized {
            return Err(FrameAllocError::AlreadyInitialized);
        }

        self.reset();

        let count = boot_info.memory_map_count.min(boot_info.memory_map.len());
        let mut raw_ranges = [PhysRange::empty(); MAX_MEMORY_REGIONS];
        let mut raw_count = 0usize;

        for region in &boot_info.memory_map[..count] {
            if region.region_type != EFI_CONVENTIONAL_MEMORY {
                continue;
            }

            if let Some(range) = align_region_to_pages(region)
                && raw_count < raw_ranges.len()
            {
                raw_ranges[raw_count] = range;
                raw_count += 1;
            }
        }

        if raw_count == 0 {
            return Err(FrameAllocError::NoUsableMemory);
        }

        sort_ranges(&mut raw_ranges, raw_count);
        let merged_count = merge_ranges(&mut raw_ranges, raw_count);

        if merged_count > self.managed_ranges.len() {
            return Err(FrameAllocError::TooManyRanges);
        }

        self.managed_ranges[..merged_count].copy_from_slice(&raw_ranges[..merged_count]);
        self.managed_count = merged_count;

        if merged_count > self.free_ranges.len() {
            return Err(FrameAllocError::TooManyRanges);
        }

        self.free_ranges[..merged_count].copy_from_slice(&raw_ranges[..merged_count]);
        self.free_count = merged_count;

        self.total_pages = count_pages(&self.managed_ranges, self.managed_count);
        self.free_pages = count_pages(&self.free_ranges, self.free_count);

        self.initialized = true;

        info!(
            "Frame allocator initialized: {} ranges, {} total pages ({} MiB)",
            self.managed_count,
            self.total_pages,
            (self.total_pages as u64 * PAGE_SIZE) / (1024 * 1024)
        );
        info!(
            "Frame allocator free: {} ranges, {} pages",
            self.free_count, self.free_pages
        );

        Ok(())
    }

    fn alloc_frame_below(&mut self, limit_exclusive: u64) -> Result<u64, FrameAllocError> {
        if !self.initialized {
            return Err(FrameAllocError::NotInitialized);
        }

        let limit = align_down(limit_exclusive, PAGE_SIZE);
        if limit < PAGE_SIZE {
            return Err(FrameAllocError::OutOfMemoryBelowLimit);
        }

        for i in 0..self.free_count {
            let range = self.free_ranges[i];
            if range.start >= limit {
                break;
            }

            let alloc_start = range.start;
            let alloc_end = alloc_start + PAGE_SIZE;

            if alloc_end > range.end || alloc_end > limit {
                continue;
            }

            self.free_ranges[i].start = alloc_end;
            if self.free_ranges[i].start == self.free_ranges[i].end {
                remove_range(&mut self.free_ranges, &mut self.free_count, i);
            }

            self.free_pages = self.free_pages.saturating_sub(1);
            return Ok(alloc_start);
        }

        Err(FrameAllocError::OutOfMemoryBelowLimit)
    }

    fn free_frame(&mut self, frame_phys: u64) -> Result<(), FrameAllocError> {
        if !self.initialized {
            return Err(FrameAllocError::NotInitialized);
        }
        if frame_phys & (PAGE_SIZE - 1) != 0 {
            return Err(FrameAllocError::InvalidAddress);
        }

        let frame_end = frame_phys
            .checked_add(PAGE_SIZE)
            .ok_or(FrameAllocError::InvalidAddress)?;

        if !self.is_managed_frame(frame_phys) {
            return Err(FrameAllocError::AddressNotManaged);
        }

        for i in 0..self.free_count {
            let range = self.free_ranges[i];
            if frame_phys < range.end && frame_end > range.start {
                return Err(FrameAllocError::DoubleFree);
            }
        }

        if self.free_count >= self.free_ranges.len() {
            return Err(FrameAllocError::TooManyRanges);
        }

        let mut insert_idx = self.free_count;
        for i in 0..self.free_count {
            if frame_phys < self.free_ranges[i].start {
                insert_idx = i;
                break;
            }
        }

        insert_range(
            &mut self.free_ranges,
            &mut self.free_count,
            insert_idx,
            PhysRange {
                start: frame_phys,
                end: frame_end,
            },
        )?;

        self.free_pages += 1;
        self.merge_neighbors(insert_idx);

        Ok(())
    }

    fn reserve_range(&mut self, start: u64, size: u64) -> Result<(), FrameAllocError> {
        if !self.initialized {
            return Err(FrameAllocError::NotInitialized);
        }
        if size == 0 {
            return Ok(());
        }

        let end = start
            .checked_add(size)
            .ok_or(FrameAllocError::InvalidRange)?;
        let reserve_start = align_down(start, PAGE_SIZE);
        let reserve_end = align_up(end, PAGE_SIZE).ok_or(FrameAllocError::InvalidRange)?;

        if reserve_start >= reserve_end {
            return Ok(());
        }

        let mut new_ranges = [PhysRange::empty(); MAX_FREE_RANGES];
        let mut new_count = 0usize;

        for i in 0..self.free_count {
            let range = self.free_ranges[i];
            if range.end <= reserve_start || range.start >= reserve_end {
                if new_count >= new_ranges.len() {
                    return Err(FrameAllocError::TooManyRanges);
                }
                new_ranges[new_count] = range;
                new_count += 1;
                continue;
            }

            if range.start < reserve_start {
                if new_count >= new_ranges.len() {
                    return Err(FrameAllocError::TooManyRanges);
                }
                new_ranges[new_count] = PhysRange {
                    start: range.start,
                    end: reserve_start,
                };
                new_count += 1;
            }

            if reserve_end < range.end {
                if new_count >= new_ranges.len() {
                    return Err(FrameAllocError::TooManyRanges);
                }
                new_ranges[new_count] = PhysRange {
                    start: reserve_end,
                    end: range.end,
                };
                new_count += 1;
            }
        }

        self.free_ranges[..new_count].copy_from_slice(&new_ranges[..new_count]);
        for i in new_count..self.free_ranges.len() {
            self.free_ranges[i] = PhysRange::empty();
        }
        self.free_count = new_count;
        self.free_pages = count_pages(&self.free_ranges, self.free_count);

        Ok(())
    }

    fn is_managed_frame(&self, frame_phys: u64) -> bool {
        let frame_end = frame_phys + PAGE_SIZE;
        for i in 0..self.managed_count {
            let range = self.managed_ranges[i];
            if frame_phys >= range.start && frame_end <= range.end {
                return true;
            }
            if frame_phys < range.start {
                break;
            }
        }
        false
    }

    fn merge_neighbors(&mut self, mut idx: usize) {
        if self.free_count == 0 {
            return;
        }

        if idx > 0 {
            let prev = self.free_ranges[idx - 1];
            let curr = self.free_ranges[idx];
            if prev.end == curr.start {
                self.free_ranges[idx - 1].end = curr.end;
                remove_range(&mut self.free_ranges, &mut self.free_count, idx);
                idx -= 1;
            }
        }

        if idx + 1 < self.free_count {
            let curr = self.free_ranges[idx];
            let next = self.free_ranges[idx + 1];
            if curr.end == next.start {
                self.free_ranges[idx].end = next.end;
                remove_range(&mut self.free_ranges, &mut self.free_count, idx + 1);
            }
        }
    }
}

static mut FRAME_ALLOCATOR: FrameAllocator = FrameAllocator::new();

pub fn init(boot_info: &BootInfo) -> Result<(), FrameAllocError> {
    without_interrupts(|| unsafe {
        let allocator = core::ptr::addr_of_mut!(FRAME_ALLOCATOR);
        (*allocator).init(boot_info)
    })
}

pub fn alloc_frame_below(limit_exclusive: u64) -> Result<u64, FrameAllocError> {
    without_interrupts(|| unsafe {
        let allocator = core::ptr::addr_of_mut!(FRAME_ALLOCATOR);
        (*allocator).alloc_frame_below(limit_exclusive)
    })
}

pub fn free_frame(frame_phys: u64) -> Result<(), FrameAllocError> {
    without_interrupts(|| unsafe {
        let allocator = core::ptr::addr_of_mut!(FRAME_ALLOCATOR);
        (*allocator).free_frame(frame_phys)
    })
}

pub fn reserve_range(start: u64, size: u64) -> Result<(), FrameAllocError> {
    without_interrupts(|| unsafe {
        let allocator = core::ptr::addr_of_mut!(FRAME_ALLOCATOR);
        (*allocator).reserve_range(start, size)
    })
}

#[cfg(test)]
pub(crate) fn reset_for_test() {
    without_interrupts(|| unsafe {
        let allocator = core::ptr::addr_of_mut!(FRAME_ALLOCATOR);
        (*allocator).reset();
    });
}

#[cfg(test)]
pub(crate) fn debug_free_pages() -> usize {
    without_interrupts(|| unsafe {
        let allocator = core::ptr::addr_of!(FRAME_ALLOCATOR);
        (*allocator).free_pages
    })
}

#[cfg(test)]
pub(crate) fn debug_free_count() -> usize {
    without_interrupts(|| unsafe {
        let allocator = core::ptr::addr_of!(FRAME_ALLOCATOR);
        (*allocator).free_count
    })
}

#[inline]
fn count_pages(ranges: &[PhysRange], count: usize) -> usize {
    let mut pages = 0usize;
    for range in ranges.iter().take(count) {
        pages += range.page_count();
    }
    pages
}

fn align_region_to_pages(region: &MemoryRegion) -> Option<PhysRange> {
    if region.size == 0 {
        return None;
    }

    let end = region.start.checked_add(region.size)?;
    let start_aligned = align_up(region.start, PAGE_SIZE)?;
    let end_aligned = align_down(end, PAGE_SIZE);

    if start_aligned >= end_aligned {
        return None;
    }

    Some(PhysRange {
        start: start_aligned,
        end: end_aligned,
    })
}

fn sort_ranges(ranges: &mut [PhysRange], count: usize) {
    for i in 1..count {
        let key = ranges[i];
        let mut j = i;
        while j > 0 && ranges[j - 1].start > key.start {
            ranges[j] = ranges[j - 1];
            j -= 1;
        }
        ranges[j] = key;
    }
}

fn merge_ranges(ranges: &mut [PhysRange], count: usize) -> usize {
    if count == 0 {
        return 0;
    }

    let mut write_idx = 0usize;
    for i in 1..count {
        let curr = ranges[i];
        let write = &mut ranges[write_idx];
        if curr.start <= write.end {
            write.end = write.end.max(curr.end);
        } else {
            write_idx += 1;
            ranges[write_idx] = curr;
        }
    }

    write_idx + 1
}

fn insert_range(
    ranges: &mut [PhysRange; MAX_FREE_RANGES],
    count: &mut usize,
    idx: usize,
    range: PhysRange,
) -> Result<(), FrameAllocError> {
    if !range.is_valid() {
        return Err(FrameAllocError::InvalidRange);
    }
    if *count >= ranges.len() || idx > *count {
        return Err(FrameAllocError::TooManyRanges);
    }

    for i in (idx..*count).rev() {
        ranges[i + 1] = ranges[i];
    }
    ranges[idx] = range;
    *count += 1;
    Ok(())
}

fn remove_range(ranges: &mut [PhysRange; MAX_FREE_RANGES], count: &mut usize, idx: usize) {
    if idx >= *count {
        return;
    }
    for i in idx..(*count - 1) {
        ranges[i] = ranges[i + 1];
    }
    *count -= 1;
    ranges[*count] = PhysRange::empty();
}

#[inline]
fn align_down(value: u64, align: u64) -> u64 {
    value & !(align - 1)
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

    fn boot_info_with_regions(regions: &[MemoryRegion]) -> BootInfo {
        let mut boot_info = BootInfo::new();
        for (i, region) in regions.iter().enumerate() {
            boot_info.memory_map[i] = *region;
        }
        boot_info.memory_map_count = regions.len();
        boot_info
    }

    #[test_case]
    fn test_init_conventional_only() {
        reset_for_test();
        let boot_info = boot_info_with_regions(&[
            MemoryRegion {
                start: 0x1000,
                size: 0x9000,
                region_type: vitros_common::uefi::EFI_BOOT_SERVICES_CODE,
            },
            MemoryRegion {
                start: 0x20000,
                size: 0x4000,
                region_type: EFI_CONVENTIONAL_MEMORY,
            },
        ]);

        init(&boot_info).expect("init failed");
        assert_eq!(debug_free_pages(), 4);
        assert_eq!(debug_free_count(), 1);
    }

    #[test_case]
    fn test_alloc_and_free_frame() {
        reset_for_test();
        let boot_info = boot_info_with_regions(&[MemoryRegion {
            start: 0x400000,
            size: 0x5000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);
        init(&boot_info).expect("init failed");

        let a = alloc_frame_below(0x800000).expect("alloc a failed");
        let b = alloc_frame_below(0x800000).expect("alloc b failed");
        assert_eq!(a, 0x400000);
        assert_eq!(b, 0x401000);

        free_frame(a).expect("free failed");
        let c = alloc_frame_below(0x800000).expect("alloc c failed");
        assert_eq!(c, a);
    }

    #[test_case]
    fn test_reserve_range_split() {
        reset_for_test();
        let boot_info = boot_info_with_regions(&[MemoryRegion {
            start: 0x100000,
            size: 0x10000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);
        init(&boot_info).expect("init failed");

        reserve_range(0x103000, 0x4000).expect("reserve failed");
        assert_eq!(debug_free_count(), 2);
        assert_eq!(debug_free_pages(), 12);
    }

    #[test_case]
    fn test_alloc_frame_below_limit() {
        reset_for_test();
        let boot_info = boot_info_with_regions(&[
            MemoryRegion {
                start: 0x100000,
                size: 0x4000,
                region_type: EFI_CONVENTIONAL_MEMORY,
            },
            MemoryRegion {
                start: 0x200000,
                size: 0x4000,
                region_type: EFI_CONVENTIONAL_MEMORY,
            },
        ]);
        init(&boot_info).expect("init failed");

        let f = alloc_frame_below(0x180000).expect("alloc below failed");
        assert!(f < 0x180000);

        let e = alloc_frame_below(0x100000);
        assert_eq!(e, Err(FrameAllocError::OutOfMemoryBelowLimit));
    }

    #[test_case]
    fn test_free_frame_errors() {
        reset_for_test();
        let boot_info = boot_info_with_regions(&[MemoryRegion {
            start: 0x300000,
            size: 0x2000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);
        init(&boot_info).expect("init failed");

        assert_eq!(free_frame(0x1234), Err(FrameAllocError::InvalidAddress));

        let frame = alloc_frame_below(0x400000).expect("alloc failed");
        free_frame(frame).expect("free once failed");
        assert_eq!(free_frame(frame), Err(FrameAllocError::DoubleFree));

        assert_eq!(
            free_frame(0x900000),
            Err(FrameAllocError::AddressNotManaged)
        );
    }

    #[test_case]
    fn test_init_no_usable_memory() {
        reset_for_test();
        let boot_info = boot_info_with_regions(&[MemoryRegion {
            start: 0x1000,
            size: 0x2000,
            region_type: vitros_common::uefi::EFI_ACPI_RECLAIM_MEMORY,
        }]);

        assert_eq!(init(&boot_info), Err(FrameAllocError::NoUsableMemory));
    }
}
