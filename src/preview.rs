use std::cell::Cell;
use std::collections::VecDeque;
use std::io::{self, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{fs, thread};

use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Terminal,
};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::api::{TidalClient, TrackInfo};
use crate::color_state::{self, ColorState};
use crate::config;
use crate::cover::{render_cover, render_placeholder, ART_CHARS};
use crate::panel::{self, PanelState};
use crate::spectrum::{self, FFT_SIZE, NUM_BARS};

const BUFFER_THRESHOLD: u64 = 65_536;
const DROP_DURATION_SECS: f64 = 10.0;
const TARGET_SR: u32 = 22_050;

// Discrete volume levels (as linear multipliers, 1% – 100%)
const VOL_PRESETS: &[f32] = &[
    0.01, 0.02, 0.03, 0.04, 0.05, 0.06, 0.07, 0.08, 0.09,
    0.10, 0.20, 0.30, 0.40, 0.50, 0.60, 0.70, 0.80, 0.90, 1.00,
];

fn vol_up(v: f32)   -> f32 { VOL_PRESETS.iter().copied().find(|&p| p > v + 1e-4).unwrap_or(1.00) }
fn vol_down(v: f32) -> f32 { VOL_PRESETS.iter().copied().rev().find(|&p| p < v - 1e-4).unwrap_or(0.0) }

// Persists the current song's dominant palette color so the *next* transition
// arrow can be colored even though it runs before the next API call.
thread_local! {
    static LAST_ARROW_COLOR: Cell<Option<(u8, u8, u8)>> = Cell::new(None);
}

type AppTerminal = Terminal<CrosstermBackend<io::Stdout>>;

fn setup_terminal() -> Result<AppTerminal> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn teardown_terminal(terminal: &mut AppTerminal) {
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
}

/// Consume all buffered terminal events; return the last navigation direction found.
/// Clears accumulated key presses from the OS queue so spam/hold doesn't carry over.
fn drain_events() -> Option<&'static str> {
    let mut last_nav: Option<&'static str> = None;
    while event::poll(Duration::ZERO).unwrap_or(false) {
        if let Ok(Event::Key(key)) = event::read() {
            if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat {
                match key.code {
                    KeyCode::Right | KeyCode::Char('n') | KeyCode::Char('N') => last_nav = Some("next"),
                    KeyCode::Left  | KeyCode::Char('p') | KeyCode::Char('P') => last_nav = Some("prev"),
                    KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc   => last_nav = Some("quit"),
                    _ => {}
                }
            }
        }
    }
    last_nav
}

/// Animate a directional arrow pointing to the next track label.
/// `frames` × `ms_per_frame` controls total duration.
/// The last frame stays frozen on screen while the caller blocks (e.g. API calls).
/// Purely visual — does NOT poll events so that key presses always reach play().
/// Draw a 3-line ASCII "fat arrow" transition.
/// Returns Some(nav) if a nav key was pressed during the animation, None otherwise.
///
///   next:        ----------->          (dim)
///            ==================>>  5/10  (bright)
///                 ----------->          (dim)
///
///   prev:    5/10  <<==================  (bright)
///                    <-----------        (dim)
///                    <-----------        (dim)

/// Remap every braille-cell colour in a cover to the nearest palette entry.
/// This gives the cover art a "pywal-themed" look matching the UI chrome.
fn remap_cover_to_palette(lines: &[Line<'static>], palette: &[(u8, u8, u8)]) -> Vec<Line<'static>> {
    lines.iter().map(|line| {
        let spans: Vec<Span<'static>> = line.spans.iter().map(|span| {
            let new_color = if let Some(Color::Rgb(r, g, b)) = span.style.fg {
                let &(pr, pg, pb) = palette.iter().min_by_key(|&&(pr, pg, pb)| {
                    let dr = r as i32 - pr as i32;
                    let dg = g as i32 - pg as i32;
                    let db = b as i32 - pb as i32;
                    dr * dr + dg * dg + db * db
                }).unwrap_or(&(255, 255, 255));
                Color::Rgb(pr, pg, pb)
            } else {
                span.style.fg.unwrap_or(Color::White)
            };
            Span::styled(span.content.clone(), Style::new().fg(new_color))
        }).collect();
        Line::from(spans)
    }).collect()
}

/// Register with the OS media controls (SMTC / MPRIS / MPRemoteCommandCenter).
/// Returns None silently if the platform doesn't support it or initialisation fails.
fn build_media_controls(
    title: &str,
    artist: &str,
    cover_url: Option<&str>,
    tx: std::sync::mpsc::Sender<souvlaki::MediaControlEvent>,
) -> Option<souvlaki::MediaControls> {
    use souvlaki::{MediaControls, MediaMetadata, MediaPlayback, PlatformConfig};

    #[cfg(target_os = "windows")]
    let config = {
        // Windows Terminal has no Win32 HWND (virtual console), so we create a
        // hidden 1×1 window purely to give SMTC a valid owner handle.
        extern "system" {
            fn CreateWindowExW(
                ex_style: u32, class_name: *const u16, window_name: *const u16,
                style: u32, x: i32, y: i32, w: i32, h: i32,
                parent: *mut std::ffi::c_void, menu: *mut std::ffi::c_void,
                instance: *mut std::ffi::c_void, param: *mut std::ffi::c_void,
            ) -> *mut std::ffi::c_void;
        }
        let class: Vec<u16> = "Static\0".encode_utf16().collect();
        let name:  Vec<u16> = "lumitide\0".encode_utf16().collect();
        let hwnd = unsafe {
            CreateWindowExW(0, class.as_ptr(), name.as_ptr(), 0, 0, 0, 1, 1,
                std::ptr::null_mut(), std::ptr::null_mut(),
                std::ptr::null_mut(), std::ptr::null_mut())
        };
        if hwnd.is_null() { return None; }
        PlatformConfig { dbus_name: "", display_name: "Lumitide", hwnd: Some(hwnd) }
    };

    #[cfg(target_os = "macos")]
    let config = PlatformConfig { dbus_name: "", display_name: "Lumitide", hwnd: None };

    #[cfg(target_os = "linux")]
    let config = PlatformConfig {
        dbus_name: "org.mpris.MediaPlayer2.lumitide",
        display_name: "Lumitide",
        hwnd: None,
    };

    let mut controls = MediaControls::new(config).ok()?;
    // attach must come first — it triggers SMTC session initialisation on Windows.
    // set_metadata called before attach returns 0x80070032 (media type not initialized).
    let _ = controls.attach(move |event| { let _ = tx.send(event); });
    let _ = controls.set_playback(MediaPlayback::Playing { progress: None });
    let _ = controls.set_metadata(MediaMetadata {
        title: Some(title),
        artist: Some(artist),
        album: None,
        cover_url,
        duration: None,
    });
    Some(controls)
}

