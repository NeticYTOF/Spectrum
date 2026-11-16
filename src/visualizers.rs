//! Visualizers. Every mode paints on the theme's own canvas colour (so
//! inverted partial blocks and glows look the same in every terminal)
//! and draws its subcell detail with block elements and braille, which
//! VTE-class terminals render geometrically:
//!
//! - spectrum: separated bars, eighth-block anti-aliased tops, gravity
//!   peak caps, a true log-frequency axis;
//! - oscilloscope: a connected braille trace with time-based phosphor
//!   persistence over a quiet graticule;
//! - spectrogram: half-block cells (two frequency rows per cell, each
//!   with its own colour), frequency labels;
//! - radial: spectrum petals around a waveform ring;
//! - mirror: centre-out symmetric bars (bass in the middle), mirrored
//!   vertically with a dimmer reflection.
//!
//! All motion is time-based (`dt` in seconds); nothing depends on the
//! frame rate, and every renderer tolerates empty input and tiny areas.

use std::collections::VecDeque;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;

use crate::analysis::{MAX_FREQ, MIN_FREQ};
use crate::theme::Theme;

/// Lower partial blocks, 1/8 .. 8/8.
const BLOCKS: [char; 8] = [
    '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}',
];

/// Linear interpolation into `vals` at `t` in 0..=1. Panic-free.
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

/// Mean of `vals` over the span [t0, t1] (anti-aliased band sampling).
fn sample_span(vals: &[f32], t0: f32, t1: f32) -> f32 {
    let n = 6;
    let mut s = 0.0;
    for k in 0..n {
        let t = t0 + (t1 - t0) * (k as f32 + 0.5) / n as f32;
        s += sample_at(vals, t);
    }
    s / n as f32
}

/// Reusable per-mode buffers so rendering never allocates per frame.
#[derive(Default)]
pub struct RenderScratch {
    bars: Vec<f32>,
    /// Phosphor/heat per braille dot.
    heat: Vec<f32>,
    heat_w: usize,
    heat_h: usize,
}

impl RenderScratch {
    pub fn new() -> Self {
        Self::default()
    }

    fn heat_for(&mut self, w: usize, h: usize) -> &mut Vec<f32> {
        if self.heat_w != w || self.heat_h != h {
            self.heat_w = w;
            self.heat_h = h;
            self.heat.clear();
            self.heat.resize(w * h, 0.0);
        }
        &mut self.heat
    }
}

/// Resample `bars` to `n` columns (linear).
pub fn fill_sample_bars(bars: &[f32], n: usize, out: &mut Vec<f32>) {
    out.clear();
    out.resize(n, 0.0);
    let step = if n > 1 { 1.0 / (n - 1) as f32 } else { 0.0 };
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = sample_at(bars, i as f32 * step);
    }
}

fn cell(buf: &mut Buffer, x: u16, y: u16) -> Option<&mut ratatui::buffer::Cell> {
    buf.cell_mut(Position::new(x, y))
}

/// The canvas every mode paints on: the theme's floor colour.
pub fn canvas(theme: &Theme) -> Color {
    theme.at(0.0)
}

/// Fill `area` with the canvas colour.
pub fn paint_canvas(buf: &mut Buffer, area: Rect, theme: &Theme) {
    let bg = canvas(theme);
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            if let Some(c) = cell(buf, x, y) {
                c.set_char(' ').set_bg(bg);
            }
        }
    }
}

/// Band position (0..1) of a frequency on the analyzer's log scale.
pub fn freq_t(hz: f32) -> f32 {
    ((hz / MIN_FREQ).ln() / (MAX_FREQ / MIN_FREQ).ln()).clamp(0.0, 1.0)
}

const AXIS_LABELS: [(f32, &str); 9] = [
    (50.0, "50"),
    (100.0, "100"),
    (200.0, "200"),
    (500.0, "500"),
    (1000.0, "1k"),
    (2000.0, "2k"),
    (5000.0, "5k"),
    (10000.0, "10k"),
    (15000.0, "15k"),
];

fn put_str(buf: &mut Buffer, x: u16, y: u16, s: &str, fg: Color, bg: Color, max_x: u16) {
    for (i, ch) in s.chars().enumerate() {
        let xx = x + i as u16;
        if xx >= max_x {
            break;
        }
        if let Some(c) = cell(buf, xx, y) {
            c.set_char(ch).set_fg(fg).set_bg(bg);
        }
    }
}

// ---------------------------------------------------------------- peaks

/// Peak caps: hold, then fall under constant gravity. Position is solved
/// analytically from the time since the peak was set, so one long frame
/// and many short frames land in exactly the same place.
#[derive(Default, Clone)]
pub struct Peaks {
    top: Vec<f32>,
    since: Vec<f32>,
}

