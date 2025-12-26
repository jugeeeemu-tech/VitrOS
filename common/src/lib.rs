#![no_std]

pub mod allocator;
pub mod boot_info;
pub mod elf;
pub mod graphics;
pub mod io;
pub mod serial;
pub mod uefi;

#[cfg(feature = "visualize-allocator")]
pub mod allocator_visualization;
