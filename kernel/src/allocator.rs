// スラブアロケータ実装（Linuxスタイル）
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr::{NonNull, null_mut};

use crate::info;
use crate::io::without_interrupts;

// サイズクラス（8バイト～4096バイト）
pub const SIZE_CLASSES: &[usize] = &[8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096];
const NUM_SIZE_CLASSES: usize = SIZE_CLASSES.len();

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

// スラブアロケータ本体
pub struct SlabAllocator {
    caches: [SlabCache; NUM_SIZE_CLASSES],
    // TODO: 大きなサイズ用のバンプアロケータ（解放不可）
    // 将来的にはバディアロケータまたはリンクリストアロケータに置き換える
    // Issue: https://github.com/jugeeeemu-tech/vitrOS/issues/1
    large_alloc_next: UnsafeCell<usize>,
    large_alloc_end: UnsafeCell<usize>,
}

impl SlabAllocator {
    pub const fn new() -> Self {
        Self {
            caches: [
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
            large_alloc_next: UnsafeCell::new(0),
            large_alloc_end: UnsafeCell::new(0),
        }
    }

    // ヒープを初期化
    pub unsafe fn init(&self, heap_start: usize, heap_size: usize) {
        info!("Initializing Slab Allocator...");
        info!(
            "Heap: 0x{:X} - 0x{:X} ({} MB)",
            heap_start,
            heap_start + heap_size,
            heap_size / 1024 / 1024
        );

        // ヒープを2分割：前半はスラブ、後半は大きなサイズ用
        let slab_region_size = heap_size / 2;
        let large_region_start = heap_start + slab_region_size;

        // 各サイズクラスにスラブを割り当て
        let mut current = heap_start;
        for (i, &size) in SIZE_CLASSES.iter().enumerate() {
            let slab_size = slab_region_size / NUM_SIZE_CLASSES;
            let aligned_size = align_down(slab_size, size);

            unsafe {
                self.caches[i].add_slab(current, aligned_size);
            }

            current += aligned_size;
            info!("  Size class {:4}B: {} blocks", size, aligned_size / size);
        }

        // 大きなサイズ用の領域を初期化
        unsafe {
            *self.large_alloc_next.get() = large_region_start;
            *self.large_alloc_end.get() = heap_start + heap_size;
        }

        info!("Slab Allocator initialized successfully");
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
        let class_idx = bits.saturating_sub(3) as usize;
        Some(class_idx)
    }

    // 大きなサイズ用のアロケート（バンプアロケータ）
    unsafe fn allocate_large(&self, layout: Layout) -> Option<NonNull<u8>> {
        without_interrupts(|| unsafe {
            let next = *self.large_alloc_next.get();
            let end = *self.large_alloc_end.get();

            let alloc_start = align_up(next, layout.align());
            let alloc_end = alloc_start.saturating_add(layout.size());

            if alloc_end > end {
                None
            } else {
                *self.large_alloc_next.get() = alloc_end;
                NonNull::new(alloc_start as *mut u8)
            }
        })
    }
}

// GlobalAlloc トレイトを実装
unsafe impl GlobalAlloc for SlabAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size().max(layout.align());

        // サイズクラスを探す
        if let Some(class_idx) = Self::size_to_class(size)
            && let Some(ptr) = unsafe { self.caches[class_idx].allocate() }
        {
            notify_allocate(class_idx, ptr.as_ptr());
            return ptr.as_ptr();
        }

        // スラブから割り当てできない場合は大きなサイズ用アロケータを使用
        unsafe { self.allocate_large(layout) }
            .map(|ptr| ptr.as_ptr())
            .unwrap_or(null_mut())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // ZST（サイズ0）の場合は何もしない
        // RustはZST BoxにNonNull::dangling()を使用し、実際のメモリは割り当てられていない
        if layout.size() == 0 {
            return;
        }

        let size = layout.size().max(layout.align());

        // サイズクラスに該当する場合は解放
        if let Some(class_idx) = Self::size_to_class(size) {
            notify_deallocate(class_idx, ptr);
            unsafe {
                self.caches[class_idx].deallocate(ptr);
            }
        }
        // TODO: 大きなサイズの解放は無視（バンプアロケータ部分）
        // 4KB超のメモリは解放できない - バディアロケータ実装が必要
    }
}

// Sync を実装（グローバルで使用するため）
unsafe impl Sync for SlabAllocator {}

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
static ALLOCATOR: SlabAllocator = SlabAllocator::new();

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
