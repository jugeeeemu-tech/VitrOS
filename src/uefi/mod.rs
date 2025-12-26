// UEFI型定義
pub type EfiHandle = *mut core::ffi::c_void;
pub type EfiStatus = usize;

// EFIステータスコード
pub const EFI_SUCCESS: EfiStatus = 0;

// GUID (プロトコル識別子)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EfiGuid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

// Graphics Output Protocol GUID
pub const EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID: EfiGuid = EfiGuid {
    data1: 0x9042a9de,
    data2: 0x23dc,
    data3: 0x4a38,
    data4: [0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a],
};

// テーブルヘッダ
#[repr(C)]
pub struct EfiTableHeader {
    pub signature: u64,
    pub revision: u32,
    pub header_size: u32,
    pub crc32: u32,
    pub reserved: u32,
}

const _: () = assert!(core::mem::size_of::<EfiTableHeader>() == 24);

// Graphics Output Protocol Mode Information
#[repr(C)]
pub struct EfiGraphicsOutputModeInformation {
    pub version: u32,
    pub horizontal_resolution: u32,
    pub vertical_resolution: u32,
    pub pixel_format: u32,
    pub pixel_information: [u32; 4],
    pub pixels_per_scan_line: u32,
}

// Graphics Output Protocol Mode
#[repr(C)]
pub struct EfiGraphicsOutputProtocolMode {
    pub max_mode: u32,
    pub mode: u32,
    pub info: *mut EfiGraphicsOutputModeInformation,
    pub size_of_info: usize,
    pub frame_buffer_base: u64,
    pub frame_buffer_size: usize,
}

// Graphics Output Protocol
#[repr(C)]
pub struct EfiGraphicsOutputProtocol {
    pub query_mode: usize,
    pub set_mode: usize,
    pub blt: usize,
    pub mode: *mut EfiGraphicsOutputProtocolMode,
}

const _: () = assert!(core::mem::offset_of!(EfiGraphicsOutputProtocol, mode) == 24);

// メモリタイプ
pub const EFI_RESERVED_MEMORY_TYPE: u32 = 0;
pub const EFI_LOADER_CODE: u32 = 1;
pub const EFI_LOADER_DATA: u32 = 2;
pub const EFI_BOOT_SERVICES_CODE: u32 = 3;
pub const EFI_BOOT_SERVICES_DATA: u32 = 4;
pub const EFI_RUNTIME_SERVICES_CODE: u32 = 5;
pub const EFI_RUNTIME_SERVICES_DATA: u32 = 6;
pub const EFI_CONVENTIONAL_MEMORY: u32 = 7;
pub const EFI_UNUSABLE_MEMORY: u32 = 8;
pub const EFI_ACPI_RECLAIM_MEMORY: u32 = 9;
pub const EFI_ACPI_MEMORY_NVS: u32 = 10;
pub const EFI_MEMORY_MAPPED_IO: u32 = 11;
pub const EFI_MEMORY_MAPPED_IO_PORT_SPACE: u32 = 12;
pub const EFI_PAL_CODE: u32 = 13;

// メモリディスクリプタ
#[repr(C)]
pub struct EfiMemoryDescriptor {
    pub r#type: u32,
    pub physical_start: u64,
    pub virtual_start: u64,
    pub number_of_pages: u64,
    pub attribute: u64,
}

// Boot Services（最小限）
#[repr(C)]
pub struct EfiBootServices {
    pub hdr: EfiTableHeader,
    _pad1: [usize; 4],  // 1-4: RaiseTPL, RestoreTPL, AllocatePages, FreePages
    pub get_memory_map: extern "efiapi" fn(
        *mut usize,                      // MemoryMapSize
        *mut EfiMemoryDescriptor,        // MemoryMap
        *mut usize,                      // MapKey
        *mut usize,                      // DescriptorSize
        *mut u32,                        // DescriptorVersion
    ) -> EfiStatus,
    _pad2: [usize; 21], // 6-26: その他の関数
    pub exit_boot_services: extern "efiapi" fn(
        EfiHandle,  // ImageHandle
        usize,      // MapKey
    ) -> EfiStatus,
    _pad3: [usize; 10], // 28-37: その他の関数
    pub locate_protocol: extern "efiapi" fn(
        *const EfiGuid,
        *mut core::ffi::c_void,
        *mut *mut core::ffi::c_void,
    ) -> EfiStatus,
}

const _: () = {
    assert!(core::mem::offset_of!(EfiBootServices, get_memory_map) == 56);
    assert!(core::mem::offset_of!(EfiBootServices, exit_boot_services) == 232);
    assert!(core::mem::offset_of!(EfiBootServices, locate_protocol) == 320);
};

// システムテーブル（必要最小限）
#[repr(C)]
pub struct EfiSystemTable {
    pub hdr: EfiTableHeader,
    pub firmware_vendor: *const u16,
    pub firmware_revision: u32,
    pub console_in_handle: EfiHandle,
    pub con_in: usize,
    pub console_out_handle: EfiHandle,
    pub con_out: usize,
    pub console_err_handle: EfiHandle,
    pub std_err: usize,
    pub runtime_services: usize,
    pub boot_services: *mut EfiBootServices,
    pub number_of_table_entries: usize,
    pub configuration_table: usize,
}

const _: () = assert!(core::mem::offset_of!(EfiSystemTable, boot_services) == 96);
