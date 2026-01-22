// カーネルアロケータ実装（スラブ + バディ、Linuxスタイル）
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr::{NonNull, null_mut};

use crate::info;
use crate::io::without_interrupts;

// サイズクラス（8バイト～4096バイト）
pub const SIZE_CLASSES: &[usize] = &[8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096];
const NUM_SIZE_CLASSES: usize = SIZE_CLASSES.len();

/// MIN_SLAB_SIZEのlog2（8 = 2^3）
const MIN_SLAB_SIZE_LOG2: u32 = 3;

// =============================================================================
// バディアロケータ
// =============================================================================

/// 最小ブロックサイズ（4KB = ページサイズ）
const MIN_BLOCK_SIZE: usize = 4096;

/// MIN_BLOCK_SIZEのlog2（4096 = 2^12）
const MIN_BLOCK_SIZE_LOG2: u32 = 12;

/// 最大オーダー数（0〜12の13段階、最大16MB）
const MAX_ORDER: usize = 13;

/// フリーブロックノード（双方向リンクリスト）
#[repr(C)]
struct BuddyFreeNode {
    next: Option<NonNull<BuddyFreeNode>>,
    prev: Option<NonNull<BuddyFreeNode>>,
}

/// バディアロケータ
struct BuddyAllocator {
    free_lists: [UnsafeCell<Option<NonNull<BuddyFreeNode>>>; MAX_ORDER],
    region_start: UnsafeCell<usize>,
    region_size: UnsafeCell<usize>,
}

impl BuddyAllocator {
    /// constコンストラクタ
    const fn new() -> Self {
        Self {
            free_lists: [
                UnsafeCell::new(None),
                UnsafeCell::new(None),
                UnsafeCell::new(None),
                UnsafeCell::new(None),
                UnsafeCell::new(None),
                UnsafeCell::new(None),
                UnsafeCell::new(None),
                UnsafeCell::new(None),
                UnsafeCell::new(None),
                UnsafeCell::new(None),
                UnsafeCell::new(None),
                UnsafeCell::new(None),
                UnsafeCell::new(None),
            ],
            region_start: UnsafeCell::new(0),
            region_size: UnsafeCell::new(0),
        }
    }

    // =========================================================================
    // ヘルパー関数
    // =========================================================================

    /// オーダーからブロックサイズを計算
    #[inline]
    const fn order_to_size(order: usize) -> usize {
        MIN_BLOCK_SIZE << order
    }

    /// サイズを格納できる最小オーダーを計算（割り当て用）
    #[inline]
    fn size_to_order(size: usize) -> usize {
        if size <= MIN_BLOCK_SIZE {
            return 0;
        }
        // 2のべき乗に切り上げ
        let bits = usize::BITS - (size - 1).leading_zeros();
        // MIN_BLOCK_SIZE = 4096 = 2^12 なのでMIN_BLOCK_SIZE_LOG2を引く
        bits.saturating_sub(MIN_BLOCK_SIZE_LOG2) as usize
    }

    /// サイズに収まる最大オーダーを計算（初期化用）
    #[inline]
    fn max_order_for_size(size: usize) -> usize {
        if size < MIN_BLOCK_SIZE {
            return 0;
        }
        // 2のべき乗に切り下げ
        let bits = usize::BITS - 1 - size.leading_zeros();
        // MIN_BLOCK_SIZE = 4096 = 2^12 なのでMIN_BLOCK_SIZE_LOG2を引く
        bits.saturating_sub(MIN_BLOCK_SIZE_LOG2) as usize
    }

    /// バディアドレスを計算（XOR演算）
    ///
    /// # Safety
    /// - `addr`は`region_start`以上であること
    /// - `addr`はバディ領域内の有効なアドレスであること
    #[inline]
    fn buddy_address(&self, addr: usize, order: usize) -> usize {
        let region_start = unsafe { *self.region_start.get() };
        debug_assert!(addr >= region_start, "addr must be >= region_start");
        let relative = addr - region_start;
        let buddy_relative = relative ^ Self::order_to_size(order);
        region_start + buddy_relative
    }

    // =========================================================================
    // フリーリスト操作
    // =========================================================================

