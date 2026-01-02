//! シャドウフレームバッファ
//!
//! ハードウェアフレームバッファへの直接描画を避け、
//! フレーム完成後に一括転送することでちらつきを防止します。

use alloc::vec;
use alloc::vec::Vec;

use super::region::Region;

/// シャドウフレームバッファ
pub struct ShadowBuffer {
    /// ピクセルデータ（ARGB 32bit）
    buffer: Vec<u32>,
    /// バッファの幅（ピクセル）
    width: u32,
    /// バッファの高さ（ピクセル）
    height: u32,
    /// 変更された領域（None = 変更なし）
    dirty_rect: Option<Region>,
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
            dirty_rect: None,
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
    #[inline]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// バッファ全体をクリア
    #[inline]
    pub fn clear(&mut self, color: u32) {
        self.buffer.fill(color);
        self.mark_all_dirty();
    }

    /// 変更された領域をマーク
    ///
    /// 既存のdirty rectと新しい領域をマージし、
    /// 両方を含む最小のバウンディングボックスを作成します。
    ///
    /// # Arguments
    /// * `region` - 変更された領域
    pub fn mark_dirty(&mut self, region: &Region) {
        // 画面境界でクリップ
        let x = region.x.min(self.width);
        let y = region.y.min(self.height);
        let right = (region.x + region.width).min(self.width);
        let bottom = (region.y + region.height).min(self.height);

        // 幅または高さが0の場合は無視
        if right <= x || bottom <= y {
            return;
        }

        let clipped = Region::new(x, y, right - x, bottom - y);

        self.dirty_rect = Some(match self.dirty_rect {
            Some(existing) => {
                // 既存のdirty rectとマージ（バウンディングボックス）
                let min_x = existing.x.min(clipped.x);
                let min_y = existing.y.min(clipped.y);
                let max_x = existing.right().max(clipped.right());
                let max_y = existing.bottom().max(clipped.bottom());
                Region::new(min_x, min_y, max_x - min_x, max_y - min_y)
            }
            None => clipped,
        });
    }

    /// dirty rectをクリアして現在の値を返す
    ///
    /// # Returns
    /// 変更された領域。変更がなければNone
    #[inline]
    pub fn take_dirty_rect(&mut self) -> Option<Region> {
        self.dirty_rect.take()
    }

    /// 全画面をdirtyとしてマーク
    ///
    /// clear()呼び出し時や初期化時に使用
    #[inline]
    pub fn mark_all_dirty(&mut self) {
        self.dirty_rect = Some(Region::new(0, 0, self.width, self.height));
    }

    /// ハードウェアフレームバッファに転送（blit）
    ///
    /// dirty rectがある場合はその領域のみ転送し、
    /// なければ何も転送しません。転送後、dirty rectはクリアされます。
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
        let dirty = match self.take_dirty_rect() {
            Some(r) => r,
            None => return false, // 変更なし、転送不要
        };

        let dst_base = hw_fb_base as *mut u32;
        let src_base = self.buffer.as_ptr();
        let stride = self.width as usize;

        // dirty rect内の各行をコピー
        for y in dirty.y..(dirty.y + dirty.height) {
            let row_offset = (y as usize) * stride + (dirty.x as usize);
            // SAFETY: row_offset < width * height が保証されている
            // （dirty rectは画面境界でクリップ済み）
            unsafe {
                let src = src_base.add(row_offset);
                let dst = dst_base.add(row_offset);
                let count = dirty.width as usize;
                core::ptr::copy_nonoverlapping(src, dst, count);
            }
        }

        true // 転送が行われた
    }
}
