use crate::config;

/// Reads ~/.cache/wal/colors.json (pywal) and returns color1–color3 as RGB tuples.
/// Returns None if pywal is not installed or the file cannot be parsed.
///
/// Python pywal always writes to ~/.cache/wal/ on all platforms, so we prefer
/// that path. On Linux this is identical to dirs::cache_dir(); on Windows/macOS
/// it differs from the platform cache dir.
pub fn load_pywal_palette() -> Option<Vec<(u8, u8, u8)>> {
    let path = dirs::home_dir()?.join(".cache").join("wal").join("colors.json");
    let text = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let colors = json.get("colors")?;
    let palette: Vec<(u8, u8, u8)> = ["color1", "color2", "color3"]
        .iter()
        .filter_map(|key| {
            let hex = colors.get(key)?.as_str()?;
            parse_hex(hex)
        })
        .collect();
    if palette.is_empty() { None } else { Some(palette) }
}

fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 { return None; }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some((r, g, b))
}

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
