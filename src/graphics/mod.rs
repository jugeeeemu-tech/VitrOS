mod font;

pub use font::FONT_8X8;

// フレームバッファに文字を描画
pub fn draw_char(fb_base: u64, width: u32, x: usize, y: usize, ch: u8, color: u32) {
    let fb_ptr = fb_base as *mut u32;

    if ch < 32 || ch > 126 {
        return; // サポート外の文字
    }

    let font_index = (ch - 32) as usize;
    let glyph = FONT_8X8[font_index];

    unsafe {
        for row in 0..8 {
            for col in 0..8 {
                if (glyph[row] >> col) & 1 == 1 {
                    let pixel_offset = (y + row) * width as usize + (x + col);
                    *fb_ptr.add(pixel_offset) = color;
                }
            }
        }
    }
}

// 文字列を描画
pub fn draw_string(fb_base: u64, width: u32, x: usize, y: usize, s: &str, color: u32) {
    let mut cur_x = x;
    for ch in s.bytes() {
        draw_char(fb_base, width, cur_x, y, ch, color);
        cur_x += 8; // 次の文字へ（8ピクセル幅）
    }
}

// 矩形を描画（塗りつぶし）
pub fn draw_rect(fb_base: u64, width: u32, x: usize, y: usize, w: usize, h: usize, color: u32) {
    let fb = fb_base as *mut u32;
    for dy in 0..h {
        for dx in 0..w {
            let pixel_x = x + dx;
            let pixel_y = y + dy;
            let offset = pixel_y * width as usize + pixel_x;
            unsafe {
                *fb.add(offset) = color;
            }
        }
    }
}

// 矩形の枠線を描画
pub fn draw_rect_outline(fb_base: u64, width: u32, x: usize, y: usize, w: usize, h: usize, color: u32) {
    let fb = fb_base as *mut u32;

    // 上下の辺
    for dx in 0..w {
        unsafe {
            *fb.add(y * width as usize + x + dx) = color;
            *fb.add((y + h - 1) * width as usize + x + dx) = color;
        }
    }

    // 左右の辺
    for dy in 0..h {
        unsafe {
            *fb.add((y + dy) * width as usize + x) = color;
            *fb.add((y + dy) * width as usize + x + w - 1) = color;
        }
    }
}

// フレームバッファライター（writeln!マクロ対応）
pub struct FramebufferWriter {
    pub fb_base: u64,
    pub width: u32,
    pub height: u32,
    pub x: usize,
    pub y: usize,
    pub color: u32,
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

    // 改行処理
    fn newline(&mut self) {
        self.x = 0;
        self.y += 10; // 1行分（8ピクセル + マージン2ピクセル）
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

                draw_char(self.fb_base, self.width, self.x, self.y, ch, self.color);
                self.x += 8;
            }
        }
        Ok(())
    }
}
