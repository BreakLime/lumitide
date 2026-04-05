use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use rustfft::{num_complex::Complex, FftPlanner};

pub const FFT_SIZE: usize = 2048;
pub const NUM_BARS: usize = 12;
pub const BAR_HEIGHT: usize = 5;
const PEAK_HOLD_FRAMES: u32 = 8;
const PEAK_FALL_SPEED: f32 = 0.025;

/// Static spectrum shown in calm mode — bass-heavy shape, never moves.
pub const CALM_SPECTRUM: [f32; NUM_BARS] =
    [0.70, 0.85, 0.95, 0.80, 0.65, 0.55, 0.48, 0.42, 0.38, 0.32, 0.26, 0.20];

/// Compute log-spaced band edges (indices into the FFT magnitude array).
pub fn compute_band_edges(sample_rate: u32) -> Vec<usize> {
    let bin_hz = sample_rate as f64 / FFT_SIZE as f64;
    let edges_hz: Vec<f64> = (0..=NUM_BARS)
        .map(|i| {
            let t = i as f64 / NUM_BARS as f64;
            let log_min = (60.0f64).log10();
            let log_max = (16000.0f64).log10();
            10f64.powf(log_min + t * (log_max - log_min))
        })
        .collect();

    let mut edges: Vec<usize> = edges_hz
        .iter()
        .map(|&hz| ((hz / bin_hz).round() as usize).clamp(1, FFT_SIZE / 2))
        .collect();

    // Ensure strictly increasing
    for i in 1..edges.len() {
        if edges[i] <= edges[i - 1] {
            edges[i] = edges[i - 1] + 1;
        }
    }
    edges
}

/// Compute normalised bar magnitudes [0,1] from a PCM ring buffer.
pub fn compute_spectrum(spec_buf: &[f32], band_edges: &[usize]) -> Vec<f32> {
    if spec_buf.is_empty() {
        return vec![0.0; NUM_BARS];
    }

    // Hann window + FFT
    let len = spec_buf.len();
    let mut buffer: Vec<Complex<f32>> = spec_buf
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let w = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (len - 1) as f32).cos());
            Complex::new(s * w, 0.0)
        })
        .collect();

    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(len);
    fft.process(&mut buffer);

    // Only positive frequencies
    let half = &buffer[1..len / 2];
    if half.is_empty() {
        return vec![0.0; NUM_BARS];
    }

    // Band magnitudes
    let mags: Vec<f32> = (0..NUM_BARS)
        .map(|i| {
            let s = band_edges[i].min(half.len());
            let e = band_edges[i + 1].min(half.len());
            if e > s {
                half[s..e].iter().map(|c| c.norm()).sum::<f32>() / (e - s) as f32
            } else {
                half[s.min(half.len() - 1)].norm()
            }
        })
        .collect();

    let peak = mags.iter().cloned().fold(0.0f32, f32::max).max(1e-6);
    mags.iter().map(|&m| (m / peak).min(1.0)).collect()
}

fn to_color(c: Option<(u8, u8, u8)>) -> Color {
    match c {
        Some((r, g, b)) => Color::Rgb(r, g, b),
        None => Color::White,
    }
}