const PEAK_HOLD_S: f32 = 0.32;
const PEAK_GRAVITY: f32 = 3.2; // bar-heights per second^2

impl Peaks {
    pub fn new() -> Self {
        Self::default()
    }

    fn resize(&mut self, n: usize) {
        if self.top.len() != n {
            self.top = vec![0.0; n];
            self.since = vec![0.0; n];
        }
    }

    /// Advance bar `i` by `dt` against the current value `v`; returns the
    /// cap height.
    fn step(&mut self, i: usize, v: f32, dt: f32) -> f32 {
        self.since[i] += dt;
        let fall = (self.since[i] - PEAK_HOLD_S).max(0.0);
        let cur = self.top[i] - 0.5 * PEAK_GRAVITY * fall * fall;
        if v >= cur {
            self.top[i] = v;
            self.since[i] = 0.0;
            v
        } else {
            cur
        }
    }

    pub fn value(&self, i: usize) -> f32 {
        let fall = (self.since.get(i).copied().unwrap_or(0.0) - PEAK_HOLD_S).max(0.0);
        self.top.get(i).copied().unwrap_or(0.0) - 0.5 * PEAK_GRAVITY * fall * fall
    }
}

// ------------------------------------------------------------- spectrum

/// Bar slots for a width: (slot width, bar width, count, left offset).
fn bar_layout(w: usize) -> (usize, usize, usize, usize) {
    let (slot, bar) = if w >= 48 {
        (3, 2)
    } else if w >= 20 {
        (2, 1)
    } else {
        (1, 1)
    };
    let n = (w / slot).max(1);
    let used = n * slot - (slot - bar);
    (slot, bar, n, w.saturating_sub(used) / 2)
}

/// Spectrum analyzer: separated bars rising from the floor with eighth-
/// block tops (8x vertical resolution), a height gradient, gravity peak
/// caps and a true log-frequency axis.
pub fn render_spectrum(
    buf: &mut Buffer,
    area: Rect,
    bars: &[f32],
    theme: &Theme,
    peaks: &mut Peaks,
    scratch: &mut RenderScratch,
    dt: f32,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    paint_canvas(buf, area, theme);
    let bg = canvas(theme);
    let w = area.width as usize;
    let axis = area.height >= 12 && area.width >= 50;
    let h = area.height as usize - usize::from(axis);
    if h == 0 {
        return;
    }
    let (slot, bar_w, n, off) = bar_layout(w);
    peaks.resize(n);
    scratch.bars.clear();
    for i in 0..n {
        let t0 = i as f32 / n as f32;
        let t1 = (i + 1) as f32 / n as f32;
        scratch.bars.push(sample_span(bars, t0, t1).clamp(0.0, 1.0));
    }
    let eighths = h * 8;
    for i in 0..n {
        let v = scratch.bars[i];
        let cap = peaks.step(i, v, dt).clamp(0.0, 1.0);
        let fill = (v * eighths as f32).round() as usize;
        let x0 = area.x + (off + i * slot) as u16;
        for dx in 0..bar_w as u16 {
            let x = x0 + dx;
            if x >= area.x + area.width {
                continue;
            }
            for row in 0..h {
                let have = fill.saturating_sub(row * 8).min(8);
                if have == 0 {
                    break;
                }
                let y = area.y + (h - 1 - row) as u16;
                let t = (row as f32 + 0.5) / h as f32;
                let fg = theme.at_dither(0.2 + 0.8 * t.powf(0.9), x, y);
                if let Some(c) = cell(buf, x, y) {
                    c.set_char(BLOCKS[have - 1]).set_fg(fg).set_bg(bg);
                }
            }
            // Cap: a hairline at the held peak, only above the bar top.
            let cap_e = (cap * eighths as f32).round() as usize;
            if cap_e > fill + 2 && cap_e <= eighths {
                let row = (cap_e - 1) / 8;
                let e = (cap_e - 1) % 8;
                let y = area.y + (h - 1 - row) as u16;
                let ch = if e >= 4 { '\u{2594}' } else { '\u{2581}' };
                if let Some(c) = cell(buf, x, y) {
                    if c.symbol() == " " {
                        c.set_char(ch).set_fg(theme.at(0.92)).set_bg(bg);
                    }
                }
            }
        }
    }
    if axis {
        let y = area.y + area.height - 1;
        let span = (n * slot) as f32;
        let dim = theme.at(0.38);
        for (hz, label) in AXIS_LABELS {
            let x = off as f32 + freq_t(hz) * span - label.len() as f32 / 2.0;
            if x < 0.0 || x as usize + label.len() > w {
                continue;
            }
            put_str(
                buf,
                area.x + x as u16,
                y,
                label,
                dim,
                bg,
                area.x + area.width,
            );
        }
    }
}

