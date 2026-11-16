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
pub struct RenderScratch {
    bars: Vec<f32>,
    grid: Vec<u16>,
}

impl RenderScratch {
    pub fn new() -> Self {
        Self {
            bars: Vec::new(),
            grid: Vec::new(),
        }
    }
}

/// Map `bars` (band values) onto `n` screen columns, writing into `out`.
pub fn fill_sample_bars(bars: &[f32], n: usize, out: &mut Vec<f32>) {
    out.clear();
    out.resize(n, 0.0);
    for i in 0..n {
        out[i] = sample_at(
            bars,
            if n == 1 {
                0.0
            } else {
                i as f32 / (n - 1) as f32
            },
        );
    }
}

fn put(buf: &mut Buffer, x: u16, y: u16, ch: char, fg: Color) {
    if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
        cell.set_char(ch).set_fg(fg);
    }
}

/// Bottom-anchored spectrum bars with partial-block tops and peak caps.
///
/// `peaks` carries per-column peak-hold values across frames.
pub fn render_spectrum(
    buf: &mut Buffer,
    area: Rect,
    bars: &[f32],
    theme: &Theme,
    peaks: &mut Vec<f32>,
    scratch: &mut RenderScratch,
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

        // Peak-hold with slow decay.
        let peak = &mut peaks[x];
        *peak = (*peak - 0.02).max(v);

        let filled = v * h as f32;
        let full = filled.floor() as usize;
        let frac = filled - full as f32;

        for j in 0..full.min(h) {
            let y = area.y + (h - 1 - j) as u16;
            let t = (j + 1) as f32 / h as f32;
            put(buf, area.x + x as u16, y, BLOCKS[7], theme.at(t));
        }
        if full < h && frac > 0.06 {
            let y = area.y + (h - 1 - full) as u16;
            let t = (full + 1) as f32 / h as f32;
            let ch = BLOCKS[((frac * 8.0) as usize).min(7)];
            put(buf, area.x + x as u16, y, ch, theme.at(t));
        }

        // Peak cap marker.
        let peak = peaks[x];
        let prow = (peak * h as f32).round() as usize;
        if prow >= 1 && prow <= h {
            let y = area.y + (h - prow) as u16;
            put(buf, area.x + x as u16, y, '\u{2594}', theme.at(1.0));
        }
    }
}

/// Braille dot position -> bit in the Unicode braille pattern.
fn braille_bit(col: usize, row: usize) -> u16 {
    match (col, row) {
        (0, 0) => 0x01,
        (0, 1) => 0x02,
        (0, 2) => 0x04,
        (0, 3) => 0x80,
        (1, 0) => 0x08,
        (1, 1) => 0x10,
        (1, 2) => 0x20,
        (1, 3) => 0x40,
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

    // Heat coloring: brightness grows with distance from the centerline,
    // so loud excursions light up while quiet passages stay dim.
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
            put(
                buf,
                area.x + gx as u16,
                area.y + gy as u16,
                ch,
                theme.at(dist),
            );
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
                cell.set_bg(theme.at(v));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn braille_bit_table() {
        assert_eq!(braille_bit(0, 0), 0x01);
        assert_eq!(braille_bit(0, 1), 0x02);
        assert_eq!(braille_bit(0, 2), 0x04);
        assert_eq!(braille_bit(0, 3), 0x80);
        assert_eq!(braille_bit(1, 0), 0x08);
        assert_eq!(braille_bit(1, 1), 0x10);
        assert_eq!(braille_bit(1, 2), 0x20);
        assert_eq!(braille_bit(1, 3), 0x40);
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
        );
        render_spectrum(
            &mut buf,
            Rect::new(0, 0, 4, 3),
            &[],
            &theme,
            &mut Vec::new(),
            &mut scratch,
        );
        render_oscilloscope(&mut buf, Rect::new(0, 0, 4, 3), &[], &theme, 1.0, &mut scratch);
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

    #[test]
    fn spectrum_draws_in_bounds_only() {
        let theme = crate::theme::find("ice").unwrap();
        let area = Rect::new(0, 0, 5, 4);
        let mut buf = Buffer::empty(area);
        let bars = vec![1.0; 5];
        let mut peaks = Vec::new();
        let mut scratch = RenderScratch::new();
        render_spectrum(&mut buf, area, &bars, &theme, &mut peaks, &mut scratch);
        // Full-height bars fill the bottom row entirely.
        let filled = (0..5)
            .filter(|&x| buf[(x as u16, 3)].symbol() == "\u{2588}")
            .count();
        assert_eq!(filled, 5, "bottom row should be full blocks");
    }
}