/// Render the spectrum as BAR_HEIGHT ratatui Lines.
/// `bar_peaks` and `bar_peak_hold` are updated in place.
pub fn render_spectrum(
    normalized: &[f32],
    bar_peaks: &mut Vec<f32>,
    bar_peak_hold: &mut Vec<u32>,
    bar_color: Option<(u8, u8, u8)>,
) -> Vec<Line<'static>> {
    // Update peak-hold state
    for (i, &val) in normalized.iter().enumerate() {
        if i >= NUM_BARS { break; }
        if val >= bar_peaks[i] {
            bar_peaks[i] = val;
            bar_peak_hold[i] = 0;
        } else {
            bar_peak_hold[i] += 1;
            if bar_peak_hold[i] > PEAK_HOLD_FRAMES {
                bar_peaks[i] = (bar_peaks[i] - PEAK_FALL_SPEED).max(0.0);
            }
        }
    }

    // Highest row where each bar's peak renders
    let mut peak_row = vec![0usize; NUM_BARS];
    for i in 0..NUM_BARS {
        for row in (1..=BAR_HEIGHT).rev() {
            if bar_peaks[i] >= (row as f32 - 0.5) / BAR_HEIGHT as f32 {
                peak_row[i] = row;
                break;
            }
        }
    }

    let color = to_color(bar_color);
    let bar_style = Style::new().fg(color);

    let mut lines = Vec::new();
    for row in (1..=BAR_HEIGHT).rev() {
        let full_thresh = row as f32 / BAR_HEIGHT as f32;
        let half_thresh = (row as f32 - 0.5) / BAR_HEIGHT as f32;
        let mut spans: Vec<Span<'static>> = Vec::new();
        for i in 0..NUM_BARS {
            if i > 0 {
                spans.push(Span::raw(" "));
            }
            let val = if i < normalized.len() { normalized[i] } else { 0.0 };
            if val >= full_thresh {
                spans.push(Span::styled("██", bar_style));
            } else if val >= half_thresh {
                spans.push(Span::styled("▄▄", bar_style));
            } else if row == peak_row[i] {
                spans.push(Span::styled("▁▁", bar_style));
            } else {
                spans.push(Span::raw("  "));
            }
        }
        lines.push(Line::from(spans));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_edges_count_and_strictly_increasing() {
        let edges = compute_band_edges(44100);
        assert_eq!(edges.len(), NUM_BARS + 1);
        for i in 1..edges.len() {
            assert!(edges[i] > edges[i - 1], "edges not strictly increasing at {i}");
        }
    }

    #[test]
    fn band_edges_within_fft_bounds() {
        let edges = compute_band_edges(44100);
        for &e in &edges {
            assert!(e >= 1 && e <= FFT_SIZE / 2);
        }
    }

    #[test]
    fn compute_spectrum_empty_buffer_returns_zeros() {
        let edges = compute_band_edges(44100);
        let result = compute_spectrum(&[], &edges);
        assert_eq!(result.len(), NUM_BARS);
        assert!(result.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn compute_spectrum_silent_buffer_returns_valid_length() {
        let edges = compute_band_edges(44100);
        let buf = vec![0.0f32; FFT_SIZE];
        let result = compute_spectrum(&buf, &edges);
        assert_eq!(result.len(), NUM_BARS);
    }

    #[test]
    fn compute_spectrum_values_normalised_between_0_and_1() {
        let edges = compute_band_edges(44100);
        let buf: Vec<f32> = (0..FFT_SIZE)
            .map(|i| (i as f32 * 0.1).sin())
            .collect();
        let result = compute_spectrum(&buf, &edges);
        assert_eq!(result.len(), NUM_BARS);
        for &v in &result {
            assert!(v >= 0.0 && v <= 1.0, "value out of range: {v}");
        }
    }

    #[test]
    fn render_dl_progress_returns_bar_height_lines() {
        let lines = render_dl_progress(0, 0, None);
        assert_eq!(lines.len(), BAR_HEIGHT);
    }

    #[test]
    fn render_dl_progress_zero_total_shows_zero_filled() {
        let lines = render_dl_progress(0, 0, None);
        // bar line (index 2) should contain no filled blocks
        let bar_text: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!bar_text.contains('█'));
    }

    #[test]
    fn render_dl_progress_full_shows_full_bar() {
        let lines = render_dl_progress(1000, 1000, None);
        let bar_text: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!bar_text.contains('░'));
    }
}

/// Render download progress as BAR_HEIGHT ratatui Lines.
pub fn render_dl_progress(
    dl_bytes: u64,
    dl_total: u64,
    bar_color: Option<(u8, u8, u8)>,
) -> Vec<Line<'static>> {
    let bar_width: usize = 30;
    let ratio = if dl_total > 0 {
        (dl_bytes as f64 / dl_total as f64).min(1.0)
    } else {
        0.0
    };
    let filled = (ratio * bar_width as f64) as usize;
    let color = to_color(bar_color);
    let dim = Color::DarkGray;

    let bar = Line::from(vec![
        Span::styled("█".repeat(filled), Style::new().fg(color)),
        Span::styled("░".repeat(bar_width - filled), Style::new().fg(dim)),
    ]);

    let info_str = if dl_total > 0 {
        format!(
            "{:.1} MB / {:.1} MB  {}%",
            dl_bytes as f64 / 1_048_576.0,
            dl_total as f64 / 1_048_576.0,
            (ratio * 100.0) as u64
        )
    } else {
        format!("{:.1} MB", dl_bytes as f64 / 1_048_576.0)
    };

    vec![
        Line::raw(""),
        Line::styled("⬇ Downloading...", Style::new().fg(color)),
        bar,
        Line::styled(info_str, Style::new().fg(color)),
        Line::raw(""),
    ]
}