/// Interpolate between two RGB colors. t=0.0 → `from`, t=1.0 → `to`.
fn lerp_color(from: (u8, u8, u8), to: (u8, u8, u8), t: f32) -> Color {
    let r = (from.0 as f32 + (to.0 as f32 - from.0 as f32) * t) as u8;
    let g = (from.1 as f32 + (to.1 as f32 - from.1 as f32) * t) as u8;
    let b = (from.2 as f32 + (to.2 as f32 - from.2 as f32) * t) as u8;
    Color::Rgb(r, g, b)
}

/// Build a list of (text, Color) segments that fade from dark (tail) to `full_color` (head).
/// `chars` is the total number of characters to fill. `segments` is how many gradient steps.
/// `dim_factor` scales the `full_color` brightness for side-line dimming (1.0 = full, 0.6 = dimmed).
fn gradient_spans(chars: usize, segments: usize, full_color: (u8, u8, u8), dim_factor: f32, ch: char) -> Vec<(String, Color)> {
    if chars == 0 { return vec![]; }
    let dark = (20u8, 20u8, 20u8);
    let target = (
        (full_color.0 as f32 * dim_factor) as u8,
        (full_color.1 as f32 * dim_factor) as u8,
        (full_color.2 as f32 * dim_factor) as u8,
    );
    let segs = segments.min(chars);
    let base = chars / segs;
    let extra = chars % segs;
    let mut result = Vec::with_capacity(segs);
    for i in 0..segs {
        let seg_len = base + if i < extra { 1 } else { 0 };
        let t = if segs == 1 { 1.0 } else { i as f32 / (segs - 1) as f32 };
        let color = lerp_color(dark, target, t);
        result.push((ch.to_string().repeat(seg_len), color));
    }
    result
}

fn draw_transition(
    terminal: &mut AppTerminal,
    direction: &str,
    label: Option<&str>,
    frames: usize,
    ms_per_frame: u64,
    color: Option<(u8, u8, u8)>,
) -> Option<&'static str> {
    let label_str = label.unwrap_or("");
    let is_next = direction != "prev";
    const SEGS: usize = 8;

    for frame in 0..frames {
        let _ = terminal.draw(|f| {
            let area = f.area();

            // Middle shaft: up to 50 `=` chars, leaving room for `>>` (2), spaces (2), label
            let max_shaft = ((area.width as usize).saturating_sub(label_str.len() + 6)).min(50);
            let shaft_len = (frame + 1) * max_shaft / frames;
            // Top/bottom shafts are 2/3 the length so arrowheads align at the same column
            let side_len  = shaft_len * 2 / 3;

            // Fixed total width so nothing jumps between frames
            let total_w = (max_shaft + 4 + label_str.len()) as u16; // shaft + >> + "  " + label
            let x = area.width.saturating_sub(total_w) / 2;
            let cy = area.height / 2; // centre row

            let dim    = Style::new().fg(Color::DarkGray);
            let bright = Style::new().fg(Color::White).add_modifier(Modifier::BOLD);

            if is_next {
                // Gradient: tail (left) = dark, head (right) = album color
                let mid_pad = " ".repeat(max_shaft - shaft_len);
                let (mid_line, side_line) = if let Some(col) = color {
                    let head_color = Color::Rgb(col.0, col.1, col.2);
                    let head_style = Style::new().fg(head_color).add_modifier(Modifier::BOLD);

                    // Middle: gradient shaft + >> head + label
                    let mut mid_spans = vec![Span::raw(mid_pad.clone())];
                    for (text, c) in gradient_spans(shaft_len, SEGS, col, 1.0, '=') {
                        mid_spans.push(Span::styled(text, Style::new().fg(c).add_modifier(Modifier::BOLD)));
                    }
                    mid_spans.push(Span::styled(">>", head_style));
                    mid_spans.push(Span::raw("  "));
                    mid_spans.push(Span::styled(label_str.to_string(), head_style));
                    let mid = Line::from(mid_spans);

                    // Side: dimmed gradient + > head
                    let side_head_color = Color::Rgb(
                        (col.0 as f32 * 0.6) as u8,
                        (col.1 as f32 * 0.6) as u8,
                        (col.2 as f32 * 0.6) as u8,
                    );
                    let side_head_style = Style::new().fg(side_head_color);
                    let side_pad = " ".repeat(max_shaft - side_len);
                    let mut side_spans = vec![Span::raw(side_pad)];
                    for (text, c) in gradient_spans(side_len, SEGS, col, 0.6, '-') {
                        side_spans.push(Span::styled(text, Style::new().fg(c)));
                    }
                    side_spans.push(Span::styled(">", side_head_style));
                    side_spans.push(Span::raw(" ".repeat(2 + label_str.len() + 1)));
                    let side = Line::from(side_spans);
                    (mid, side)
                } else {
                    // No color — original plain white rendering
                    let side_pad  = " ".repeat(max_shaft - side_len);
                    let side_dash = "-".repeat(side_len);
                    let side = Line::from(vec![
                        Span::raw(side_pad),
                        Span::styled(format!("{side_dash}>"), dim),
                        Span::raw(" ".repeat(2 + label_str.len() + 1)),
                    ]);
                    let mid_eq = "=".repeat(shaft_len);
                    let mid = Line::from(vec![
                        Span::raw(mid_pad),
                        Span::styled(format!("{mid_eq}>>"), bright),
                        Span::raw("  "),
                        Span::styled(label_str.to_string(), bright),
                    ]);
                    (mid, side)
                };

                let w = total_w.min(area.width);
                if cy > 0 {
                    f.render_widget(Paragraph::new(side_line.clone()), Rect::new(x, cy - 1, w, 1));
                }
                f.render_widget(Paragraph::new(mid_line), Rect::new(x, cy, w, 1));
                if cy + 1 < area.height {
                    f.render_widget(Paragraph::new(side_line), Rect::new(x, cy + 1, w, 1));
                }
            } else {
                // prev: head on the LEFT — gradient reversed (head=color, tail=dark, right→left)
                // For prev, the shaft grows right from head. Segment 0 is the head (full color),
                // segment N-1 is the tail (dark).
                let (mid_line, side_line) = if let Some(col) = color {
                    let head_color = Color::Rgb(col.0, col.1, col.2);
                    let head_style = Style::new().fg(head_color).add_modifier(Modifier::BOLD);

                    // Middle: label + << head + gradient shaft (head first = full color, then fades)
                    let mut mid_spans = vec![
                        Span::styled(label_str.to_string(), head_style),
                        Span::raw("  "),
                        Span::styled("<<", head_style),
                    ];
                    // Gradient reversed: segment 0 = full color (near head), last = dark (tail)
                    let grads = gradient_spans(shaft_len, SEGS, col, 1.0, '=');
                    for (text, c) in grads.into_iter().rev() {
                        mid_spans.push(Span::styled(text, Style::new().fg(c).add_modifier(Modifier::BOLD)));
                    }
                    mid_spans.push(Span::raw(" ".repeat(max_shaft - shaft_len)));
                    let mid = Line::from(mid_spans);

                    // Side: dimmed, reversed gradient
                    let side_head_color = Color::Rgb(
                        (col.0 as f32 * 0.6) as u8,
                        (col.1 as f32 * 0.6) as u8,
                        (col.2 as f32 * 0.6) as u8,
                    );
                    let side_head_style = Style::new().fg(side_head_color);
                    let head_col = label_str.len() + 3;
                    let mut side_spans = vec![
                        Span::raw(" ".repeat(head_col)),
                        Span::styled("<", side_head_style),
                    ];
                    let side_grads = gradient_spans(side_len, SEGS, col, 0.6, '-');
                    for (text, c) in side_grads.into_iter().rev() {
                        side_spans.push(Span::styled(text, Style::new().fg(c)));
                    }
                    side_spans.push(Span::raw(" ".repeat(max_shaft - side_len)));
                    let side = Line::from(side_spans);
                    (mid, side)
                } else {
                    // No color — original plain white rendering
                    let mid_eq  = "=".repeat(shaft_len);
                    let mid_pad = " ".repeat(max_shaft - shaft_len);
                    let mid = Line::from(vec![
                        Span::styled(label_str.to_string(), bright),
                        Span::raw("  "),
                        Span::styled(format!("<<{mid_eq}"), bright),
                        Span::raw(mid_pad),
                    ]);
                    let head_col  = label_str.len() + 3;
                    let side_dash = "-".repeat(side_len);
                    let side_pad  = " ".repeat(max_shaft - side_len);
                    let side = Line::from(vec![
                        Span::raw(" ".repeat(head_col)),
                        Span::styled(format!("<{side_dash}"), dim),
                        Span::raw(side_pad),
                    ]);
                    (mid, side)
                };

                let w = total_w.min(area.width);
                if cy > 0 {
                    f.render_widget(Paragraph::new(side_line.clone()), Rect::new(x, cy - 1, w, 1));
                }
                f.render_widget(Paragraph::new(mid_line), Rect::new(x, cy, w, 1));
                if cy + 1 < area.height {
                    f.render_widget(Paragraph::new(side_line), Rect::new(x, cy + 1, w, 1));
                }
            }
        });

        thread::sleep(Duration::from_millis(ms_per_frame));

        // Poll once with zero timeout — interrupt on any nav key (Press or Repeat)
        if event::poll(Duration::ZERO).unwrap_or(false) {
            if let Ok(Event::Key(key)) = event::read() {
                if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat {
                    match key.code {
                        KeyCode::Right | KeyCode::Char('n') | KeyCode::Char('N') => return Some("next"),
                        KeyCode::Left  | KeyCode::Char('p') | KeyCode::Char('P') => return Some("prev"),
                        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc   => return Some("quit"),
                        _ => {}
                    }
                }
            }
        }
    }
    None
}

