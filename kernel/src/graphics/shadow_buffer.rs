//! シャドウフレームバッファ
//!
//! ハードウェアフレームバッファへの直接描画を避け、
//! フレーム完成後に一括転送することでちらつきを防止します。

use alloc::vec;
use alloc::vec::Vec;

use super::region::Region;

const MAX_DIRTY_REGIONS: usize = 32;

/// シャドウフレームバッファ
pub struct ShadowBuffer {
    /// ピクセルデータ（ARGB 32bit）
    buffer: Vec<u32>,
    /// バッファの幅（ピクセル）
    width: u32,
    /// バッファの高さ（ピクセル）
    height: u32,
    /// 変更された領域
    dirty_regions: Vec<Region>,
}

impl ShadowBuffer {
    /// 新しいシャドウバッファを作成
    ///
    /// # Arguments
    /// * `width` - バッファの幅（ピクセル）
    /// * `height` - バッファの高さ（ピクセル）
    ///
    /// # Panics
    /// `width * height`がオーバーフローする場合にパニックします。
    pub fn new(width: u32, height: u32) -> Self {
        let size = (width as usize)
            .checked_mul(height as usize)
            .expect("ShadowBuffer size overflow");
        let buffer = vec![0u32; size]; // 黒で初期化
        Self {
            buffer,
            width,
            height,
            dirty_regions: Vec::with_capacity(MAX_DIRTY_REGIONS),
        }
    }

    /// バッファをu64アドレスとして取得（既存描画関数との互換性）
    #[inline]
    pub fn base_addr(&self) -> u64 {
        self.buffer.as_ptr() as u64
    }

    /// 幅を取得
    #[inline]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// 高さを取得
    #[allow(dead_code)]
    #[inline]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// バッファ全体をクリア
    #[allow(dead_code)]
    #[inline]
    pub fn clear(&mut self, color: u32) {
        self.buffer.fill(color);
        self.mark_all_dirty();
    }

    pub fn mark_dirty(&mut self, region: &Region) {
        let Some(mut clipped) = self.clip_region(region) else {
            return;
        };

        let mut index = 0;
        while index < self.dirty_regions.len() {
            if regions_overlap_or_touch(self.dirty_regions[index], clipped) {
                clipped = union_regions(self.dirty_regions[index], clipped);
                self.dirty_regions.swap_remove(index);
            } else {
                index += 1;
            }
        }

        if self.dirty_regions.len() < MAX_DIRTY_REGIONS {
            self.dirty_regions.push(clipped);
            return;
        }

        let merged = self
            .dirty_regions
            .iter()
            .copied()
            .fold(clipped, union_regions);
        self.dirty_regions.clear();
        self.dirty_regions.push(merged);
    }

    fn clip_region(&self, region: &Region) -> Option<Region> {
        let x = region.x.min(self.width);
        let y = region.y.min(self.height);
        let right = region.right().min(self.width);
        let bottom = region.bottom().min(self.height);

        if right <= x || bottom <= y {
            return None;
        }

        Some(Region::new(x, y, right - x, bottom - y))
    }

    /// 全画面をdirtyとしてマーク
    ///
    /// clear()呼び出し時や初期化時に使用
    #[allow(dead_code)]
    #[inline]
    pub fn mark_all_dirty(&mut self) {
        self.dirty_regions.clear();
        self.dirty_regions
            .push(Region::new(0, 0, self.width, self.height));
    }

    #[inline]
    pub fn drain_dirty_regions(&mut self, out: &mut Vec<Region>) {
        out.clear();
        core::mem::swap(out, &mut self.dirty_regions);
    }

    /// ハードウェアフレームバッファに転送（blit）
    ///
    /// dirty領域がある場合はその領域のみ転送し、
    /// なければ何も転送しません。転送後、dirty領域はクリアされます。
    ///
    /// # Returns
    /// 転送が行われた場合は`true`、dirty rectがなく転送されなかった場合は`false`
    ///
    /// # Safety
    /// - `hw_fb_base`は有効なフレームバッファアドレスであること
    /// - `hw_fb_base`は4バイト境界にアライメントされていること
    /// - 転送先には`self.buffer.len() * 4`バイト以上の書き込み可能な領域があること
    /// - 呼び出し元は転送先メモリへの排他的アクセス権を持つこと
    pub unsafe fn blit_to(&mut self, hw_fb_base: u64) -> bool {
        if self.dirty_regions.is_empty() {
            return false;
        }

        let dst_base = hw_fb_base as *mut u32;
        let src_base = self.buffer.as_ptr();
        let stride = self.width as usize;

        for dirty in &self.dirty_regions {
            for y in dirty.y..dirty.bottom() {
                let row_offset = (y as usize) * stride + (dirty.x as usize);
                unsafe {
                    let src = src_base.add(row_offset);
                    let dst = dst_base.add(row_offset);
                    let count = dirty.width as usize;
                    core::ptr::copy_nonoverlapping(src, dst, count);
                }
            }
        }

        self.dirty_regions.clear();
        true
    }
}

fn regions_overlap_or_touch(lhs: Region, rhs: Region) -> bool {
    lhs.x <= rhs.right() && rhs.x <= lhs.right() && lhs.y <= rhs.bottom() && rhs.y <= lhs.bottom()
}

fn union_regions(lhs: Region, rhs: Region) -> Region {
    let min_x = lhs.x.min(rhs.x);
    let min_y = lhs.y.min(rhs.y);
    let max_x = lhs.right().max(rhs.right());
    let max_y = lhs.bottom().max(rhs.bottom());
    Region::new(min_x, min_y, max_x - min_x, max_y - min_y)
}

