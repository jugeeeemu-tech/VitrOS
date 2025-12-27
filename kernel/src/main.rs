#![no_std]
#![no_main]

extern crate alloc;

// OS カーネル処理
// アロケータ初期化、可視化テスト、メインループ

mod apic;
mod gdt;
mod idt;
mod pit;
mod timer;

use je4os_common::boot_info::BootInfo;
use je4os_common::graphics::FramebufferWriter;
use je4os_common::{allocator, error, info, println, uefi};
use alloc::boxed::Box;
use core::arch::asm;
use core::fmt::Write;
use core::panic::PanicInfo;
use lazy_static::lazy_static;
use spin::Mutex;

#[cfg(feature = "visualize-allocator")]
use je4os_common::allocator_visualization;

// グローバルフレームバッファライター
lazy_static! {
    static ref GLOBAL_FRAMEBUFFER: Mutex<Option<FramebufferWriter>> = Mutex::new(None);
}

/// グローバルフレームバッファを初期化
fn init_global_framebuffer(fb_base: u64, width: u32, height: u32, color: u32) {
    let mut fb = GLOBAL_FRAMEBUFFER.lock();
    *fb = Some(FramebufferWriter::new(fb_base, width, height, color));
}

/// グローバルフレームバッファに文字列を書き込む
fn fb_write(s: &str) {
    let mut fb = GLOBAL_FRAMEBUFFER.lock();
    if let Some(writer) = fb.as_mut() {
        let _ = write!(writer, "{}", s);
    }
}

/// グローバルフレームバッファに1行書き込む
fn fb_writeln(s: &str) {
    let mut fb = GLOBAL_FRAMEBUFFER.lock();
    if let Some(writer) = fb.as_mut() {
        let _ = writeln!(writer, "{}", s);
    }
}

// パニックハンドラ
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("\n!!! KERNEL PANIC !!!");
    println!("{}", info);
    loop {
        hlt()
    }
}

fn hlt() {
    unsafe {
        asm!("hlt");
    }
}

/// カーネルエントリポイント（トランポリン）
/// UEFIブートローダから呼ばれる - MS x64 ABI (RCX) から System V ABI (RDI) に変換
#[unsafe(no_mangle)]
#[unsafe(naked)]
extern "efiapi" fn kernel_main() -> ! {
    core::arch::naked_asm!(
        // MS x64 ABI: 第1引数は RCX
        // System V ABI: 第1引数は RDI
        // RCX の値を RDI に移動
        "mov rdi, rcx",
        // 実際のカーネルメイン関数を呼び出し
        "jmp {kernel_main_inner}",
        kernel_main_inner = sym kernel_main_inner,
    )
}