// ─── Streaming file wrapper ───────────────────────────────────────────────────

struct StreamingFile {
    file: fs::File,
    download_done: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl io::Read for StreamingFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.file.read(buf) {
                Ok(0) => {
                    if self.stop.load(Ordering::Relaxed) {
                        return Ok(0);
                    }
                    if self.download_done.load(Ordering::Relaxed) {
                        return Ok(0); // true EOF
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                other => return other,
            }
        }
    }
}

impl Seek for StreamingFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
}

impl MediaSource for StreamingFile {
    fn is_seekable(&self) -> bool { true }
    fn byte_len(&self) -> Option<u64> {
        // Symphonia's MP4/M4A demuxer requires a known byte length to seek within
        // the container. Return the current file size — correct for local files
        // (always complete) and reflects downloaded bytes for streaming files.
        self.file.metadata().ok().map(|m| m.len())
    }
}

// ─── Public entry points ──────────────────────────────────────────────────────

/// Stream-preview a Tidal track (download while playing).
pub fn run(
    client: &mut TidalClient,
    track_id: u64,
    debug: bool,
    track_label: Option<String>,
    shared_volume: Option<Arc<Mutex<f32>>>,
    already_saved: bool,
    direction: Option<&str>,
) -> Result<String> {
    // ── Terminal up before API calls so the transition stays on screen ────────
    let mut terminal = setup_terminal()?;
    if let Some(dir) = direction {
        let arrow_color = LAST_ARROW_COLOR.with(|c| c.get());
        if let Some(early) = draw_transition(&mut terminal, dir, track_label.as_deref(), 10, 40, arrow_color) {
            teardown_terminal(&mut terminal);
            return Ok(early.to_string());
        }
    }

    // API calls — last animation frame stays frozen while these block
    let track = match client.track(track_id) {
        Ok(t) => t,
        Err(e) => { teardown_terminal(&mut terminal); return Err(e); }
    };
    let url = match client.stream_url(track_id) {
        Ok(u) => u,
        Err(e) => { teardown_terminal(&mut terminal); return Err(e); }
    };

    // Fetch cover
    let cover_bytes = track.album_cover.as_deref()
        .and_then(|id| client.fetch_cover(id, 320).ok());

    // Download to temp file in a background thread
    let tmp = match tempfile::Builder::new().suffix(".flac").tempfile() {
        Ok(t) => t,
        Err(e) => { teardown_terminal(&mut terminal); return Err(e.into()); }
    };
    let tmp_path = tmp.path().to_path_buf();
    let _ = tmp.keep(); // we'll delete manually

    let download_done = Arc::new(AtomicBool::new(false));
    let header_ready = Arc::new(AtomicBool::new(false));

    let url_clone = url.clone();
    let path_clone = tmp_path.clone();
    let dd_clone = download_done.clone();
    let hr_clone = header_ready.clone();

    let dl_bytes_shared = Arc::new(AtomicU64::new(0));
    let dl_total_shared = Arc::new(AtomicU64::new(0));
    let dl_bytes_dl = dl_bytes_shared.clone();
    let dl_total_dl = dl_total_shared.clone();

    thread::spawn(move || {
        download_to_file(&url_clone, &path_clone, &dd_clone, &hr_clone, &dl_bytes_dl, &dl_total_dl);
    });

    // Wait until header bytes are ready
    loop {
        if header_ready.load(Ordering::Relaxed) { break; }
        thread::sleep(Duration::from_millis(50));
    }

    // Drain any nav keys that queued during API/download wait — skip before audio starts
    if let Some(nav) = drain_events() {
        download_done.store(true, Ordering::Relaxed);
        let _ = fs::remove_file(&tmp_path);
        let mut t = terminal;
        teardown_terminal(&mut t);
        return Ok(nav.to_string());
    }

    let volume = shared_volume.unwrap_or_else(|| {
        Arc::new(Mutex::new(config::load().volume))
    });

    let result = play(
        terminal,
        &tmp_path,
        download_done.clone(),
        dl_bytes_shared,
        dl_total_shared,
        &track,
        cover_bytes.as_deref(),
        debug,
        track_label.as_deref(),
        volume,
        false,
        already_saved,
    )?;

    download_done.store(true, Ordering::Relaxed); // signal dl thread to stop
    let _ = fs::remove_file(&tmp_path);
    Ok(result)
}