// ----------------------------------------------------------- braille art

/// Braille dot position -> bit in the Unicode braille pattern. Dots 1-4
/// are the left column top-to-bottom except that dot 7 (bottom-left) is
/// 0x40 and dot 8 (bottom-right) is 0x80.
fn braille_bit(col: usize, row: usize) -> u16 {
    match (col, row) {
        (0, 0) => 0x01,
        (0, 1) => 0x02,
        (0, 2) => 0x04,
        (0, 3) => 0x40,
        (1, 0) => 0x08,
        (1, 1) => 0x10,
        (1, 2) => 0x20,
        (1, 3) => 0x80,
        _ => 0,
    }
}

/// Plot a dot into a heat field (max-combined).
#[inline]
fn plot(heat: &mut [f32], w: usize, h: usize, x: i64, y: i64, v: f32) {
    if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
        let k = y as usize * w + x as usize;
        if v > heat[k] {
            heat[k] = v;
        }
    }
}

/// Line between two dot positions (heat field), inclusive.
fn plot_line(heat: &mut [f32], w: usize, h: usize, a: (f32, f32), b: (f32, f32), v: f32) {
    let steps = ((b.0 - a.0).abs().max((b.1 - a.1).abs()).ceil() as usize).max(1);
    for s in 0..=steps {
        let u = s as f32 / steps as f32;
        let x = a.0 + (b.0 - a.0) * u;
        let y = a.1 + (b.1 - a.1) * u;
        plot(heat, w, h, x.round() as i64, y.round() as i64, v);
    }
}

/// Turn a dot heat field into braille cells; `color(heat)` picks a cell's
/// colour from its hottest dot. Dots below `floor` are off.
fn present_braille(
    buf: &mut Buffer,
    area: Rect,
    heat: &[f32],
    floor: f32,
    bg: Color,
    color: impl Fn(f32, u16, u16) -> Color,
) {
    let dw = area.width as usize * 2;
    for cy in 0..area.height as usize {
        for cx in 0..area.width as usize {
            let mut bits = 0u16;
            let mut hot = 0.0f32;
            for r in 0..4 {
                for c in 0..2 {
                    let k = (cy * 4 + r) * dw + cx * 2 + c;
                    let v = heat.get(k).copied().unwrap_or(0.0);
                    if v > floor {
                        bits |= braille_bit(c, r);
                        hot = hot.max(v);
                    }
                }
            }
            if bits == 0 {
                continue;
            }
            let (x, y) = (area.x + cx as u16, area.y + cy as u16);
            let ch = char::from_u32(0x2800 + bits as u32).unwrap_or('\u{28FF}');
            if let Some(c) = cell(buf, x, y) {
                // Keep a glow already painted behind the dots.
                if c.bg == Color::Reset {
                    c.set_bg(bg);
                }
                c.set_char(ch).set_fg(color(hot, x, y));
            }
        }
    }
}

// --------------------------------------------------------- oscilloscope

/// Phosphor decay time constant (seconds).
const PHOSPHOR_TAU: f32 = 0.09;

