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

pub const MAX_MEMORY_REGIONS: usize = 128;

#[repr(C)]
pub struct BootInfo {
    pub framebuffer: FramebufferInfo,
    pub memory_map: [MemoryRegion; MAX_MEMORY_REGIONS],
    pub memory_map_count: usize,
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
            memory_map: [MemoryRegion {
                start: 0,
                size: 0,
                region_type: 0,
            }; MAX_MEMORY_REGIONS],
            memory_map_count: 0,
        }
    }
}
