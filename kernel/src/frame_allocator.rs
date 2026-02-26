//! 物理メモリアロケータ（4KBページフレーム管理）
//!
//! UEFIメモリマップから direct-map 対象の RAM 領域を取り込み、
//! 4KBフレーム単位で割り当て/解放を行う。

use crate::info;
use crate::io::without_interrupts;
use core::fmt;
use vitros_common::boot_info::{BootInfo, MAX_MEMORY_REGIONS, MemoryRegion};
use vitros_common::uefi::{
    EFI_ACPI_RECLAIM_MEMORY, EFI_BOOT_SERVICES_CODE, EFI_BOOT_SERVICES_DATA,
    EFI_CONVENTIONAL_MEMORY, EFI_LOADER_CODE, EFI_LOADER_DATA,
};

const PAGE_SIZE: u64 = 4096;
const MAX_FREE_RANGES: usize = MAX_MEMORY_REGIONS * 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContiguousConstraints {
    pub alignment: u64,
    pub boundary: u64,
    pub max_address: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameAllocError {
    NotInitialized,
    AlreadyInitialized,
    InvalidAddress,
    InvalidRange,
    OutOfMemory,
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
            if !is_allocator_ram_type(region.region_type) {
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

        let reserved_count = boot_info
            .reserved_range_count
            .min(boot_info.reserved_ranges.len());
        for reserved in &boot_info.reserved_ranges[..reserved_count] {
            self.reserve_range(reserved.start, reserved.size)?;
        }

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
        info!(
            "Frame allocator reserved ranges applied: {}",
            reserved_count
        );

        Ok(())
    }

    fn alloc_frame(&mut self) -> Result<u64, FrameAllocError> {
        if !self.initialized {
            return Err(FrameAllocError::NotInitialized);
        }

        for i in 0..self.free_count {
            let range = self.free_ranges[i];
            let alloc_start = range.start;
            let alloc_end = alloc_start
                .checked_add(PAGE_SIZE)
                .ok_or(FrameAllocError::OutOfMemory)?;

            if alloc_end > range.end {
                continue;
            }

            self.free_ranges[i].start = alloc_end;
            if self.free_ranges[i].start == self.free_ranges[i].end {
                remove_range(&mut self.free_ranges, &mut self.free_count, i);
            }

            self.free_pages = self.free_pages.saturating_sub(1);
            return Ok(alloc_start);
        }

        Err(FrameAllocError::OutOfMemory)
    }

    fn alloc_contiguous(
        &mut self,
        size: usize,
        constraints: ContiguousConstraints,
    ) -> Result<(u64, usize), FrameAllocError> {
        if !self.initialized {
            return Err(FrameAllocError::NotInitialized);
        }
        if size == 0 {
            return Err(FrameAllocError::InvalidRange);
        }
        if constraints.alignment == 0 || !constraints.alignment.is_power_of_two() {
            return Err(FrameAllocError::InvalidRange);
        }
        if constraints.boundary != 0 && !constraints.boundary.is_power_of_two() {
            return Err(FrameAllocError::InvalidRange);
        }

        let size_u64 = u64::try_from(size).map_err(|_| FrameAllocError::InvalidRange)?;
        let alloc_size = align_up(size_u64, PAGE_SIZE).ok_or(FrameAllocError::InvalidRange)?;
        if alloc_size == 0 {
            return Err(FrameAllocError::InvalidRange);
        }

        let max_exclusive = if constraints.max_address == u64::MAX {
            u64::MAX
        } else {
            constraints.max_address + 1
        };
        if max_exclusive < alloc_size {
            return Err(FrameAllocError::OutOfMemory);
        }

        let effective_alignment = constraints.alignment.max(PAGE_SIZE);
        let alloc_pages = (alloc_size / PAGE_SIZE) as usize;

        for i in 0..self.free_count {
            let range = self.free_ranges[i];
            if range.start >= max_exclusive {
                break;
            }

            let range_end = range.end.min(max_exclusive);
            if range_end <= range.start {
                continue;
            }

            let mut candidate_start = match align_up(range.start, effective_alignment) {
                Some(v) => v,
                None => continue,
            };

            while candidate_start < range_end {
                let candidate_end = match candidate_start.checked_add(alloc_size) {
                    Some(v) => v,
                    None => break,
                };
                if candidate_end > range_end {
                    break;
                }

                if crosses_boundary(candidate_start, candidate_end, constraints.boundary) {
                    let boundary_start = align_down(candidate_start, constraints.boundary);
                    let next_boundary = match boundary_start.checked_add(constraints.boundary) {
                        Some(v) => v,
                        None => break,
                    };
                    candidate_start = match align_up(next_boundary, effective_alignment) {
                        Some(v) => v,
                        None => break,
                    };
                    continue;
                }

                match self.commit_contiguous_alloc(i, candidate_start, candidate_end) {
                    Ok(()) => {
                        self.free_pages = self.free_pages.saturating_sub(alloc_pages);
                        return Ok((candidate_start, alloc_size as usize));
                    }
                    Err(FrameAllocError::TooManyRanges) => {
                        candidate_start = match candidate_start.checked_add(effective_alignment) {
                            Some(v) => v,
                            None => break,
                        };
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        Err(FrameAllocError::OutOfMemory)
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

    fn free_contiguous(&mut self, start: u64, size: usize) -> Result<(), FrameAllocError> {
        if !self.initialized {
            return Err(FrameAllocError::NotInitialized);
        }
        if size == 0 {
            return Ok(());
        }
        if start & (PAGE_SIZE - 1) != 0 {
            return Err(FrameAllocError::InvalidAddress);
        }

        let size_u64 = u64::try_from(size).map_err(|_| FrameAllocError::InvalidRange)?;
        if size_u64 & (PAGE_SIZE - 1) != 0 {
            return Err(FrameAllocError::InvalidRange);
        }

        let end = start
            .checked_add(size_u64)
            .ok_or(FrameAllocError::InvalidRange)?;
        if !self.is_managed_range(start, end) {
            return Err(FrameAllocError::AddressNotManaged);
        }

        for i in 0..self.free_count {
            let range = self.free_ranges[i];
            if end <= range.start {
                break;
            }
            if start < range.end && end > range.start {
                return Err(FrameAllocError::DoubleFree);
            }
        }

        if self.free_count >= self.free_ranges.len() {
            return Err(FrameAllocError::TooManyRanges);
        }

        let mut insert_idx = self.free_count;
        for i in 0..self.free_count {
            if start < self.free_ranges[i].start {
                insert_idx = i;
                break;
            }
        }

        insert_range(
            &mut self.free_ranges,
            &mut self.free_count,
            insert_idx,
            PhysRange { start, end },
        )?;

        let pages = (size_u64 / PAGE_SIZE) as usize;
        self.free_pages = self.free_pages.saturating_add(pages);
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
        let frame_end = match frame_phys.checked_add(PAGE_SIZE) {
            Some(v) => v,
            None => return false,
        };
        self.is_managed_range(frame_phys, frame_end)
    }

    fn is_managed_range(&self, start: u64, end: u64) -> bool {
        if start >= end {
            return false;
        }

        for i in 0..self.managed_count {
            let range = self.managed_ranges[i];
            if start >= range.start && end <= range.end {
                return true;
            }
            if end <= range.start {
                break;
            }
        }
        false
    }

    fn commit_contiguous_alloc(
        &mut self,
        range_idx: usize,
        alloc_start: u64,
        alloc_end: u64,
    ) -> Result<(), FrameAllocError> {
        let range = self.free_ranges[range_idx];
        if alloc_start < range.start || alloc_end > range.end || alloc_start >= alloc_end {
            return Err(FrameAllocError::InvalidRange);
        }

        if alloc_start == range.start && alloc_end == range.end {
            remove_range(&mut self.free_ranges, &mut self.free_count, range_idx);
            return Ok(());
        }
        if alloc_start == range.start {
            self.free_ranges[range_idx].start = alloc_end;
            return Ok(());
        }
        if alloc_end == range.end {
            self.free_ranges[range_idx].end = alloc_start;
            return Ok(());
        }

        if self.free_count >= self.free_ranges.len() {
            return Err(FrameAllocError::TooManyRanges);
        }

        self.free_ranges[range_idx].end = alloc_start;
        insert_range(
            &mut self.free_ranges,
            &mut self.free_count,
            range_idx + 1,
            PhysRange {
                start: alloc_end,
                end: range.end,
            },
        )?;
        Ok(())
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

pub fn alloc_frame() -> Result<u64, FrameAllocError> {
    without_interrupts(|| unsafe {
        let allocator = core::ptr::addr_of_mut!(FRAME_ALLOCATOR);
        (*allocator).alloc_frame()
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

pub(crate) fn alloc_contiguous(
    size: usize,
    constraints: ContiguousConstraints,
) -> Result<(u64, usize), FrameAllocError> {
    without_interrupts(|| unsafe {
        let allocator = core::ptr::addr_of_mut!(FRAME_ALLOCATOR);
        (*allocator).alloc_contiguous(size, constraints)
    })
}

pub(crate) fn free_contiguous(start: u64, size: usize) -> Result<(), FrameAllocError> {
    without_interrupts(|| unsafe {
        let allocator = core::ptr::addr_of_mut!(FRAME_ALLOCATOR);
        (*allocator).free_contiguous(start, size)
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

#[inline]
fn is_allocator_ram_type(region_type: u32) -> bool {
    matches!(
        region_type,
        EFI_CONVENTIONAL_MEMORY
            | EFI_LOADER_CODE
            | EFI_LOADER_DATA
            | EFI_BOOT_SERVICES_CODE
            | EFI_BOOT_SERVICES_DATA
            | EFI_ACPI_RECLAIM_MEMORY
    )
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

#[inline]
fn crosses_boundary(start: u64, end_exclusive: u64, boundary: u64) -> bool {
    if boundary == 0 {
        return false;
    }

    let window_start = align_down(start, boundary);
    match window_start.checked_add(boundary) {
        Some(window_end) => end_exclusive > window_end,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vitros_common::boot_info::{
        BootInfo, MemoryRegion, RESERVED_RANGE_KIND_KERNEL_IMAGE, ReservedMemoryRange,
    };

    fn boot_info_with_regions(regions: &[MemoryRegion]) -> BootInfo {
        let mut boot_info = BootInfo::new();
        for (i, region) in regions.iter().enumerate() {
            boot_info.memory_map[i] = *region;
        }
        boot_info.memory_map_count = regions.len();
        boot_info
    }

    #[test_case]
    fn test_init_managed_ram_types() {
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
        assert_eq!(debug_free_pages(), 13);
        assert_eq!(debug_free_count(), 2);
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

        let a = alloc_frame().expect("alloc a failed");
        let b = alloc_frame().expect("alloc b failed");
        assert_eq!(a, 0x400000);
        assert_eq!(b, 0x401000);

        free_frame(a).expect("free failed");
        let c = alloc_frame().expect("alloc c failed");
        assert_eq!(c, a);
    }

    #[test_case]
    fn test_alloc_frame_and_free_frame() {
        reset_for_test();
        let boot_info = boot_info_with_regions(&[MemoryRegion {
            start: 0x600000,
            size: 0x3000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);
        init(&boot_info).expect("init failed");

        let a = alloc_frame().expect("alloc_frame a failed");
        let b = alloc_frame().expect("alloc_frame b failed");
        assert_eq!(a, 0x600000);
        assert_eq!(b, 0x601000);

        free_frame(a).expect("free_frame failed");
        let c = alloc_frame().expect("alloc_frame c failed");
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
    fn test_init_applies_reserved_ranges() {
        reset_for_test();
        let mut boot_info = boot_info_with_regions(&[MemoryRegion {
            start: 0x100000,
            size: 0x10000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);
        boot_info.reserved_ranges[0] = ReservedMemoryRange {
            start: 0x104000,
            size: 0x2000,
            kind: RESERVED_RANGE_KIND_KERNEL_IMAGE,
        };
        boot_info.reserved_range_count = 1;

        init(&boot_info).expect("init failed");

        assert_eq!(alloc_frame().expect("alloc 0"), 0x100000);
        assert_eq!(alloc_frame().expect("alloc 1"), 0x101000);
        assert_eq!(alloc_frame().expect("alloc 2"), 0x102000);
        assert_eq!(alloc_frame().expect("alloc 3"), 0x103000);
        assert_eq!(alloc_frame().expect("alloc 4"), 0x106000);
    }

    #[test_case]
    fn test_alloc_contiguous_max_address_limit() {
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

        let constraints = ContiguousConstraints {
            alignment: PAGE_SIZE,
            boundary: 0,
            max_address: 0x17_FFFF,
        };

        let (f, allocated_len) =
            alloc_contiguous(PAGE_SIZE as usize, constraints).expect("alloc_contiguous below failed");
        assert!(f <= constraints.max_address);
        assert!(f + allocated_len as u64 - 1 <= constraints.max_address);

        let low_only_constraints = ContiguousConstraints {
            alignment: PAGE_SIZE,
            boundary: 0,
            max_address: 0x0F_FFFF,
        };
        let e = alloc_contiguous(PAGE_SIZE as usize, low_only_constraints);
        assert_eq!(e, Err(FrameAllocError::OutOfMemory));
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

        let frame = alloc_frame().expect("alloc failed");
        free_frame(frame).expect("free once failed");
        assert_eq!(free_frame(frame), Err(FrameAllocError::DoubleFree));

        assert_eq!(
            free_frame(0x900000),
            Err(FrameAllocError::AddressNotManaged)
        );
    }

    #[test_case]
    fn test_alloc_contiguous_alignment_and_boundary() {
        reset_for_test();
        let boot_info = boot_info_with_regions(&[MemoryRegion {
            start: 0xF000,
            size: 0x50000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);
        init(&boot_info).expect("init failed");

        let constraints = ContiguousConstraints {
            alignment: 0x10000,
            boundary: 0x10000,
            max_address: 0x5EFFF,
        };
        let (phys, allocated_len) =
            alloc_contiguous(0x3000, constraints).expect("alloc_contiguous failed");

        assert_eq!(phys, 0x10000);
        assert_eq!(allocated_len, 0x3000);
        assert_eq!(phys & 0xFFFF, 0);
        assert!(!crosses_boundary(
            phys,
            phys + allocated_len as u64,
            constraints.boundary
        ));
    }

    #[test_case]
    fn test_alloc_contiguous_with_max_address() {
        reset_for_test();
        let boot_info = boot_info_with_regions(&[MemoryRegion {
            start: 0x10000,
            size: 0x8000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);
        init(&boot_info).expect("init failed");

        let constraints = ContiguousConstraints {
            alignment: PAGE_SIZE,
            boundary: 0,
            max_address: 0x12FFF,
        };

        let (phys, allocated_len) =
            alloc_contiguous(0x3000, constraints).expect("alloc_contiguous failed");
        assert_eq!(phys, 0x10000);
        assert_eq!(allocated_len, 0x3000);
        assert!(phys + allocated_len as u64 - 1 <= constraints.max_address);
    }

    #[test_case]
    fn test_alloc_contiguous_failure_keeps_state() {
        reset_for_test();
        let boot_info = boot_info_with_regions(&[MemoryRegion {
            start: 0x300000,
            size: 0x8000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);
        init(&boot_info).expect("init failed");

        let before_pages = debug_free_pages();
        let before_count = debug_free_count();

        let constraints = ContiguousConstraints {
            alignment: PAGE_SIZE,
            boundary: 0,
            max_address: 0x1FFF,
        };
        assert_eq!(
            alloc_contiguous(0x2000, constraints),
            Err(FrameAllocError::OutOfMemory)
        );
        assert_eq!(debug_free_pages(), before_pages);
        assert_eq!(debug_free_count(), before_count);
    }

    #[test_case]
    fn test_free_contiguous_round_trip() {
        reset_for_test();
        let boot_info = boot_info_with_regions(&[MemoryRegion {
            start: 0x400000,
            size: 0x10000,
            region_type: EFI_CONVENTIONAL_MEMORY,
        }]);
        init(&boot_info).expect("init failed");

        let constraints = ContiguousConstraints {
            alignment: PAGE_SIZE,
            boundary: 0,
            max_address: u64::MAX,
        };

        let (a, size_a) = alloc_contiguous(0x3000, constraints).expect("alloc a failed");
        free_contiguous(a, size_a).expect("free_contiguous failed");
        let (b, size_b) = alloc_contiguous(0x3000, constraints).expect("alloc b failed");
        assert_eq!(a, b);
        assert_eq!(size_a, size_b);
    }

    #[test_case]
    fn test_init_no_usable_memory() {
        reset_for_test();
        let boot_info = boot_info_with_regions(&[MemoryRegion {
            start: 0x1000,
            size: 0x2000,
            region_type: vitros_common::uefi::EFI_RUNTIME_SERVICES_DATA,
        }]);

        assert_eq!(init(&boot_info), Err(FrameAllocError::NoUsableMemory));
    }
}