/// Oscilloscope: a connected braille trace with phosphor persistence over
/// a quiet graticule. Silence decays to a calm centre line.
pub fn render_oscilloscope(
    buf: &mut Buffer,
    area: Rect,
    waveform: &[f32],
    theme: &Theme,
    gain: f32,
    scratch: &mut RenderScratch,
    dt: f32,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    paint_canvas(buf, area, theme);
    let bg = canvas(theme);
    let (dw, dh) = (area.width as usize * 2, area.height as usize * 4);
    let heat = scratch.heat_for(dw, dh);
    let decay = (-dt.max(0.0) / PHOSPHOR_TAU).exp();
    for v in heat.iter_mut() {
        *v *= decay;
    }
    let mid = (dh as f32 - 1.0) / 2.0;
    let gain = gain.max(0.1);
    let thick = dh >= 24;
    if waveform.len() >= 2 {
        let last = (waveform.len() - 1) as f32;
        let mut prev: Option<(f32, f32)> = None;
        for i in 0..dw {
            let t = if dw > 1 {
                i as f32 / (dw - 1) as f32
            } else {
                0.0
            };
            let v = (sample_at(waveform, t) * gain).clamp(-1.0, 1.0);
            let _ = last;
            let y = mid - v * mid;
            let p = (i as f32, y);
            if let Some(q) = prev {
                plot_line(heat, dw, dh, q, p, 1.0);
                if thick {
                    plot_line(heat, dw, dh, (q.0, q.1 + 1.0), (p.0, p.1 + 1.0), 0.7);
                }
            } else {
                plot(heat, dw, dh, p.0 as i64, p.1.round() as i64, 1.0);
            }
            prev = Some(p);
        }
    }
    // Graticule: a dotted centre line, plus small division ticks on it
    // when there is room - quiet enough to read as a scope face, not as
    // texture. Drawn first; the trace covers it.
    if area.height >= 3 {
        let gy = area.y + area.height / 2;
        let div = area.width / 8;
        for x in area.x..area.x + area.width {
            let k = x - area.x;
            let tick = area.width >= 48 && div > 0 && k.is_multiple_of(div) && k > 0;
            let (ch, t) = if tick {
                ('\u{253C}', 0.22)
            } else if k.is_multiple_of(2) {
                ('\u{2508}', 0.16)
            } else {
                (' ', 0.0)
            };
            if ch != ' ' {
                if let Some(c) = cell(buf, x, gy) {
                    c.set_char(ch).set_fg(theme.at(t)).set_bg(bg);
                }
            }
        }
    }
    let heat = &scratch.heat;
    // Phosphor halo: cells around a lit trace get a faint glow behind the
    // dots, so the line has the weight of a real scope beam.
    let glow_c = theme.at(0.55);
    for cy in 0..area.height as usize {
        for cx in 0..area.width as usize {
            let mut hot = 0.0f32;
            for r in 0..4 {
                for c in 0..2 {
                    let k = (cy * 4 + r) * dw + cx * 2 + c;
                    hot = hot.max(heat.get(k).copied().unwrap_or(0.0));
                }
            }
            if hot > 0.06 {
                let (x, y) = (area.x + cx as u16, area.y + cy as u16);
                if let Some(c) = cell(buf, x, y) {
                    c.set_bg(crate::theme::lerp_color(bg, glow_c, 0.16 * hot.min(1.0)));
                }
            }
        }
    }
    present_braille(buf, area, heat, 0.06, bg, |h, x, y| {
        theme.at_dither(0.3 + 0.7 * h.clamp(0.0, 1.0).powf(0.7), x, y)
    });
}

// ---------------------------------------------------------------- radial

/// Quadrant block for a TL/TR/BL/BR on-mask (bit 0 = TL ... bit 3 = BR).
fn quadrant_char(bits: u8) -> char {
    match bits {
        0b0001 => '\u{2598}',
        0b0010 => '\u{259D}',
        0b0100 => '\u{2596}',
        0b1000 => '\u{2597}',
        0b0011 => '\u{2580}',
        0b1100 => '\u{2584}',
        0b0101 => '\u{258C}',
        0b1010 => '\u{2590}',
        0b1001 => '\u{259A}',
        0b0110 => '\u{259E}',
        0b0111 => '\u{259B}',
        0b1011 => '\u{259C}',
        0b1101 => '\u{2599}',
        0b1110 => '\u{259F}',
        _ => '\u{2588}',
    }
}