// ============================================================================
// DrawTarget trait 実装
// ============================================================================

use super::draw_target::{DirtyTrackingTarget, DrawTarget};

impl DrawTarget for ShadowBuffer {
    fn base_addr(&self) -> u64 {
        self.buffer.as_ptr() as u64
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, color: u32) {
        // SAFETY: base_addrは自身のバッファを指す有効なアドレス
        unsafe {
            super::draw_rect(
                self.base_addr(),
                self.width,
                x as usize,
                y as usize,
                w as usize,
                h as usize,
                color,
            );
        }
        self.mark_dirty(&Region::new(x, y, w, h));
    }

    fn draw_char(&mut self, x: u32, y: u32, ch: u8, color: u32) {
        // SAFETY: base_addrは自身のバッファを指す有効なアドレス
        unsafe {
            super::draw_char(
                self.base_addr(),
                self.width,
                x as usize,
                y as usize,
                ch,
                color,
            );
        }
        self.mark_dirty(&Region::new(x, y, 8, 8));
    }

    fn draw_string(&mut self, x: u32, y: u32, s: &str, color: u32) {
        // SAFETY: base_addrは自身のバッファを指す有効なアドレス
        unsafe {
            super::draw_string(
                self.base_addr(),
                self.width,
                x as usize,
                y as usize,
                s,
                color,
            );
        }
        let str_width = (s.len() as u32) * 8;
        self.mark_dirty(&Region::new(x, y, str_width, 8));
    }
}

impl DirtyTrackingTarget for ShadowBuffer {
    fn mark_dirty(&mut self, region: &Region) {
        ShadowBuffer::mark_dirty(self, region);
    }

    fn mark_all_dirty(&mut self) {
        ShadowBuffer::mark_all_dirty(self);
    }

    fn drain_dirty_regions(&mut self, out: &mut Vec<Region>) {
        ShadowBuffer::drain_dirty_regions(self, out);
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test_case]
    fn test_mark_dirty_merges_overlapping_regions() {
        let mut buffer = ShadowBuffer::new(32, 32);

        buffer.mark_dirty(&Region::new(0, 0, 8, 8));
        buffer.mark_dirty(&Region::new(4, 4, 8, 8));

        assert_eq!(buffer.dirty_regions, vec![Region::new(0, 0, 12, 12)]);
    }

    #[test_case]
    fn test_mark_dirty_merges_touching_regions() {
        let mut buffer = ShadowBuffer::new(32, 32);

        buffer.mark_dirty(&Region::new(0, 0, 8, 8));
        buffer.mark_dirty(&Region::new(8, 0, 8, 8));

        assert_eq!(buffer.dirty_regions, vec![Region::new(0, 0, 16, 8)]);
    }

    #[test_case]
    fn test_mark_dirty_keeps_disjoint_regions_separate() {
        let mut buffer = ShadowBuffer::new(32, 32);

        buffer.mark_dirty(&Region::new(0, 0, 8, 8));
        buffer.mark_dirty(&Region::new(16, 16, 8, 8));

        assert_eq!(buffer.dirty_regions.len(), 2);
        assert!(buffer.dirty_regions.contains(&Region::new(0, 0, 8, 8)));
        assert!(buffer.dirty_regions.contains(&Region::new(16, 16, 8, 8)));
    }

    #[test_case]
    fn test_mark_dirty_collapses_when_region_limit_is_exceeded() {
        let mut buffer = ShadowBuffer::new(520, 1);

        for index in 0..=MAX_DIRTY_REGIONS {
            buffer.mark_dirty(&Region::new((index as u32) * 16, 0, 8, 1));
        }

        assert!(buffer.dirty_regions.len() == 1);
        let merged = buffer.dirty_regions[0];
        assert!(merged.x == 0);
        assert!(merged.y == 0);
        assert!(merged.width == 520);
        assert!(merged.height == 1);
    }

    #[test_case]
    fn test_mark_all_dirty_replaces_existing_regions() {
        let mut buffer = ShadowBuffer::new(64, 48);
        buffer.mark_dirty(&Region::new(8, 8, 8, 8));

        buffer.mark_all_dirty();

        assert_eq!(buffer.dirty_regions, vec![Region::new(0, 0, 64, 48)]);
    }

    #[test_case]
    fn test_drain_dirty_regions_moves_regions_out() {
        let mut buffer = ShadowBuffer::new(32, 32);
        buffer.mark_dirty(&Region::new(0, 0, 8, 8));
        let mut drained = vec![Region::new(9, 9, 1, 1)];

        buffer.drain_dirty_regions(&mut drained);

        assert_eq!(drained, vec![Region::new(0, 0, 8, 8)]);
        assert!(buffer.dirty_regions.is_empty());
    }

    #[test_case]
    fn test_blit_to_copies_only_dirty_regions() {
        let mut buffer = ShadowBuffer::new(4, 4);
        buffer.buffer[0] = 0x11;
        buffer.buffer[15] = 0x22;
        buffer.mark_dirty(&Region::new(0, 0, 1, 1));
        buffer.mark_dirty(&Region::new(3, 3, 1, 1));

        let mut hw = vec![0xFFFF_FFFFu32; 16];
        let changed = unsafe { buffer.blit_to(hw.as_mut_ptr() as u64) };

        assert!(changed);
        assert_eq!(hw[0], 0x11);
        assert_eq!(hw[15], 0x22);
        assert_eq!(hw[1], 0xFFFF_FFFFu32);
        assert_eq!(hw[14], 0xFFFF_FFFFu32);
        assert!(buffer.dirty_regions.is_empty());
    }
}
