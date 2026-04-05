use crate::config;

/// Centralised colour source — all UI components read from one instance
/// so title, spectrum bars, and download bar always show identical colours.
pub struct ColorState {
    palette: Vec<(u8, u8, u8)>, // 3 dominant colours from the album cover
    offset: usize,
    active: bool, // true while a drop is running
}

impl ColorState {
    pub fn new(palette: Vec<(u8, u8, u8)>) -> Self {
        Self { palette, offset: 0, active: false }
    }

    /// Call once per render frame with the current drop state.
    pub fn update(&mut self, drop_active: bool) {
        self.active = drop_active;
        if !drop_active {
            self.offset = 0; // reset to base colour when drop ends
        }
    }

    /// Step to next palette colour (call once per beat during a drop).
    pub fn advance(&mut self) {
        self.offset += 1;
    }

    /// Returns (r, g, b) or None when no colour should be shown.
    ///
    /// - No colour mode: None
    /// - always_color, no drop: palette[0] (stable base)
    /// - always_color or in drop: cycles with beats
    pub fn current_color(&self) -> Option<(u8, u8, u8)> {
        let cfg = config::load();
        let colors_on = self.active || cfg.always_color;
        if !colors_on || self.palette.is_empty() {
            return None;
        }
        let offset = if self.active { self.offset } else { 0 };
        Some(self.palette[offset % self.palette.len()])
    }

    pub fn colors_active(&self) -> bool {
        let cfg = config::load();
        self.active || cfg.always_color
    }
}
