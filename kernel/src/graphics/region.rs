//! 描画領域定義

/// 描画領域を定義する構造体
#[derive(Debug, Clone, Copy)]
pub struct Region {
    /// 領域の左上X座標
    pub x: u32,
    /// 領域の左上Y座標
    pub y: u32,
    /// 領域の幅
    pub width: u32,
    /// 領域の高さ
    pub height: u32,
}

impl Region {
    /// 新しい描画領域を作成
    ///
    /// # Arguments
    /// * `x` - 左上X座標
    /// * `y` - 左上Y座標
    /// * `width` - 幅
    /// * `height` - 高さ
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// 点が領域内にあるかチェック
    ///
    /// # Arguments
    /// * `px` - チェックするX座標
    /// * `py` - チェックするY座標
    ///
    /// # Returns
    /// 点が領域内ならtrue
    #[allow(dead_code)]
    pub fn contains(&self, px: u32, py: u32) -> bool {
        px >= self.x && px < self.x + self.width && py >= self.y && py < self.y + self.height
    }

    /// 領域の右端X座標を取得
    pub fn right(&self) -> u32 {
        self.x + self.width
    }

    /// 領域の下端Y座標を取得
    pub fn bottom(&self) -> u32 {
        self.y + self.height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_region_new() {
        let region = Region::new(10, 20, 100, 50);
        assert_eq!(region.x, 10);
        assert_eq!(region.y, 20);
        assert_eq!(region.width, 100);
        assert_eq!(region.height, 50);
    }

    #[test_case]
    fn test_region_right_bottom() {
        let region = Region::new(10, 20, 100, 50);
        assert_eq!(region.right(), 110);
        assert_eq!(region.bottom(), 70);
    }

    #[test_case]
    fn test_region_contains_inside() {
        let region = Region::new(10, 20, 100, 50);
        // 内部の点
        assert!(region.contains(50, 40));
        // 左上の点
        assert!(region.contains(10, 20));
    }

    #[test_case]
    fn test_region_contains_boundary() {
        let region = Region::new(10, 20, 100, 50);
        // 右端（110）は含まない
        assert!(!region.contains(110, 40));
        // 下端（70）は含まない
        assert!(!region.contains(50, 70));
        // 右下角は含まない
        assert!(!region.contains(110, 70));
        // 右端の直前は含む
        assert!(region.contains(109, 40));
        // 下端の直前は含む
        assert!(region.contains(50, 69));
    }

    #[test_case]
    fn test_region_contains_outside() {
        let region = Region::new(10, 20, 100, 50);
        // 左側
        assert!(!region.contains(5, 40));
        // 上側
        assert!(!region.contains(50, 10));
        // 完全に外
        assert!(!region.contains(200, 200));
    }

    #[test_case]
    fn test_region_zero_size() {
        let region = Region::new(10, 20, 0, 0);
        // 幅・高さが0の場合、どの点も含まない
        assert!(!region.contains(10, 20));
        assert!(!region.contains(10, 21));
        assert_eq!(region.right(), 10);
        assert_eq!(region.bottom(), 20);
    }
}