    /// フリーリストの先頭にブロックを追加
    ///
    /// # Safety
    /// - `addr`は有効なメモリアドレスで、MIN_BLOCK_SIZE以上のサイズを持つこと
    /// - `addr`はBuddyFreeNodeのアライメント要件を満たすこと
    /// - `addr`は他のフリーリストに含まれていないこと
    /// - `order`は0..MAX_ORDERの範囲内であること
    unsafe fn add_to_free_list(&self, addr: usize, order: usize) {
        let node = addr as *mut BuddyFreeNode;
        let free_list = &mut *self.free_lists[order].get();

        // 双方向リストの先頭に追加
        (*node).prev = None;
        (*node).next = *free_list;

        // 既存の先頭ノードのprevを更新
        if let Some(head) = *free_list {
            (*head.as_ptr()).prev = NonNull::new(node);
        }

        *free_list = NonNull::new(node);
    }

    /// フリーリストの先頭からブロックを取り出し
    ///
    /// # Safety
    /// - `order`は0..MAX_ORDERの範囲内であること
    /// - フリーリスト内のノードは全て有効なポインタであること
    unsafe fn remove_from_free_list(&self, order: usize) -> Option<NonNull<BuddyFreeNode>> {
        let free_list = &mut *self.free_lists[order].get();

        if let Some(head) = *free_list {
            let next = (*head.as_ptr()).next;

            // 次のノードのprevをNoneに
            if let Some(next_node) = next {
                (*next_node.as_ptr()).prev = None;
            }

            *free_list = next;
            Some(head)
        } else {
            None
        }
    }

    /// 指定したアドレスのノードをフリーリストから削除（O(1)）
    ///
    /// # Safety
    /// - `addr`はこのorder用フリーリストに含まれていること
    /// - `order`は0..MAX_ORDERの範囲内であること
    unsafe fn remove_node_from_free_list(&self, addr: usize, order: usize) {
        let node = addr as *mut BuddyFreeNode;
        let free_list = &mut *self.free_lists[order].get();

        let prev = (*node).prev;
        let next = (*node).next;

        // 前のノードのnextを更新
        if let Some(prev_node) = prev {
            (*prev_node.as_ptr()).next = next;
        } else {
            // このノードが先頭だった
            *free_list = next;
        }

        // 次のノードのprevを更新
        if let Some(next_node) = next {
            (*next_node.as_ptr()).prev = prev;
        }
    }

    /// 指定したアドレスがフリーリストに存在するかチェック
    ///
    /// # Safety
    /// - `order`は0..MAX_ORDERの範囲内であること
    /// - フリーリスト内のノードは全て有効なポインタであること
    ///
    /// # Performance
    /// この関数はO(n)の線形探索を行う。deallocate時のバディ結合で呼び出されるため、
    /// 大量のフリーブロックがある場合は割り込みレイテンシが増大する可能性がある。
    ///
    /// TODO: ビットマップベースの実装でO(1)判定を可能にする (Issue #41)
    unsafe fn is_in_free_list(&self, addr: usize, order: usize) -> bool {
        let free_list = *self.free_lists[order].get();
        let mut current = free_list;

        while let Some(node) = current {
            if node.as_ptr() as usize == addr {
                return true;
            }
            current = (*node.as_ptr()).next;
        }
        false
    }

    // =========================================================================
    // 初期化
    // =========================================================================

    /// バディアロケータを初期化
    ///
    /// # Safety
    /// - `region_start`は有効なメモリ領域の先頭アドレスであること
    /// - `region_size`は実際に利用可能なサイズであること
    /// - この関数は一度だけ呼び出すこと
    pub unsafe fn init(&self, region_start: usize, region_size: usize) {
        // 4KB境界にアライン
        let aligned_start = align_up(region_start, MIN_BLOCK_SIZE);
        let aligned_end = align_down(region_start + region_size, MIN_BLOCK_SIZE);
        let aligned_size = aligned_end.saturating_sub(aligned_start);

        unsafe {
            *self.region_start.get() = aligned_start;
            *self.region_size.get() = aligned_size;
        }

        info!(
            "Buddy allocator region: 0x{:X} - 0x{:X} ({} MB)",
            aligned_start,
            aligned_end,
            aligned_size / 1024 / 1024
        );

        // 領域を可能な限り大きなブロックに分割してフリーリストに追加
        let mut current = aligned_start;
        let mut remaining = aligned_size;

        while remaining >= MIN_BLOCK_SIZE {
            // 現在の位置から追加できる最大のオーダーを計算
            let max_order_by_size = Self::max_order_for_size(remaining).min(MAX_ORDER - 1);

            // アライメント制約: アドレスはブロックサイズでアラインされている必要がある
            // アドレスの下位ビットを見て、追加可能な最大オーダーを決定
            let relative = current - aligned_start;
            let max_order_by_align = if relative == 0 {
                MAX_ORDER - 1
            } else {
                (relative.trailing_zeros()).saturating_sub(MIN_BLOCK_SIZE_LOG2) as usize
            };

            let order = max_order_by_size.min(max_order_by_align);
            let block_size = Self::order_to_size(order);

            unsafe {
                self.add_to_free_list(current, order);
            }

            current += block_size;
            remaining -= block_size;
        }

        // 初期化結果をログ出力
        for order in 0..MAX_ORDER {
            let count = unsafe { self.count_free_blocks(order) };
            if count > 0 {
                info!(
                    "  Order {:2} ({:6} KB): {} blocks",
                    order,
                    Self::order_to_size(order) / 1024,
                    count
                );
            }
        }
    }

