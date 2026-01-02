//! シャドウフレームバッファ
//!
//! ハードウェアフレームバッファへの直接描画を避け、
//! フレーム完成後に一括転送することでちらつきを防止します。

use alloc::vec;
use alloc::vec::Vec;

/// シャドウフレームバッファ
pub struct ShadowBuffer {
    /// ピクセルデータ（ARGB 32bit）
    buffer: Vec<u32>,
    /// バッファの幅（ピクセル）
    width: u32,
    /// バッファの高さ（ピクセル）
    height: u32,
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
    }

    /// ハードウェアフレームバッファに転送（blit）
    ///
    /// # Safety
    /// - `hw_fb_base`は有効なフレームバッファアドレスであること
    /// - `hw_fb_base`は4バイト境界にアライメントされていること
    /// - 転送先には`self.buffer.len() * 4`バイト以上の書き込み可能な領域があること
    /// - 呼び出し元は転送先メモリへの排他的アクセス権を持つこと
    pub unsafe fn blit_to(&self, hw_fb_base: u64) {
        let dst = hw_fb_base as *mut u32;
        let src = self.buffer.as_ptr();
        let count = self.buffer.len();

        // 全画面転送
        core::ptr::copy_nonoverlapping(src, dst, count);
    }
}
