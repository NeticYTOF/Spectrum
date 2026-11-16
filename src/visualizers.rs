//! Terminal renderers: spectrum bars, braille oscilloscope, spectrogram.
//!
//! Every renderer is defensive: empty input, zero-sized areas or missing
//! data never panic; they just draw nothing.

use std::collections::VecDeque;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;

use crate::theme::Theme;

/// Partial vertical block characters, bottom-eighth .. full block.
const BLOCKS: [char; 8] = [
    '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}',
];

/// Linear interpolation into `vals` treated as a 0..=1 range.
pub fn sample_at(vals: &[f32], t: f32) -> f32 {
    if vals.is_empty() {
        return 0.0;
    }
    if vals.len() == 1 {
        return vals[0];
    }
    let x = t.clamp(0.0, 1.0) * (vals.len() - 1) as f32;
    let i = x.floor() as usize;
    let f = x - i as f32;
    let j = (i + 1).min(vals.len() - 1);
    vals[i] * (1.0 - f) + vals[j] * f
}

/// Reusable per-render scratch space: keeps the 60 fps render path free
/// of per-frame allocations.
#[derive(Default)]
pub struct RenderScratch {
    bars: Vec<f32>,
    grid: Vec<u16>,
}

impl RenderScratch {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Map `bars` (band values) onto `n` screen columns, writing into `out`.
pub fn fill_sample_bars(bars: &[f32], n: usize, out: &mut Vec<f32>) {
    out.clear();
    out.resize(n, 0.0);
    let step = if n > 1 { 1.0 / (n - 1) as f32 } else { 0.0 };
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = sample_at(bars, i as f32 * step);
    }
}

fn put(buf: &mut Buffer, x: u16, y: u16, ch: char, fg: Color) {
    if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
        cell.set_char(ch).set_fg(fg);
    }
}

/// Peak-cap fall speed, in fraction of the bar area per second.
const PEAK_FALL_PER_SEC: f32 = 1.2;

/// Shade cells from sparse to solid: blank, light, medium, dark, full.
const SHADES: [char; 5] = [' ', '\u{2591}', '\u{2592}', '\u{2593}', '\u{2588}'];

/// Sub-cell halftone: map a coverage in 0..=1 to a shade glyph, ordered-
/// dithered between density levels by the same 4x4 Bayer pattern the
/// color dithering uses. A pure function of (coverage, x, y): temporally
/// stable, no per-frame noise, and together with the terminal's own dot
/// structure inside \u{2591..2593} this yields a fine stipple instead of
/// full-cell color checks.
fn shade_glyph(coverage: f32, x: u16, y: u16) -> char {
    let c = coverage.clamp(0.0, 1.0) * 4.0;
    let base = c.floor();
    let frac = c - base;
    let up = if frac > crate::theme::bayer_threshold(x, y) {
        1.0
    } else {
        0.0
    };
    SHADES[((base + up) as usize).min(4)]
}

/// Halftone color for a bar body cell: the gradient at a floored
/// intensity, so sparse stipples still read against the background
/// (never darker than ~30% of the ramp) while loud tops glow.
fn body_color_t(t: f32) -> f32 {
    0.3 + 0.7 * t.clamp(0.0, 1.0)
}
/// Coverage for a bar body cell: density rises with height, floored so
/// body cells are never blank.
fn body_coverage(t: f32) -> f32 {
    0.35 + 0.65 * t.clamp(0.0, 1.0)
}