/// Play a local audio file using the same UI.
pub fn run_local(
    path: &str,
    track: &TrackInfo,
    cover_bytes: Option<&[u8]>,
    debug: bool,
    track_label: Option<String>,
    shared_volume: Option<Arc<Mutex<f32>>>,
) -> Result<String> {
    let terminal = setup_terminal()?;
    let download_done = Arc::new(AtomicBool::new(true)); // already on disk
    let dl_bytes = Arc::new(AtomicU64::new(0));
    let dl_total = Arc::new(AtomicU64::new(0));
    let volume = shared_volume.unwrap_or_else(|| {
        Arc::new(Mutex::new(config::load().volume))
    });
    play(
        terminal,
        std::path::Path::new(path),
        download_done,
        dl_bytes,
        dl_total,
        track,
        cover_bytes,
        debug,
        track_label.as_deref(),
        volume,
        true,
        false,
    )
}

// ─── Download helper ──────────────────────────────────────────────────────────

fn download_to_file(
    url: &str,
    path: &std::path::Path,
    done: &AtomicBool,
    header_ready: &AtomicBool,
    dl_bytes: &AtomicU64,
    dl_total: &AtomicU64,
) {
    let client = reqwest::blocking::Client::new();
    let resp = match client.get(url).send() {
        Ok(r) => r,
        Err(_) => { done.store(true, Ordering::Relaxed); return; }
    };
    if let Some(len) = resp.content_length() {
        dl_total.store(len, Ordering::Relaxed);
    }
    let mut file = match fs::File::create(path) {
        Ok(f) => f,
        Err(_) => { done.store(true, Ordering::Relaxed); return; }
    };
    use std::io::Write;
    let mut written: u64 = 0;
    if let Ok(mut reader) = resp.error_for_status() {
        let mut buf = vec![0u8; 65_536];
        loop {
            use std::io::Read;
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = file.write_all(&buf[..n]);
                    written += n as u64;
                    dl_bytes.store(written, Ordering::Relaxed);
                    if !header_ready.load(Ordering::Relaxed) && written >= BUFFER_THRESHOLD {
                        header_ready.store(true, Ordering::Relaxed);
                    }
                }
                Err(_) => break,
            }
        }
    }
    header_ready.store(true, Ordering::Relaxed);
    done.store(true, Ordering::Relaxed);
}

// ─── Core playback + UI loop ──────────────────────────────────────────────────