/// Radial: spectrum petals radiating from a waveform ring, mirrored left
/// to right (bass at the bottom, highs at the top), the ring breathing
/// gently with the spectral flux. Petals are solid quadrant-block wedges
/// (they need mass); the ring is fine braille line art over them.
pub fn render_radial(
    buf: &mut Buffer,
    area: Rect,
    bars: &[f32],
    waveform: &[f32],
    flux: f32,
    theme: &Theme,
    scratch: &mut RenderScratch,
) {
    if area.width < 4 || area.height < 2 {
        return;
    }
    paint_canvas(buf, area, theme);
    let bg = canvas(theme);
    let (dw, dh) = (area.width as usize * 2, area.height as usize * 4);
    let heat = scratch.heat_for(dw, dh);
    heat.iter_mut().for_each(|v| *v = 0.0);
    // Braille dots are square on a 1:2 cell, so circles stay round.
    let (cx, cy) = (dw as f32 / 2.0, dh as f32 / 2.0);
    let r_max = (dw as f32).min(dh as f32) * 0.48;
    let r0 = r_max * (0.34 + 0.03 * flux.clamp(0.0, 1.0));
    let tau = std::f32::consts::TAU;

    // Petals: a polar raster of solid wedges with thin angular gaps (a
    // sunburst), so they carry weight at any size instead of reading as
    // hairlines.
    let n = ((tau * r0) / 3.2).clamp(12.0, 96.0) as usize;
    let a = r0 * 1.14;
    let span = (r_max - a).max(1.0);
    let mut lengths = vec![0.0f32; n];
    for (k, len) in lengths.iter_mut().enumerate() {
        let th = tau * (k as f32 + 0.5) / n as f32;
        // Distance from the bottom (y down: bottom is th = pi/2).
        let mut d = (th - std::f32::consts::FRAC_PI_2).abs();
        if d > std::f32::consts::PI {
            d = tau - d;
        }
        let t = d / std::f32::consts::PI;
        let half = 0.5 / n as f32;
        *len = sample_span(bars, (t - half).max(0.0), (t + half).min(1.0)).clamp(0.0, 1.0) * span;
    }
    // Sample the petal field at quadrant centres (2x2 per cell; in dot
    // units each quadrant is 1 dot wide and 2 dots tall).
    let petal_heat = |px: f32, py: f32| -> f32 {
        let (dx, dy) = (px - cx, py - cy);
        let r = (dx * dx + dy * dy).sqrt();
        if r < a || r > a + span {
            return 0.0;
        }
        let th = dy.atan2(dx).rem_euclid(tau);
        let pos = th / tau * n as f32;
        if pos.fract() > 0.8 {
            return 0.0; // gap between petals
        }
        let len = lengths[(pos as usize).min(n - 1)];
        if r - a <= len {
            0.32 + 0.68 * ((r - a) / span).clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    for cyc in 0..area.height as usize {
        for cxc in 0..area.width as usize {
            let mut bits = 0u8;
            let (mut sum, mut cnt) = (0.0f32, 0.0f32);
            for q in 0..4usize {
                let (qx, qy) = (q % 2, q / 2);
                let px = (cxc * 2 + qx) as f32 + 0.5;
                let py = (cyc * 4 + qy * 2) as f32 + 1.0;
                let hq = petal_heat(px, py);
                if hq > 0.0 {
                    bits |= 1 << q;
                    sum += hq;
                    cnt += 1.0;
                }
            }
            if bits == 0 {
                continue;
            }
            let (x, y) = (area.x + cxc as u16, area.y + cyc as u16);
            let fg = theme.at_dither(sum / cnt, x, y);
            if let Some(c) = cell(buf, x, y) {
                c.set_char(quadrant_char(bits)).set_fg(fg).set_bg(bg);
            }
        }
    }
    // Waveform ring.
    let m = ((tau * r0) * 1.5).clamp(24.0, 720.0) as usize;
    let mut prev: Option<(f32, f32)> = None;
    let mut first: Option<(f32, f32)> = None;
    for k in 0..=m {
        let u = k as f32 / m as f32;
        let w = if waveform.is_empty() {
            0.0
        } else {
            sample_at(waveform, u).clamp(-1.0, 1.0)
        };
        let r = r0 * (1.0 + 0.16 * w);
        let th = tau * u;
        let p = (cx + th.cos() * r, cy + th.sin() * r);
        if let Some(q) = prev {
            plot_line(heat, dw, dh, q, p, 0.95);
            // A second, slightly outer pass gives the ring weight.
            let k = 1.0 + 1.2 / r.max(1.0);
            plot_line(
                heat,
                dw,
                dh,
                (cx + (q.0 - cx) * k, cy + (q.1 - cy) * k),
                (cx + (p.0 - cx) * k, cy + (p.1 - cy) * k),
                0.8,
            );
        }
        first.get_or_insert(p);
        prev = Some(p);
    }
    let heat = &scratch.heat;
    present_braille(buf, area, heat, 0.05, bg, |h, x, y| {
        theme.at_dither(h, x, y)
    });
}

// ---------------------------------------------------------------- mirror

/// Mirrored spectrum: bass in the centre, frequency rising toward both
/// edges, bars growing up and down from the centre line (the lower half a
/// dimmer reflection). Tips have eighth resolution in both directions:
/// downward tips use the inverted-block trick (canvas-coloured lower
/// block over a bar-coloured background).
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
    paint_canvas(buf, area, theme);
    let bg = canvas(theme);
    let w = area.width as usize;
    let h = area.height as usize;
    let up = h / 2;
    let down = h - up;
    let half_w = w as f32 / 2.0;
    scratch.bars.clear();
    for x in 0..w {
        let d0 = ((x as f32 - half_w).abs() / half_w).clamp(0.0, 1.0);
        let d1 = (((x + 1) as f32 - half_w).abs() / half_w).clamp(0.0, 1.0);
        let (a, b) = if d0 < d1 { (d0, d1) } else { (d1, d0) };
        scratch
            .bars
            .push(sample_span(bars, a, b.max(a + 1e-3)).clamp(0.0, 1.0));
    }
    let color = |dist: f32, x: u16, y: u16, reflect: bool| -> Color {
        let c = theme.at_dither(0.22 + 0.78 * dist.clamp(0.0, 1.0).powf(0.85), x, y);
        if reflect {
            crate::theme::lerp_color(c, bg, 0.38)
        } else {
            c
        }
    };
    for x in 0..w {
        let v = scratch.bars[x];
        let sx = area.x + x as u16;
        // Upper half grows upward from the centre.
        let fill = (v * (up * 8) as f32).round() as usize;
        for row in 0..up {
            let have = fill.saturating_sub(row * 8).min(8);
            if have == 0 {
                break;
            }
            let y = area.y + (up - 1 - row) as u16;
            let dist = (row as f32 + 0.5) / up.max(1) as f32;
            if let Some(c) = cell(buf, sx, y) {
                c.set_char(BLOCKS[have - 1])
                    .set_fg(color(dist, sx, y, false))
                    .set_bg(bg);
            }
        }
        // Lower half: the reflection, growing downward.
        let fill = (v * (down * 8) as f32).round() as usize;
        for row in 0..down {
            let have = fill.saturating_sub(row * 8).min(8);
            if have == 0 {
                break;
            }
            let y = area.y + (up + row) as u16;
            let dist = (row as f32 + 0.5) / down.max(1) as f32;
            let col = color(dist, sx, y, true);
            if let Some(c) = cell(buf, sx, y) {
                if have == 8 {
                    c.set_char('\u{2588}').set_fg(col).set_bg(bg);
                } else {
                    // Top `have` eighths in bar colour: a canvas-coloured
                    // lower block of (8 - have) eighths over a bar bg.
                    c.set_char(BLOCKS[7 - have]).set_fg(bg).set_bg(col);
                }
            }
        }
    }
    // A calm centre hairline keeps the composition anchored in silence.
    if up >= 1 {
        let y = area.y + up as u16 - 1;
        for x in area.x..area.x + area.width {
            if let Some(c) = cell(buf, x, y) {
                if c.symbol() == " " {
                    c.set_char('\u{2581}').set_fg(theme.at(0.22)).set_bg(bg);
                }
            }
        }
    }
}

// ----------------------------------------------------------- spectrogram

/// Scrolling spectrogram (waterfall): newest column at the right edge,
/// low frequencies at the bottom, two frequency rows per cell via an
/// upper-half block with independent fg/bg colours.
pub fn render_spectrogram(
    buf: &mut Buffer,
    area: Rect,
    columns: &VecDeque<Vec<f32>>,
    theme: &Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    paint_canvas(buf, area, theme);
    let bg = canvas(theme);
    let w = area.width as usize;
    let h = area.height as usize;
    let rows = h * 2;
    let level = |v: f32| v.clamp(0.0, 1.0).powf(0.85);
    for x in 0..w {
        let idx = columns.len() as isize - w as isize + x as isize;
        let Some(col) = usize::try_from(idx).ok().and_then(|i| columns.get(i)) else {
            continue;
        };
        for y in 0..h {
            let t_top = 1.0 - (2 * y) as f32 / (rows - 1).max(1) as f32;
            let t_bot = 1.0 - (2 * y + 1) as f32 / (rows - 1).max(1) as f32;
            let (vt, vb) = (level(sample_at(col, t_top)), level(sample_at(col, t_bot)));
            let (sx, sy) = (area.x + x as u16, area.y + y as u16);
            let ct = if vt < 0.04 {
                bg
            } else {
                theme.at_dither(vt, sx, sy)
            };
            let cb = if vb < 0.04 {
                bg
            } else {
                theme.at_dither(vb, sx, sy)
            };
            if let Some(c) = cell(buf, sx, sy) {
                c.set_char('\u{2580}').set_fg(ct).set_bg(cb);
            }
        }
    }
    if area.width >= 60 && area.height >= 10 {
        let dim = theme.at(0.5);
        for (hz, label) in [(100.0, "100"), (1000.0, "1k"), (10000.0, "10k")] {
            let t = freq_t(hz);
            let y = area.y + ((1.0 - t) * (h - 1) as f32).round() as u16;
            put_str(buf, area.x, y, label, dim, bg, area.x + area.width);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    #[test]
    fn peak_decay_is_time_based() {
        let theme = crate::theme::find("fire").unwrap();
        let area = Rect::new(0, 0, 2, 10);
        let mut peaks = Peaks::new();
        let mut scratch = RenderScratch::new();
        render_spectrum(
            &mut Buffer::empty(area),
            area,
            &[1.0, 1.0],
            &theme,
            &mut peaks,
            &mut scratch,
            DT,
        );
        assert!((peaks.value(0) - 1.0).abs() < 1e-6);
        // Many short frames and one long frame land in the same place.
        let zero = [0.0, 0.0];
        let mut p_short = peaks.clone();
        for _ in 0..48 {
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
            48.0 * DT,
        );
        assert!(
            (p_short.value(0) - p_long.value(0)).abs() < 1e-3,
            "decay must track wall-clock time: {} vs {}",
            p_short.value(0),
            p_long.value(0)
        );
        assert!(p_long.value(0) < 0.9, "peaks must visibly fall");
    }

    #[test]
    fn braille_bit_table() {
        assert_eq!(braille_bit(0, 0), 0x01);
        assert_eq!(braille_bit(0, 1), 0x02);
        assert_eq!(braille_bit(0, 2), 0x04);
        assert_eq!(braille_bit(0, 3), 0x40, "dot 7 is bottom-LEFT");
        assert_eq!(braille_bit(1, 0), 0x08);
        assert_eq!(braille_bit(1, 1), 0x10);
        assert_eq!(braille_bit(1, 2), 0x20);
        assert_eq!(braille_bit(1, 3), 0x80, "dot 8 is bottom-RIGHT");
    }

    #[test]
    fn oscilloscope_uses_true_unicode_dots() {
        // A 1x1 cell: the left sample at -1 (bottom-left dot 7, 0x40) and
        // the right one at +1 (top-right dot 4, 0x08) must both be set,
        // and the glyph must be a real braille codepoint.
        let theme = crate::theme::find("fire").unwrap();
        let area = Rect::new(0, 0, 1, 1);
        let mut buf = Buffer::empty(area);
        let mut scratch = RenderScratch::new();
        render_oscilloscope(&mut buf, area, &[-1.0, 1.0], &theme, 1.0, &mut scratch, DT);
        let ch = buf[(0, 0)].symbol().chars().next().unwrap();
        let bits = ch as u32 - 0x2800;
        assert!(('\u{2800}'..='\u{28FF}').contains(&ch), "got {ch:?}");
        assert_eq!(bits & 0x48, 0x48, "dots 7 and 4 must be set: {bits:#x}");
    }

    #[test]
    fn oscilloscope_trace_is_continuous() {
        // A steep wave must not leave gaps between columns.
        let theme = crate::theme::find("ice").unwrap();
        let area = Rect::new(0, 0, 20, 8);
        let mut buf = Buffer::empty(area);
        let mut scratch = RenderScratch::new();
        let wave: Vec<f32> = (0..64).map(|i| ((i as f32) * 0.6).sin()).collect();
        render_oscilloscope(&mut buf, area, &wave, &theme, 1.0, &mut scratch, DT);
        for x in 0..20u16 {
            let has = (0..8u16).any(|y| {
                let s = buf[(x, y)].symbol();
                s.chars()
                    .next()
                    .is_some_and(|c| ('\u{2801}'..='\u{28FF}').contains(&c))
            });
            assert!(has, "column {x} has no trace");
        }
    }

    #[test]
    fn phosphor_fades_with_time_not_frames() {
        let theme = crate::theme::find("ice").unwrap();
        let area = Rect::new(0, 0, 8, 4);
        let wave = vec![0.5; 32];
        let mut a = RenderScratch::new();
        let mut b = RenderScratch::new();
        render_oscilloscope(
            &mut Buffer::empty(area),
            area,
            &wave,
            &theme,
            1.0,
            &mut a,
            DT,
        );
        render_oscilloscope(
            &mut Buffer::empty(area),
            area,
            &wave,
            &theme,
            1.0,
            &mut b,
            DT,
        );
        for _ in 0..10 {
            render_oscilloscope(&mut Buffer::empty(area), area, &[], &theme, 1.0, &mut a, DT);
        }
        render_oscilloscope(
            &mut Buffer::empty(area),
            area,
            &[],
            &theme,
            1.0,
            &mut b,
            10.0 * DT,
        );
        let sa: f32 = a.heat.iter().sum();
        let sb: f32 = b.heat.iter().sum();
        assert!((sa - sb).abs() < 1e-3, "{sa} vs {sb}");
    }

    #[test]
    fn sample_bars_interpolates() {
        let mut v = Vec::new();
        fill_sample_bars(&[0.0, 1.0], 5, &mut v);
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
    }

    #[test]
    fn mirror_is_symmetric_and_centre_out() {
        // Bass loud, highs quiet: the centre columns are tall, the edges
        // short, and the left/right halves mirror each other.
        let theme = crate::theme::find("spectrum").unwrap();
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);
        let bars: Vec<f32> = (0..64).map(|i| 1.0 - i as f32 / 63.0).collect();
        render_mirror(&mut buf, area, &bars, &theme, &mut RenderScratch::new());
        let height = |x: u16| (0..12u16).filter(|&y| buf[(x, y)].symbol() != " ").count();
        assert!(height(20) > height(1), "centre must be taller than edge");
        for x in 0..20u16 {
            assert_eq!(height(x), height(39 - x), "asymmetric at {x}");
        }
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
        let mut s = RenderScratch::new();
        let mut p = Peaks::new();
        render_spectrum(
            &mut buf,
            Rect::new(0, 0, 0, 0),
            &[0.5],
            &theme,
            &mut p,
            &mut s,
            DT,
        );
        render_spectrum(
            &mut buf,
            Rect::new(0, 0, 4, 3),
            &[],
            &theme,
            &mut p,
            &mut s,
            DT,
        );
        render_oscilloscope(
            &mut buf,
            Rect::new(0, 0, 4, 3),
            &[],
            &theme,
            1.0,
            &mut s,
            DT,
        );
        render_oscilloscope(
            &mut buf,
            Rect::new(0, 0, 4, 3),
            &[0.0, 0.5, -0.5, 0.2],
            &theme,
            1.0,
            &mut s,
            DT,
        );
        render_radial(
            &mut buf,
            Rect::new(0, 0, 4, 3),
            &[],
            &[],
            0.0,
            &theme,
            &mut s,
        );
        render_radial(
            &mut buf,
            Rect::new(0, 0, 1, 1),
            &[0.5],
            &[0.1],
            1.0,
            &theme,
            &mut s,
        );
        let cols: VecDeque<Vec<f32>> = VecDeque::new();
        render_spectrogram(&mut buf, Rect::new(0, 0, 4, 3), &cols, &theme);
    }

    #[test]
    fn spectrum_draws_in_bounds_with_axis() {
        let theme = crate::theme::find("ice").unwrap();
        let area = Rect::new(0, 0, 60, 14);
        let mut buf = Buffer::empty(area);
        render_spectrum(
            &mut buf,
            area,
            &[1.0; 64],
            &theme,
            &mut Peaks::new(),
            &mut RenderScratch::new(),
            DT,
        );
        // Full bars reach the top; the bottom row is the frequency axis.
        assert!((0..60u16).any(|x| buf[(x, 0)].symbol() == "\u{2588}"));
        let axis: String = (0..60u16)
            .map(|x| buf[(x, 13)].symbol().to_string())
            .collect();
        assert!(axis.contains("1k"), "axis labels missing: {axis:?}");
    }

    #[test]
    fn spectrogram_uses_two_rows_per_cell() {
        let theme = crate::theme::find("spectrum").unwrap();
        let area = Rect::new(0, 0, 4, 3);
        let mut buf = Buffer::empty(area);
        let mut cols = VecDeque::new();
        cols.push_back((0..64).map(|i| i as f32 / 63.0).collect::<Vec<f32>>());
        render_spectrogram(&mut buf, area, &cols, &theme);
        let c = &buf[(3, 1)];
        assert_eq!(c.symbol(), "\u{2580}");
        assert_ne!(c.fg, c.bg, "top and bottom halves carry distinct rows");
    }

    #[test]
    fn radial_draws_a_round_ring_and_petals() {
        let theme = crate::theme::find("vapor").unwrap();
        let area = Rect::new(0, 0, 60, 24);
        let mut buf = Buffer::empty(area);
        let wave = vec![0.0; 64];
        render_radial(
            &mut buf,
            area,
            &[0.8; 64],
            &wave,
            0.0,
            &theme,
            &mut RenderScratch::new(),
        );
        // Braille ring plus quadrant-block petals.
        let lit = |ch: char| {
            ('\u{2801}'..='\u{28FF}').contains(&ch) || ('\u{2580}'..='\u{259F}').contains(&ch)
        };
        let n = buf
            .content()
            .iter()
            .filter(|c| c.symbol().chars().next().is_some_and(lit))
            .count();
        assert!(n > 100, "radial drew only {n} cells");
        // Rough symmetry: as many lit cells left of centre as right.
        let side = |r: std::ops::Range<u16>| -> usize {
            r.map(|x| (0..24u16).filter(|&y| buf[(x, y)].symbol() != " ").count())
                .sum()
        };
        let (l, rr) = (side(0..30), side(30..60));
        assert!(
            (l as i64 - rr as i64).abs() < (l.max(rr) / 5) as i64 + 4,
            "{l} vs {rr}"
        );
    }

    #[test]
    fn freq_axis_positions_follow_the_analyzer_scale() {
        assert!(freq_t(MIN_FREQ) < 1e-6);
        assert!((freq_t(MAX_FREQ) - 1.0).abs() < 1e-6);
        assert!(freq_t(1000.0) > freq_t(500.0));
    }
}
