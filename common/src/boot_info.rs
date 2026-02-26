// ブートローダからカーネルに渡す情報
#![allow(dead_code)]

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct FramebufferInfo {
    pub base: u64,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct MemoryRegion {
    pub start: u64,
    pub size: u64,
    pub region_type: u32,
}

pub const BOOT_INFO_ABI_VERSION: u32 = 2;
pub const MAX_MEMORY_REGIONS: usize = 256;
pub const MAX_BOOTLOADER_PAGE_TABLE_RANGES: usize = 32;
pub const MAX_RESERVED_RANGES: usize = 128;

pub const RESERVED_RANGE_KIND_KERNEL_IMAGE: u32 = 1;
pub const RESERVED_RANGE_KIND_BOOT_INFO: u32 = 2;
pub const RESERVED_RANGE_KIND_BOOTLOADER_PT: u32 = 3;
pub const RESERVED_RANGE_KIND_ACPI_TABLE: u32 = 4;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct BootloaderPageTableRange {
    pub start: u64,
    pub size: u64,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct ReservedMemoryRange {
    pub start: u64,
    pub size: u64,
    pub kind: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct BootInfo {
    pub framebuffer: FramebufferInfo,
    pub abi_version: u32,
    pub memory_map: [MemoryRegion; MAX_MEMORY_REGIONS],
    pub memory_map_count: usize,
    pub rsdp_address: u64,
    /// マッピングが必要な最大物理アドレス（UEFIメモリマップから計算）
    pub max_physical_address: u64,
    pub bootloader_page_table_ranges: [BootloaderPageTableRange; MAX_BOOTLOADER_PAGE_TABLE_RANGES],
    pub bootloader_page_table_range_count: usize,
    pub reserved_ranges: [ReservedMemoryRange; MAX_RESERVED_RANGES],
    pub reserved_range_count: usize,
}

impl BootInfo {
    pub const fn new() -> Self {
        Self {
            framebuffer: FramebufferInfo {
                base: 0,
                size: 0,
                width: 0,
                height: 0,
                stride: 0,
            },
            abi_version: BOOT_INFO_ABI_VERSION,
            memory_map: [MemoryRegion {
                start: 0,
                size: 0,
                region_type: 0,
            }; MAX_MEMORY_REGIONS],
            memory_map_count: 0,
            rsdp_address: 0,
            max_physical_address: 0,
            bootloader_page_table_ranges: [BootloaderPageTableRange { start: 0, size: 0 };
                MAX_BOOTLOADER_PAGE_TABLE_RANGES],
            bootloader_page_table_range_count: 0,
            reserved_ranges: [ReservedMemoryRange {
                start: 0,
                size: 0,
                kind: 0,
            }; MAX_RESERVED_RANGES],
            reserved_range_count: 0,
        }
    }
}

impl Default for BootInfo {
    fn default() -> Self {
        Self::new()
    }
}
