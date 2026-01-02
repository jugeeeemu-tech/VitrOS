#![no_std]
#![no_main]

extern crate alloc;

// OS カーネル処理
// アロケータ初期化、可視化テスト、メインループ

mod acpi;
mod addr;
mod allocator;
mod apic;
mod gdt;
mod graphics;
mod idt;
mod io;
mod paging;
mod pci;
mod pit;
mod serial;
mod sync;
mod task;
mod timer;

#[cfg(feature = "visualize-allocator")]
mod allocator_visualization;

use crate::graphics::FramebufferWriter;
use alloc::boxed::Box;
use core::arch::asm;
use core::fmt::Write;
use core::panic::PanicInfo;
use vitros_common::boot_info::BootInfo;
use vitros_common::uefi;

#[cfg(feature = "visualize-allocator")]
use crate::allocator_visualization;

// カーネル仮想アドレスベース（ブートローダと同じ値）
const KERNEL_VMA: u64 = 0xFFFF800000000000;

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

/// カーネル起動完了マーカー
/// GDBブレークポイント用（シンボル名を固定）
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn boot_complete() {
    // 最適化で消されないようにvolatile read
    unsafe {
        core::ptr::read_volatile(&0u8);
    }
}

// =============================================================================
// タスクエントリポイント
// =============================================================================

/// アイドルタスク：CPUを休止状態にし続ける
extern "C" fn idle_task() -> ! {
    info!("[Idle] Idle task started");
    loop {
        unsafe {
            asm!("hlt");
        }
    }
}

/// タスク1：カウンタを表示し続ける（優先度：高）
extern "C" fn task1() -> ! {
    info!("[Task1] Started (High Priority)");

    // 新Writer方式：固有の描画領域を取得
    let region = graphics::Region::new(400, 500, 350, 20);
    let buffer = graphics::compositor::register_writer(region).expect("Failed to register writer");
    let mut writer = graphics::TaskWriter::new(buffer, 0xFFFFFFFF);

    let mut counter = 0u64;
    loop {
        writer.clear(0x00000000);
        // tick数を表示してタイマー割り込みが発生しているか確認
        let tick = timer::current_tick();
        let _ = write!(writer, "[Task1] Count:{} Tick:{}", counter, tick);
        counter += 1;
        // 描画頻度を制限（16ms = 約60fps）
        task::sleep_ms(16);
    }
}

/// タスク2：カウンタを表示し続ける（優先度：中）
extern "C" fn task2() -> ! {
    info!("[Task2] Started (Medium Priority)");

    // 新Writer方式：固有の描画領域を取得
    let region = graphics::Region::new(400, 520, 300, 20);
    let buffer = graphics::compositor::register_writer(region).expect("Failed to register writer");
    let mut writer = graphics::TaskWriter::new(buffer, 0xFFFFFFFF);

    let mut counter = 0u64;
    loop {
        writer.clear(0x00000000);
        let _ = write!(writer, "[Task2 Med ] Count: {}", counter);
        counter += 1;
        // 描画頻度を制限（16ms = 約60fps）
        task::sleep_ms(16);
    }
}

/// タスク3：カウンタを表示し続ける（優先度：低）
extern "C" fn task3() -> ! {
    info!("[Task3] Started (Low Priority)");

    // 新Writer方式：固有の描画領域を取得
    let region = graphics::Region::new(400, 540, 300, 20);
    let buffer = graphics::compositor::register_writer(region).expect("Failed to register writer");
    let mut writer = graphics::TaskWriter::new(buffer, 0xFFFFFFFF);

    let mut counter = 0u64;
    loop {
        writer.clear(0x00000000);
        let _ = write!(writer, "[Task3 Low ] Count: {}", counter);
        counter += 1;
        // 描画頻度を制限（16ms = 約60fps）
        task::sleep_ms(16);
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

        // カーネルスタックに切り替え（Rust関数を呼ぶ前に実行）
        "lea rsp, [rip + {kernel_stack}]",
        "add rsp, {stack_size}",

        // 実際のカーネルメイン関数を呼び出し
        // この時点で既に新しいカーネルスタック上で動作している
        "call {kernel_main_inner}",

        // kernel_main_inner は戻ってこないが、念のため無限ループ
        "2: jmp 2b",

        kernel_stack = sym paging::KERNEL_STACK,
        stack_size = const core::mem::size_of::<paging::KernelStack>(),
        kernel_main_inner = sym kernel_main_inner,
    )
}

