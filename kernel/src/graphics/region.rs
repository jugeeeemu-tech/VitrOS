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