fn play(
    mut terminal: AppTerminal,
    path: &std::path::Path,
    download_done: Arc<AtomicBool>,
    dl_bytes: Arc<AtomicU64>,
    dl_total: Arc<AtomicU64>,
    track: &TrackInfo,
    cover_bytes: Option<&[u8]>,
    _debug: bool,
    track_label: Option<&str>,
    volume: Arc<Mutex<f32>>,
    is_local: bool,
    already_saved: bool,
) -> Result<String> {
    let cfg = config::load();

    // ── Probe for sample rate / channels ─────────────────────────────────────
    let probe_result = probe_flac(path);
    if probe_result.is_err() {
        // Must teardown before propagating — play() owns terminal by value
        teardown_terminal(&mut terminal);
    }
    let (sample_rate, channels) = probe_result?;

    // ── Render cover art (once) ───────────────────────────────────────────────
    let art_chars = if track_label.is_some() { 22 } else { ART_CHARS };
    let cover_art = cover_bytes
        .map(|b| render_cover(b, art_chars))
        .unwrap_or_else(|| render_placeholder(art_chars));
    let palette = if cfg.pywal {
        color_state::load_pywal_palette().unwrap_or_else(|| cover_art.palette.clone())
    } else {
        cover_art.palette.clone()
    };
    let pywal_cover: Option<Vec<Line<'static>>> = if cfg.pywal {
        Some(remap_cover_to_palette(&cover_art.color, &palette))
    } else {
        None
    };

    // ── Media controls (SMTC / MPRIS / MPRemoteCommandCenter) ────────────────
    // Write cover bytes to a temp file and pass a file:// URL.
    // HTTPS URLs via CreateFromUri require a message pump for async download;
    // GetFileFromPathAsync blocks synchronously and works without one.
    // Keep _cover_tmp alive for the track duration so the file isn't deleted.
    let _cover_tmp: Option<tempfile::NamedTempFile> = cover_bytes.and_then(|bytes| {
        use std::io::Write;
        let mut tmp = tempfile::Builder::new().suffix(".jpg").tempfile().ok()?;
        tmp.write_all(bytes).ok()?;
        Some(tmp)
    });
    let cover_url_str: Option<String> = _cover_tmp.as_ref().map(|tmp| {
        // souvlaki strips "file://" then passes the remainder to GetFileFromPathAsync,
        // which requires native Windows backslash paths — do NOT convert to forward slashes.
        format!("file://{}", tmp.path().to_string_lossy())
    });
    let (media_tx, media_rx) = std::sync::mpsc::channel::<souvlaki::MediaControlEvent>();
    let mut media_controls = build_media_controls(
        &track.title, &track.artist_name, cover_url_str.as_deref(), media_tx,
    );

    // ── Shared state ──────────────────────────────────────────────────────────
    let spec_buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(vec![0.0f32; FFT_SIZE]));
    let audio_buf: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::with_capacity(sample_rate as usize * 4)));
    let current_sample = Arc::new(AtomicU64::new(0));
    let playback_done = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    let stop_all = Arc::new(AtomicBool::new(false));

    let dl_status: Arc<Mutex<String>> = Arc::new(Mutex::new(
        if already_saved { "✓ Saved".to_string() } else { String::new() }
    ));
    let dl_flash_until: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));

    let drop_times: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let beat_times: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let analysis_done = Arc::new(AtomicBool::new(false));

    let total_frames = probe_total_frames(path).unwrap_or(0);

    // Query device config early so decode thread can resample to device rate
    let (device_sr, device_ch) = {
        let host = cpal::default_host();
        host.default_output_device()
            .and_then(|d| d.default_output_config().ok())
            .map(|c| (c.sample_rate().0, c.channels() as usize))
            .unwrap_or((sample_rate, channels))
    };

    // ── Decode thread ─────────────────────────────────────────────────────────
    {
        let spec_buf = spec_buf.clone();
        let audio_buf = audio_buf.clone();
        let current_sample = current_sample.clone();
        let playback_done = playback_done.clone();
        let stop_all = stop_all.clone();
        let download_done = download_done.clone();
        let path = path.to_path_buf();
        let ch = channels;
        let file_sr = sample_rate;

        thread::spawn(move || {
            let file = match fs::File::open(&path) {
                Ok(f) => f,
                Err(_) => { playback_done.store(true, Ordering::Relaxed); return; }
            };
            let streaming = StreamingFile {
                file,
                download_done: download_done.clone(),
                stop: stop_all.clone(),
            };
            let mss = MediaSourceStream::new(Box::new(streaming), Default::default());
            let probed = match symphonia::default::get_probe().format(
                &Hint::new(), mss, &FormatOptions::default(), &MetadataOptions::default(),
            ) {
                Ok(p) => p,
                Err(_) => { playback_done.store(true, Ordering::Relaxed); return; }
            };
            let mut format = probed.format;
            let track_info = match format.default_track() {
                Some(t) => t.clone(),
                None => { playback_done.store(true, Ordering::Relaxed); return; }
            };
            let mut decoder = match symphonia::default::get_codecs()
                .make(&track_info.codec_params, &DecoderOptions::default())
            {
                Ok(d) => d,
                Err(_) => { playback_done.store(true, Ordering::Relaxed); return; }
            };

            loop {
                if stop_all.load(Ordering::Relaxed) { break; }

                // Backpressure: don't decode too far ahead
                loop {
                    if stop_all.load(Ordering::Relaxed) { return; }
                    let buf_len = audio_buf.lock().unwrap_or_else(|e| e.into_inner()).len();
                    if buf_len < sample_rate as usize * 3 { break; }
                    thread::sleep(Duration::from_millis(5));
                }

                match format.next_packet() {
                    Ok(packet) => {
                        match decoder.decode(&packet) {
                            Ok(decoded) => {
                                let spec = *decoded.spec();
                                let mut sbuf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                                sbuf.copy_interleaved_ref(decoded);
                                let samples = sbuf.samples();

                                // Update spectrum ring buffer (mono mix)
                                {
                                    let mono: Vec<f32> = if ch == 2 {
                                        samples.chunks(2).map(|c| (c[0] + c[1]) * 0.5).collect()
                                    } else {
                                        samples.to_vec()
                                    };
                                    let mut sb = spec_buf.lock().unwrap_or_else(|e| e.into_inner());
                                    let n = mono.len().min(sb.len());
                                    let len = sb.len();
                                    sb.rotate_left(n);
                                    sb[len - n..].copy_from_slice(&mono[..n]);
                                }

                                current_sample.fetch_add(
                                    (samples.len() / ch.max(1)) as u64,
                                    Ordering::Relaxed,
                                );

                                // Convert channels then resample for the device
                                let device_samples = convert_audio(
                                    samples, ch, device_ch, file_sr, device_sr,
                                );
                                let mut buf = audio_buf.lock().unwrap_or_else(|e| e.into_inner());
                                buf.extend(device_samples.iter().copied());
                            }
                            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
                            Err(_) => break,
                        }
                    }
                    Err(_) => break,
                }
            }
            playback_done.store(true, Ordering::Relaxed);
        });
    }

    // ── cpal audio stream ─────────────────────────────────────────────────────
    let host = cpal::default_host();
    let device = host.default_output_device()
        .ok_or_else(|| anyhow::anyhow!("No audio output device"))?;

    let cpal_config = cpal::StreamConfig {
        channels: device_ch as cpal::ChannelCount,
        sample_rate: cpal::SampleRate(device_sr),
        buffer_size: cpal::BufferSize::Default,
    };

    let audio_buf_cpal = audio_buf.clone();
    let paused_cpal = paused.clone();
    let volume_cpal = volume.clone();
    let playback_done_cpal = playback_done.clone();

    let stream = device.build_output_stream::<f32, _, _>(
        &cpal_config,
        move |data: &mut [f32], _| {
            if paused_cpal.load(Ordering::Relaxed) {
                data.fill(0.0);
                return;
            }
            let vol = *volume_cpal.lock().unwrap_or_else(|e| e.into_inner());
            let mut buf = audio_buf_cpal.lock().unwrap_or_else(|e| e.into_inner());
            let mut underrun = false;
            for sample in data.iter_mut() {
                if let Some(s) = buf.pop_front() {
                    *sample = s * vol;
                } else {
                    *sample = 0.0;
                    underrun = true;
                }
            }
            if underrun && buf.is_empty() && playback_done_cpal.load(Ordering::Relaxed) {
                // All decoded audio consumed
            }
        },
        |err| eprintln!("Audio error: {err}"),
        None,
    )?;
    stream.play()?;

    // ── Analysis thread (drop + beat detection) ───────────────────────────────
    if !cfg.calm_mode {
        let path = path.to_path_buf();
        let drop_times = drop_times.clone();
        let beat_times = beat_times.clone();
        let analysis_done = analysis_done.clone();
        let stop_all = stop_all.clone();
        let download_done = download_done.clone();
        let drop_det = cfg.drop_detection;

        thread::spawn(move || {
            // Wait for full download before analysis
            while !download_done.load(Ordering::Relaxed) {
                if stop_all.load(Ordering::Relaxed) { return; }
                thread::sleep(Duration::from_millis(200));
            }
            if stop_all.load(Ordering::Relaxed) { return; }

            if let Ok(audio) = load_mono_f32(&path, TARGET_SR, &stop_all) {
                // Beat detection
                let beats = detect_beats(&audio, TARGET_SR);
                *beat_times.lock().unwrap_or_else(|e| e.into_inner()) = beats;

                if drop_det {
                    let drops = detect_drops(&audio, TARGET_SR);
                    *drop_times.lock().unwrap_or_else(|e| e.into_inner()) = drops;
                }
            }
            analysis_done.store(true, Ordering::Relaxed);
        });
    }

    // terminal already set up by caller

    let mut bar_peaks = vec![0.0f32; NUM_BARS];
    let mut bar_peak_hold = vec![0u32; NUM_BARS];
    let mut cs = ColorState::new(palette.clone());
    let mut drop_active_until: Option<Instant> = None;
    let mut show_controls = false;
    let mut fired_drops: std::collections::HashSet<u64> = Default::default();
    let mut beat_snap: Vec<f64> = Vec::new();
    let mut beat_idx: usize = 0;
    let band_edges = spectrum::compute_band_edges(sample_rate);

    let result_str = loop {
        let elapsed_secs = current_sample.load(Ordering::Relaxed) as f64 / sample_rate as f64;
        let total_secs = if total_frames > 0 { total_frames as f64 / sample_rate as f64 } else { 0.0 };

        // Check for drops
        if !cfg.calm_mode {
            let dt_list = drop_times.lock().unwrap_or_else(|e| e.into_inner()).clone();
            for &dt in &dt_list {
                let key = (dt * 1000.0) as u64;
                if !fired_drops.contains(&key) && elapsed_secs >= dt {
                    fired_drops.insert(key);
                    drop_active_until = Some(Instant::now() + Duration::from_secs_f64(DROP_DURATION_SECS));
                }
            }
        }
        let drop_active = drop_active_until.map_or(false, |t| Instant::now() < t) && !cfg.calm_mode;
        cs.update(drop_active);

        // Beat advance
        if drop_active {
            if beat_snap.is_empty() {
                let bt = beat_times.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if !bt.is_empty() { beat_snap = bt; }
            }
            while beat_idx < beat_snap.len() && elapsed_secs >= beat_snap[beat_idx] {
                cs.advance();
                beat_idx += 1;
            }
        }

        let bar_color = cs.current_color();
        let cover_lines = if cs.colors_active() {
            pywal_cover.as_deref().unwrap_or(&cover_art.color)
        } else {
            &cover_art.mono
        };

        let dl_stat = dl_status.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let dl_b = dl_bytes.load(Ordering::Relaxed);
        let dl_t = dl_total.load(Ordering::Relaxed);

        // Build visualisation lines
        let spec = spec_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let vis = panel::build_vis_lines(
            &spec, &band_edges, &mut bar_peaks, &mut bar_peak_hold,
            bar_color, &dl_stat, dl_b, dl_t, cfg.calm_mode,
        );

        let vol = *volume.lock().unwrap_or_else(|e| e.into_inner());
        let panel_state = PanelState {
            cover_lines,
            track_name: &track.title,
            artist_name: &track.artist_name,
            album_name: &track.album_name,
            track_label,
            elapsed: elapsed_secs,
            total: total_secs,
            volume: vol,
            paused: paused.load(Ordering::Relaxed),
            dl_status: &dl_stat,
            bar_color,
            vis_lines: &vis,
            is_local,
            show_controls,
            show_controls_hint: cfg.show_controls_hint,
        };

        terminal.draw(|f| panel::render(f, &panel_state))?;

        // ── Keyboard ─────────────────────────────────────────────────────────
        if event::poll(Duration::from_millis(90))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
                            stop_all.store(true, Ordering::Relaxed);
                            break "quit".to_string();
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Right => {
                            stop_all.store(true, Ordering::Relaxed);
                            break "next".to_string();
                        }
                        KeyCode::Char('p') | KeyCode::Left => {
                            stop_all.store(true, Ordering::Relaxed);
                            break "prev".to_string();
                        }
                        KeyCode::Char('r') | KeyCode::Char('R') => {
                            stop_all.store(true, Ordering::Relaxed);
                            break format!("radio:{}", track.id);
                        }
                        KeyCode::Char(' ') => {
                            let p = paused.load(Ordering::Relaxed);
                            paused.store(!p, Ordering::Relaxed);
                            if let Some(ref mut mc) = media_controls {
                                let _ = mc.set_playback(if p {
                                    souvlaki::MediaPlayback::Playing { progress: None }
                                } else {
                                    souvlaki::MediaPlayback::Paused { progress: None }
                                });
                            }
                        }
                        KeyCode::Up | KeyCode::Char('+') | KeyCode::Char('=') => {
                            {
                                let mut v = volume.lock().unwrap_or_else(|e| e.into_inner());
                                *v = vol_up(*v);
                            }
                            let _ = config::save_volume(*volume.lock().unwrap_or_else(|e| e.into_inner()));
                        }
                        KeyCode::Down | KeyCode::Char('-') => {
                            {
                                let mut v = volume.lock().unwrap_or_else(|e| e.into_inner());
                                *v = vol_down(*v);
                            }
                            let _ = config::save_volume(*volume.lock().unwrap_or_else(|e| e.into_inner()));
                        }
                        KeyCode::Char('?') => {
                            show_controls = !show_controls;
                        }
                        KeyCode::Char('d') | KeyCode::Char('D') => {
                            if already_saved || is_local {
                                *dl_flash_until.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now() + Duration::from_secs(2));
                            } else {
                                let mut stat = dl_status.lock().unwrap_or_else(|e| e.into_inner());
                                if stat.is_empty() {
                                    let already_buffered = download_done.load(Ordering::Relaxed);
                                    if already_buffered {
                                        // Streaming done — copy is instant, no bar needed.
                                        // Show status in the info line only.
                                        *stat = "✓ Saving...".to_string();
                                        drop(stat);
                                        let temp_path = path.to_path_buf();
                                        let track_clone = track.clone();
                                        let cover_owned: Option<Vec<u8>> = cover_bytes.map(|b| b.to_vec());
                                        let out_dir = cfg.output_dir.clone();
                                        let dl_status2 = dl_status.clone();
                                        let dummy = Arc::new(AtomicU64::new(0));
                                        thread::spawn(move || {
                                            match save_streamed_track(
                                                &temp_path, &track_clone,
                                                cover_owned.as_deref(), &out_dir, &dummy,
                                            ) {
                                                Ok(_) => *dl_status2.lock().unwrap_or_else(|e| e.into_inner()) = "✓ Saved".to_string(),
                                                Err(e) => *dl_status2.lock().unwrap_or_else(|e| e.into_inner()) = format!("✗ {e}"),
                                            }
                                        });
                                    } else {
                                        // Still buffering — show the real streaming
                                        // progress bar (dl_bytes/dl_total already animating).
                                        *stat = "⬇ Downloading...".to_string();
                                        drop(stat);
                                        let temp_path = path.to_path_buf();
                                        let track_clone = track.clone();
                                        let cover_owned: Option<Vec<u8>> = cover_bytes.map(|b| b.to_vec());
                                        let dl_done = download_done.clone();
                                        let out_dir = cfg.output_dir.clone();
                                        let dl_status2 = dl_status.clone();
                                        let dummy = Arc::new(AtomicU64::new(0));
                                        thread::spawn(move || {
                                            while !dl_done.load(Ordering::Relaxed) {
                                                thread::sleep(Duration::from_millis(200));
                                            }
                                            match save_streamed_track(
                                                &temp_path, &track_clone,
                                                cover_owned.as_deref(), &out_dir, &dummy,
                                            ) {
                                                Ok(_) => *dl_status2.lock().unwrap_or_else(|e| e.into_inner()) = "✓ Saved".to_string(),
                                                Err(e) => *dl_status2.lock().unwrap_or_else(|e| e.into_inner()) = format!("✗ {e}"),
                                            }
                                        });
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // ── Media control events (OS media keys / Bluetooth) ─────────────────
        let mut media_nav: Option<String> = None;
        while let Ok(event) = media_rx.try_recv() {
            use souvlaki::MediaControlEvent::*;
            match event {
                Play => {
                    paused.store(false, Ordering::Relaxed);
                    if let Some(ref mut mc) = media_controls {
                        let _ = mc.set_playback(souvlaki::MediaPlayback::Playing { progress: None });
                    }
                }
                Pause => {
                    paused.store(true, Ordering::Relaxed);
                    if let Some(ref mut mc) = media_controls {
                        let _ = mc.set_playback(souvlaki::MediaPlayback::Paused { progress: None });
                    }
                }
                Toggle => {
                    let p = paused.load(Ordering::Relaxed);
                    paused.store(!p, Ordering::Relaxed);
                    if let Some(ref mut mc) = media_controls {
                        let _ = mc.set_playback(if p {
                            souvlaki::MediaPlayback::Playing { progress: None }
                        } else {
                            souvlaki::MediaPlayback::Paused { progress: None }
                        });
                    }
                }
                Next     => { stop_all.store(true, Ordering::Relaxed); media_nav = Some("next".into()); break; }
                Previous => { stop_all.store(true, Ordering::Relaxed); media_nav = Some("prev".into()); break; }
                Stop     => { stop_all.store(true, Ordering::Relaxed); media_nav = Some("quit".into()); break; }
                _ => {}
            }
        }
        if let Some(nav) = media_nav { break nav; }

        // Check if playback finished
        if playback_done.load(Ordering::Relaxed)
            && audio_buf.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
        {
            // If song ended in < 0.5s it's a decode failure, not a real ending
            let played = current_sample.load(Ordering::Relaxed) as f64 / sample_rate as f64;
            if played < 0.5 {
                break "fail".to_string();
            }
            break "end".to_string();
        }
    };

    // ── TUI teardown ──────────────────────────────────────────────────────────
    // Persist first palette color for the next song's transition arrow
    LAST_ARROW_COLOR.with(|c| c.set(palette.first().copied()));

    // Use teardown_terminal (ignores errors) so a teardown failure never loses result_str
    teardown_terminal(&mut terminal);

    Ok(result_str)
}

// ─── In-preview save ─────────────────────────────────────────────────────────

/// Copy the fully-streamed temp file to the output dir with a proper name,
/// then embed track metadata and cover art. Updates `progress` as bytes are written.
fn save_streamed_track(
    temp_path: &std::path::Path,
    track: &TrackInfo,
    cover_bytes: Option<&[u8]>,
    output_dir: &str,
    progress: &AtomicU64,
) -> Result<()> {
    use crate::metadata;
    use crate::utils::safe_filename;
    use std::io::{Read, Write};

    let out_dir = std::path::Path::new(output_dir);
    std::fs::create_dir_all(out_dir)?;

    let filename = safe_filename(&format!("{} - {}.flac", track.artist_name, track.title));
    let dest = out_dir.join(&filename);

    // Copy in chunks so the progress bar actually moves
    let mut src = fs::File::open(temp_path)?;
    let mut dst = fs::File::create(&dest)?;
    let mut buf = vec![0u8; 256 * 1024]; // 256 KB chunks
    let mut written: u64 = 0;
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 { break; }
        dst.write_all(&buf[..n])?;
        written += n as u64;
        progress.store(written, Ordering::Relaxed);
    }

    metadata::embed(&dest, track, cover_bytes)?;
    Ok(())
}

// ─── Audio utilities ──────────────────────────────────────────────────────────

fn probe_flac(path: &std::path::Path) -> Result<(u32, usize)> {
    let file = fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let probed = symphonia::default::get_probe().format(
        &Hint::new(), mss, &FormatOptions::default(), &MetadataOptions::default(),
    )?;
    let track = probed.format.default_track()
        .ok_or_else(|| anyhow::anyhow!("No default track"))?;
    let sr = track.codec_params.sample_rate.unwrap_or(44100);
    let ch = track.codec_params.channels.map(|c| c.count()).unwrap_or(2);
    Ok((sr, ch))
}

fn probe_total_frames(path: &std::path::Path) -> Option<u64> {
    let file = fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let probed = symphonia::default::get_probe().format(
        &Hint::new(), mss, &FormatOptions::default(), &MetadataOptions::default(),
    ).ok()?;
    probed.format.default_track()?.codec_params.n_frames
}

/// Read whole FLAC, mix to mono, optionally downsample to `target_sr`.
fn load_mono_f32(
    path: &std::path::Path,
    target_sr: u32,
    stop: &AtomicBool,
) -> Result<Vec<f32>> {
    let file = fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let probed = symphonia::default::get_probe().format(
        &Hint::new(), mss, &FormatOptions::default(), &MetadataOptions::default(),
    )?;
    let mut format = probed.format;
    let track = format.default_track()
        .ok_or_else(|| anyhow::anyhow!("No track"))?
        .clone();
    let file_sr = track.codec_params.sample_rate.unwrap_or(44100);
    let ch = track.codec_params.channels.map(|c| c.count()).unwrap_or(2);
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())?;

    let mut samples: Vec<f32> = Vec::with_capacity(file_sr as usize * 300);
    loop {
        // Bail out early if the song was skipped — frees memory immediately
        if stop.load(Ordering::Relaxed) {
            return Err(anyhow::anyhow!("stopped"));
        }
        match format.next_packet() {
            Ok(packet) => match decoder.decode(&packet) {
                Ok(decoded) => {
                    let mut sbuf = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
                    sbuf.copy_interleaved_ref(decoded);
                    let s = sbuf.samples();
                    if ch == 2 {
                        samples.extend(s.chunks(2).map(|c| (c[0] + c[1]) * 0.5));
                    } else {
                        samples.extend_from_slice(s);
                    }
                }
                Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
                Err(_) => break,
            },
            Err(_) => break,
        }
    }

    // Nearest-neighbour downsample if needed
    if file_sr != target_sr {
        let ratio = file_sr as f64 / target_sr as f64;
        let new_len = (samples.len() as f64 / ratio) as usize;
        samples = (0..new_len)
            .map(|i| {
                let idx = ((i as f64 * ratio) as usize).min(samples.len() - 1);
                samples[idx]
            })
            .collect();
    }
    Ok(samples)
}

