#![no_std]
#![no_main]

use core::fmt::Write;
#[cfg(not(test))]
use core::panic::PanicInfo;
use vitros_common::boot_info::{
    BOOT_INFO_ABI_VERSION, BootInfo, BootloaderPageTableRange, FramebufferInfo, MemoryRegion,
    RESERVED_RANGE_KIND_ACPI_TABLE, RESERVED_RANGE_KIND_BOOT_INFO,
    RESERVED_RANGE_KIND_BOOTLOADER_PT, RESERVED_RANGE_KIND_KERNEL_IMAGE,
};
use vitros_common::elf::{Elf64Header, Elf64ProgramHeader, PT_LOAD};
use vitros_common::uefi::*;

// BOOT_INFOを静的変数として配置
// リンカがアドレスを決定し、物理アドレスをカーネルに渡す
static mut BOOT_INFO: BootInfo = BootInfo::new();

// グローバルなConOut（初期化後に設定）
static mut CON_OUT: Option<*mut EfiSimpleTextOutputProtocol> = None;
static mut MEMORY_MAP_BUFFER: [u8; 4096 * 64] = [0; 4096 * 64];

// ConOutに文字列を出力するヘルパー関数
fn print_con(s: &str) {
    unsafe {
        if let Some(con_out) = CON_OUT {
            let mut buffer = [0u16; 256];
            let mut len = 0;
            for c in s.chars() {
                if len >= buffer.len() - 1 {
                    break;
                }
                buffer[len] = c as u16;
                len += 1;
            }
            buffer[len] = 0; // null terminator
            ((*con_out).output_string)(con_out, buffer.as_ptr());
        }
    }
}

// 改行付き出力
fn println_con(s: &str) {
    print_con(s);
    print_con("\r\n");
}

// 固定サイズバッファを使ったフォーマット出力
struct BufWriter {
    buf: [u8; 512],
    pos: usize,
}

impl BufWriter {
    fn new() -> Self {
        Self {
            buf: [0; 512],
            pos: 0,
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.pos]).unwrap_or("")
    }
}

impl Write for BufWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let remaining = self.buf.len() - self.pos;
        let to_write = bytes.len().min(remaining);
        self.buf[self.pos..self.pos + to_write].copy_from_slice(&bytes[..to_write]);
        self.pos += to_write;
        Ok(())
    }
}

// マクロライクなヘルパー
macro_rules! println_uefi {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let mut buf = BufWriter::new();
        let _ = write!(buf, $($arg)*);
        println_con(buf.as_str());
    }};
}

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println_con("\n!!! BOOTLOADER PANIC !!!");
    println_uefi!("{}", info);
    loop {
        unsafe { core::arch::asm!("hlt") }
    }
}

// メモリタイプを文字列に変換
fn memory_type_str(mem_type: u32) -> &'static str {
    match mem_type {
        EFI_RESERVED_MEMORY_TYPE => "Reserved",
        EFI_LOADER_CODE => "LoaderCode",
        EFI_LOADER_DATA => "LoaderData",
        EFI_BOOT_SERVICES_CODE => "BSCode",
        EFI_BOOT_SERVICES_DATA => "BSData",
        EFI_RUNTIME_SERVICES_CODE => "RTCode",
        EFI_RUNTIME_SERVICES_DATA => "RTData",
        EFI_CONVENTIONAL_MEMORY => "Available",
        EFI_UNUSABLE_MEMORY => "Unusable",
        EFI_ACPI_RECLAIM_MEMORY => "ACPIReclaim",
        EFI_ACPI_MEMORY_NVS => "ACPINVS",
        EFI_MEMORY_MAPPED_IO => "MMIO",
        EFI_MEMORY_MAPPED_IO_PORT_SPACE => "MMIOPort",
        EFI_PAL_CODE => "PALCode",
        _ => "Unknown",
    }
}

// ページテーブルエントリのフラグ
const PAGE_PRESENT: u64 = 1 << 0;
const PAGE_WRITABLE: u64 = 1 << 1;
const PAGE_HUGE: u64 = 1 << 7;
const PAGE_SIZE_4KB: u64 = 4096;
const PAGE_SIZE_4KB_USIZE: usize = 4096;
const PAGE_SIZE_2MB: u64 = 2 * 1024 * 1024;
const PAGE_SIZE_1GB: u64 = 1024 * 1024 * 1024;
const DIRECT_MAP_WINDOW_BYTES: u64 = 1u64 << 47; // 48bit paging前提での高位半分直写窓
const EXIT_BOOT_SERVICES_RETRY_LIMIT: usize = 8;
const PAGE_TABLE_ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const MAX_ACPI_TABLE_LENGTH: u32 = 100 * 1024 * 1024;

