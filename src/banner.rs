use crate::color_state;
use crate::config;

// ── Building blocks ──────────────────────────────────────────────

const THREE_DIAMONDS: &str = "  /\\      /\\      /\\
 /  \\    /  \\    /  \\
/    \\  /    \\  /    \\
\\    /  \\    /  \\    /
 \\  /    \\  /    \\  /
  \\/      \\/      \\/";
const THREE_DIAMONDS_W: usize = 22;

const ONE_DIAMOND: &str = "  /\\
 /  \\
/    \\
\\    /
 \\  /
  \\/";

const THREE_SMALL: &str = " /\\    /\\    /\\
/  \\  /  \\  /  \\
\\  /  \\  /  \\  /
 \\/    \\/    \\/";
const THREE_SMALL_W: usize = 16;

const ONE_SMALL: &str = " /\\
/  \\
\\  /
 \\/";

// ── Pre-composed stacked layouts ─────────────────────────────────

const STACKED_LARGE: &str = "  /\\      /\\      /\\
 /  \\    /  \\    /  \\
/    \\  /    \\  /    \\
\\    /  \\    /  \\    /
 \\  /    \\  /    \\  /
  \\/      \\/      \\/
          /\\
         /  \\
        /    \\
        \\    /
         \\  /
          \\/

   L U M I T I D E";

const STACKED_MEDIUM: &str = " /\\    /\\    /\\
/  \\  /  \\  /  \\
\\  /  \\  /  \\  /
 \\/    \\/    \\/
       /\\
      /  \\
      \\  /
       \\/

 L U M I T I D E";

const BANNER_TEXT: &str = "L U M I T I D E";

const DEFAULT_ACCENT: (u8, u8, u8) = (0, 200, 200);

// ── Compose a horizontal (side-by-side) layout at runtime ────────

fn compose_horizontal(
    left: &str,
    left_w: usize,
    right: &str,
    label: &str,
) -> (String, usize) {
    let ll: Vec<&str> = left.lines().collect();
    let rl: Vec<&str> = right.lines().collect();
    let right_w = rl.iter().map(|l| l.len()).max().unwrap_or(0);
    let gap = 4_usize;
    let text_gap = 3_usize;
    let n = ll.len().max(rl.len());
    let text_line = n / 2;

    let total_w = left_w
        + gap
        + right_w
        + if label.is_empty() {
            0
        } else {
            text_gap + label.len()
        };

    let mut buf = String::new();
    for i in 0..n {
        let l = ll.get(i).copied().unwrap_or("");
        let r = rl.get(i).copied().unwrap_or("");

        // Left block, padded to fixed width
        buf.push_str(l);
        (l.len()..left_w).for_each(|_| buf.push(' '));

        // Gap
        (0..gap).for_each(|_| buf.push(' '));

        // Right block
        buf.push_str(r);

        // Label on the middle line
        if i == text_line && !label.is_empty() {
            (r.len()..right_w).for_each(|_| buf.push(' '));
            (0..text_gap).for_each(|_| buf.push(' '));
            buf.push_str(label);
        }

        if i + 1 < n {
            buf.push('\n');
        }
    }
    (buf, total_w)
}

// ── Public entry point ───────────────────────────────────────────

pub fn print_banner() {
    let (w, h) = crossterm::terminal::size().unwrap_or((80, 24));
    let term_w = w as usize;
    let term_h = h as usize;

    // Reserve lines for the menu, prompt, and some breathing room.
    let menu_lines = 10;
    let avail_h = term_h.saturating_sub(menu_lines);

    // Pick the best layout given available space.
    // Prefer stacked (tall) layouts; fall back to horizontal (wide) ones
    // when vertical space is tight but horizontal space is plentiful.
    enum Layout {
        Block(&'static str),
        Horizontal(String),
    }

    let layout = if avail_h >= 15 && term_w >= 28 {
        Layout::Block(STACKED_LARGE)
    } else if avail_h >= 7 && term_w >= 48 {
        let (art, _) =
            compose_horizontal(THREE_DIAMONDS, THREE_DIAMONDS_W, ONE_DIAMOND, "L U M I T I D E");
        Layout::Horizontal(art)
    } else if avail_h >= 11 && term_w >= 22 {
        Layout::Block(STACKED_MEDIUM)
    } else if avail_h >= 5 && term_w >= 38 {
        let (art, _) =
            compose_horizontal(THREE_SMALL, THREE_SMALL_W, ONE_SMALL, "L U M I T I D E");
        Layout::Horizontal(art)
    } else if avail_h >= 2 && term_w >= 16 {
        Layout::Block(BANNER_TEXT)
    } else {
        return;
    };

    let art: &str = match &layout {
        Layout::Block(s) => s,
        Layout::Horizontal(s) => s.as_str(),
    };

    let cfg = config::load();
    let accent = if cfg.pywal {
        color_state::load_pywal_palette()
            .and_then(|p| p.first().copied())
            .unwrap_or(DEFAULT_ACCENT)
    } else {
        DEFAULT_ACCENT
    };

    // Left-align with a small indent to match the menu's position.
    let prefix = "  ";
    let color = format!("\x1B[38;2;{};{};{}m\x1B[1m", accent.0, accent.1, accent.2);

    for line in art.lines() {
        println!("{prefix}{color}{line}\x1B[0m");
    }
    println!();
}
