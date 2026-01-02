mod font;

pub mod buffer;
pub mod compositor;
pub mod region;
pub mod shadow_buffer;
pub mod writer;

pub use font::FONT_8X8;
pub use region::Region;
pub use writer::TaskWriter;

// フレームバッファに文字を描画
//
// # Safety
// fb_base は有効なフレームバッファアドレスである必要があり、
// 描画範囲が画面内に収まっていることを呼び出し側が保証する必要があります。
pub unsafe fn draw_char(fb_base: u64, width: u32, x: usize, y: usize, ch: u8, color: u32) {
    let fb_ptr = fb_base as *mut u32;

    if ch < 32 || ch > 126 {
        return; // サポート外の文字
    }

    let font_index = (ch - 32) as usize;
    let glyph = FONT_8X8[font_index];

    for row in 0..8 {
        for col in 0..8 {
            if (glyph[row] >> col) & 1 == 1 {
                // オーバーフローと境界チェック
                if let (Some(pixel_x), Some(pixel_y)) = (x.checked_add(col), y.checked_add(row))
                    && pixel_x < width as usize
                {
                    let pixel_offset = pixel_y
                        .checked_mul(width as usize)
                        .and_then(|y_offset| y_offset.checked_add(pixel_x));

                    if let Some(offset) = pixel_offset {
                        unsafe {
                            *fb_ptr.add(offset) = color;
                        }
                    }
                }
            }
        }
    }
}

// 文字列を描画
//
// # Safety
// fb_base は有効なフレームバッファアドレスである必要があり、
// 描画範囲が画面内に収まっていることを呼び出し側が保証する必要があります。
pub unsafe fn draw_string(fb_base: u64, width: u32, x: usize, y: usize, s: &str, color: u32) {
    let mut cur_x = x;
    for ch in s.bytes() {
        unsafe {
            draw_char(fb_base, width, cur_x, y, ch, color);
        }
        // オーバーフローチェック
        if let Some(next_x) = cur_x.checked_add(8) {
            cur_x = next_x;
        } else {
            break; // オーバーフロー時は描画を停止
        }
    }
}

// 矩形を描画（塗りつぶし）
//
// # Safety
// fb_base は有効なフレームバッファアドレスである必要があり、
// 描画範囲が画面内に収まっていることを呼び出し側が保証する必要があります。
pub unsafe fn draw_rect(
    fb_base: u64,
    width: u32,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    color: u32,
) {
    // 空の矩形は何もしない
    if w == 0 || h == 0 {
        return;
    }

    let fb = fb_base as *mut u32;
    let stride = width as usize;

    // 描画範囲を画面境界でクリップ
    let x_end = x.saturating_add(w).min(stride);
    if x >= x_end {
        return; // 完全に画面外
    }
    let clipped_w = x_end - x;

    // 行単位で塗りつぶし（高速化）
    for dy in 0..h {
        let pixel_y = y.saturating_add(dy);
        // Y座標のオーバーフローチェックは省略（通常の画面サイズでは発生しない）

        let row_start = pixel_y * stride + x;
        // SAFETY: 呼び出し側が描画範囲の有効性を保証
        unsafe {
            let row_ptr = fb.add(row_start);
            let row_slice = core::slice::from_raw_parts_mut(row_ptr, clipped_w);
            row_slice.fill(color);
        }
    }
}

