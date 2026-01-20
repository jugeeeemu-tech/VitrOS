mod font;

pub mod buffer;
pub mod compositor;
pub mod draw_target;
pub mod region;
pub mod shadow_buffer;
pub mod writer;

pub use font::FONT_8X8;
pub use region::Region;
pub use writer::TaskWriter;

/// 高速なメモリ塗りつぶし（rep stosd使用）
///
/// x86-64の`rep stosd`命令を使用して、32ビット値を連続してメモリに書き込みます。
/// ループオーバーヘッドなしで高速に動作します。
///
/// # Safety
/// - ptrは有効なメモリアドレスで、count個のu32を書き込める領域を指す必要がある
#[inline(always)]
unsafe fn fast_fill_u32(ptr: *mut u32, value: u32, count: usize) {
    if count == 0 {
        return;
    }
    // SAFETY: 呼び出し元がptrの有効性とcount個の書き込み可能領域を保証する
    unsafe {
        core::arch::asm!(
            "rep stosd",
            inout("rdi") ptr => _,
            inout("ecx") count => _,
            in("eax") value,
            options(nostack, preserves_flags)
        );
    }
}

// フレームバッファに文字を描画
//
// # Safety
// fb_base は有効なフレームバッファアドレスである必要があり、
// 描画範囲が画面内に収まっていることを呼び出し側が保証する必要があります。
pub unsafe fn draw_char(fb_base: u64, width: u32, x: usize, y: usize, ch: u8, color: u32) {
    let fb_ptr = fb_base as *mut u32;
    let stride = width as usize;

    if ch < 32 || ch > 126 {
        return; // サポート外の文字
    }

    // 事前に境界チェック: 文字全体（8x8）が画面内に収まるか確認
    // 文字の右端 (x + 7) と下端 (y + 7) が画面内であればOK
    let x_end = match x.checked_add(8) {
        Some(end) => end,
        None => return, // オーバーフロー
    };
    let y_end = match y.checked_add(8) {
        Some(end) => end,
        None => return, // オーバーフロー
    };

    // 文字が完全に画面外の場合は早期リターン
    if x >= stride || y_end == 0 {
        return;
    }

    let font_index = (ch - 32) as usize;
    let glyph = FONT_8X8[font_index];

    // 文字が完全に画面内に収まる場合は高速パス
    // SAFETY: 呼び出し元が描画範囲の有効性を保証する
    if x_end <= stride {
        // 高速パス: 境界チェック不要
        for row in 0..8 {
            let glyph_row = glyph[row];
            if glyph_row == 0 {
                continue; // この行には描画するピクセルがない
            }
            let row_offset = (y + row) * stride + x;
            for col in 0..8 {
                if (glyph_row >> col) & 1 == 1 {
                    unsafe { *fb_ptr.add(row_offset + col) = color };
                }
            }
        }
    } else {
        // 低速パス: 右端がクリップされる場合
        let visible_cols = stride.saturating_sub(x).min(8);
        for row in 0..8 {
            let glyph_row = glyph[row];
            if glyph_row == 0 {
                continue;
            }
            let row_offset = (y + row) * stride + x;
            for col in 0..visible_cols {
                if (glyph_row >> col) & 1 == 1 {
                    unsafe { *fb_ptr.add(row_offset + col) = color };
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

    // 行単位で塗りつぶし（rep stosd使用で高速化）
    for dy in 0..h {
        let pixel_y = y.saturating_add(dy);
        // Y座標のオーバーフローチェックは省略（通常の画面サイズでは発生しない）

        let row_start = pixel_y * stride + x;
        // SAFETY: 呼び出し側が描画範囲の有効性を保証
        unsafe {
            let row_ptr = fb.add(row_start);
            fast_fill_u32(row_ptr, color, clipped_w);
        }
    }
}

// 矩形の枠線を描画
//
// # Safety
// fb_base は有効なフレームバッファアドレスである必要があり、
// 描画範囲が画面内に収まっていることを呼び出し側が保証する必要があります。
#[allow(dead_code)]
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
    #[cfg(any(feature = "visualize-allocator", feature = "visualize-pipeline"))]
    pub fb_base: u64,
    #[cfg(not(any(feature = "visualize-allocator", feature = "visualize-pipeline")))]
    fb_base: u64,

    #[cfg(any(feature = "visualize-allocator", feature = "visualize-pipeline"))]
    pub width: u32,
    #[cfg(not(any(feature = "visualize-allocator", feature = "visualize-pipeline")))]
    width: u32,

    #[cfg(any(feature = "visualize-allocator", feature = "visualize-pipeline"))]
    pub height: u32,
    #[cfg(not(any(feature = "visualize-allocator", feature = "visualize-pipeline")))]
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
    #[allow(dead_code)]
    pub fn set_position(&mut self, x: usize, y: usize) {
        self.x = x;
        self.y = y;
    }

    // 文字色を設定
    #[allow(dead_code)]
    pub fn set_color(&mut self, color: u32) {
        self.color = color;
    }

    // 現在位置から指定幅をクリア（背景色で塗りつぶし）
    #[allow(dead_code)]
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
        // rep stosdを使用して高速に塗りつぶし
        // SAFETY: fb_baseは有効なフレームバッファアドレスであり、
        // total_pixelsはwidth * heightで計算された有効な範囲
        unsafe {
            fast_fill_u32(fb, color, total_pixels);
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
