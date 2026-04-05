use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, block::Title},
    Frame,
};

use crate::spectrum;
use crate::utils::fmt_time;

/// All state the panel needs to render one frame.
pub struct PanelState<'a> {
    pub cover_lines: &'a [Line<'static>],
    pub track_name: &'a str,
    pub artist_name: &'a str,
    pub album_name: &'a str,
    pub track_label: Option<&'a str>,
    pub elapsed: f64,
    pub total: f64,
    pub volume: f32,
    pub paused: bool,
    pub dl_status: &'a str,
    pub bar_color: Option<(u8, u8, u8)>,
    /// Pre-rendered spectrum or download-progress lines (BAR_HEIGHT rows).
    pub vis_lines: &'a [Line<'static>],
    pub is_local: bool,
    pub show_controls: bool,
    pub show_controls_hint: bool,
}

pub fn render(frame: &mut Frame, state: &PanelState) {
    let terminal = frame.area();

    // ── Calculate content-fitted size ─────────────────────────────────────────
    let cover_col_w = (state.cover_lines.first().map(|l| l.width()).unwrap_or(0) + 1) as u16;
    let right_col_w: u16 = 50;

    // rows: 1 top-pad + title + artist + album + [label] + time + empty + vis
    let right_rows = 1 + 3
        + if state.track_label.is_some() { 1 } else { 0 }
        + 1 + 1
        + state.vis_lines.len() as u16;
    let cover_rows = state.cover_lines.len() as u16 + 1; // +1 top-pad

    let dim = Style::new().fg(Color::DarkGray);

    // ── "? controls" hint pinned to terminal bottom-right ────────────────────
    if state.show_controls_hint {
        let hint_text = " Press ? for ctrl ";
        let hint_w = hint_text.len() as u16;
        let hint_area = Rect::new(
            terminal.x + terminal.width.saturating_sub(hint_w),
            terminal.y + terminal.height.saturating_sub(1),
            hint_w.min(terminal.width),
            1,
        );
        frame.render_widget(Paragraph::new(Line::styled(hint_text, dim)), hint_area);
    }

    // ── Border + controls (only when show_controls) ───────────────────────────
    let inner = if state.show_controls {
        let block_w = cover_col_w + right_col_w + 2; // +2 borders
        let block_h = right_rows.max(cover_rows) + 2; // +2 borders
        let x = terminal.x + terminal.width.saturating_sub(block_w) / 2;
        let y = terminal.y + terminal.height.saturating_sub(block_h) / 2;
        let area = Rect::new(x, y, block_w.min(terminal.width), block_h.min(terminal.height));

        let hint_text = if state.is_local {
            "← prev  Spc pause  → next  ↑↓ vol  q/Esc quit"
        } else {
            "← prev  Spc pause  → next  ↑↓ vol  d download  q/Esc quit"
        };
        let controls = Title::from(Line::styled(hint_text, dim))
            .alignment(Alignment::Center);
        let outer = Block::default()
            .borders(Borders::ALL)
            .border_style(dim)
            .title(controls);
        let inner = outer.inner(area);
        frame.render_widget(outer, area);
        inner
    } else {
        // No border — centre content directly
        let content_w = cover_col_w + right_col_w;
        let content_h = right_rows.max(cover_rows);
        let x = terminal.x + terminal.width.saturating_sub(content_w) / 2;
        let y = terminal.y + terminal.height.saturating_sub(content_h) / 2;
        Rect::new(x, y, content_w.min(terminal.width), content_h.min(terminal.height))
    };

    // Two columns: cover | info+spectrum
    let cols = Layout::horizontal([
        Constraint::Length((state.cover_lines.first().map(|l| l.width()).unwrap_or(0) + 1) as u16),
        Constraint::Min(0),
    ])
    .split(inner);

    // ── Left: album cover ─────────────────────────────────────────────────────
    let mut cover_lines = vec![Line::raw("")];
    cover_lines.extend(state.cover_lines.iter().cloned());
    frame.render_widget(Paragraph::new(Text::from(cover_lines)), cols[0]);

    // ── Right: track info + visualisation ────────────────────────────────────
    let title_style = match state.bar_color {
        Some((r, g, b)) => Style::new().fg(Color::Rgb(r, g, b)).add_modifier(Modifier::BOLD),
        None => Style::new().add_modifier(Modifier::BOLD),
    };
    let dim = Style::new().fg(Color::DarkGray);

    let mut right: Vec<Line<'static>> = Vec::new();

    right.push(Line::raw(""));
    right.push(Line::from(Span::styled(state.track_name.to_string(), title_style)));
    right.push(Line::from(Span::styled(state.artist_name.to_string(), dim)));
    right.push(Line::from(Span::styled(state.album_name.to_string(), dim)));
    if let Some(label) = state.track_label {
        right.push(Line::from(Span::styled(label.to_string(), dim)));
    }

    // Time + volume line
    let vol_pct = (state.volume * 100.0) as u32;
    let pause_str = if state.paused { "  ⏸" } else { "" };
    let time_str = fmt_time(state.elapsed);
    let mut time_line: Vec<Span<'static>> = vec![
        Span::styled(time_str, Style::new().add_modifier(Modifier::BOLD)),
    ];
    if state.total > 0.0 {
        time_line.push(Span::styled(
            format!(" / {}", fmt_time(state.total)),
            dim,
        ));
    }

    if !state.dl_status.is_empty() && !state.dl_status.starts_with('⬇') {
        // Show status ("✓ Saved", "✓ Saving...", "✗ Error") in the info line
        let status_style = if state.dl_status.starts_with('✓') {
            match state.bar_color {
                Some((r, g, b)) => Style::new().fg(Color::Rgb(r, g, b)).add_modifier(Modifier::BOLD),
                None => Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            }
        } else {
            dim // error states
        };
        time_line.push(Span::styled(
            format!("  vol {}%{}", vol_pct, pause_str),
            Style::new().add_modifier(Modifier::BOLD),
        ));
        time_line.push(Span::styled(
            format!("  {}", state.dl_status),
            status_style,
        ));
    } else {
        time_line.push(Span::styled(
            format!("  vol {}%{}", vol_pct, pause_str),
            Style::new(),
        ));
    }
    right.push(Line::from(time_line));
    right.push(Line::raw(""));

    // Visualisation lines
    for line in state.vis_lines {
        right.push(line.clone());
    }

    frame.render_widget(Paragraph::new(Text::from(right)), cols[1]);
}

/// Build pre-rendered visualisation lines for the current frame.
/// Returns either spectrum or download-progress lines.
pub fn build_vis_lines(
    spec_buf: &[f32],
    band_edges: &[usize],
    bar_peaks: &mut Vec<f32>,
    bar_peak_hold: &mut Vec<u32>,
    bar_color: Option<(u8, u8, u8)>,
    dl_status: &str,
    dl_bytes: u64,
    dl_total: u64,
    calm: bool,
) -> Vec<Line<'static>> {
    if dl_status.starts_with('⬇') {
        spectrum::render_dl_progress(dl_bytes, dl_total, bar_color)
    } else {
        let normalized: Vec<f32> = if calm {
            spectrum::CALM_SPECTRUM.to_vec()
        } else {
            spectrum::compute_spectrum(spec_buf, band_edges)
        };
        spectrum::render_spectrum(&normalized, bar_peaks, bar_peak_hold, bar_color)
    }
}