/// 実際のカーネルメイン関数 (System V ABI)
extern "C" fn kernel_main_inner(boot_info: &'static BootInfo) -> ! {
    info!("=== Kernel Started ===");

    // GDTを初期化
    info!("Initializing GDT...");
    gdt::init();
    info!("GDT initialized");

    // IDTを初期化
    info!("Initializing IDT...");
    idt::init();
    info!("IDT initialized");

    // Local APICを初期化
    info!("Initializing Local APIC...");
    apic::init();
    info!("Local APIC initialized");

    // APIC Timerをキャリブレーション（割り込み無効状態で実行）
    info!("Calibrating APIC Timer...");
    apic::calibrate_timer();

    // 割り込みを有効化
    unsafe {
        asm!("sti");
    }
    info!("Interrupts enabled");

    // グローバルフレームバッファを初期化（表示はヒープ初期化後）
    init_global_framebuffer(
        boot_info.framebuffer.base,
        boot_info.framebuffer.width,
        boot_info.framebuffer.height,
        0xFFFFFFFF,
    );

    // APIC Timerを初期化（100Hz）
    info!("Initializing APIC Timer...");
    const TIMER_FREQUENCY_HZ: u64 = 100;
    apic::init_timer(TIMER_FREQUENCY_HZ as u32);

    // タイマーシステムを初期化
    timer::init(TIMER_FREQUENCY_HZ);
    info!("Memory map count: {}", boot_info.memory_map_count);
    info!("Memory map array len: {}", boot_info.memory_map.len());

    // 利用可能なメモリを探してアロケータを初期化
    let mut largest_start = 0;
    let mut largest_size = 0;

    // 配列の範囲内に制限
    let safe_count = boot_info.memory_map_count.min(boot_info.memory_map.len());
    info!("Using safe count: {}", safe_count);

    for i in 0..safe_count {
        let region = &boot_info.memory_map[i];
        // region_type == 7 は EFI_CONVENTIONAL_MEMORY
        if region.region_type == uefi::EFI_CONVENTIONAL_MEMORY && region.size > largest_size as u64
        {
            largest_start = region.start as usize;
            largest_size = region.size as usize;
        }
    }

    if largest_size > 0 {
        info!("Found usable memory");

        // ヒープサイズを決定
        #[cfg(feature = "visualize-allocator")]
        let heap_size = largest_size.min(256 * 1024); // 可視化のため256KBに制限

        #[cfg(not(feature = "visualize-allocator"))]
        let heap_size = largest_size; // 本番環境では全て使用

        unsafe {
            allocator::init_heap(largest_start, heap_size);
        }

        info!("Heap initialized successfully");

        // ヒープ初期化後、フレームバッファに情報を表示
        // ブートローダーの出力の後に配置（時系列順）
        {
            let mut fb = GLOBAL_FRAMEBUFFER.lock();
            if let Some(writer) = fb.as_mut() {
                writer.set_position(10, 350);
            }
        }

        fb_writeln(&alloc::format!(
            "Framebuffer: 0x{:X}, {}x{}",
            boot_info.framebuffer.base, boot_info.framebuffer.width, boot_info.framebuffer.height
        ));
        fb_writeln(&alloc::format!("Memory regions: {}", boot_info.memory_map_count));
        fb_writeln(&alloc::format!(
            "Largest usable memory: 0x{:X} - 0x{:X} ({} MB)",
            largest_start,
            largest_start + largest_size,
            largest_size / 1024 / 1024
        ));
        fb_writeln(&alloc::format!("Heap initialized: {} KB", heap_size / 1024));

        // ヒープが初期化されたので、タイマーを登録できる
        info!("Registering test timers...");

        // 1秒後に実行されるタイマー
        timer::register_timer(timer::seconds_to_ticks(1), Box::new(|| {
            info!("Timer 1: 1 second elapsed!");
            fb_writeln("Timer 1: 1 second elapsed!");
        }));

        // 2秒後に実行されるタイマー
        timer::register_timer(timer::seconds_to_ticks(2), Box::new(|| {
            info!("Timer 2: 2 seconds elapsed!");
            fb_writeln("Timer 2: 2 seconds elapsed!");
        }));

        // 3秒後に実行されるタイマー
        timer::register_timer(timer::seconds_to_ticks(3), Box::new(|| {
            info!("Timer 3: 3 seconds elapsed!");
            fb_writeln("Timer 3: 3 seconds elapsed!");
        }));

        info!("Test timers registered");
    } else {
        error!("No usable memory found!");
        fb_writeln("ERROR: No usable memory!");
    }

    // 可視化テストを実行
    #[cfg(feature = "visualize-allocator")]
    {
        info!("Starting allocator visualization");
        // 可視化テストのために一時的にローカルライターを作成
        let mut local_writer = {
            let mut fb = GLOBAL_FRAMEBUFFER.lock();
            if let Some(writer) = fb.take() {
                writer
            } else {
                panic!("Global framebuffer not initialized!");
            }
        };
        allocator_visualization::run_visualization_tests(&mut local_writer);
        // 使用後にグローバルに戻す
        {
            let mut fb = GLOBAL_FRAMEBUFFER.lock();
            *fb = Some(local_writer);
        }
    }

    #[cfg(not(feature = "visualize-allocator"))]
    {
        fb_writeln("");
        fb_writeln("Kernel running...");
        fb_writeln("System ready.");
    }

    info!("Entering main loop");

    // メインループ
    loop {
        // ペンディングキューのタイマーを処理
        // この処理は割り込み有効状態で実行されるため、コールバック実行中も割り込みを受け付けられる
        timer::process_pending_timers();

        // CPUを省電力モードに（次の割り込みまで待機）
        hlt()
    }
}
