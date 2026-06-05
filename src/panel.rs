use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap, block::{Position, Title}},
    Frame,
};

use crate::api::{ArtistDetail, Credit, TrackInfo};
use crate::cover::CoverArt;
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
    pub queue_status: Option<String>,
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
    // Hidden while a download is active so the two don't overlap
    if state.show_controls_hint && state.queue_status.is_none() {
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

    // Queue download status pinned to bottom-right (replaces the controls hint row)
    if let Some(ref qs) = state.queue_status {
        let qs_text = format!(" {} ", qs);
        let qs_w = qs_text.chars().count() as u16;
        let qs_area = Rect::new(
            terminal.x + terminal.width.saturating_sub(qs_w),
            terminal.y + terminal.height.saturating_sub(1),
            qs_w.min(terminal.width),
            1,
        );
        frame.render_widget(Paragraph::new(Line::styled(qs_text, dim)), qs_area);
    }

    // ── Border + controls (only when show_controls) ───────────────────────────
    let inner = if state.show_controls {
        let hint_text = if state.is_local {
            "← prev  Spc pause  → next  ↑↓ vol  t info  a artist  q/Esc quit"
        } else {
            "← prev  Spc pause  → next  ↑↓ vol  d download  r radio  t info  a artist  q/Esc quit"
        };
        let hint_line = Line::styled(hint_text, dim);
        // The box must fit the cover+info columns *and* the controls hint in the
        // border title, else a long hint is clipped at both ends.
        let block_w = (cover_col_w + right_col_w + 2).max(hint_line.width() as u16 + 2);
        let block_h = right_rows.max(cover_rows) + 2; // +2 borders
        let x = terminal.x + terminal.width.saturating_sub(block_w) / 2;
        let y = terminal.y + terminal.height.saturating_sub(block_h) / 2;
        let area = Rect::new(x, y, block_w.min(terminal.width), block_h.min(terminal.height));

        let controls = Title::from(hint_line).alignment(Alignment::Center);
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

// ─── Info popups ───────────────────────────────────────────────────────────────

fn accent_style(accent: Option<(u8, u8, u8)>) -> Style {
    match accent {
        Some((r, g, b)) => Style::new().fg(Color::Rgb(r, g, b)),
        None => Style::new().fg(Color::White),
    }
}

/// A "Label  value" line for the info popups.
fn field(label: &str, value: String, accent: Option<(u8, u8, u8)>) -> Line<'static> {
    let dim = Style::new().fg(Color::DarkGray);
    Line::from(vec![
        Span::styled(format!("{:<11}", label), dim),
        Span::styled(value, accent_style(accent)),
    ])
}

/// Build the body lines for the track-info popup.
pub fn build_track_info_lines(
    track: &TrackInfo,
    credits: Option<&[Credit]>,
    accent: Option<(u8, u8, u8)>,
) -> Vec<Line<'static>> {
    let dim = Style::new().fg(Color::DarkGray);
    let bold_accent = accent_style(accent).add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Title (+ version + explicit marker)
    let mut title_spans = vec![Span::styled(track.title.clone(), bold_accent)];
    if let Some(v) = &track.version {
        if !v.is_empty() {
            title_spans.push(Span::styled(format!("  ({v})"), dim));
        }
    }
    if track.explicit {
        title_spans.push(Span::styled("  [E]", dim));
    }
    lines.push(Line::from(title_spans));
    lines.push(Line::raw(""));

    let artists = if track.artists.is_empty() {
        track.artist_name.clone()
    } else {
        track.artists.join(", ")
    };
    lines.push(field("Artists", artists, accent));

    let mut album = track.album_name.clone();
    if let Some(y) = track.album_release_year {
        album.push_str(&format!(" ({y})"));
    }
    lines.push(field("Album", album, accent));
    lines.push(field("Track", format!("{} · disc {}", track.track_num, track.volume_num), accent));
    lines.push(field("Duration", fmt_time(track.duration as f64), accent));
    lines.push(field("Quality", track.audio_quality.clone(), accent));
    if let Some(p) = track.popularity {
        lines.push(field("Popularity", format!("{p}"), accent));
    }
    if let Some(isrc) = &track.isrc {
        lines.push(field("ISRC", isrc.clone(), accent));
    }
    if let Some(c) = &track.album_copyright {
        lines.push(field("©", c.clone(), accent));
    }

    // ── Credits ──────────────────────────────────────────────────────────────
    lines.push(Line::raw(""));
    match credits {
        None => lines.push(Line::from(Span::styled("Loading credits…", dim))),
        Some(c) if c.is_empty() => {
            lines.push(Line::from(Span::styled("No credits available", dim)));
        }
        Some(c) => {
            lines.push(Line::from(Span::styled("Credits", bold_accent)));
            // Pad role labels to the longest role (+2) so values never collide
            // with long roles like "Mixing Engineer" or "Drum Programmer".
            let role_w = c.iter().map(|cr| cr.role.chars().count()).max().unwrap_or(0).max(9) + 2;
            for credit in c {
                lines.push(Line::from(vec![
                    Span::styled(format!("{:<role_w$}", credit.role), dim),
                    Span::styled(credit.names.join(", "), Style::new().fg(Color::Gray)),
                ]));
            }
        }
    }

    lines
}

