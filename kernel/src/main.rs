#![no_std]
#![no_main]

extern crate alloc;

// OS カーネル処理
// アロケータ初期化、可視化テスト、メインループ

mod acpi;
mod addr;
mod apic;
mod gdt;
mod idt;
mod paging;
mod pci;
mod pit;
mod task;
mod timer;

use alloc::boxed::Box;
use core::arch::asm;
use core::fmt::Write;
use core::panic::PanicInfo;
use vitros_common::boot_info::BootInfo;
use vitros_common::graphics::FramebufferWriter;
use vitros_common::{allocator, error, info, println, uefi};
use lazy_static::lazy_static;
use spin::Mutex;

#[cfg(feature = "visualize-allocator")]
use vitros_common::allocator_visualization;

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

// =============================================================================
// タスクエントリポイント
// =============================================================================

/// アイドルタスク：CPUを休止状態にし続ける
extern "C" fn idle_task() -> ! {
    info!("[Idle] Idle task started");
    fb_writeln("[Idle] Started");
    loop {
        unsafe {
            asm!("hlt");
        }
    }
}

/// タスク1：カウンタを表示し続ける（優先度：高）
extern "C" fn task1() -> ! {
    info!("[Task1] Started (High Priority)");

    let mut counter = 0u64;
    loop {
        // info!("[Task1] Counter: {}", counter);

        // 固定位置（X=400, Y=500）にカウンタを表示
        if counter % 70000000 == 0 ||true {
            {
                let mut fb = GLOBAL_FRAMEBUFFER.lock();
                if let Some(writer) = fb.as_mut() {
                    writer.set_position(400, 500);
                    writer.clear_area(30, 0x00000000); // 30文字分、黒色でクリア
                }
            }
            fb_writeln(&alloc::format!("[Task1 High] Count: {}", counter));
        }

        counter += 1;

        // 忙しいループで時間を消費
        // for _ in 0..1_000 {
        //     core::hint::spin_loop();
        // }
    }
}

/// タスク2：カウンタを表示し続ける（優先度：中）
extern "C" fn task2() -> ! {
    info!("[Task2] Started (Medium Priority)");

    let mut counter = 0u64;
    loop {
        // info!("[Task2] Counter: {}", counter);

        // 固定位置（X=400, Y=520）にカウンタを表示
        if counter % 70000000 == 0 || true {
            {
                let mut fb = GLOBAL_FRAMEBUFFER.lock();
                if let Some(writer) = fb.as_mut() {
                    writer.set_position(400, 520);
                    writer.clear_area(30, 0x00000000); // 30文字分、黒色でクリア
                }
            }
            fb_writeln(&alloc::format!("[Task2 Med ] Count: {}", counter));
        }

        counter += 1;
        // 忙しいループで時間を消費
        // for _ in 0..1_000 {
        //     core::hint::spin_loop();
        // }
    }
}

/// タスク3：カウンタを表示し続ける（優先度：低）
extern "C" fn task3() -> ! {
    info!("[Task3] Started (Low Priority)");

    let mut counter = 0u64;
    loop {
        // info!("[Task3] Counter: {}", counter);

        // 固定位置（X=400, Y=540）にカウンタを表示
        if counter % 70000000  == 0 || true {
            {
                let mut fb = GLOBAL_FRAMEBUFFER.lock();
                if let Some(writer) = fb.as_mut() {
                    writer.set_position(400, 540);
                    writer.clear_area(30, 0x00000000); // 30文字分、黒色でクリア
                }
            }
            fb_writeln(&alloc::format!("[Task3 Low ] Count: {}", counter));
        }

        counter += 1;
        // 忙しいループで時間を消費
        // for _ in 0..1_000 {
        //     core::hint::spin_loop();
        // }
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
extern "C" fn kernel_main_inner(boot_info_ptr: &'static BootInfo) -> ! {
    info!("=== Kernel Started ===");
    info!("Running on kernel stack");

    // boot_infoを新しいスタックにコピー
    // この時点ではまだ低位アドレス（0x90000）にアクセス可能
    let boot_info = *boot_info_ptr;

    // GDTを初期化
    info!("Initializing GDT...");
    gdt::init().expect("Failed to initialize GDT");
    info!("GDT initialized");

    // ブートローダーが既にページングを設定し、高位アドレスで起動している
    info!("Running in higher-half (set up by bootloader)");

    // カーネル用のページテーブルを作成（高位アドレスのみ、低位は自動的にアンマップ）
    info!("Creating kernel page tables...");
    paging::init().expect("Failed to initialize paging system");
    info!("Kernel page tables created and loaded (low addresses now unmapped)");

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

    // グローバルフレームバッファを初期化（表示はヒープ初期化後）
    init_global_framebuffer(
        boot_info.framebuffer.base,
        boot_info.framebuffer.width,
        boot_info.framebuffer.height,
        0xFFFFFFFF,
    );

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

        info!("Heap initialized successfully");

        // タイマーシステムを初期化（ヒープが必要）
        const TIMER_FREQUENCY_HZ: u64 = 100;
        timer::init(TIMER_FREQUENCY_HZ);

        // APIC Timerを初期化（100Hz）
        info!("Initializing APIC Timer...");
        apic::init_timer(TIMER_FREQUENCY_HZ as u32).expect("Failed to initialize APIC Timer");

        // =================================================================
        // プリエンプティブマルチタスキングのタスクを作成（割り込み無効状態で）
        // =================================================================
        info!("Creating tasks for preemptive multitasking...");

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
            boot_info.framebuffer.base,
            boot_info.framebuffer.width,
            boot_info.framebuffer.height
        ));
        fb_writeln(&alloc::format!(
            "Memory regions: {}",
            boot_info.memory_map_count
        ));
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
        timer::register_timer(
            timer::seconds_to_ticks(1),
            Box::new(|| {
                info!("Timer 1: 1 second elapsed!");
                fb_writeln("Timer 1: 1 second elapsed!");
            }),
        );

        // 2秒後に実行されるタイマー
        timer::register_timer(
            timer::seconds_to_ticks(2),
            Box::new(|| {
                info!("Timer 2: 2 seconds elapsed!");
                fb_writeln("Timer 2: 2 seconds elapsed!");
            }),
        );

        // 3秒後に実行されるタイマー
        timer::register_timer(
            timer::seconds_to_ticks(3),
            Box::new(|| {
                info!("Timer 3: 3 seconds elapsed!");
                fb_writeln("Timer 3: 3 seconds elapsed!");
            }),
        );

        info!("Test timers registered");
    } else {
        error!("No usable memory found!");
        fb_writeln("ERROR: No usable memory!");
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
