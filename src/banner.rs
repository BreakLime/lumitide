use crate::color_state;
use crate::config;

const BANNER: &str = "/\\  /\\  /\\
\\/  \\/  \\/
    /\\
    \\/

L U M I T I D E";

const DEFAULT_ACCENT: (u8, u8, u8) = (0, 200, 200);

pub fn print_banner() {
    let cfg = config::load();
    let accent = if cfg.pywal {
        color_state::load_pywal_palette()
            .and_then(|p| p.first().copied())
            .unwrap_or(DEFAULT_ACCENT)
    } else {
        DEFAULT_ACCENT
    };

    let prefix = "  ";
    let color = format!("\x1B[38;2;{};{};{}m\x1B[1m", accent.0, accent.1, accent.2);

    for line in BANNER.lines() {
        println!("{prefix}{color}{line}\x1B[0m");
    }
    println!();
}