/// 実際のカーネルメイン関数 (System V ABI)
/// この関数が呼ばれた時点で既にカーネルスタック上で動作している
extern "C" fn kernel_main_inner(boot_info_phys_addr: u64) -> ! {
    info!("=== Kernel Started ===");
    info!("Running on kernel stack");

    // 物理アドレスを高位仮想アドレスに変換してboot_infoにアクセス
    // 低位物理アドレスは高位にマッピングされているため、コピー不要
    let boot_info_virt_addr = KERNEL_VMA + boot_info_phys_addr;
    let boot_info = unsafe { &*(boot_info_virt_addr as *const BootInfo) };

    // GDTを初期化
    info!("Initializing GDT...");
    gdt::init().expect("Failed to initialize GDT");
    info!("GDT initialized");

    // ブートローダーが既にページングを設定し、高位アドレスで起動している
    info!("Running in higher-half (set up by bootloader)");

    // カーネル用のページテーブルを作成（UEFIメモリマップに基づいて動的にマッピング）
    info!("Creating kernel page tables...");
    paging::init(boot_info).expect("Failed to initialize paging system");
    info!("Kernel page tables created and loaded");

    // GDTを高位アドレスで再ロード（念のため）
    info!("Reloading GDT...");
    gdt::init().expect("Failed to reload GDT");
    info!("GDT reloaded");

    // IDTを初期化
    info!("Initializing IDT...");
    idt::init().expect("Failed to initialize IDT");
    info!("IDT initialized");

    // タスクシステムを初期化
    task::init();

    // ACPI を初期化
    acpi::init(&boot_info);

    // PCIバスをスキャン
    pci::scan_pci_bus();

    // Local APICを初期化
    info!("Initializing Local APIC...");
    apic::init();
    info!("Local APIC initialized");

    // APIC Timerをキャリブレーション（割り込み無効状態で実行）
    info!("Calibrating APIC Timer...");
    apic::calibrate_timer().expect("Failed to calibrate APIC Timer");

    // ローカルフレームバッファを初期化
    // 物理アドレスを高位仮想アドレスに変換
    let fb_virt_base = paging::phys_to_virt(boot_info.framebuffer.base)
        .expect("Failed to convert framebuffer address");
    let mut fb_writer = FramebufferWriter::new(
        fb_virt_base,
        boot_info.framebuffer.width,
        boot_info.framebuffer.height,
        0xFFFFFFFF,
    );

    // カーネル起動時に画面を黒でクリア
    fb_writer.clear_screen(0x00000000);

    info!("Memory map count: {}", boot_info.memory_map_count);
    info!("Memory map array len: {}", boot_info.memory_map.len());

    // 利用可能なメモリを探してアロケータを初期化
    let mut largest_start_phys: u64 = 0;
    let mut largest_size = 0;

    // 配列の範囲内に制限
    let safe_count = boot_info.memory_map_count.min(boot_info.memory_map.len());
    info!("Using safe count: {}", safe_count);

    for i in 0..safe_count {
        let region = &boot_info.memory_map[i];
        // region_type == 7 は EFI_CONVENTIONAL_MEMORY
        if region.region_type == uefi::EFI_CONVENTIONAL_MEMORY && region.size > largest_size as u64
        {
            largest_start_phys = region.start;
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

        // 物理アドレスを高位仮想アドレスに変換
        let largest_start_virt =
            paging::phys_to_virt(largest_start_phys).expect("Failed to convert heap address");
        info!(
            "Heap: phys=0x{:X} virt=0x{:X}",
            largest_start_phys, largest_start_virt
        );

        unsafe {
            allocator::init_heap(largest_start_virt as usize, heap_size);
        }

        // 可視化テストを実行
        #[cfg(feature = "visualize-allocator")]
        {
            info!("Starting allocator visualization");
            allocator_visualization::run_visualization_tests(&mut fb_writer);
        }

        info!("Heap initialized successfully");

        // タイマーシステムを初期化（ヒープが必要）
        const TIMER_FREQUENCY_HZ: u64 = 250;
        timer::init(TIMER_FREQUENCY_HZ);

        // APIC Timerを初期化（250Hz = 4msタイムスライス）
        info!("Initializing APIC Timer...");
        apic::init_timer(TIMER_FREQUENCY_HZ as u32).expect("Failed to initialize APIC Timer");

        // =================================================================
        // Compositorを初期化
        // =================================================================
        info!("Initializing Compositor...");
        graphics::compositor::init_compositor(graphics::compositor::CompositorConfig {
            fb_base: fb_virt_base,
            fb_width: boot_info.framebuffer.width,
            fb_height: boot_info.framebuffer.height,
            refresh_interval_ticks: 10,
        });
        info!("Compositor initialized");

        // =================================================================
        // プリエンプティブマルチタスキングのタスクを作成（割り込み無効状態で）
        // =================================================================
        info!("Creating tasks for preemptive multitasking...");

        // Compositorタスク（優先度：最高）
        let compositor = Box::new(
            task::Task::new(
                "Compositor",
                task::priority::MAX,
                graphics::compositor::compositor_task,
            )
            .expect("Failed to create Compositor task"),
        );
        task::add_task(*compositor);

        // アイドルタスク（優先度：最低）
        let idle =
            Box::new(task::Task::new_idle("Idle", idle_task).expect("Failed to create idle task"));
        task::add_task(*idle);

        // ワーカータスク1（優先度：高）
        let t1 = Box::new(
            task::Task::new("Task1", task::priority::DEFAULT + 10, task1)
                .expect("Failed to create Task1"),
        );
        task::add_task(*t1);

        // ワーカータスク2（優先度：中）
        let t2 = Box::new(
            task::Task::new("Task2", task::priority::DEFAULT, task2)
                .expect("Failed to create Task2"),
        );
        task::add_task(*t2);

        // ワーカータスク3（優先度：低）
        let t3 = Box::new(
            task::Task::new("Task3", task::priority::MIN, task3).expect("Failed to create Task3"),
        );
        task::add_task(*t3);

        info!("All tasks created. Setting up kernel main task...");

        // kernel_main_innerを表すタスクを作成し、CURRENT_TASKに設定
        // 注意：entry_pointとしてidle_taskを指定しているが、これは使われない
        // このタスクはold_contextとして最初のswitch_context()で保存される側なので、
        // 初期Contextの値（rip=task_wrapper, rdi=idle_task）は上書きされる
        // 保存されるripは「schedule()から戻るアドレス」になる
        let kernel_main = Box::new(
            task::Task::new("KernelMain", task::priority::DEFAULT, idle_task)
                .expect("Failed to create KernelMain task"),
        );
        task::set_current_task(*kernel_main);
        info!("Kernel main task set as current");

        // 最初のタスクにスケジュール
        // これ以降、タイマー割り込みで自動的にタスクが切り替わる
        info!("Calling schedule()...");
        task::schedule();

        // kernel_main_innerタスクが再スケジュールされた時、ここに戻ってくる
        // 割り込みを有効化（schedule()から戻ってきた時点では割り込み無効）
        unsafe {
            asm!("sti");
        }

        info!("Returned from scheduler! KernelMain task rescheduled, entering idle loop...");

        // TaskWriterで情報を表示（Compositor経由）
        let region = graphics::Region::new(10, 350, 700, 80);
        let buffer =
            graphics::compositor::register_writer(region).expect("Failed to register writer");
        let mut writer = graphics::TaskWriter::new(buffer, 0xFFFFFFFF);

        let _ = writeln!(
            writer,
            "Framebuffer: 0x{:X}, {}x{}",
            boot_info.framebuffer.base, boot_info.framebuffer.width, boot_info.framebuffer.height
        );
        let _ = writeln!(writer, "Memory regions: {}", boot_info.memory_map_count);
        let _ = writeln!(
            writer,
            "Largest usable memory: phys=0x{:X} virt=0x{:X} - 0x{:X} ({} MB)",
            largest_start_phys,
            largest_start_virt,
            largest_start_virt + largest_size as u64,
            largest_size / 1024 / 1024
        );
        let _ = writeln!(writer, "Heap initialized: {} KB", heap_size / 1024);

        #[cfg(not(feature = "visualize-allocator"))]
        {
            let _ = writeln!(writer, "");
            let _ = writeln!(writer, "Kernel running...");
            let _ = writeln!(writer, "System ready.");
        }

        // ヒープが初期化されたので、タイマーを登録できる
        info!("Registering test timers...");

        // 1秒後に実行されるタイマー
        timer::register_timer(
            timer::seconds_to_ticks(1),
            Box::new(|| {
                info!("Timer 1: 1 second elapsed!");
            }),
        );

        // 2秒後に実行されるタイマー
        timer::register_timer(
            timer::seconds_to_ticks(2),
            Box::new(|| {
                info!("Timer 2: 2 seconds elapsed!");
            }),
        );

        // 3秒後に実行されるタイマー
        timer::register_timer(
            timer::seconds_to_ticks(3),
            Box::new(|| {
                info!("Timer 3: 3 seconds elapsed!");
            }),
        );

        info!("Test timers registered");
    } else {
        error!("No usable memory found!");
    }

    info!("Entering main loop");
    boot_complete();

    // KernelMainタスクは自身をブロックして停止
    loop {
        task::block_current_task();
    }
}