/// Compute a centred rect for a popup of the given inner content size.
/// Count how many rows `lines` occupy once greedily word-wrapped to `width`,
/// mirroring ratatui's `Wrap` so scroll can be clamped to the real content
/// height. (ratatui's own `line_count` is behind an unstable feature, so we do
/// the small amount of arithmetic ourselves.)
fn wrapped_height(lines: &[Line<'static>], width: u16) -> u16 {
    let w = width.max(1) as usize;
    lines.iter().map(|line| {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let mut rows: u16 = 1;
        let mut col = 0usize; // chars used on the current row
        for word in text.split_whitespace() {
            let wl = word.chars().count();
            if col == 0 {
                col = wl;
            } else if col + 1 + wl <= w {
                col += 1 + wl;
            } else {
                rows += 1;
                col = wl;
            }
            // A single word longer than the line hard-wraps onto extra rows.
            while col > w {
                rows += 1;
                col -= w;
            }
        }
        rows
    }).sum()
}

fn centered(area: Rect, inner_w: u16, inner_h: u16) -> Rect {
    let box_w = (inner_w + 2).min(area.width);
    let box_h = (inner_h + 2).min(area.height);
    let x = area.x + area.width.saturating_sub(box_w) / 2;
    let y = area.y + area.height.saturating_sub(box_h) / 2;
    Rect::new(x, y, box_w, box_h)
}

/// Render a bordered, centred text popup over the current frame. `scroll` is the
/// requested vertical offset; the value actually applied (clamped to the content)
/// is returned so the caller can keep its stored offset in range.
pub fn render_info_popup(
    frame: &mut Frame,
    title: &str,
    lines: Vec<Line<'static>>,
    accent: Option<(u8, u8, u8)>,
    scroll: u16,
) -> u16 {
    let area = frame.area();
    let content_w = lines.iter().map(|l| l.width()).max().unwrap_or(24) as u16;
    let inner_w = content_w.clamp(28, area.width.saturating_sub(4).max(28));
    let inner_h = (lines.len() as u16).min(area.height.saturating_sub(2));
    let rect = centered(area, inner_w + 1, inner_h); // +1 for left padding

    let border = accent_style(accent);
    // Inner geometry depends only on the borders (titles render on the border row).
    let inner = Block::default().borders(Borders::ALL).inner(rect);
    // Pad one column on the left
    let text_area = Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(1), inner.height);

    let total = wrapped_height(&lines, text_area.width);
    let max_scroll = total.saturating_sub(text_area.height);
    let scroll = scroll.min(max_scroll);
    let para = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(Title::from(Span::styled(
            format!(" {title} "),
            border.add_modifier(Modifier::BOLD),
        )).alignment(Alignment::Center));
    if max_scroll > 0 {
        block = block.title(
            Title::from(Span::styled(" ↑↓ scroll ", border))
                .position(Position::Bottom)
                .alignment(Alignment::Right),
        );
    }

    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    frame.render_widget(para.scroll((scroll, 0)), text_area);
    scroll
}