    /// 指定オーダーのフリーブロック数をカウント（デバッグ用）
    ///
    /// # Safety
    /// - `order`は0..MAX_ORDERの範囲内であること
    /// - フリーリスト内のノードは全て有効なポインタであること
    unsafe fn count_free_blocks(&self, order: usize) -> usize {
        let mut count = 0;
        let free_list = *self.free_lists[order].get();
        let mut current = free_list;

        while let Some(node) = current {
            count += 1;
            current = (*node.as_ptr()).next;
        }
        count
    }

    // =========================================================================
    // メモリ割り当て
    // =========================================================================

    /// メモリを割り当て
    ///
    /// # Safety
    /// - `layout`が有効であること
    pub unsafe fn allocate(&self, layout: Layout) -> Option<NonNull<u8>> {
        without_interrupts(|| {
            let size = layout.size().max(layout.align()).max(MIN_BLOCK_SIZE);
            let required_order = Self::size_to_order(size);

            if required_order >= MAX_ORDER {
                return None;
            }

            // 要求オーダー以上の最小フリーブロックを探す
            let mut found_order = None;
            for order in required_order..MAX_ORDER {
                let free_list = unsafe { *self.free_lists[order].get() };
                if free_list.is_some() {
                    found_order = Some(order);
                    break;
                }
            }

            let found_order = found_order?;

            // フリーリストからブロックを取り出し
            let block = unsafe { self.remove_from_free_list(found_order)? };
            let block_addr = block.as_ptr() as usize;

            // 必要に応じて分割（余りをフリーリストに追加）
            for order in (required_order..found_order).rev() {
                let buddy_addr = block_addr + Self::order_to_size(order);
                unsafe {
                    self.add_to_free_list(buddy_addr, order);
                }
            }

            Some(NonNull::new_unchecked(block_addr as *mut u8))
        })
    }

    // =========================================================================
    // メモリ解放
    // =========================================================================

    /// メモリを解放
    ///
    /// # Safety
    /// - `ptr`は`allocate`で取得したポインタであること
    /// - `layout`は`allocate`時と同じレイアウトであること
    pub unsafe fn deallocate(&self, ptr: *mut u8, layout: Layout) {
        without_interrupts(|| {
            let size = layout.size().max(layout.align()).max(MIN_BLOCK_SIZE);
            let mut order = Self::size_to_order(size);
            let mut block_addr = ptr as usize;

            let region_start = unsafe { *self.region_start.get() };
            let region_size = unsafe { *self.region_size.get() };
            let region_end = region_start + region_size;

            // バディとの結合を試みる
            while order < MAX_ORDER - 1 {
                let buddy_addr = self.buddy_address(block_addr, order);

                // バディが領域内かチェック
                if buddy_addr < region_start || buddy_addr >= region_end {
                    break;
                }

                // バディがフリーリストにあるかチェック
                if !unsafe { self.is_in_free_list(buddy_addr, order) } {
                    break;
                }

                // バディをフリーリストから削除
                unsafe {
                    self.remove_node_from_free_list(buddy_addr, order);
                }

                // 結合（小さい方のアドレスが新しいブロックの先頭）
                block_addr = block_addr.min(buddy_addr);
                order += 1;
            }

            // 最終ブロックをフリーリストに追加
            unsafe {
                self.add_to_free_list(block_addr, order);
            }
        })
    }
}

// 空きブロックのリンクリストノード
#[repr(C)]
struct FreeNode {
    next: Option<NonNull<FreeNode>>,
}

// サイズクラスごとのスラブキャッシュ
struct SlabCache {
    free_list: UnsafeCell<Option<NonNull<FreeNode>>>,
    block_size: usize,
}