// 矩形の枠線を描画
//
// # Safety
// fb_base は有効なフレームバッファアドレスである必要があり、
// 描画範囲が画面内に収まっていることを呼び出し側が保証する必要があります。
pub unsafe fn draw_rect_outline(
    fb_base: u64,
    width: u32,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    color: u32,
) {
    let fb = fb_base as *mut u32;

    if w == 0 || h == 0 {
        return; // サイズが0の場合は何もしない
    }

    // 上下の辺
    for dx in 0..w {
        if let Some(pixel_x) = x.checked_add(dx)
            && pixel_x < width as usize
        {
            // 上辺
            let top_offset = y
                .checked_mul(width as usize)
                .and_then(|y_off| y_off.checked_add(pixel_x));
            if let Some(off) = top_offset {
                unsafe {
                    *fb.add(off) = color;
                }
            }

            // 下辺
            if let Some(bottom_y) = y.checked_add(h - 1) {
                let bottom_offset = bottom_y
                    .checked_mul(width as usize)
                    .and_then(|y_off| y_off.checked_add(pixel_x));
                if let Some(off) = bottom_offset {
                    unsafe {
                        *fb.add(off) = color;
                    }
                }
            }
        }
    }

    // 左右の辺
    for dy in 0..h {
        if let Some(pixel_y) = y.checked_add(dy) {
            // 左辺
            if x < width as usize {
                let left_offset = pixel_y
                    .checked_mul(width as usize)
                    .and_then(|y_off| y_off.checked_add(x));
                if let Some(off) = left_offset {
                    unsafe {
                        *fb.add(off) = color;
                    }
                }
            }

            // 右辺
            if let Some(right_x) = x.checked_add(w - 1)
                && right_x < width as usize
            {
                let right_offset = pixel_y
                    .checked_mul(width as usize)
                    .and_then(|y_off| y_off.checked_add(right_x));
                if let Some(off) = right_offset {
                    unsafe {
                        *fb.add(off) = color;
                    }
                }
            }
        }
    }
}

// フレームバッファライター（writeln!マクロ対応）
pub struct FramebufferWriter {
    // 可視化機能が有効な場合はパブリック、それ以外はプライベート
    #[cfg(feature = "visualize-allocator")]
    pub fb_base: u64,
    #[cfg(not(feature = "visualize-allocator"))]
    fb_base: u64,

    #[cfg(feature = "visualize-allocator")]
    pub width: u32,
    #[cfg(not(feature = "visualize-allocator"))]
    width: u32,

    #[cfg(feature = "visualize-allocator")]
    pub height: u32,
    #[cfg(not(feature = "visualize-allocator"))]
    height: u32,

    x: usize,
    y: usize,
    color: u32,
}

impl FramebufferWriter {
    pub fn new(fb_base: u64, width: u32, height: u32, color: u32) -> Self {
        Self {
            fb_base,
            width,
            height,
            x: 0,
            y: 0,
            color,
        }
    }

    // カーソル位置を設定
    pub fn set_position(&mut self, x: usize, y: usize) {
        self.x = x;
        self.y = y;
    }

    // 文字色を設定
    pub fn set_color(&mut self, color: u32) {
        self.color = color;
    }

    // 現在位置から指定幅をクリア（背景色で塗りつぶし）
    pub fn clear_area(&mut self, width_chars: usize, bg_color: u32) {
        let width_pixels = width_chars * 8;
        let height_pixels = 10; // 1行分の高さ
        unsafe {
            draw_rect(
                self.fb_base,
                self.width,
                self.x,
                self.y,
                width_pixels,
                height_pixels,
                bg_color,
            );
        }
    }

    // 改行処理
    fn newline(&mut self) {
        self.x = 0;
        self.y += 10; // 1行分（8ピクセル + マージン2ピクセル）
    }

    /// 画面全体をクリア（指定色で塗りつぶし）
    ///
    /// # Arguments
    /// * `color` - 塗りつぶし色（0xRRGGBB形式）
    pub fn clear_screen(&mut self, color: u32) {
        let fb = self.fb_base as *mut u32;
        let total_pixels = (self.width as usize) * (self.height as usize);
        for i in 0..total_pixels {
            unsafe {
                *fb.add(i) = color;
            }
        }
        // カーソルを左上に戻す
        self.x = 0;
        self.y = 0;
    }
}

impl core::fmt::Write for FramebufferWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for ch in s.bytes() {
            if ch == b'\n' {
                self.newline();
            } else {
                // 画面の右端に達したら自動改行
                if self.x + 8 > self.width as usize {
                    self.newline();
                }

                unsafe {
                    draw_char(self.fb_base, self.width, self.x, self.y, ch, self.color);
                }
                self.x += 8;
            }
        }
        Ok(())
    }
}