/// Bottom-anchored spectrum bars with partial-block tops and peak caps.
///
/// `peaks` carries per-column peak-hold values across frames; `dt` is the
/// frame time in seconds so the peak decay is frame-rate independent.
pub fn render_spectrum(
    buf: &mut Buffer,
    area: Rect,
    bars: &[f32],
    theme: &Theme,
    peaks: &mut Vec<f32>,
    scratch: &mut RenderScratch,
    dt: f32,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let w = area.width as usize;
    let h = area.height as usize;
    fill_sample_bars(bars, w, &mut scratch.bars);
    if peaks.len() != w {
        peaks.clear();
        peaks.resize(w, 0.0);
    }
    let vals = &scratch.bars;

    for x in 0..w {
        let v = vals[x].clamp(0.0, 1.0);

        // Peak-hold with time-based decay (frame-rate independent).
        let peak = &mut peaks[x];
        *peak = (*peak - PEAK_FALL_PER_SEC * dt).max(v);

        let filled = v * h as f32;
        let full = filled.floor() as usize;
        let frac = filled - full as f32;

        for j in 0..full.min(h) {
            let y = area.y + (h - 1 - j) as u16;
            let t = (j + 1) as f32 / h as f32;
            let x = area.x + x as u16;
            // Halftone body: ordered-dithered shade glyph (fine stipple)
            // with the gradient color at a floored intensity.
            let glyph = shade_glyph(body_coverage(t), x, y);
            put(buf, x, y, glyph, theme.at_dither(body_color_t(t), x, y));
        }
        if full < h && frac > 0.06 {
            let y = area.y + (h - 1 - full) as u16;
            let t = (full + 1) as f32 / h as f32;
            let x = area.x + x as u16;
            let ch = BLOCKS[((frac * 8.0) as usize).min(7)];
            put(buf, x, y, ch, theme.at_dither(t, x, y));
        }

        // Peak cap marker: only when the held peak sits strictly above the
        // current bar top, so it marks receding peaks without clipping the
        // bar's own tip.
        let peak = peaks[x];
        let prow = (peak * h as f32).round() as usize;
        if prow > full.min(h) && prow <= h {
            let y = area.y + (h - prow) as u16;
            let x = area.x + x as u16;
            put(buf, x, y, '\u{2594}', theme.at_dither(1.0, x, y));
        }
    }
}

/// Braille dot position -> bit in the Unicode braille pattern.
///
/// Dots are numbered 1..8 per the Unicode standard:
/// 1-4 left column top-to-bottom, 5-8 right column, then bits follow
/// dot order: dot 7 (bottom-left) is 0x40, dot 8 (bottom-right) is 0x80.
fn braille_bit(col: usize, row: usize) -> u16 {
    match (col, row) {
        (0, 0) => 0x01, // dot 1
        (0, 1) => 0x02, // dot 2
        (0, 2) => 0x04, // dot 3
        (0, 3) => 0x40, // dot 7 (bottom-left)
        (1, 0) => 0x08, // dot 4
        (1, 1) => 0x10, // dot 5
        (1, 2) => 0x20, // dot 6
        (1, 3) => 0x80, // dot 8 (bottom-right)
        _ => 0,
    }
}

/// Waveform oscilloscope rendered with braille dots (2x4 resolution per cell).
pub fn render_oscilloscope(
    buf: &mut Buffer,
    area: Rect,
    waveform: &[f32],
    theme: &Theme,
    gain: f32,
    scratch: &mut RenderScratch,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if waveform.len() < 4 {
        return;
    }
    let w = area.width as usize;
    let h = area.height as usize;
    let sub_cols = w * 2;
    let sub_rows = h * 4;

    let grid = &mut scratch.grid;
    grid.resize(w * h, 0);
    grid.fill(0);
    let last = waveform.len() - 1;
    let gain = gain.max(0.1);

    for i in 0..sub_cols {
        let t = i as f32 / (sub_cols - 1) as f32;
        let idx = (t * last as f32).round() as usize;
        let v = (waveform[idx] * gain).clamp(-1.0, 1.0);
        // Map -1..=1 to top..bottom of the braille sub-grid.
        let sy = ((1.0 - (v + 1.0) * 0.5) * (sub_rows - 1) as f32).round() as usize;
        let gx = i / 2;
        let gy = sy / 4;
        grid[gy * w + gx] |= braille_bit(i % 2, sy % 4);
    }

    // Trace coloring: bright at the centerline where the waveform lives
    // and hottest at loud excursions - the trace must read clearly over
    // the bars behind it, never fading into the background.
    let center = (sub_rows - 1) as f32 * 0.5;
    for gy in 0..h {
        for gx in 0..w {
            let bits = grid[gy * w + gx];
            if bits == 0 {
                continue;
            }
            let ch = char::from_u32(0x2800 + bits as u32).unwrap_or('\u{28FF}');
            let sub_y = gy as f32 * 4.0 + 1.5;
            let dist = ((sub_y - center) / center.max(1.0)).abs().min(1.0);
            let heat = (0.55 + 0.45 * dist).min(1.0);
            put(
                buf,
                area.x + gx as u16,
                area.y + gy as u16,
                ch,
                theme.at_dither(heat, area.x + gx as u16, area.y + gy as u16),
            );
        }
    }
}