impl SlabCache {
    const fn new(block_size: usize) -> Self {
        Self {
            free_list: UnsafeCell::new(None),
            block_size,
        }
    }

    // ブロックを割り当て
    unsafe fn allocate(&self) -> Option<NonNull<u8>> {
        without_interrupts(|| unsafe {
            let free_list = &mut *self.free_list.get();

            if let Some(node) = *free_list {
                // フリーリストから取り出す
                let ptr = node.as_ptr() as *mut u8;
                *free_list = (*node.as_ptr()).next;
                NonNull::new(ptr)
            } else {
                // フリーリストが空の場合はNone（後でラージアロケータにフォールバック）
                None
            }
        })
    }

    // ブロックを解放
    unsafe fn deallocate(&self, ptr: *mut u8) {
        without_interrupts(|| unsafe {
            let free_list = &mut *self.free_list.get();
            let node = ptr as *mut FreeNode;

            // フリーリストの先頭に追加
            (*node).next = *free_list;
            *free_list = NonNull::new(node);
        })
    }

    // スラブを追加（大きなメモリブロックを小さなブロックに分割）
    unsafe fn add_slab(&self, slab_start: usize, slab_size: usize) {
        let num_blocks = slab_size / self.block_size;

        for i in 0..num_blocks {
            let block_addr = slab_start + i * self.block_size;
            unsafe {
                self.deallocate(block_addr as *mut u8);
            }
        }
    }
}

// カーネルアロケータ本体（スラブ + バディ）
pub struct KernelAllocator {
    // 小さなサイズ用（8B〜4KB）
    slab_caches: [SlabCache; NUM_SIZE_CLASSES],
    // 大きなサイズ用（4KB超）
    buddy: BuddyAllocator,
}

impl KernelAllocator {
    pub const fn new() -> Self {
        Self {
            slab_caches: [
                SlabCache::new(SIZE_CLASSES[0]),
                SlabCache::new(SIZE_CLASSES[1]),
                SlabCache::new(SIZE_CLASSES[2]),
                SlabCache::new(SIZE_CLASSES[3]),
                SlabCache::new(SIZE_CLASSES[4]),
                SlabCache::new(SIZE_CLASSES[5]),
                SlabCache::new(SIZE_CLASSES[6]),
                SlabCache::new(SIZE_CLASSES[7]),
                SlabCache::new(SIZE_CLASSES[8]),
                SlabCache::new(SIZE_CLASSES[9]),
            ],
            buddy: BuddyAllocator::new(),
        }
    }

    // ヒープを初期化
    pub unsafe fn init(&self, heap_start: usize, heap_size: usize) {
        info!("Initializing Kernel Allocator...");
        info!(
            "Heap: 0x{:X} - 0x{:X} ({} MB)",
            heap_start,
            heap_start + heap_size,
            heap_size / 1024 / 1024
        );

        // ヒープを2分割：前半はスラブ、後半はバディ
        let slab_region_size = heap_size / 2;
        let buddy_region_start = heap_start + slab_region_size;
        let buddy_region_size = heap_size - slab_region_size;

        // 各サイズクラスにスラブを割り当て
        info!("Initializing Slab allocator...");
        let mut current = heap_start;
        for (i, &size) in SIZE_CLASSES.iter().enumerate() {
            let slab_size = slab_region_size / NUM_SIZE_CLASSES;
            let aligned_size = align_down(slab_size, size);

            unsafe {
                self.slab_caches[i].add_slab(current, aligned_size);
            }

            current += aligned_size;
            info!("  Size class {:4}B: {} blocks", size, aligned_size / size);
        }

        // バディアロケータを初期化
        info!("Initializing Buddy allocator...");
        unsafe {
            self.buddy.init(buddy_region_start, buddy_region_size);
        }

        info!("Kernel Allocator initialized successfully");
    }

    // サイズからサイズクラスのインデックスを取得（O(1)）
    fn size_to_class(size: usize) -> Option<usize> {
        if size == 0 {
            return Some(0);
        }
        if size > 4096 {
            return None;
        }
        // 2のべき乗に切り上げてインデックスを計算
        // 8=2^3がインデックス0なので、ビット位置から3を引く
        let bits = usize::BITS - (size - 1).leading_zeros();
        let class_idx = bits.saturating_sub(MIN_SLAB_SIZE_LOG2) as usize;
        Some(class_idx)
    }
}

