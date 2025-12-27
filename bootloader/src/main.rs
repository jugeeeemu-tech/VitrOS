#![no_std]
#![no_main]

use core::fmt::Write;
use core::panic::PanicInfo;
use je4os_common::boot_info::{BootInfo, FramebufferInfo, MemoryRegion};
use je4os_common::elf::{Elf64Header, Elf64ProgramHeader, PT_LOAD};
use je4os_common::graphics::FramebufferWriter;
use je4os_common::serial;
use je4os_common::uefi::*;
use je4os_common::{error, info, println};

// BOOT_INFOをカーネル直前の固定アドレスに配置
// 0x90000 (576KB) - カーネル(0x100000=1MB)の手前で安全
// この領域はConventional Memoryで、ExitBootServices後も有効
const BOOT_INFO_ADDR: usize = 0x90000;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("\n!!! BOOTLOADER PANIC !!!");
    println!("{}", info);
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

/// UEFI エントリポイント
#[unsafe(no_mangle)]
extern "efiapi" fn efi_main(
    image_handle: EfiHandle,
    system_table: *mut EfiSystemTable,
) -> EfiStatus {
    // シリアルポートを初期化
    serial::init();
    println!("=== je4OS Bootloader ===");
    info!("Serial output initialized");
    info!("Locating Graphics Output Protocol...");

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
        error!("Failed to locate GOP!");
        loop {
            unsafe { core::arch::asm!("hlt") }
        }
    }

    info!("GOP found successfully");

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

    // SAFETY: フレームバッファへの直接書き込み（画面クリア）
    unsafe {
        let fb_ptr = fb_base as *mut u32;
        let pixel_count = fb_size / 4;
        for i in 0..pixel_count {
            *fb_ptr.add(i) = 0x00000000;
        }
    }

    // FramebufferWriter を作成
    let mut writer = FramebufferWriter::new(fb_base, width, height, 0xFFFFFFFF);
    writer.set_position(10, 10);
    let _ = writeln!(writer, "je4OS - Memory Map");

    // メモリマップを取得
    let mut map_size: usize = 0;
    let mut map_key: usize = 0;
    let mut descriptor_size: usize = 0;
    let mut descriptor_version: u32 = 0;

    // SAFETY: UEFI 関数呼び出し - メモリマップサイズ取得
    unsafe {
        ((*boot_services).get_memory_map)(
            &mut map_size,
            core::ptr::null_mut(),
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        );
    }

    // バッファを確保（スタック上に）
    let mut buffer = [0u8; 4096 * 4];
    map_size = buffer.len();

    // SAFETY: UEFI 関数呼び出し - 実際のメモリマップ取得
    let status = unsafe {
        ((*boot_services).get_memory_map)(
            &mut map_size,
            buffer.as_mut_ptr() as *mut EfiMemoryDescriptor,
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        )
    };

    // BOOT_INFOを固定アドレス（0xF000）に配置
    let boot_info = unsafe { &mut *(BOOT_INFO_ADDR as *mut BootInfo) };
    *boot_info = BootInfo::new();

    // フレームバッファ情報を設定
    boot_info.framebuffer = FramebufferInfo {
        base: fb_base,
        size: fb_size as u64,
        width,
        height,
        stride: width,
    };

    if status == EFI_SUCCESS {
        let entry_count = map_size / descriptor_size;
        info!("Memory map retrieved: {} entries", entry_count);

        // メモリマップを表示
        writer.set_position(10, 30);
        let max_display = 20;

        println!(
            "\nMemory Map (first {} entries):",
            max_display.min(entry_count)
        );
        for i in 0..entry_count.min(max_display) {
            let offset = i * descriptor_size;

            // SAFETY: バッファ内の有効なメモリディスクリプタを参照
            let desc = unsafe { &*(buffer.as_ptr().add(offset) as *const EfiMemoryDescriptor) };

            let type_str = memory_type_str(desc.r#type);
            println!(
                "  {:<12} 0x{:016X}  Pages: 0x{:X}",
                type_str, desc.physical_start, desc.number_of_pages
            );

            let _ = writeln!(
                writer,
                "{:<12} 0x{:016X}  Pages: 0x{:X}",
                type_str, desc.physical_start, desc.number_of_pages
            );
        }

        let _ = writeln!(writer);
        let _ = writeln!(writer, "Total entries: {}", entry_count);

        // BootInfo にメモリマップをコピー
        for i in 0..entry_count.min(boot_info.memory_map.len()) {
            let offset = i * descriptor_size;
            let desc = unsafe { &*(buffer.as_ptr().add(offset) as *const EfiMemoryDescriptor) };

            boot_info.memory_map[i] = MemoryRegion {
                start: desc.physical_start,
                size: desc.number_of_pages * 4096,
                region_type: desc.r#type,
            };
        }
        boot_info.memory_map_count = entry_count.min(boot_info.memory_map.len());
        info!("BOOT_INFO at 0x{:X}", BOOT_INFO_ADDR);
        info!("BOOT_INFO.memory_map_count = {}", boot_info.memory_map_count);
        info!(
            "BOOT_INFO.memory_map[0]: start=0x{:X}, size=0x{:X}, type={}",
            boot_info.memory_map[0].start, boot_info.memory_map[0].size, boot_info.memory_map[0].region_type
        );
    }

    // カーネルをロード (ブートサービス終了前に実行)
    info!("Loading kernel from ELF...");
    let kernel_entry = load_kernel_elf(image_handle, boot_services);
    if kernel_entry == 0 {
        error!("Failed to load kernel!");
        let _ = writeln!(writer, "ERROR: Failed to load kernel!");
        loop {
            unsafe { core::arch::asm!("hlt") }
        }
    }
    info!("Kernel entry point: 0x{:X}", kernel_entry);

    // カーネルロード後にメモリマップが変更されているので、再取得
    info!("Updating memory map before ExitBootServices...");

    // まず必要なサイズを取得
    map_size = 0;
    unsafe {
        ((*boot_services).get_memory_map)(
            &mut map_size,
            core::ptr::null_mut(),
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        );
    }

    // 余裕を持たせる（メモリマップが増える可能性があるため）
    map_size += descriptor_size * 2;

    if map_size > buffer.len() {
        error!("Memory map too large! Required: {}, Available: {}", map_size, buffer.len());
        loop {
            unsafe { core::arch::asm!("hlt") }
        }
    }

    let status = unsafe {
        ((*boot_services).get_memory_map)(
            &mut map_size,
            buffer.as_mut_ptr() as *mut EfiMemoryDescriptor,
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        )
    };
    if status != EFI_SUCCESS {
        error!("Failed to get updated memory map! Status: 0x{:X}", status);
        loop {
            unsafe { core::arch::asm!("hlt") }
        }
    }

    // ブートローダーの最終メッセージ（メモリマップの後に表示）
    let _ = writeln!(writer);
    let _ = writeln!(writer, "Exiting boot services...");

    // SAFETY: UEFI 関数呼び出し - ブートサービス終了
    info!("Exiting boot services...");
    let status = unsafe { ((*boot_services).exit_boot_services)(image_handle, map_key) };

    if status == EFI_SUCCESS {
        info!("Boot services exited successfully!");
        let _ = writeln!(writer, "Boot Services Exited!");
        let _ = writeln!(writer);
        let _ = writeln!(writer, "Jumping to kernel...");
        let _ = writeln!(writer);
        let _ = writeln!(writer, "--- Kernel Output ---");
    } else {
        error!("Failed to exit boot services! Status: 0x{:X}", status);
        writer.set_color(0xFF0000);
        let _ = writeln!(writer, "Exit failed!");
        loop {
            unsafe { core::arch::asm!("hlt") }
        }
    }

    info!("Bootloader finished, jumping to kernel...");

    // カーネルジャンプ直前にBOOT_INFOを再確認
    let boot_info_check = unsafe { &*(BOOT_INFO_ADDR as *const BootInfo) };
    info!("Pre-jump check: BOOT_INFO.memory_map_count = {}", boot_info_check.memory_map_count);
    info!("Pre-jump check: BOOT_INFO.framebuffer.base = 0x{:X}", boot_info_check.framebuffer.base);

    // カーネルにジャンプ (efiapi calling convention to match kernel entry point)
    type KernelEntry = extern "efiapi" fn(&'static BootInfo) -> !;
    let kernel_fn: KernelEntry = unsafe { core::mem::transmute(kernel_entry as *const ()) };
    kernel_fn(boot_info_check);
}

/// ELFファイルからカーネルをロード
fn load_kernel_elf(_image_handle: EfiHandle, boot_services: *mut EfiBootServices) -> u64 {
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
        error!("Failed to locate Simple File System Protocol");
        return 0;
    }

    // ルートディレクトリを開く
    let mut root: *mut EfiFileProtocol = core::ptr::null_mut();
    let status = unsafe { ((*sfs).open_volume)(sfs, &mut root) };
    if status != EFI_SUCCESS {
        error!("Failed to open root volume");
        return 0;
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
        error!("Failed to open kernel.elf");
        return 0;
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
        error!("Failed to read kernel file");
        return 0;
    }

    info!("Kernel loaded: {} bytes", file_size);

    // ELFヘッダーを検証
    let elf_header = unsafe { &*(file_buffer.as_ptr() as *const Elf64Header) };
    if !elf_header.is_valid() {
        error!("Invalid ELF header");
        return 0;
    }

    // プログラムヘッダーを処理してLOADセグメントをメモリにコピー
    for i in 0..elf_header.e_phnum {
        let ph_offset = elf_header.e_phoff as usize + (i as usize * core::mem::size_of::<Elf64ProgramHeader>());
        let ph = unsafe { &*(file_buffer.as_ptr().add(ph_offset) as *const Elf64ProgramHeader) };

        if ph.p_type == PT_LOAD {
            // ファイルからメモリにコピー
            unsafe {
                let src = file_buffer.as_ptr().add(ph.p_offset as usize);
                let dst = ph.p_paddr as *mut u8;
                core::ptr::copy_nonoverlapping(src, dst, ph.p_filesz as usize);

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

    elf_header.e_entry
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