/// Mirrored spectrum: bars grow outward from a horizontal center line,
/// the top half rendered upside-down relative to the bottom half.
pub fn render_mirror(
    buf: &mut Buffer,
    area: Rect,
    bars: &[f32],
    theme: &Theme,
    scratch: &mut RenderScratch,
) {
    if area.width == 0 || area.height < 2 {
        return;
    }
    let w = area.width as usize;
    let h = area.height as usize;
    fill_sample_bars(bars, w, &mut scratch.bars);
    let half = h / 2;
    let bottom = h - half;

    for x in 0..w {
        let v = scratch.bars[x].clamp(0.0, 1.0);
        let filled = v * half as f32;
        let full = filled.floor() as usize;
        let frac = filled - full as f32;
        let sx = area.x + x as u16;

        // Top half: grows upward from the center line; the partial tip
        // uses bottom-filled blocks like ordinary bars.
        for j in 0..full.min(half) {
            let y = area.y + (half - 1 - j) as u16;
            let t = (j + 1) as f32 / half as f32;
            let glyph = shade_glyph(body_coverage(t), sx, y);
            put(buf, sx, y, glyph, theme.at_dither(body_color_t(t), sx, y));
        }
        if full < half && frac > 0.06 {
            let y = area.y + (half - 1 - full) as u16;
            let t = (full + 1) as f32 / half as f32;
            let ch = BLOCKS[((frac * 8.0) as usize).min(7)];
            put(buf, sx, y, ch, theme.at_dither(t, sx, y));
        }

        // Bottom half: grows downward from the center line; the partial
        // tip (which visually hangs from the block above it) uses an
        // upper-half block when more than half filled.
        for j in 0..full.min(bottom) {
            let y = area.y + (half + j) as u16;
            let t = (j + 1) as f32 / half as f32;
            let glyph = shade_glyph(body_coverage(t), sx, y);
            put(buf, sx, y, glyph, theme.at_dither(body_color_t(t), sx, y));
        }
        if full < bottom && frac > 0.5 {
            let y = area.y + (half + full) as u16;
            let t = (full + 1) as f32 / half as f32;
            put(buf, sx, y, '\u{2580}', theme.at_dither(t, sx, y));
        }
    }
}