// ─── Drop + beat detection ────────────────────────────────────────────────────

fn detect_beats(audio: &[f32], sr: u32) -> Vec<f64> {
    let hop = (0.05 * sr as f64) as usize;
    if hop == 0 || audio.len() < hop { return Vec::new(); }
    let n = audio.len() / hop;
    let energy: Vec<f32> = (0..n)
        .map(|i| {
            let seg = &audio[i * hop..(i + 1) * hop];
            seg.iter().map(|&s| s * s).sum::<f32>() / hop as f32
        })
        .collect();
    let onset: Vec<f32> = energy.windows(2).map(|w| (w[1] - w[0]).max(0.0)).collect();
    let height = percentile(&onset, 65.0);
    let min_dist = (0.25 / 0.05) as usize;
    find_peaks(&onset, Some(height), Some(min_dist))
        .into_iter()
        .map(|i| i as f64 * 0.05)
        .collect()
}

fn detect_drops(audio: &[f32], sr: u32) -> Vec<f64> {
    const FRAME_SEC: f64 = 0.1;
    const SUPPRESS_SEC: f64 = 30.0;
    const MIN_DROP_SEC: f64 = 25.0;
    const MAX_DROPS: usize = 3;
    const CTX_BACK_SEC: f64 = 20.0;
    const CTX_FRONT_SEC: f64 = 3.0;
    const SUSTAIN_SEC: f64 = 10.0;
    const DELTA_DB: f64 = 2.0;

    let frame_step = (FRAME_SEC * sr as f64) as usize;
    if frame_step == 0 { return Vec::new(); }
    let n = audio.len() / frame_step;
    let lookback = (2.0 * sr as f64) as usize;
    let ctx_back = (CTX_BACK_SEC / FRAME_SEC) as usize;
    let ctx_front = (CTX_FRONT_SEC / FRAME_SEC) as usize;
    let sustain = (SUSTAIN_SEC / FRAME_SEC) as usize;

    let lb: Vec<f64> = (0..n)
        .map(|fi| {
            let pos = fi * frame_step;
            let seg = &audio[pos.saturating_sub(lookback)..pos];
            if seg.is_empty() { return -90.0; }
            let rms = (seg.iter().map(|&s| (s * s) as f64).sum::<f64>() / seg.len() as f64).sqrt();
            20.0 * (rms + 1e-9).log10()
        })
        .collect();

    let long_rms_db = {
        let rms = (audio.iter().map(|&s| (s * s) as f64).sum::<f64>() / audio.len() as f64).sqrt();
        20.0 * (rms + 1e-9).log10()
    };

    let quiet_thresh = long_rms_db - 1.5;
    let loud_thresh = long_rms_db;
    let sustain_thresh = long_rms_db;

    let mut drops = Vec::new();
    let mut last_drop = -SUPPRESS_SEC;

    for fi in 0..n {
        if drops.len() >= MAX_DROPS { break; }
        let ft = fi as f64 * FRAME_SEC;
        if ft < MIN_DROP_SEC { continue; }

        let cs = fi.saturating_sub(ctx_back);
        let ce = fi.saturating_sub(ctx_front);
        if ce <= cs { continue; }
        let lb_ctx = lb[cs..ce].iter().cloned().fold(f64::INFINITY, f64::min);
        let lb_now = lb[fi];
        let lb_delta = lb_now - lb_ctx;
        let lb_fut = lb[fi..n.min(fi + sustain)].iter().sum::<f64>()
            / (n.min(fi + sustain) - fi).max(1) as f64;

        if lb_ctx < quiet_thresh
            && lb_now > loud_thresh
            && lb_delta > DELTA_DB
            && lb_fut > sustain_thresh
            && ft - last_drop > SUPPRESS_SEC
        {
            drops.push(ft);
            last_drop = ft;
        }
    }
    drops
}