// カーネル仮想アドレスベース
const KERNEL_VMA: u64 = 0xFFFF800000000000;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RsdpV1 {
    signature: [u8; 8],
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_address: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RsdpV2 {
    v1: RsdpV1,
    length: u32,
    xsdt_address: u64,
    extended_checksum: u8,
    reserved: [u8; 3],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct AcpiSdtHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
    checksum: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id: u32,
    creator_revision: u32,
}

#[derive(Clone, Copy)]
struct KernelLoadInfo {
    entry_phys: u64,
    image_phys_start: u64,
    image_size: u64,
}

#[inline]
fn align_down_4k(value: u64) -> u64 {
    value & !(PAGE_SIZE_4KB - 1)
}

#[inline]
fn align_up_4k(value: u64) -> Option<u64> {
    value
        .checked_add(PAGE_SIZE_4KB - 1)
        .map(|v| align_down_4k(v))
}

fn push_reserved_range(boot_info: &mut BootInfo, start: u64, size: u64, kind: u32) -> bool {
    if size == 0 {
        return true;
    }

    let end = match start.checked_add(size) {
        Some(v) => v,
        None => return false,
    };
    let start_aligned = align_down_4k(start);
    let end_aligned = match align_up_4k(end) {
        Some(v) => v,
        None => return false,
    };
    if start_aligned >= end_aligned {
        return true;
    }

    let count = boot_info.reserved_range_count;
    if count >= boot_info.reserved_ranges.len() {
        return false;
    }
    boot_info.reserved_ranges[count].start = start_aligned;
    boot_info.reserved_ranges[count].size = end_aligned - start_aligned;
    boot_info.reserved_ranges[count].kind = kind;
    boot_info.reserved_range_count = count + 1;
    true
}

unsafe fn checksum_is_zero(addr: u64, len: usize) -> bool {
    if len == 0 {
        return false;
    }
    let bytes = unsafe { core::slice::from_raw_parts(addr as *const u8, len) };
    let sum = bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    sum == 0
}

fn parse_sdt_header(addr: u64) -> Option<AcpiSdtHeader> {
    if addr == 0 {
        return None;
    }
    Some(unsafe { (addr as *const AcpiSdtHeader).read_unaligned() })
}

fn validate_and_reserve_sdt(boot_info: &mut BootInfo, table_addr: u64) -> bool {
    let header = match parse_sdt_header(table_addr) {
        Some(v) => v,
        None => return false,
    };
    let length = header.length as usize;
    if length < core::mem::size_of::<AcpiSdtHeader>() || header.length > MAX_ACPI_TABLE_LENGTH {
        return false;
    }
    if !unsafe { checksum_is_zero(table_addr, length) } {
        return false;
    }
    push_reserved_range(
        boot_info,
        table_addr,
        header.length as u64,
        RESERVED_RANGE_KIND_ACPI_TABLE,
    )
}

fn collect_acpi_table_ranges(boot_info: &mut BootInfo) -> bool {
    let rsdp_addr = boot_info.rsdp_address;
    if rsdp_addr == 0 {
        return true;
    }

    let rsdp_v1 = unsafe { (rsdp_addr as *const RsdpV1).read_unaligned() };
    if rsdp_v1.signature != *b"RSD PTR " {
        return false;
    }
    if !unsafe { checksum_is_zero(rsdp_addr, core::mem::size_of::<RsdpV1>()) } {
        return false;
    }

    let mut root_addr = rsdp_v1.rsdt_address as u64;
    let mut root_signature = *b"RSDT";
    let mut root_entry_size = core::mem::size_of::<u32>();
    let mut rsdp_length = core::mem::size_of::<RsdpV1>() as u64;

    if rsdp_v1.revision >= 2 {
        let rsdp_v2 = unsafe { (rsdp_addr as *const RsdpV2).read_unaligned() };
        if rsdp_v2.length < core::mem::size_of::<RsdpV2>() as u32
            || rsdp_v2.length > MAX_ACPI_TABLE_LENGTH
        {
            return false;
        }
        if !unsafe { checksum_is_zero(rsdp_addr, rsdp_v2.length as usize) } {
            return false;
        }
        rsdp_length = rsdp_v2.length as u64;
        if rsdp_v2.xsdt_address != 0 {
            root_addr = rsdp_v2.xsdt_address;
            root_signature = *b"XSDT";
            root_entry_size = core::mem::size_of::<u64>();
        } else if root_addr == 0 {
            return false;
        }
    } else if root_addr == 0 {
        return false;
    }

    if !push_reserved_range(
        boot_info,
        rsdp_addr,
        rsdp_length,
        RESERVED_RANGE_KIND_ACPI_TABLE,
    ) {
        return false;
    }

    let root_header = match parse_sdt_header(root_addr) {
        Some(v) => v,
        None => return false,
    };
    if root_header.signature != root_signature {
        return false;
    }
    let root_length = root_header.length as usize;
    let header_size = core::mem::size_of::<AcpiSdtHeader>();
    if root_length < header_size || root_header.length > MAX_ACPI_TABLE_LENGTH {
        return false;
    }
    if (root_length - header_size) % root_entry_size != 0 {
        return false;
    }
    if !unsafe { checksum_is_zero(root_addr, root_length) } {
        return false;
    }
    if !push_reserved_range(
        boot_info,
        root_addr,
        root_header.length as u64,
        RESERVED_RANGE_KIND_ACPI_TABLE,
    ) {
        return false;
    }

    let entry_count = (root_length - header_size) / root_entry_size;
    let entries_base = root_addr + header_size as u64;
    for i in 0..entry_count {
        let entry_addr = entries_base + (i as u64) * (root_entry_size as u64);
        let table_addr = if root_entry_size == core::mem::size_of::<u64>() {
            unsafe { (entry_addr as *const u64).read_unaligned() }
        } else {
            unsafe { (entry_addr as *const u32).read_unaligned() as u64 }
        };
        if table_addr == 0 {
            continue;
        }
        if !validate_and_reserve_sdt(boot_info, table_addr) {
            return false;
        }
    }

    true
}

// ページテーブル構造体（4KBアラインメント）
#[repr(C, align(4096))]
struct PageTable {
    entries: [u64; 512],
}

impl PageTable {
    fn clear(&mut self) {
        self.entries = [0; 512];
    }
}

struct AllocatedPageTablePool {
    base_phys: u64,
    total_pages: usize,
    next_page: usize,
}

impl AllocatedPageTablePool {
    unsafe fn allocate(
        boot_services: *mut EfiBootServices,
        pages: usize,
    ) -> Result<Self, EfiStatus> {
        let mut base = 0u64;
        let status = unsafe {
            ((*boot_services).allocate_pages)(
                EFI_ALLOCATE_ANY_PAGES,
                EFI_LOADER_DATA,
                pages,
                &mut base,
            )
        };
        if status != EFI_SUCCESS {
            return Err(status);
        }

        let byte_len = pages
            .checked_mul(PAGE_SIZE_4KB_USIZE)
            .ok_or(EFI_INVALID_PARAMETER)?;
        unsafe {
            core::ptr::write_bytes(base as *mut u8, 0, byte_len);
        }

        Ok(Self {
            base_phys: base,
            total_pages: pages,
            next_page: 0,
        })
    }

    fn range(&self) -> BootloaderPageTableRange {
        BootloaderPageTableRange {
            start: self.base_phys,
            size: (self.total_pages as u64) * PAGE_SIZE_4KB,
        }
    }

    fn used_pages(&self) -> usize {
        self.next_page
    }

    fn alloc_table(&mut self) -> Option<*mut PageTable> {
        if self.next_page >= self.total_pages {
            return None;
        }
        let phys = self.base_phys + (self.next_page as u64) * PAGE_SIZE_4KB;
        self.next_page += 1;
        Some(phys as *mut PageTable)
    }
}

#[inline]
fn ceil_div_u64(value: u64, divisor: u64) -> u64 {
    if value == 0 {
        0
    } else {
        1 + ((value - 1) / divisor)
    }
}

#[inline]
fn is_bootstrap_map_type(mem_type: u32) -> bool {
    matches!(
        mem_type,
        EFI_CONVENTIONAL_MEMORY
            | EFI_LOADER_CODE
            | EFI_LOADER_DATA
            | EFI_BOOT_SERVICES_CODE
            | EFI_BOOT_SERVICES_DATA
            | EFI_ACPI_RECLAIM_MEMORY
    )
}

fn analyze_bootstrap_map_limit(
    buffer: &[u8],
    entry_count: usize,
    descriptor_size: usize,
) -> Option<u64> {
    let mut max_phys_addr = 0u64;

    for i in 0..entry_count {
        let offset = i.checked_mul(descriptor_size)?;
        let desc = unsafe { &*(buffer.as_ptr().add(offset) as *const EfiMemoryDescriptor) };

        if !is_bootstrap_map_type(desc.r#type) {
            continue;
        }

        let region_size = desc.number_of_pages.checked_mul(PAGE_SIZE_4KB)?;
        let region_end = desc.physical_start.checked_add(region_size)?;
        if region_end > max_phys_addr {
            max_phys_addr = region_end;
        }
    }

    Some(max_phys_addr)
}

fn estimate_page_table_pages(max_phys_addr: u64) -> Option<usize> {
    let required_gb = ceil_div_u64(max_phys_addr, PAGE_SIZE_1GB);
    let pdp_tables_per_side = ceil_div_u64(required_gb, 512);
    let total_tables = 1u64
        .checked_add(2u64.checked_mul(pdp_tables_per_side)?)?
        .checked_add(2u64.checked_mul(required_gb)?)?;
    usize::try_from(total_tables).ok()
}

#[inline]
fn pml4_index(virt_addr: u64) -> usize {
    ((virt_addr >> 39) & 0x1FF) as usize
}

#[inline]
fn pdp_index(virt_addr: u64) -> usize {
    ((virt_addr >> 30) & 0x1FF) as usize
}

#[inline]
fn pd_index(virt_addr: u64) -> usize {
    ((virt_addr >> 21) & 0x1FF) as usize
}

unsafe fn ensure_child_table(
    parent: *mut PageTable,
    index: usize,
    link_flags: u64,
    pool: &mut AllocatedPageTablePool,
) -> Option<*mut PageTable> {
    unsafe {
        let entry = (*parent).entries[index];
        if entry & PAGE_PRESENT != 0 {
            if entry & PAGE_HUGE != 0 {
                return None;
            }
            let child_phys = entry & PAGE_TABLE_ADDR_MASK;
            if child_phys == 0 {
                return None;
            }
            return Some(child_phys as *mut PageTable);
        }

        let table = pool.alloc_table()?;
        (*table).clear();
        (*parent).entries[index] = (table as u64) | link_flags;
        Some(table)
    }
}

unsafe fn ensure_pd_for_virt(
    pml4: *mut PageTable,
    virt_addr: u64,
    link_flags: u64,
    pool: &mut AllocatedPageTablePool,
) -> Option<*mut PageTable> {
    unsafe {
        let pdp = ensure_child_table(pml4, pml4_index(virt_addr), link_flags, pool)?;
        ensure_child_table(pdp, pdp_index(virt_addr), link_flags, pool)
    }
}

unsafe fn setup_initial_page_tables(
    max_phys_addr: u64,
    pool: &mut AllocatedPageTablePool,
) -> Option<u64> {
    let link_flags = PAGE_PRESENT | PAGE_WRITABLE;
    let huge_flags = link_flags | PAGE_HUGE;
    let pml4 = pool.alloc_table()?;
    unsafe {
        (*pml4).clear();
    }

    let mut phys_addr = 0u64;
    while phys_addr < max_phys_addr {
        let low_pd = unsafe { ensure_pd_for_virt(pml4, phys_addr, link_flags, pool)? };
        let high_virt = KERNEL_VMA.checked_add(phys_addr)?;
        let high_pd = unsafe { ensure_pd_for_virt(pml4, high_virt, link_flags, pool)? };

        unsafe {
            (*low_pd).entries[pd_index(phys_addr)] = phys_addr | huge_flags;
            (*high_pd).entries[pd_index(high_virt)] = phys_addr | huge_flags;
        }

        phys_addr = phys_addr.checked_add(PAGE_SIZE_2MB)?;
    }

    Some(pml4 as u64)
}

unsafe fn fetch_memory_map(
    boot_services: *mut EfiBootServices,
    buffer: &mut [u8],
    map_key: &mut usize,
    descriptor_size: &mut usize,
    descriptor_version: &mut u32,
) -> Result<usize, EfiStatus> {
    let mut required_size = 0usize;
    let status = unsafe {
        ((*boot_services).get_memory_map)(
            &mut required_size,
            core::ptr::null_mut(),
            map_key,
            descriptor_size,
            descriptor_version,
        )
    };

    if status != EFI_BUFFER_TOO_SMALL && status != EFI_SUCCESS {
        return Err(status);
    }
    if *descriptor_size == 0 {
        return Err(EFI_INVALID_PARAMETER);
    }

    let mut map_size = required_size
        .checked_add(*descriptor_size)
        .ok_or(EFI_INVALID_PARAMETER)?;
    if map_size > buffer.len() {
        return Err(EFI_BUFFER_TOO_SMALL);
    }

    let status = unsafe {
        ((*boot_services).get_memory_map)(
            &mut map_size,
            buffer.as_mut_ptr() as *mut EfiMemoryDescriptor,
            map_key,
            descriptor_size,
            descriptor_version,
        )
    };
    if status != EFI_SUCCESS {
        return Err(status);
    }

    Ok(map_size)
}

fn copy_memory_map_to_boot_info(
    boot_info: &mut BootInfo,
    buffer: &[u8],
    map_size: usize,
    descriptor_size: usize,
) -> usize {
    let entry_count = map_size / descriptor_size;
    for i in 0..entry_count.min(boot_info.memory_map.len()) {
        let offset = i * descriptor_size;
        let desc = unsafe { &*(buffer.as_ptr().add(offset) as *const EfiMemoryDescriptor) };

        boot_info.memory_map[i] = MemoryRegion {
            start: desc.physical_start,
            size: desc.number_of_pages.saturating_mul(PAGE_SIZE_4KB),
            region_type: desc.r#type,
        };
    }
    boot_info.memory_map_count = entry_count.min(boot_info.memory_map.len());
    entry_count
}

/// CR3にページテーブルをロードしてページングを有効化（既に有効なのでCR3のみ更新）
unsafe fn load_page_tables(pml4_addr: u64) {
    unsafe {
        core::arch::asm!(
            "mov cr3, {0}",
            in(reg) pml4_addr,
            options(nostack, preserves_flags)
        );
    }
}

/// UEFI エントリポイント
#[unsafe(no_mangle)]
extern "efiapi" fn efi_main(
    image_handle: EfiHandle,
    system_table: *mut EfiSystemTable,
) -> EfiStatus {
    // ConOut (UEFI Simple Text Output Protocol) を初期化
    unsafe {
        CON_OUT = Some((*system_table).con_out);
    }

    println_con("=== VitrOS Bootloader ===");
    println_uefi!("[INFO] UEFI ConOut initialized");
    println_uefi!("[INFO] Locating Graphics Output Protocol...");

    // SAFETY: system_table は UEFI から渡される有効なポインタ
    let boot_services = unsafe { (*system_table).boot_services };

    // Graphics Output Protocol を検索
    let mut gop: *mut EfiGraphicsOutputProtocol = core::ptr::null_mut();

    // SAFETY: UEFI 関数の呼び出し
    let status = unsafe {
        ((*boot_services).locate_protocol)(
            &EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID,
            core::ptr::null_mut(),
            &mut gop as *mut *mut _ as *mut *mut core::ffi::c_void,
        )
    };

    if status != EFI_SUCCESS {
        println_uefi!("[ERROR] Failed to locate GOP!");
        loop {
            unsafe { core::arch::asm!("hlt") }
        }
    }

    println_uefi!("[INFO] GOP found successfully");

    // SAFETY: GOP から有効なフレームバッファ情報を取得
    let (fb_base, fb_size, width, height) = unsafe {
        let mode = (*gop).mode;
        let mode_info = (*mode).info;
        (
            (*mode).frame_buffer_base,
            (*mode).frame_buffer_size,
            (*mode_info).horizontal_resolution,
            (*mode_info).vertical_resolution,
        )
    };

    // 画面クリア（ConOut使用）
    unsafe {
        if let Some(con_out) = CON_OUT {
            ((*con_out).clear_screen)(con_out);
        }
    }

    println_uefi!("\nVitrOS - Memory Map\n");

    let mut map_key: usize = 0;
    let mut descriptor_size: usize = 0;
    let mut descriptor_version: u32 = 0;
    let memory_map_buffer = unsafe { &mut *core::ptr::addr_of_mut!(MEMORY_MAP_BUFFER) };

    let mut map_size = unsafe {
        match fetch_memory_map(
            boot_services,
            memory_map_buffer,
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        ) {
            Ok(size) => size,
            Err(status) => {
                println_uefi!(
                    "[ERROR] Failed to get initial memory map! Status: 0x{:X}, BufferSize={} bytes",
                    status,
                    memory_map_buffer.len()
                );
                loop {
                    core::arch::asm!("hlt");
                }
            }
        }
    };

    // BOOT_INFOを静的変数から取得
    let boot_info = unsafe { &mut *core::ptr::addr_of_mut!(BOOT_INFO) };
    let boot_info_phys_addr = core::ptr::addr_of!(BOOT_INFO) as u64;

    // フレームバッファ情報を設定
    boot_info.framebuffer = FramebufferInfo {
        base: fb_base,
        size: fb_size as u64,
        width,
        height,
        stride: width,
    };
    boot_info.abi_version = BOOT_INFO_ABI_VERSION;
    boot_info.bootloader_page_table_range_count = 0;
    boot_info.reserved_range_count = 0;

    // RSDP (ACPI Root System Description Pointer) を UEFI Configuration Table から取得
    unsafe {
        let config_table_ptr = (*system_table).configuration_table as *const EfiConfigurationTable;
        let num_entries = (*system_table).number_of_table_entries;

        let mut rsdp_addr = 0u64;
        for i in 0..num_entries {
            let entry = &*config_table_ptr.add(i);

            // ACPI 2.0 を優先的に検索
            if entry.vendor_guid == EFI_ACPI_20_TABLE_GUID {
                rsdp_addr = entry.vendor_table;
                println_uefi!("[INFO] Found ACPI 2.0 RSDP at 0x{:016X}", rsdp_addr);
                break;
            }
            // ACPI 1.0 をフォールバック
            else if entry.vendor_guid == EFI_ACPI_TABLE_GUID {
                rsdp_addr = entry.vendor_table;
                println_uefi!("[INFO] Found ACPI 1.0 RSDP at 0x{:016X}", rsdp_addr);
            }
        }

        if rsdp_addr == 0 {
            println_uefi!("[INFO] RSDP not found in UEFI Configuration Table");
        }

        boot_info.rsdp_address = rsdp_addr;
    }

    if !collect_acpi_table_ranges(boot_info) {
        println_uefi!("[ERROR] Failed to enumerate ACPI table ranges");
        loop {
            unsafe { core::arch::asm!("hlt") }
        }
    }
    if !push_reserved_range(
        boot_info,
        boot_info_phys_addr,
        core::mem::size_of::<BootInfo>() as u64,
        RESERVED_RANGE_KIND_BOOT_INFO,
    ) {
        println_uefi!("[ERROR] Failed to reserve BOOT_INFO range");
        loop {
            unsafe { core::arch::asm!("hlt") }
        }
    }

    let entry_count = map_size / descriptor_size;
    println_uefi!("[INFO] Memory map retrieved: {} entries", entry_count);

    // メモリマップを表示
    let max_display = 20;
    println_uefi!(
        "\nMemory Map (first {} entries):",
        max_display.min(entry_count)
    );
    for i in 0..entry_count.min(max_display) {
        let offset = i * descriptor_size;
        let desc =
            unsafe { &*(memory_map_buffer.as_ptr().add(offset) as *const EfiMemoryDescriptor) };

        let type_str = memory_type_str(desc.r#type);
        println_uefi!(
            "  {:<12} 0x{:016X}  Pages: 0x{:X}",
            type_str,
            desc.physical_start,
            desc.number_of_pages
        );
    }
    println_uefi!("\nTotal entries: {}", entry_count);

    copy_memory_map_to_boot_info(
        boot_info,
        &memory_map_buffer[..map_size],
        map_size,
        descriptor_size,
    );
    let initial_max_phys = match analyze_bootstrap_map_limit(
        &memory_map_buffer[..map_size],
        entry_count,
        descriptor_size,
    ) {
        Some(value) if value != 0 => value,
        _ => {
            println_uefi!("[ERROR] Bootstrap max physical address analysis failed");
            loop {
                unsafe { core::arch::asm!("hlt") }
            }
        }
    };
    if initial_max_phys > DIRECT_MAP_WINDOW_BYTES {
        println_uefi!(
            "[ERROR] Direct-map window overflow: max_phys=0x{:X}, limit=0x{:X}",
            initial_max_phys,
            DIRECT_MAP_WINDOW_BYTES
        );
        loop {
            unsafe { core::arch::asm!("hlt") }
        }
    }
    boot_info.max_physical_address = initial_max_phys;

    println_uefi!("[INFO] BOOT_INFO at 0x{:X}", boot_info_phys_addr);
    println_uefi!(
        "[INFO] BOOT_INFO.memory_map_count = {}",
        boot_info.memory_map_count
    );
    println_uefi!(
        "[INFO] BOOT_INFO.max_physical_address = 0x{:X} ({} MB)",
        boot_info.max_physical_address,
        boot_info.max_physical_address / (1024 * 1024)
    );
    println_uefi!(
        "[INFO] BOOT_INFO.memory_map[0]: start=0x{:X}, size=0x{:X}, type={}",
        boot_info.memory_map[0].start,
        boot_info.memory_map[0].size,
        boot_info.memory_map[0].region_type
    );

    // カーネルをロード (ブートサービス終了前に実行)
    println_uefi!("[INFO] Loading kernel from ELF...");
    let kernel_load = match load_kernel_elf(image_handle, boot_services) {
        Some(v) => v,
        None => {
            println_uefi!("[ERROR] Failed to load kernel!");
            loop {
                unsafe { core::arch::asm!("hlt") }
            }
        }
    };
    println_uefi!("[INFO] Kernel entry point: 0x{:X}", kernel_load.entry_phys);
    if !push_reserved_range(
        boot_info,
        kernel_load.image_phys_start,
        kernel_load.image_size,
        RESERVED_RANGE_KIND_KERNEL_IMAGE,
    ) {
        println_uefi!("[ERROR] Failed to reserve kernel image range");
        loop {
            unsafe { core::arch::asm!("hlt") }
        }
    }

    // カーネルロード後にメモリマップが変更されているので、再取得
    println_uefi!("[INFO] Updating memory map before page-table allocation...");
    map_size = unsafe {
        match fetch_memory_map(
            boot_services,
            memory_map_buffer,
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        ) {
            Ok(size) => size,
            Err(status) => {
                println_uefi!(
                    "[ERROR] Failed to get updated memory map! Status: 0x{:X}",
                    status
                );
                loop {
                    core::arch::asm!("hlt");
                }
            }
        }
    };

    let updated_entry_count = map_size / descriptor_size;
    let max_phys_addr = match analyze_bootstrap_map_limit(
        &memory_map_buffer[..map_size],
        updated_entry_count,
        descriptor_size,
    ) {
        Some(value) if value != 0 => value,
        _ => {
            println_uefi!("[ERROR] Updated bootstrap max physical address analysis failed");
            loop {
                unsafe { core::arch::asm!("hlt") }
            }
        }
    };
    if max_phys_addr > DIRECT_MAP_WINDOW_BYTES {
        println_uefi!(
            "[ERROR] Direct-map window overflow: max_phys=0x{:X}, limit=0x{:X}",
            max_phys_addr,
            DIRECT_MAP_WINDOW_BYTES
        );
        loop {
            unsafe { core::arch::asm!("hlt") }
        }
    }

    let required_gb = ceil_div_u64(max_phys_addr, PAGE_SIZE_1GB);
    let estimated_table_pages = match estimate_page_table_pages(max_phys_addr) {
        Some(pages) if pages > 0 => pages,
        _ => {
            println_uefi!(
                "[ERROR] Failed to estimate page-table pages (max_phys=0x{:X})",
                max_phys_addr
            );
            loop {
                unsafe { core::arch::asm!("hlt") }
            }
        }
    };
    println_uefi!("[INFO] required_gb = {}", required_gb);
    println_uefi!(
        "[INFO] Estimated page-table pages = {}",
        estimated_table_pages
    );

    let mut page_table_pool = unsafe {
        match AllocatedPageTablePool::allocate(boot_services, estimated_table_pages) {
            Ok(pool) => pool,
            Err(status) => {
                println_uefi!(
                    "[ERROR] AllocatePages for page tables failed! Status: 0x{:X}",
                    status
                );
                loop {
                    core::arch::asm!("hlt");
                }
            }
        }
    };
    boot_info.bootloader_page_table_ranges[0] = page_table_pool.range();
    boot_info.bootloader_page_table_range_count = 1;
    println_uefi!(
        "[INFO] Bootloader page-table range[0]: start=0x{:X}, size=0x{:X}",
        boot_info.bootloader_page_table_ranges[0].start,
        boot_info.bootloader_page_table_ranges[0].size
    );
    for i in 0..boot_info.bootloader_page_table_range_count {
        let range = boot_info.bootloader_page_table_ranges[i];
        if !push_reserved_range(
            boot_info,
            range.start,
            range.size,
            RESERVED_RANGE_KIND_BOOTLOADER_PT,
        ) {
            println_uefi!("[ERROR] Failed to reserve bootloader page-table ranges");
            loop {
                unsafe { core::arch::asm!("hlt") }
            }
        }
    }

    let pml4_addr = unsafe {
        match setup_initial_page_tables(max_phys_addr, &mut page_table_pool) {
            Some(addr) => addr,
            None => {
                println_uefi!("[ERROR] Failed to build initial page tables");
                loop {
                    core::arch::asm!("hlt");
                }
            }
        }
    };
    println_uefi!(
        "[INFO] Page-table allocation used {}/{} pages",
        page_table_pool.used_pages(),
        estimated_table_pages
    );

    println_uefi!("[INFO] Fetching final memory map before ExitBootServices...");
    map_size = unsafe {
        match fetch_memory_map(
            boot_services,
            memory_map_buffer,
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        ) {
            Ok(size) => size,
            Err(status) => {
                println_uefi!(
                    "[ERROR] Failed to get final memory map! Status: 0x{:X}",
                    status
                );
                loop {
                    core::arch::asm!("hlt");
                }
            }
        }
    };

    let final_entry_count = copy_memory_map_to_boot_info(
        boot_info,
        &memory_map_buffer[..map_size],
        map_size,
        descriptor_size,
    );
    let final_max_phys = match analyze_bootstrap_map_limit(
        &memory_map_buffer[..map_size],
        final_entry_count,
        descriptor_size,
    ) {
        Some(value) if value != 0 => value,
        _ => {
            println_uefi!("[ERROR] Final bootstrap max physical address analysis failed");
            loop {
                unsafe { core::arch::asm!("hlt") }
            }
        }
    };
    if final_max_phys > DIRECT_MAP_WINDOW_BYTES {
        println_uefi!(
            "[ERROR] Direct-map window overflow: max_phys=0x{:X}, limit=0x{:X}",
            final_max_phys,
            DIRECT_MAP_WINDOW_BYTES
        );
        loop {
            unsafe { core::arch::asm!("hlt") }
        }
    }
    boot_info.max_physical_address = final_max_phys;
    println_uefi!(
        "[INFO] BOOT_INFO.memory_map_count(final) = {}",
        boot_info.memory_map_count
    );
    println_uefi!(
        "[INFO] BOOT_INFO.max_physical_address(final) = 0x{:X} ({} MB)",
        boot_info.max_physical_address,
        boot_info.max_physical_address / (1024 * 1024)
    );
    println_uefi!(
        "[INFO] BOOT_INFO.bootloader_page_table_range_count = {}",
        boot_info.bootloader_page_table_range_count
    );
    println_uefi!(
        "[INFO] BOOT_INFO.reserved_range_count = {}",
        boot_info.reserved_range_count
    );

    let mut exit_retry = 0usize;
    loop {
        let status = unsafe { ((*boot_services).exit_boot_services)(image_handle, map_key) };
        if status == EFI_SUCCESS {
            break;
        }

        if status != EFI_INVALID_PARAMETER || exit_retry >= EXIT_BOOT_SERVICES_RETRY_LIMIT {
            println_uefi!(
                "[ERROR] Failed to exit boot services! Status: 0x{:X}, retries={}",
                status,
                exit_retry
            );
            loop {
                unsafe { core::arch::asm!("hlt") }
            }
        }

        exit_retry += 1;
        println_uefi!(
            "[WARN] ExitBootServices returned EFI_INVALID_PARAMETER, retrying ({}/{})",
            exit_retry,
            EXIT_BOOT_SERVICES_RETRY_LIMIT
        );

        map_size = unsafe {
            match fetch_memory_map(
                boot_services,
                memory_map_buffer,
                &mut map_key,
                &mut descriptor_size,
                &mut descriptor_version,
            ) {
                Ok(size) => size,
                Err(retry_status) => {
                    println_uefi!(
                        "[ERROR] Retry GetMemoryMap failed! Status: 0x{:X}",
                        retry_status
                    );
                    loop {
                        core::arch::asm!("hlt");
                    }
                }
            }
        };

        let retry_entry_count = copy_memory_map_to_boot_info(
            boot_info,
            &memory_map_buffer[..map_size],
            map_size,
            descriptor_size,
        );
        let retry_max_phys = match analyze_bootstrap_map_limit(
            &memory_map_buffer[..map_size],
            retry_entry_count,
            descriptor_size,
        ) {
            Some(value) if value != 0 => value,
            _ => {
                println_uefi!("[ERROR] Retry max physical address analysis failed");
                loop {
                    unsafe { core::arch::asm!("hlt") }
                }
            }
        };
        if retry_max_phys > DIRECT_MAP_WINDOW_BYTES {
            println_uefi!(
                "[ERROR] Direct-map window overflow during retry: max_phys=0x{:X}, limit=0x{:X}",
                retry_max_phys,
                DIRECT_MAP_WINDOW_BYTES
            );
            loop {
                unsafe { core::arch::asm!("hlt") }
            }
        }
        boot_info.max_physical_address = retry_max_phys;
    }

    // ExitBootServices成功 - ここから先はBoot Servicesは使用不可
    unsafe { load_page_tables(pml4_addr) };

    let kernel_high_addr = match kernel_load.entry_phys.checked_add(KERNEL_VMA) {
        Some(addr) => addr,
        None => loop {
            unsafe { core::arch::asm!("hlt") }
        },
    };

    // カーネルにジャンプ（BOOT_INFOの物理アドレスを渡す）
    type KernelEntry = extern "efiapi" fn(u64) -> !;
    let kernel_fn: KernelEntry = unsafe { core::mem::transmute(kernel_high_addr as *const ()) };
    kernel_fn(boot_info_phys_addr);
}

/// ELFファイルからカーネルをロード
fn load_kernel_elf(
    _image_handle: EfiHandle,
    boot_services: *mut EfiBootServices,
) -> Option<KernelLoadInfo> {
    // Simple File System Protocolを直接検索
    let mut sfs: *mut EfiSimpleFileSystemProtocol = core::ptr::null_mut();
    let status = unsafe {
        ((*boot_services).locate_protocol)(
            &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID,
            core::ptr::null_mut(),
            &mut sfs as *mut *mut _ as *mut *mut core::ffi::c_void,
        )
    };
    if status != EFI_SUCCESS {
        println_uefi!("[ERROR] Failed to locate Simple File System Protocol");
        return None;
    }

    // ルートディレクトリを開く
    let mut root: *mut EfiFileProtocol = core::ptr::null_mut();
    let status = unsafe { ((*sfs).open_volume)(sfs, &mut root) };
    if status != EFI_SUCCESS {
        println_uefi!("[ERROR] Failed to open root volume");
        return None;
    }

    // kernel.elfを開く
    let kernel_name = to_utf16("kernel.elf");
    let mut kernel_file: *mut EfiFileProtocol = core::ptr::null_mut();
    let status = unsafe {
        ((*root).open)(
            root,
            &mut kernel_file,
            kernel_name.as_ptr(),
            EFI_FILE_MODE_READ,
            0,
        )
    };
    if status != EFI_SUCCESS {
        println_uefi!("[ERROR] Failed to open kernel.elf");
        return None;
    }

    // ファイルを一時バッファに読み込む (最大2MB - staticを使用)
    static mut FILE_BUFFER: [u8; 2 * 1024 * 1024] = [0; 2 * 1024 * 1024];
    let file_buffer = unsafe { &mut *core::ptr::addr_of_mut!(FILE_BUFFER) };
    let mut file_size = file_buffer.len();
    let status = unsafe {
        ((*kernel_file).read)(
            kernel_file,
            &mut file_size,
            file_buffer.as_mut_ptr() as *mut core::ffi::c_void,
        )
    };
    unsafe {
        ((*kernel_file).close)(kernel_file);
        ((*root).close)(root);
    }

    if status != EFI_SUCCESS {
        println_uefi!("[ERROR] Failed to read kernel file");
        return None;
    }

    println_uefi!("[INFO] Kernel loaded: {} bytes", file_size);
    if file_size < core::mem::size_of::<Elf64Header>() {
        println_uefi!("[ERROR] Kernel file too small");
        return None;
    }

    // ELFヘッダーを検証
    let elf_header = unsafe { &*(file_buffer.as_ptr() as *const Elf64Header) };
    if !elf_header.is_valid() {
        println_uefi!("[ERROR] Invalid ELF header");
        return None;
    }

    // プログラムヘッダーを処理してLOADセグメントをメモリにコピー
    // 最初のLOADセグメントから仮想/物理アドレスのオフセットを計算
    let mut kernel_virt_offset: Option<u64> = None;
    let mut image_phys_start = u64::MAX;
    let mut image_phys_end = 0u64;
    let mut saw_load_segment = false;

    for i in 0..elf_header.e_phnum {
        let ph_offset =
            elf_header.e_phoff as usize + (i as usize * core::mem::size_of::<Elf64ProgramHeader>());
        let ph_end = ph_offset + core::mem::size_of::<Elf64ProgramHeader>();
        if ph_end > file_size {
            println_uefi!("[ERROR] Program header out of file bounds");
            return None;
        }
        let ph = unsafe { &*(file_buffer.as_ptr().add(ph_offset) as *const Elf64ProgramHeader) };

        if ph.p_type == PT_LOAD {
            if ph.p_memsz == 0 {
                continue;
            }
            if ph.p_filesz > ph.p_memsz {
                println_uefi!("[ERROR] Invalid PT_LOAD sizes");
                return None;
            }

            let segment_file_end = match (ph.p_offset as usize).checked_add(ph.p_filesz as usize) {
                Some(v) => v,
                None => {
                    println_uefi!("[ERROR] PT_LOAD file range overflow");
                    return None;
                }
            };
            if segment_file_end > file_size {
                println_uefi!("[ERROR] PT_LOAD file range exceeds input");
                return None;
            }

            let segment_start = ph.p_paddr;
            let segment_end = match ph.p_paddr.checked_add(ph.p_memsz) {
                Some(v) => v,
                None => {
                    println_uefi!("[ERROR] PT_LOAD physical range overflow");
                    return None;
                }
            };
            image_phys_start = image_phys_start.min(segment_start);
            image_phys_end = image_phys_end.max(segment_end);
            saw_load_segment = true;

            // 最初のLOADセグメントから仮想/物理アドレスのオフセットを記録
            if kernel_virt_offset.is_none() && ph.p_vaddr != ph.p_paddr {
                kernel_virt_offset = Some(ph.p_vaddr - ph.p_paddr);
            }

            // ファイルからメモリにコピー
            unsafe {
                let dst = ph.p_paddr as *mut u8;
                if ph.p_filesz != 0 {
                    let src = file_buffer.as_ptr().add(ph.p_offset as usize);
                    core::ptr::copy_nonoverlapping(src, dst, ph.p_filesz as usize);
                }

                // 残りをゼロクリア (BSS領域)
                if ph.p_memsz > ph.p_filesz {
                    core::ptr::write_bytes(
                        dst.add(ph.p_filesz as usize),
                        0,
                        (ph.p_memsz - ph.p_filesz) as usize,
                    );
                }
            }
        }
    }
    if !saw_load_segment {
        println_uefi!("[ERROR] ELF has no PT_LOAD segments");
        return None;
    }

    // エントリポイントを物理アドレスに変換
    // カーネルが高位アドレスでリンクされている場合、仮想アドレスを物理アドレスに変換
    let entry_phys = if let Some(offset) = kernel_virt_offset {
        match elf_header.e_entry.checked_sub(offset) {
            Some(v) => v,
            None => {
                println_uefi!("[ERROR] Kernel entry conversion overflow");
                return None;
            }
        }
    } else {
        elf_header.e_entry
    };

    Some(KernelLoadInfo {
        entry_phys,
        image_phys_start,
        image_size: image_phys_end - image_phys_start,
    })
}

/// 文字列をUTF-16に変換
fn to_utf16(s: &str) -> [u16; 32] {
    let mut buf = [0u16; 32];
    for (i, c) in s.chars().enumerate() {
        if i >= 31 {
            break;
        }
        buf[i] = c as u16;
    }
    buf
}