/// Scrolling spectrogram: rows = log-spaced frequency bands, columns = time.
/// The newest column is drawn flush against the right edge.
pub fn render_spectrogram(
    buf: &mut Buffer,
    area: Rect,
    columns: &VecDeque<Vec<f32>>,
    theme: &Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let w = area.width as usize;
    let h = area.height as usize;

    for x in 0..w {
        let idx = columns.len() as isize - w as isize + x as isize;
        let idx = match usize::try_from(idx) {
            Ok(i) if i < columns.len() => i,
            _ => continue,
        };
        let col = match columns.get(idx) {
            Some(c) => c,
            None => continue,
        };
        for y in 0..h {
            // Bottom row = low frequencies.
            let t = if h == 1 {
                0.0
            } else {
                (h - 1 - y) as f32 / (h - 1) as f32
            };
            let v = sample_at(col, t);
            if v < 0.03 {
                continue;
            }
            if let Some(cell) = buf.cell_mut(Position::new(area.x + x as u16, area.y + y as u16)) {
                cell.set_bg(theme.at_dither(v, area.x + x as u16, area.y + y as u16));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_decay_is_time_based() {
        let theme = crate::theme::find("fire").unwrap();
        let area = Rect::new(0, 0, 2, 10);
        let bars = vec![1.0, 1.0];

        // Two frames at 60 fps advance peaks by ~2/60 s of fall.
        let mut peaks = Vec::new();
        let mut scratch = RenderScratch::new();
        let mut buf = Buffer::empty(area);
        render_spectrum(&mut buf, area, &bars, &theme, &mut peaks, &mut scratch, DT);
        // Bar still at 1.0 keeps the peak pinned at the top.
        assert_eq!(peaks[0], 1.0);

        // Bar drops to zero; decay must depend on elapsed time, not the
        // number of calls: one long frame equals many short ones.
        let zero = vec![0.0, 0.0];
        let mut p_short = peaks.clone();
        for _ in 0..6 {
            render_spectrum(
                &mut Buffer::empty(area),
                area,
                &zero,
                &theme,
                &mut p_short,
                &mut scratch,
                DT,
            );
        }
        let mut p_long = peaks.clone();
        render_spectrum(
            &mut Buffer::empty(area),
            area,
            &zero,
            &theme,
            &mut p_long,
            &mut scratch,
            6.0 * DT,
        );
        assert!(
            (p_short[0] - p_long[0]).abs() < 1e-3,
            "decay must track wall-clock time"
        );
        assert!(p_long[0] < 0.9, "peaks must visibly fall");
    }

    #[test]
    fn braille_bit_table() {
        // Unicode braille bits: dots 1-8 map to 0x01,0x02,0x04,0x40 (left
        // column) and 0x08,0x10,0x20,0x80 (right column).
        assert_eq!(braille_bit(0, 0), 0x01);
        assert_eq!(braille_bit(0, 1), 0x02);
        assert_eq!(braille_bit(0, 2), 0x04);
        assert_eq!(braille_bit(0, 3), 0x40, "dot 7 is bottom-LEFT");
        assert_eq!(braille_bit(1, 0), 0x08);
        assert_eq!(braille_bit(1, 1), 0x10);
        assert_eq!(braille_bit(1, 2), 0x20);
        assert_eq!(braille_bit(1, 3), 0x80, "dot 8 is bottom-RIGHT");
    }

    /// Regression: rendered braille characters must match the real
    /// Unicode codepoints for the dot positions involved.
    #[test]
    fn oscilloscope_renders_true_unicode_dots() {
        let theme = crate::theme::find("fire").unwrap();
        let area = Rect::new(0, 0, 1, 1);
        let mut buf = Buffer::empty(area);
        let mut scratch = RenderScratch::new();
        // 2 sub-columns: left sample dips to -1 (bottom-left dot 7,
        // U+2840 alone), right sample peaks to +1 (top-right dot 4).
        let wave = [-1.0, 1.0, 1.0, 1.0];
        render_oscilloscope(&mut buf, area, &wave, &theme, 1.0, &mut scratch);
        assert_eq!(
            buf[(0, 0)].symbol(),
            "\u{2848}",
            "must combine dot 7 (0x40) and dot 4 (0x08), not the swap"
        );

        // Bottom row only: constant -1 -> both bottom dots 7+8 = U+28C0.
        let mut buf2 = Buffer::empty(area);
        let wave = [-1.0, -1.0, -1.0, -1.0];
        render_oscilloscope(&mut buf2, area, &wave, &theme, 1.0, &mut scratch);
        assert_eq!(buf2[(0, 0)].symbol(), "\u{28C0}");
    }

    #[test]
    fn sample_bars_interpolates() {
        let bars = vec![0.0, 1.0];
        let mut v = Vec::new();
        fill_sample_bars(&bars, 5, &mut v);
        assert_eq!(v.len(), 5);
        assert_eq!(v[0], 0.0);
        assert!((v[2] - 0.5).abs() < 1e-6);
        assert_eq!(v[4], 1.0);
    }

    #[test]
    fn sample_at_handles_degenerate() {
        assert_eq!(sample_at(&[], 0.5), 0.0);
        assert_eq!(sample_at(&[0.7], 0.9), 0.7);
        let mut v = Vec::new();
        fill_sample_bars(&[0.5], 0, &mut v);
        assert!(v.is_empty());
        fill_sample_bars(&[0.5], 1, &mut v);
        assert_eq!(v[0], 0.5);
    }

    #[test]
    fn mirror_renders_outward_from_center() {
        let theme = crate::theme::find("fire").unwrap();
        let area = Rect::new(0, 0, 4, 6);
        let mut buf = Buffer::empty(area);
        let bars = vec![1.0, 1.0, 1.0, 1.0]; // full height both halves
        let mut scratch = RenderScratch::new();
        render_mirror(&mut buf, area, &bars, &theme, &mut scratch);

        // Full-height bars: the outermost rows of each half are solid;
        // center-adjacent rows are airy halftone but never blank.
        assert_eq!(buf[(0, 0)].symbol(), "\u{2588}", "outer top is solid");
        assert_eq!(buf[(0, 5)].symbol(), "\u{2588}", "outer bottom is solid");
        for y in 1..5 {
            assert_ne!(
                buf[(0, y)].symbol(),
                " ",
                "body cells must never be blank (row {y})"
            );
        }
        assert!(
            (1..5).any(|y| buf[(0, y)].symbol() != "\u{2588}"),
            "halftone density must vary across the halves"
        );
    }

    #[test]
    fn mirror_tolerates_degenerate_input() {
        let theme = crate::theme::find("ice").unwrap();
        let mut scratch = RenderScratch::new();
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 3));
        render_mirror(
            &mut buf,
            Rect::new(0, 0, 4, 0),
            &[0.5],
            &theme,
            &mut scratch,
        );
        render_mirror(
            &mut buf,
            Rect::new(0, 0, 4, 1),
            &[0.5],
            &theme,
            &mut scratch,
        );
        render_mirror(&mut buf, Rect::new(0, 0, 4, 3), &[], &theme, &mut scratch);
    }

    #[test]
    fn renderers_tolerate_empty_and_tiny() {
        let theme = crate::theme::find("fire").unwrap();
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 3));
        let mut scratch = RenderScratch::new();

        // Zero-sized and near-empty inputs must not panic.
        render_spectrum(
            &mut buf,
            Rect::new(0, 0, 0, 0),
            &[0.5],
            &theme,
            &mut Vec::new(),
            &mut scratch,
            DT,
        );
        render_spectrum(
            &mut buf,
            Rect::new(0, 0, 4, 3),
            &[],
            &theme,
            &mut Vec::new(),
            &mut scratch,
            DT,
        );
        render_oscilloscope(
            &mut buf,
            Rect::new(0, 0, 4, 3),
            &[],
            &theme,
            1.0,
            &mut scratch,
        );
        render_oscilloscope(
            &mut buf,
            Rect::new(0, 0, 4, 3),
            &[0.0, 0.5, -0.5, 0.2],
            &theme,
            1.0,
            &mut scratch,
        );
        let cols: VecDeque<Vec<f32>> = VecDeque::new();
        render_spectrogram(&mut buf, Rect::new(0, 0, 4, 3), &cols, &theme);
    }

    /// Nominal 60 fps frame time used by renderer tests.
    const DT: f32 = 1.0 / 60.0;

    #[test]
    fn spectrum_draws_in_bounds_only() {
        let theme = crate::theme::find("ice").unwrap();
        let area = Rect::new(0, 0, 5, 4);
        let mut buf = Buffer::empty(area);
        let bars = vec![1.0; 5];
        let mut peaks = Vec::new();
        let mut scratch = RenderScratch::new();
        render_spectrum(&mut buf, area, &bars, &theme, &mut peaks, &mut scratch, DT);
        // Full-height bars: the top row is solid, lower rows are airy
        // halftone but never blank.
        let solid = (0..5)
            .filter(|&x| buf[(x as u16, 0)].symbol() == "\u{2588}")
            .count();
        assert_eq!(solid, 5, "top row should be full blocks");
        for y in 1..4 {
            for x in 0..5u16 {
                assert_ne!(buf[(x, y)].symbol(), " ", "body cell must never be blank");
            }
        }
    }

    #[test]
    fn shade_glyph_is_deterministic_and_graduated() {
        // Pure function of (coverage, x, y): identical repeated calls.
        for y in 0..6u16 {
            for x in 0..6u16 {
                assert_eq!(shade_glyph(0.6, x, y), shade_glyph(0.6, x, y));
            }
        }
        // Extremes: 0 -> blank, 1 -> solid everywhere.
        for y in 0..4u16 {
            for x in 0..4u16 {
                assert_eq!(shade_glyph(0.0, x, y), ' ');
                assert_eq!(shade_glyph(1.0, x, y), '\u{2588}');
            }
        }
        // Ordered stipple: a mid coverage produces more than one density
        // level across the Bayer pattern.
        let mut levels = std::collections::HashSet::new();
        for y in 0..4u16 {
            for x in 0..4u16 {
                levels.insert(shade_glyph(0.55, x, y));
            }
        }
        assert!(levels.len() >= 2, "no stipple: {levels:?}");
        // Body coverage floor keeps bar interiors non-blank.
        assert!(body_coverage(0.0) >= 0.35);
    }
}