/// Render the artist-info popup: pixelated picture on the left, name +
/// popularity + biography on the right.
pub fn render_artist_popup(
    frame: &mut Frame,
    fallback_name: &str,
    detail: Option<&ArtistDetail>,
    art: Option<&CoverArt>,
    accent: Option<(u8, u8, u8)>,
    scroll: u16,
) -> u16 {
    let area = frame.area();
    let dim = Style::new().fg(Color::DarkGray);
    let bold_accent = accent_style(accent).add_modifier(Modifier::BOLD);

    // ── Right-hand text ────────────────────────────────────────────────────────
    let name = detail.map(|d| d.name.clone()).unwrap_or_else(|| fallback_name.to_string());
    let mut info: Vec<Line<'static>> = vec![
        Line::from(Span::styled(name, bold_accent)),
    ];
    if let Some(d) = detail {
        if let Some(p) = d.popularity {
            info.push(Line::from(Span::styled(format!("Popularity {p}"), dim)));
        }
        info.push(Line::raw(""));
        match &d.bio {
            Some(b) if !b.is_empty() => {
                info.push(Line::from(Span::styled(b.clone(), Style::new().fg(Color::Gray))));
            }
            _ => info.push(Line::from(Span::styled("No biography available", dim))),
        }
    } else {
        info.push(Line::raw(""));
        info.push(Line::from(Span::styled("Loading artist…", dim)));
    }

    // ── Geometry ───────────────────────────────────────────────────────────────
    let cover_lines: &[Line<'static>] = art.map(|a| a.color.as_slice()).unwrap_or(&[]);
    let cover_w = cover_lines.first().map(|l| l.width()).unwrap_or(0) as u16;
    let pic_col = if cover_w > 0 { cover_w + 2 } else { 0 };
    let info_col: u16 = 46;
    let inner_w = pic_col + info_col;
    let inner_h = (cover_lines.len() as u16).max(16);
    let rect = centered(area, inner_w, inner_h);

    let border = accent_style(accent);
    // Inner geometry depends only on the borders (titles render on the border row).
    let inner = Block::default().borders(Borders::ALL).inner(rect);

    // Work out where the bio text goes (and its width) so scroll can be clamped.
    let info_area = if pic_col > 0 {
        Layout::horizontal([Constraint::Length(pic_col), Constraint::Min(0)]).split(inner)[1]
    } else {
        Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(1), inner.height)
    };

    let total = wrapped_height(&info, info_area.width);
    let max_scroll = total.saturating_sub(info_area.height);
    let scroll = scroll.min(max_scroll);
    let para = Paragraph::new(Text::from(info)).wrap(Wrap { trim: true }).scroll((scroll, 0));

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(Title::from(Span::styled(
            " Artist ",
            border.add_modifier(Modifier::BOLD),
        )).alignment(Alignment::Center));
    if max_scroll > 0 {
        block = block.title(
            Title::from(Span::styled(" ↑↓ scroll ", border))
                .position(Position::Bottom)
                .alignment(Alignment::Right),
        );
    }

    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);

    if pic_col > 0 {
        let cols = Layout::horizontal([
            Constraint::Length(pic_col),
            Constraint::Min(0),
        ])
        .split(inner);
        frame.render_widget(Paragraph::new(Text::from(cover_lines.to_vec())), cols[0]);
        frame.render_widget(para, cols[1]);
    } else {
        frame.render_widget(para, info_area);
    }
    scroll
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(s: &str) -> Line<'static> {
        Line::from(s.to_string())
    }

    #[test]
    fn wrapped_height_single_short_line() {
        assert_eq!(wrapped_height(&[line("hello world")], 40), 1);
    }

    #[test]
    fn wrapped_height_wraps_on_word_boundary() {
        // "aaaa bbbb" fills width 9, "cccc" spills to a second row.
        assert_eq!(wrapped_height(&[line("aaaa bbbb cccc")], 9), 2);
    }

    #[test]
    fn wrapped_height_hard_wraps_long_word() {
        // An 8-char word at width 3 needs ceil(8/3) = 3 rows.
        assert_eq!(wrapped_height(&[line("aaaaaaaa")], 3), 3);
    }

    #[test]
    fn wrapped_height_sums_multiple_lines() {
        assert_eq!(wrapped_height(&[line("one"), line(""), line("two")], 40), 3);
    }
}