// GlobalAlloc トレイトを実装
unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size().max(layout.align());

        // サイズクラスを探す（4KB以下はスラブ）
        if let Some(class_idx) = Self::size_to_class(size)
            && let Some(ptr) = unsafe { self.slab_caches[class_idx].allocate() }
        {
            notify_allocate(class_idx, ptr.as_ptr());
            return ptr.as_ptr();
        }

        // スラブから割り当てできない場合はバディアロケータを使用
        unsafe { self.buddy.allocate(layout) }
            .map(|ptr| ptr.as_ptr())
            .unwrap_or(null_mut())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // ZST（サイズ0）の場合は何もしない
        // RustはZST BoxにNonNull::dangling()を使用し、実際のメモリは割り当てられていない
        if layout.size() == 0 {
            return;
        }

        let ptr_addr = ptr as usize;
        let buddy_start = unsafe { *self.buddy.region_start.get() };

        // アドレス範囲で解放先を判断
        // スラブが空でバディにフォールバックした場合も正しく解放できる
        if ptr_addr >= buddy_start {
            // バディ領域のアドレスならバディに解放
            unsafe {
                self.buddy.deallocate(ptr, layout);
            }
        } else {
            // スラブ領域のアドレスならスラブに解放
            let size = layout.size().max(layout.align());
            if let Some(class_idx) = Self::size_to_class(size) {
                notify_deallocate(class_idx, ptr);
                unsafe {
                    self.slab_caches[class_idx].deallocate(ptr);
                }
            }
        }
    }
}

// SAFETY: KernelAllocatorは以下の理由でSyncを安全に実装できる:
// 1. シングルコアシステムであり、真の並行アクセスは発生しない
// 2. allocate/deallocateはwithout_interrupts()で保護されており、
//    割り込みコンテキストからの再入を防いでいる
// 3. initは起動時に一度だけ呼び出される（シングルスレッド環境）
unsafe impl Sync for KernelAllocator {}

// アドレスをアラインメントに合わせて切り上げ
fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

// アドレスをアラインメントに合わせて切り下げ
fn align_down(addr: usize, align: usize) -> usize {
    addr & !(align - 1)
}

// グローバルアロケータを登録
#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator::new();

// アロケータを初期化する公開関数
pub unsafe fn init_heap(heap_start: usize, heap_size: usize) {
    unsafe {
        ALLOCATOR.init(heap_start, heap_size);
    }
}

// =============================================================================
// アロケータオブザーバーフック関数
// 可視化機能が有効な場合のみ通知を行う
// =============================================================================

/// アロケート通知フック
///
/// # Arguments
/// * `class_idx` - サイズクラスのインデックス
/// * `ptr` - 割り当てられたポインタ
///
/// # Safety Contract
/// この関数は`without_interrupts`ブロックの外で呼び出される。
/// フック先（allocator_visualization::on_allocate_hook）はAtomicUsize操作のみ
/// を使用するため、割り込みセーフである。
/// 将来の変更で割り込みを必要とする操作を追加する場合は、
/// 呼び出し側も適切に保護する必要がある。
#[cfg(feature = "visualize-allocator")]
#[inline(always)]
pub(crate) fn notify_allocate(class_idx: usize, ptr: *mut u8) {
    crate::allocator_visualization::on_allocate_hook(class_idx, ptr);
}

/// アロケート通知フック（no-op版）
#[cfg(not(feature = "visualize-allocator"))]
#[inline(always)]
pub(crate) fn notify_allocate(_class_idx: usize, _ptr: *mut u8) {}

/// デアロケート通知フック
///
/// # Arguments
/// * `class_idx` - サイズクラスのインデックス
/// * `ptr` - 解放されるポインタ
///
/// # Safety Contract
/// この関数は`without_interrupts`ブロックの外で呼び出される。
/// フック先（allocator_visualization::on_deallocate_hook）はAtomicUsize操作のみ
/// を使用するため、割り込みセーフである。
/// 将来の変更で割り込みを必要とする操作を追加する場合は、
/// 呼び出し側も適切に保護する必要がある。
#[cfg(feature = "visualize-allocator")]
#[inline(always)]
pub(crate) fn notify_deallocate(class_idx: usize, ptr: *mut u8) {
    crate::allocator_visualization::on_deallocate_hook(class_idx, ptr);
}

/// デアロケート通知フック（no-op版）
#[cfg(not(feature = "visualize-allocator"))]
#[inline(always)]
pub(crate) fn notify_deallocate(_class_idx: usize, _ptr: *mut u8) {}