fn find_peaks(x: &[f32], height: Option<f32>, distance: Option<usize>) -> Vec<usize> {
    let mut peaks = Vec::new();
    for i in 1..x.len().saturating_sub(1) {
        if x[i] > x[i - 1] && x[i] > x[i + 1] {
            if height.map_or(true, |h| x[i] >= h) {
                peaks.push(i);
            }
        }
    }
    if let Some(dist) = distance {
        let mut filtered = Vec::new();
        for p in peaks {
            if filtered.last().map_or(true, |&last| p - last >= dist) {
                filtered.push(p);
            }
        }
        filtered
    } else {
        peaks
    }
}

fn percentile(data: &[f32], p: f64) -> f32 {
    if data.is_empty() { return 0.0; }
    let mut sorted = data.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = (p / 100.0 * (sorted.len() - 1) as f64) as usize;
    sorted[idx]
}

/// Convert interleaved audio from (file_ch, file_sr) to (device_ch, device_sr).
fn convert_audio(
    input: &[f32],
    file_ch: usize,
    device_ch: usize,
    file_sr: u32,
    device_sr: u32,
) -> Vec<f32> {
    let file_ch = file_ch.max(1);
    let device_ch = device_ch.max(1);
    let frames_in = input.len() / file_ch;

    // Step 1: channel conversion (produce mono-ish or stereo from file channels)
    let mono_or_converted: Vec<f32> = if file_ch == device_ch {
        input.to_vec()
    } else if device_ch == 1 {
        // Mix all file channels down to mono
        (0..frames_in)
            .map(|f| {
                let sum: f32 = (0..file_ch).map(|c| input[f * file_ch + c]).sum();
                sum / file_ch as f32
            })
            .collect()
    } else if file_ch == 1 {
        // Duplicate mono to all device channels
        let mut out = Vec::with_capacity(frames_in * device_ch);
        for f in 0..frames_in {
            let s = input[f];
            for _ in 0..device_ch {
                out.push(s);
            }
        }
        out
    } else {
        // Generic: mix file channels to stereo (left = ch0, right = ch1, rest ignored)
        let mut out = Vec::with_capacity(frames_in * device_ch);
        for f in 0..frames_in {
            for dc in 0..device_ch {
                let fc = dc.min(file_ch - 1);
                out.push(input[f * file_ch + fc]);
            }
        }
        out
    };

    // Step 2: sample rate conversion via linear interpolation
    if file_sr == device_sr {
        return mono_or_converted;
    }
    let ratio = file_sr as f64 / device_sr as f64;
    let frames_out = (frames_in as f64 / ratio).ceil() as usize;
    let mut out = Vec::with_capacity(frames_out * device_ch);
    for i in 0..frames_out {
        let pos = i as f64 * ratio;
        let idx0 = pos as usize;
        let idx1 = (idx0 + 1).min(frames_in.saturating_sub(1));
        let frac = (pos - idx0 as f64) as f32;
        for c in 0..device_ch {
            let s0 = mono_or_converted[idx0 * device_ch + c];
            let s1 = mono_or_converted[idx1 * device_ch + c];
            out.push(s0 + (s1 - s0) * frac);
        }
    }
    out
}
