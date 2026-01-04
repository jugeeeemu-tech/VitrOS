#![no_std]
#![no_main]

extern crate alloc;

// OS カーネル処理
// アロケータ初期化、可視化テスト、メインループ

mod acpi;
mod addr;
mod allocator;
mod apic;
mod debug_overlay;
mod gdt;
mod graphics;
mod hpet;
mod idt;
mod io;
mod paging;
mod pci;
mod pit;
mod sched;
mod serial;
mod sync;
mod timer;

// 後方互換性のためのエイリアス
use sched as task;

#[cfg(feature = "visualize-allocator")]
mod allocator_visualization;

#[cfg(feature = "visualize-pipeline")]
mod pipeline_visualization;

use crate::graphics::FramebufferWriter;
use alloc::boxed::Box;
use core::arch::asm;
use core::fmt::Write;
use core::panic::PanicInfo;
use vitros_common::boot_info::BootInfo;
use vitros_common::uefi;

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
    // SAFETY: hlt命令はCPUを低消費電力状態にする特権命令。
    // 次の割り込みで復帰するため、メモリ安全性に影響しない。
    // カーネルモード（Ring 0）で実行されることが前提。
    unsafe {
        asm!("hlt");
    }
}

/// カーネル起動完了マーカー
/// GDBブレークポイント用（シンボル名を固定）
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn boot_complete() {
    // SAFETY: スタック上の一時変数からvolatile readを行う。
    // この操作は最適化防止のためのダミー読み込みであり、
    // 有効なメモリ領域へのアクセスなので安全。
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
        // SAFETY: hlt命令はCPUを低消費電力状態にする特権命令。
        // 次の割り込みで復帰するため、メモリ安全性に影響しない。
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

        // 可視化モード: 同期フラッシュ（Compositorの処理完了を待機）
        #[cfg(feature = "visualize-pipeline")]
        writer.sync_flush();

        // 通常モード: 非同期フラッシュ + スリープ
        #[cfg(not(feature = "visualize-pipeline"))]
        {
            writer.flush();
            task::sleep_ms(16);
        }

        counter += 1;
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

        // 可視化モード: 同期フラッシュ（Compositorの処理完了を待機）
        #[cfg(feature = "visualize-pipeline")]
        writer.sync_flush();

        // 通常モード: 非同期フラッシュ + スリープ
        #[cfg(not(feature = "visualize-pipeline"))]
        {
            writer.flush();
            task::sleep_ms(16);
        }

        counter += 1;
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

        // 可視化モード: 同期フラッシュ（Compositorの処理完了を待機）
        #[cfg(feature = "visualize-pipeline")]
        writer.sync_flush();

        // 通常モード: 非同期フラッシュ + スリープ
        #[cfg(not(feature = "visualize-pipeline"))]
        {
            writer.flush();
            task::sleep_ms(16);
        }

        counter += 1;
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
    // SAFETY: boot_info_phys_addrはブートローダーから渡された有効なBootInfo構造体の
    // 物理アドレス。KERNEL_VMAを加算することで高位仮想アドレスに変換し、
    // ブートローダーが設定したページテーブルによりアクセス可能な領域を参照する。
    // BootInfoはブートローダーによって正しく初期化されており、
    // カーネル実行中は不変であることが保証されている。
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

    // MTRR/PAT設定をダンプ（デバッグ用）
    paging::dump_mtrr();

    // ローカルフレームバッファを初期化
    // 物理アドレスを高位仮想アドレスに変換
    //
    // TODO: フレームバッファにWrite-Combining (WC)を設定してパフォーマンス向上
    // 現状: 0x80000000〜がMTRR=UC → 描画が遅い
    // 改善案:
    //   - MTRR0-6: 0x81000000〜0xFFFFFFFFを7分割でUC設定
    //   - MTRR7: 768GB地点のUC維持
    //   - PAT[1]=WCに書き換え、フレームバッファのPTEでPWT=1,PCD=0設定
    //   - 結果: フレームバッファ=WC、他MMIO=UC
    // See: https://github.com/jugeeeemu-tech/VitrOS/issues/7
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

        // SAFETY: largest_start_virtはphys_to_virtで変換された有効な仮想アドレス。
        // heap_sizeはEFI_CONVENTIONAL_MEMORYリージョンのサイズ以下に制限されている。
        // この領域はUEFIメモリマップで使用可能と報告されており、
        // カーネルの他の部分では使用されていない。
        // init_heapは一度だけ呼び出され、以降はグローバルアロケータとして機能する。
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

        // Compositorタスク（Realtimeクラス、最高優先度）
        let compositor = Box::new(
            task::Task::new_realtime(
                "Compositor",
                task::rt_priority::MAX,
                graphics::compositor::compositor_task,
            )
            .expect("Failed to create Compositor task"),
        );
        task::add_task(*compositor);

        // アイドルタスク（Idleクラス）
        let idle =
            Box::new(task::Task::new_idle("Idle", idle_task).expect("Failed to create idle task"));
        task::add_task(*idle);

        // ワーカータスク1（やや高い優先度）
        let t1 = Box::new(
            task::Task::new("Task1", task::nice::DEFAULT - 5, task1)
                .expect("Failed to create Task1"),
        );
        task::add_task(*t1);

        // ワーカータスク2（標準優先度）
        let t2 = Box::new(
            task::Task::new("Task2", task::nice::DEFAULT, task2).expect("Failed to create Task2"),
        );
        task::add_task(*t2);

        // ワーカータスク3（最低優先度）
        let t3 = Box::new(
            task::Task::new("Task3", task::nice::MAX, task3).expect("Failed to create Task3"),
        );
        task::add_task(*t3);

        // デバッグオーバーレイタスク（Normalクラス、標準優先度）
        // 可視化モードでは無効（UIが画面全体を使用するため）
        #[cfg(not(feature = "visualize-pipeline"))]
        {
            let debug = Box::new(
                task::Task::new(
                    "DebugOverlay",
                    task::nice::DEFAULT,
                    debug_overlay::debug_overlay_task,
                )
                .expect("Failed to create DebugOverlay task"),
            );
            task::add_task(*debug);
        }

        // パイプライン可視化タスク（visualize-pipeline feature有効時のみ）
        #[cfg(feature = "visualize-pipeline")]
        {
            // 可視化状態を初期化
            pipeline_visualization::init_visualization_state();

            // 可視化UIタスクを作成（最高優先度で滑らかな描画）
            let vis_ui = Box::new(
                task::Task::new(
                    "VisualizationUI",
                    task::nice::MIN,
                    pipeline_visualization::visualization_ui_task,
                )
                .expect("Failed to create VisualizationUI task"),
            );
            task::add_task(*vis_ui);

            info!("Pipeline visualization task registered");
        }

        info!("All tasks created. Setting up kernel main task...");

        // kernel_main_innerを表すタスクを作成し、CURRENT_TASKに設定
        // 注意：entry_pointとしてidle_taskを指定しているが、これは使われない
        // このタスクはold_contextとして最初のswitch_context()で保存される側なので、
        // 初期Contextの値（rip=task_wrapper, rdi=idle_task）は上書きされる
        // 保存されるripは「schedule()から戻るアドレス」になる
        let kernel_main = Box::new(
            task::Task::new("KernelMain", task::nice::DEFAULT, idle_task)
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
        // SAFETY: sti命令は割り込みフラグ(IF)をセットする特権命令。
        // この時点でIDT、APIC、タイマーは全て初期化済みであり、
        // 割り込みを安全に受け付けられる状態にある。
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
        // ローカルバッファを共有バッファに一括転送
        writer.flush();

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
