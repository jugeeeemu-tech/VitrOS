//! 描画ターゲットの抽象化

use super::Region;

/// 描画ターゲットの抽象化
pub trait DrawTarget {
    fn base_addr(&self) -> u64;
    fn width(&self) -> u32;
    fn height(&self) -> u32;

    fn stride(&self) -> u32 {
        self.width()
    }

    fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, color: u32);
    fn draw_char(&mut self, x: u32, y: u32, ch: u8, color: u32);
    fn draw_string(&mut self, x: u32, y: u32, s: &str, color: u32);
}

/// ダーティ領域追跡機能を持つ描画ターゲット
pub trait DirtyTrackingTarget: DrawTarget {
    fn mark_dirty(&mut self, region: &Region);
    fn mark_all_dirty(&mut self);
    fn take_dirty_rect(&mut self) -> Option<Region>;
}
