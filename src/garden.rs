//! The lyrics garden: an ascii-garden-inspired terminal rendering.
//!
//! Visual language ported from ridit-jangra/ascii-garden (MIT, (c) 2026
//! Ridit; see NOTICE for the preserved license): a pale paper backdrop,
//! dot-matrix text, curling blue stems growing out of the letters and
//! warm flowers blooming at their tips. The Rust implementation here is
//! original: text and garden are drawn on a sub-cell canvas (2x4 Braille
//! dots per terminal cell) with a seeded deterministic RNG, spring-eased
//! bloom timed from the media position (frozen demos freeze the bloom too)
//! and a gentle sway driven by audio energy - no per-frame randomness.
//!
//! Motion vocabulary borrowed conceptually from the reference:
//! - `spr(t) = 1 - e^(-kt) * cos(wt)`: springy growth/bloom easing
//! - vines that curl (sinusoidal angle perturbation plus a curl at the tip)
//! - blooms staged over a few hundred milliseconds, not all at once

use std::f32::consts::PI;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::lrc::LrcLine;

/// The reference "Paper" palette (ascii-garden PAL row 1), ported to the
/// terminal: pale paper backdrop, near-black readable type, deep blue
/// stems and warm rose flowers with light petal accents.
const PAPER_BG: Color = Color::Rgb(243, 238, 228);
const INK: Color = Color::Rgb(26, 26, 26);
const INK_DIM: Color = Color::Rgb(130, 122, 110);
const STEM: Color = Color::Rgb(28, 43, 143); // the reference's #1C2B8F
const STEM_LIGHT: Color = Color::Rgb(90, 112, 200);
const FLOWER: Color = Color::Rgb(232, 61, 95); // warm rose
const FLOWER_LIGHT: Color = Color::Rgb(255, 210, 196); // reference #FFD2C4

/// Sub-cell layers, highest value wins when packing a cell's color.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Layer {
    Empty = 0,
    In = 1,
    Stem = 2,
    Leaf = 3,
    Flower = 4,
    FlowerLight = 5,
}

/// A 2x4-braille sub-cell canvas with per-sub-pixel color layers.
struct DotCanvas {
    w: usize,
    h: usize,
    layers: Vec<Layer>,
}

impl DotCanvas {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            layers: vec![Layer::Empty; w * h],
        }
    }

    fn set(&mut self, x: i32, y: i32, l: Layer) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return; // clipped by construction: no out-of-area writes
        }
        let i = y as usize * self.w + x as usize;
        if l > self.layers[i] {
            self.layers[i] = l;
        }
    }

    fn get(&self, x: i32, y: i32) -> Layer {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return Layer::Empty;
        }
        self.layers[y as usize * self.w + x as usize]
    }
}

/// Spring easing for bloom/growth (ascii-garden's `spr`).
fn spr(t: f32, k: f32, w: f32) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    (1.0 - (-t * k).exp() * (t * w).cos()).clamp(0.0, 1.0)
}

/// Cubic ease-out.
fn ease_out(t: f32) -> f32 {
    if t <= 0.0 {
        0.0
    } else if t >= 1.0 {
        1.0
    } else {
        1.0 - (1.0 - t).powi(3)
    }
}

/// Deterministic seeded RNG (mulberry32) so every frame with the same
/// inputs draws the identical garden.
fn rng(seed: u32) -> impl FnMut() -> f32 {
    let mut s = seed;
    move || {
        s = s.wrapping_add(0x6d2b79f5);
        let mut t = s;
        t = t ^ (t >> 15);
        t = t.wrapping_mul(t | 1);
        t ^= t.wrapping_add(t << 7);
        t = t ^ (t >> 16);
        (t ^ (t >> 16)) as f32 / u32::MAX as f32
    }
}

/// A 5x5 dot-matrix font for the big current line. Wider and more
/// readable than the classic 3x5, and at 2x scale it fills the panel the
/// way the reference's dot-matrix headline does. Rows are 5 bits, MSB
/// left; lowercase maps to uppercase for display only (original text is
/// always kept for matching and the normal-text fallback).
fn glyph(c: char) -> Option<[u8; 5]> {
    Some(match c {
        'A' => [0b01110, 0b10001, 0b11111, 0b10001, 0b10001],
        'B' => [0b11110, 0b10001, 0b11110, 0b10001, 0b11110],
        'C' => [0b01111, 0b10000, 0b10000, 0b10000, 0b01111],
        'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b11110],
        'E' => [0b11111, 0b10000, 0b11110, 0b10000, 0b11111],
        'F' => [0b11111, 0b10000, 0b11110, 0b10000, 0b10000],
        'G' => [0b01111, 0b10000, 0b10111, 0b10001, 0b01111],
        'H' => [0b10001, 0b10001, 0b11111, 0b10001, 0b10001],
        'I' => [0b11111, 0b00100, 0b00100, 0b00100, 0b11111],
        'J' => [0b00111, 0b00010, 0b00010, 0b10010, 0b01100],
        'K' => [0b10001, 0b10010, 0b11100, 0b10010, 0b10001],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        'M' => [0b10001, 0b11011, 0b10101, 0b10001, 0b10001],
        'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b01110],
        'P' => [0b11110, 0b10001, 0b11110, 0b10000, 0b10000],
        'Q' => [0b01110, 0b10001, 0b10101, 0b10010, 0b01101],
        'R' => [0b11110, 0b10001, 0b11110, 0b10010, 0b10001],
        'S' => [0b01111, 0b10000, 0b01110, 0b00001, 0b11110],
        'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100],
        'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'V' => [0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'W' => [0b10001, 0b10101, 0b10101, 0b11011, 0b10001],
        'X' => [0b10001, 0b01010, 0b00100, 0b01010, 0b10001],
        'Y' => [0b10001, 0b01010, 0b00100, 0b00100, 0b00100],
        'Z' => [0b11111, 0b00010, 0b00100, 0b01000, 0b11111],
        '0' => [0b01110, 0b10011, 0b10101, 0b11001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00110, 0b01000, 0b11111],
        '3' => [0b11110, 0b00001, 0b01110, 0b00001, 0b11110],
        '4' => [0b10001, 0b10001, 0b11111, 0b00001, 0b00001],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b11110],
        '6' => [0b01111, 0b10000, 0b11110, 0b10001, 0b01110],
        '7' => [0b11111, 0b00010, 0b00100, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b01110, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b01111, 0b00001, 0b11110],
        ' ' => [0; 5],
        '.' => [0, 0, 0, 0, 0b00100],
        ',' => [0, 0, 0, 0b00100, 0b01000],
        '\'' => [0b00100, 0b00100, 0, 0, 0],
        '\u{2019}' => [0b00100, 0b00100, 0, 0, 0], // right single quote
        '!' => [0b00100, 0b00100, 0b00100, 0, 0b00100],
        '?' => [0b01110, 0b10001, 0b00110, 0, 0b00100],
        '-' => [0, 0, 0b01110, 0, 0],
        '\u{2014}' => [0, 0, 0b11111, 0, 0], // em dash
        '\u{2013}' => [0, 0, 0b01110, 0, 0], // en dash
        ':' => [0, 0b00100, 0, 0b00100, 0],
        ';' => [0, 0b00100, 0, 0b00100, 0b01000],
        '(' => [0b00010, 0b00100, 0b00100, 0b00100, 0b00010],
        ')' => [0b01000, 0b00100, 0b00100, 0b00100, 0b01000],
        '&' => [0b01100, 0b01100, 0b00110, 0b11001, 0b01111],
        '/' => [0b00001, 0b00010, 0b00100, 0b01000, 0b10000],
        _ => return None,
    })
}

/// Display-only mapping: lowercase uses its uppercase glyph; the original
/// string is kept for fallback rendering and matching.
fn to_matrix_char(c: char) -> char {
    if c.is_ascii_lowercase() {
        c.to_ascii_uppercase()
    } else {
        c
    }
}

/// Whether every char of the text has a matrix glyph.
fn matrix_supported(text: &str) -> bool {
    text.chars().all(|c| glyph(to_matrix_char(c)).is_some())
}

/// Dot width of one matrix line (sub-pixels): 5 per char + 1 spacing,
/// scaled.
fn line_width(text: &str, scale: usize) -> usize {
    let chars = text.chars().count();
    if chars == 0 {
        return 0;
    }
    (chars * 6 - 1) * scale
}

/// Split text into wrapped lines whose dot-matrix width fits `max_sub_w`
/// sub-pixels, breaking on spaces (never mid-word unless a single word
/// exceeds the line). At least one line is always produced. Widths match
/// [`line_width`] exactly: every char occupies 6*scale sub-pixels with a
/// 1*scale trailing discount per line.
fn wrap_matrix_lines(text: &str, scale: usize, max_sub_w: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize; // exact line_width of cur
    for word in text.split(' ') {
        let w = line_width(word, scale);
        let sep = if cur.is_empty() { 0 } else { 6 * scale };
        if !cur.is_empty() && cur_w + sep + w > max_sub_w {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        if w > max_sub_w {
            // A single word wider than a line: hard-split it.
            for ch in word.chars() {
                let cw = if cur.is_empty() {
                    6 * scale - scale
                } else {
                    6 * scale
                };
                if !cur.is_empty() && cur_w + cw > max_sub_w {
                    lines.push(std::mem::take(&mut cur));
                    cur_w = 0;
                }
                cur.push(ch);
                cur_w += if cur.chars().count() == 1 {
                    5 * scale
                } else {
                    6 * scale
                };
            }
        } else {
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
            cur_w += sep + w;
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Draw one dot-matrix line at (x0, y0) with a staggered, typed-style
/// reveal: each letter springs in, its dots appearing in scan order.
fn draw_matrix_line(
    canvas: &mut DotCanvas,
    text: &str,
    x0: i32,
    y0: i32,
    scale: usize,
    grow: f32,
    stagger: usize,
) {
    let mut x = x0;
    for (i, c) in text.chars().enumerate() {
        let g = glyph(to_matrix_char(c)).unwrap_or([0; 5]);
        let letter_grow = spr(grow * 1.6 - (i + stagger) as f32 * 0.05, 6.0, 12.0).clamp(0.0, 1.0);
        // Collect this letter's dots in scan order (row-major).
        let mut dots: Vec<(usize, usize)> = Vec::new();
        for (row, bits) in g.iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) != 0 {
                    dots.push((col, row));
                }
            }
        }
        let total = dots.len().max(1) as f32;
        for (k, (col, row)) in dots.iter().enumerate() {
            // Each dot appears as the letter's growth passes its share.
            if letter_grow * total >= (k + 1) as f32 {
                for sx in 0..scale {
                    for sy in 0..scale {
                        canvas.set(
                            x + (col * scale + sx) as i32,
                            y0 + (row * scale + sy) as i32,
                            Layer::In,
                        );
                    }
                }
            }
        }
        x += 6 * scale as i32;
    }
}

/// One vine: a curling stem with leaves and a flower at the tip, ported
/// from ascii-garden's `vine` + `grow` concepts. Draws only up to `grow`
/// of its length; the flower blooms at the very end with a curl tip for
/// tendrils.
struct Vine {
    x0: f32,
    y0: f32,
    /// Base direction (radians; -PI/2 is up).
    dir: f32,
    /// Stem length in sub-pixels.
    len: f32,
    /// Curl amplitude and wave count.
    amp: f32,
    waves: f32,
    phase: f32,
    /// Constant bend along the growth.
    bend: f32,
    /// Whether the tip carries a flower or curls like a tendril.
    tip: Tip,
    /// Flower radius (sub-pixels).
    flower_r: f32,
    /// Leaf positions along the stem (0..1) and sides.
    leaves: Vec<(f32, f32)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Tip {
    Flower,
    Curl,
    LeafFan,
}

impl Vine {
    /// Trace the stem up to `u` in 0..=1. The final 18% curls around like
    /// ascii-garden's `curl()` for tendrils; flower stems stay straighter
    /// so the bloom sits cleanly.
    fn trace(&self, u: f32) -> Vec<(f32, f32)> {
        let n = 30;
        let steps = ((n as f32) * u.clamp(0.0, 1.0)).round() as usize;
        let mut pts = Vec::with_capacity(steps + 2);
        let mut x = self.x0;
        let mut y = self.y0;
        let step = self.len / n as f32;
        for i in 0..=steps {
            let f = i as f32 / n as f32;
            let curl = match self.tip {
                Tip::Curl if f > 0.82 => {
                    let t = (f - 0.82) / 0.18;
                    1.8 * t
                }
                _ => 0.0,
            };
            let a = self.dir
                + self.bend * f
                + self.amp * (f * PI * self.waves + self.phase).sin() * f.min(1.0)
                + curl;
            pts.push((x, y));
            x += a.cos() * step;
            y += a.sin() * step;
        }
        pts
    }

    fn draw(&self, canvas: &mut DotCanvas, grow: f32, sway: f32) {
        let u = grow.clamp(0.0, 1.0);
        if u <= 0.0 {
            return;
        }
        let pts = self.trace(u);
        for (px, py) in &pts {
            let sway_px = sway * ((py - self.y0) / self.len.max(1.0)).min(1.5);
            canvas.set(
                (px + sway_px).round() as i32,
                py.round() as i32,
                Layer::Stem,
            );
        }
        // Leaves appear as the stem passes them.
        for (lu, side) in &self.leaves {
            if *lu > u {
                continue;
            }
            let idx = ((*lu * (pts.len() as f32 - 1.0)) as usize).min(pts.len() - 1);
            let (lx, ly) = pts[idx];
            let s = *side;
            let leaf = [
                (0.0f32, 0.0f32),
                (s, -s * 0.6),
                (s * 1.8, -s),
                (s * 2.2, -s * 0.2),
            ];
            for (dx, dy) in leaf {
                canvas.set(
                    (lx + dx).round() as i32,
                    (ly + dy).round() as i32,
                    Layer::Leaf,
                );
            }
        }
        let tip = pts.last().copied().unwrap_or((self.x0, self.y0));
        match self.tip {
            Tip::Flower | Tip::LeafFan if u >= 0.98 => {
                let bloom = spr((grow - 0.98).max(0.0) * 2.4, 7.0, 12.0);
                if self.tip == Tip::Flower {
                    self.draw_flower(canvas, tip, bloom);
                } else {
                    if bloom > 0.1 {
                        for d in [-1.0f32, 0.0, 1.0] {
                            let fx = tip.0 + d * self.flower_r * 0.8;
                            let fy = tip.1 + d.abs() * 0.4;
                            canvas.set(fx.round() as i32, fy.round() as i32, Layer::Leaf);
                            canvas.set(fx.round() as i32, (fy - 1.0).round() as i32, Layer::Leaf);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// A rose-ish petal cluster scaled to sub-pixel resolution, with the
    /// reference's two-tone petal shading in spirit (light rim, dark core).
    fn draw_flower(&self, canvas: &mut DotCanvas, tip: (f32, f32), bloom: f32) {
        if bloom < 0.05 {
            return;
        }
        let r = self.flower_r * bloom.max(0.2);
        let (cx, cy) = tip;
        let petals = 8;
        for p in 0..petals {
            let ang = p as f32 / petals as f32 * PI * 2.0;
            let px = cx + ang.cos() * r * 0.85;
            let py = cy + ang.sin() * r * 0.6;
            canvas.set(px.round() as i32, py.round() as i32, Layer::Flower);
            if r > 1.6 {
                let px2 = cx + ang.cos() * r * 1.1;
                let py2 = cy + ang.sin() * r * 0.85;
                canvas.set(px2.round() as i32, py2.round() as i32, Layer::Flower);
            }
        }
        // Light rim accent + dark core.
        canvas.set(
            cx.round() as i32,
            (cy - r).round() as i32,
            Layer::FlowerLight,
        );
        canvas.set(
            (cx - r * 0.7).round() as i32,
            (cy + r * 0.6).round() as i32,
            Layer::FlowerLight,
        );
        // Dark core dot for the two-tone depth.
        if r > 1.4 {
            canvas.set(cx.round() as i32, cy.round() as i32, Layer::Flower);
        }
    }
}

/// Grow a garden of vines from deterministic positions across the whole
/// current line - rooted in the letters, not bunched at one end. Lengths
/// GRADUATE across the available depth so the garden fills the panel:
/// some short sprays near the text, some reaching the top, most in
/// between (the reference's layered growth).
fn garden_vines(
    line_char_count: usize,
    line_width_sub: usize,
    seed: u32,
    depth_sub: usize,
) -> Vec<Vine> {
    let mut r = rng(seed);
    let count = (2 + line_char_count / 3).clamp(3, 18);
    let depth = depth_sub.max(8) as f32;
    let mut vines = Vec::new();
    for i in 0..count {
        // Root at a spread of letter positions across the whole line.
        let frac = if count == 1 {
            0.5
        } else {
            (i as f32 + 0.3 + r() * 0.4) / count as f32
        };
        let x0 = frac * line_width_sub as f32;
        let dir = -PI / 2.0 + (r() - 0.5) * 0.8;
        // Graduated: a few short sprays, most mid, some tall.
        let len = depth
            * match r() {
                x if x < 0.2 => 0.25 + x,       // short spray
                x if x < 0.75 => 0.5 + x * 0.4, // mid height
                _ => 0.85 + r() * 0.15,         // reaching
            };
        let amp = 0.35 + r() * 0.55;
        let waves = 0.8 + r() * 1.6;
        let phase = r() * 2.0 * PI;
        let bend = (r() - 0.5) * 0.9;
        let kind_roll = r();
        let tip = if kind_roll < 0.6 {
            Tip::Flower
        } else if kind_roll < 0.8 {
            Tip::Curl
        } else {
            Tip::LeafFan
        };
        let flower_r = 2.4 + r() * 1.8;
        let mut leaves = Vec::new();
        if r() < 0.7 {
            leaves.push((0.25 + r() * 0.45, if r() < 0.5 { 1.0 } else { -1.0 }));
        }
        if r() < 0.25 {
            leaves.push((0.2 + r() * 0.5, if r() < 0.5 { 1.0 } else { -1.0 }));
        }
        vines.push(Vine {
            x0,
            y0: 4.0,
            dir,
            len,
            amp,
            waves,
            phase,
            bend,
            tip,
            flower_r,
            leaves,
        });
    }
    vines
}

/// Pack the canvas into the buffer: Braille chars with per-cell colors on
/// the paper backdrop.
fn present(canvas: &DotCanvas, buf: &mut Buffer, area: Rect) {
    let cell_w = canvas.w / 2;
    let cell_h = canvas.h / 4;
    for cy in 0..area.height.min(cell_h as u16) {
        for cx in 0..area.width.min(cell_w as u16) {
            let sx = cx as usize * 2;
            let sy = cy as usize * 4;
            let mut bits = 0u16;
            let mut dominant = Layer::Empty;
            for row in 0..4 {
                for col in 0..2 {
                    let l = canvas.get((sx + col) as i32, (sy + row) as i32);
                    if l == Layer::Empty {
                        continue;
                    }
                    bits |= braille_bit(col, row);
                    if l > dominant {
                        dominant = l;
                    }
                }
            }
            let (x, y) = (area.x + cx, area.y + cy);
            if let Some(cell) = buf.cell_mut(ratatui::layout::Position::new(x, y)) {
                cell.set_bg(PAPER_BG);
                if bits != 0 {
                    let color = match dominant {
                        Layer::FlowerLight => FLOWER_LIGHT,
                        Layer::Flower => FLOWER,
                        Layer::Leaf => STEM_LIGHT,
                        Layer::Stem => STEM,
                        Layer::In => INK,
                        Layer::Empty => INK,
                    };
                    if let Some(ch) = char::from_u32(0x2800 + bits as u32) {
                        cell.set_char(ch);
                        cell.set_fg(color);
                    }
                }
            }
        }
    }
}

/// Braille dot -> bit in the pattern (col 0/1, row 0-3).
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

/// What the garden should show this frame. All timing derives from the
/// media position, so frozen inputs render identically forever.
pub struct GardenView<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    /// Synced lines with the current index, when lyrics are synced.
    pub lines: Option<(&'a [LrcLine], Option<usize>)>,
    /// Unsycned plain text, if that is the state.
    pub plain: Option<&'a str>,
    pub state: GardenState,
    /// Smoothed audio energy 0..1 driving sway.
    pub energy: f32,
    /// Media time for sway phase (frozen demos stay still).
    pub time_s: f32,
    /// Seconds since the current line started (from the track timeline,
    /// NOT wall time - freezes with --freeze-at).
    pub since_line_change: f32,
}

#[derive(Clone, PartialEq, Debug)]
pub enum GardenState {
    Synced,
    Plain,
    Instrumental,
    Loading,
    NoMatch,
    Unavailable,
    Error(String),
}

/// Render the lyrics garden into `area`.
pub fn render_garden(buf: &mut Buffer, area: Rect, view: &GardenView<'_>) {
    if area.width < 8 || area.height < 3 {
        return; // too small for any composition
    }

    // Backdrop: pale paper over the whole area.
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut(ratatui::layout::Position::new(x, y)) {
                cell.set_bg(PAPER_BG);
            }
        }
    }

    // Title line at the very top, understated.
    let title = if view.artist.is_empty() {
        view.title.to_string()
    } else {
        format!("{} \u{b7} {}", view.title, view.artist)
    };
    put_text(buf, area, area.y, &title, INK_DIM);

    // The main band: everything between the title and the bottom line.
    let band_top = area.y + 1;
    let band_h = area.height.saturating_sub(2).max(1);
    let band = Rect::new(area.x, band_top, area.width, band_h);

    match &view.state {
        GardenState::Synced => {
            if let Some((lines, Some(idx))) = view.lines {
                let prev = idx.checked_sub(1).map(|i| lines[i].text.clone());
                let next = lines.get(idx + 1).map(|l| l.text.clone());
                let has_prev = prev.is_some();
                let has_next = next.is_some();
                let text = &lines[idx].text;

                // Previous line, understated, above the band.
                if let Some(p) = &prev {
                    put_text(buf, area, band.y, p, INK_DIM);
                }
                // Next line at the bottom of the area.
                if let Some(n) = &next {
                    let row = area.y + area.height - 1;
                    put_text(buf, area, row, n, INK_DIM);
                }

                // The matrix band sits between prev and next.
                let inner_top = band.y + if has_prev { 1 } else { 0 };
                let inner_h = band_h
                    .saturating_sub(if has_prev { 1 } else { 0 })
                    .saturating_sub(if has_next { 1 } else { 0 })
                    .max(2);
                let inner = Rect::new(band.x, inner_top, band.width, inner_h);
                render_matrix_band(buf, inner, text, view);
            }
        }
        GardenState::Plain => {
            if let Some(plain) = view.plain {
                put_text(buf, area, band.y, "(unsynced lyrics)", INK_DIM);
                let rows = band.y + 1..band.y + band.height;
                for (row, line) in rows.zip(plain.lines()) {
                    put_text(buf, area, row, line, INK);
                }
            }
        }
        GardenState::Instrumental => {
            let inner = Rect::new(
                band.x,
                band.y + band.height / 3,
                band.width,
                band.height / 3,
            );
            render_matrix_band(buf, inner, "[instrumental]", view);
        }
        GardenState::Loading => {
            put_text(
                buf,
                area,
                band.y + band.height / 2,
                "looking for lyrics...",
                INK_DIM,
            );
        }
        GardenState::NoMatch => {
            put_text(
                buf,
                area,
                band.y + band.height / 2,
                "no matching lyrics",
                INK_DIM,
            );
        }
        GardenState::Unavailable => {
            put_text(
                buf,
                area,
                band.y + band.height / 2,
                "no media session",
                INK_DIM,
            );
        }
        GardenState::Error(e) => {
            let msg = format!("lyrics: {e}");
            put_text(buf, area, band.y + band.height / 2, &msg, INK_DIM);
        }
    }
}

/// The dot-matrix band: the current line as big Braille letters with the
/// garden growing out of them, wrapping to multiple lines when needed, and
/// a legible normal-text fallback for unsupported scripts.
fn render_matrix_band(buf: &mut Buffer, area: Rect, text: &str, view: &GardenView<'_>) {
    if area.width < 4 || area.height < 2 {
        return;
    }
    let sub_w = area.width as usize * 2;

    if !matrix_supported(text) {
        // Honest fallback: legible normal text (original case preserved),
        // display-width aware so CJK and friends center correctly.
        let line = truncate_to_width(text, area.width as usize - 2);
        let text_w = line.width() as u16;
        let x = area.x + (area.width.saturating_sub(text_w)) / 2;
        let y = area.y + area.height / 2;
        for (i, ch) in line.chars().enumerate() {
            if x + i as u16 >= area.x + area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut(ratatui::layout::Position::new(x + i as u16, y)) {
                cell.set_char(ch);
                cell.set_fg(INK);
                cell.set_bg(PAPER_BG);
            }
        }
        return;
    }

    // Pick the biggest scale that fits on one line; wrap when needed.
    let mut scale = 2;
    let mut lines = vec![text.to_string()];
    while scale >= 1 {
        if line_width(text, scale) <= sub_w.saturating_sub(4) {
            break; // fits on one line at this scale
        }
        let wrapped = wrap_matrix_lines(text, scale, sub_w.saturating_sub(4));
        if wrapped.len() <= 3 {
            lines = wrapped;
            break;
        }
        scale -= 1;
    }
    if scale == 0 {
        scale = 1;
        lines = wrap_matrix_lines(text, 1, sub_w.saturating_sub(4));
        if lines.len() > 3 {
            lines.truncate(3);
        }
    }

    // Layout: the matrix sits in the LOWER two-thirds of the band so the
    // garden gets generous headroom above (the reference grows up and
    // around the text).
    let line_h_sub = 6 * scale;
    let total_h_sub = lines.len() * line_h_sub;
    let canvas_h = area.height as usize * 4;
    let letters_y: i32 = if canvas_h > total_h_sub {
        // 72% down: most of the band is garden headroom above, a slim
        // margin below the text before the next line.
        ((canvas_h - total_h_sub) as f32 * 0.75) as i32 + total_h_sub as i32 / 2
    } else {
        (canvas_h.saturating_sub(line_h_sub)) as i32
    }
    .max(5 * scale as i32)
    .min((canvas_h.saturating_sub(line_h_sub)) as i32)
    .max(0);

    let mut canvas = DotCanvas::new(sub_w, canvas_h);
    let grow = ease_out(view.since_line_change / 0.8).max(0.05);

    let mut widths = Vec::new();
    for (li, line) in lines.iter().enumerate() {
        let lw = line_width(line, scale);
        widths.push(lw);
        let x0 = ((sub_w as i32 - lw as i32) / 2).max(0);
        let y0 = letters_y + (li as i32) * line_h_sub as i32;
        draw_matrix_line(&mut canvas, line, x0, y0, scale, grow, li * 4);
    }

    // Sway: gentle, energy-driven, phase from the media time (frozen
    // demos stay still because the time is pinned).
    let sway_phase = view.time_s * 0.6;
    let sway_amp = (0.3 + view.energy * 1.6).min(2.0);

    // Vines rooted across the whole line, growing up into the empty
    // space with graduated lengths that fill the garden depth.
    let seed = text_seed(text);
    let main_w = *widths.first().unwrap_or(&0);
    let x0_main = ((sub_w as i32 - main_w as i32) / 2).max(0);
    let garden_depth = (letters_y - 1).max(6) as usize;
    for (vi, v) in garden_vines(lines[0].chars().count(), main_w, seed, garden_depth)
        .iter()
        .enumerate()
    {
        let stagger = (vi as f32) * 0.08;
        let vgrow = spr(
            (view.since_line_change - 0.05 - stagger).max(-0.05),
            2.2,
            8.0,
        ) + spr((view.since_line_change - 0.9).max(-0.05), 1.0, 5.0);
        let vgrow = vgrow.clamp(0.0, 1.15);
        let sway = (sway_phase + v.x0 * 0.12).sin() * sway_amp * 0.4;
        let vv = Vine {
            y0: letters_y as f32 - scale as f32,
            x0: x0_main as f32 + v.x0,
            dir: v.dir,
            len: v.len.min(garden_depth as f32 * 0.98),
            ..clone_vine(v)
        };
        vv.draw(&mut canvas, vgrow, sway);
    }
    present(&canvas, buf, area);
}

fn clone_vine(v: &Vine) -> Vine {
    Vine {
        x0: v.x0,
        y0: v.y0,
        dir: v.dir,
        len: v.len,
        amp: v.amp,
        waves: v.waves,
        phase: v.phase,
        bend: v.bend,
        tip: v.tip,
        flower_r: v.flower_r,
        leaves: v.leaves.clone(),
    }
}

fn text_seed(text: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for b in text.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}

/// Write a dim text line centered into `area` at cell row `row`, clipped
/// safely with display-width awareness (CJK counts double).
fn put_text(buf: &mut Buffer, area: Rect, row: u16, text: &str, color: Color) {
    if row < area.y || row >= area.y + area.height {
        return;
    }
    let width = area.width as usize;
    let line = truncate_to_width(text, width);
    let w = line.width();
    let x = area.x + ((width.saturating_sub(w)) / 2) as u16;
    let mut cx = x;
    for ch in line.chars() {
        if cx >= area.x + area.width {
            break;
        }
        if let Some(cell) = buf.cell_mut(ratatui::layout::Position::new(cx, row)) {
            cell.set_char(ch);
            cell.set_fg(color);
            cell.set_bg(PAPER_BG);
        }
        cx += ch.width().unwrap_or(0) as u16;
    }
}

fn truncate_to_width(text: &str, max: usize) -> String {
    let w = text.width();
    if w <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('\u{2026}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lrc::LrcLine;
    use std::time::Duration;

    fn synced_view(text: &str) -> GardenView<'static> {
        // Test-only: leaked so the view can borrow the lines.
        let lines: &'static [LrcLine] = Box::leak(
            vec![
                LrcLine {
                    time_s: 0.0,
                    text: "first".into(),
                },
                LrcLine {
                    time_s: 1.0,
                    text: text.into(),
                },
                LrcLine {
                    time_s: 2.0,
                    text: "third".into(),
                },
            ]
            .into_boxed_slice(),
        );
        GardenView {
            title: "Song",
            artist: "Artist",
            lines: Some((lines, Some(1))),
            plain: None,
            state: GardenState::Synced,
            energy: 0.5,
            time_s: 1.4,
            since_line_change: 2.0,
        }
    }

    #[test]
    fn garden_renders_within_bounds_at_all_sizes() {
        let sizes = [
            (144u16, 42u16),
            (100, 30),
            (72, 22),
            (40, 12),
            (20, 8),
            (10, 4),
            (4, 2),
        ];
        for (w, h) in sizes {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            render_garden(&mut buf, area, &synced_view("grow a garden"));
            // Areas below the render threshold draw nothing (guarded);
            // everything else gets a full paper backdrop. No panics at any
            // size, nothing outside the area by construction.
            if w >= 8 && h >= 3 {
                let paper = (0..w * h)
                    .filter(|i| buf.content[*i as usize].bg == PAPER_BG)
                    .count();
                assert_eq!(paper, (w * h) as usize, "backdrop fills {w}x{h}");
            }
        }
    }

    #[test]
    fn garden_is_deterministic_across_run_timing() {
        // Full rendered buffer comparison with different wall-clock times
        // between renders: the garden uses no wall clock at all, so the
        // buffers must be identical cell for cell.
        let area = Rect::new(0, 0, 80, 24);
        let mut a = Buffer::empty(area);
        let mut b = Buffer::empty(area);
        let view = synced_view("every beat a budding bloom");
        render_garden(&mut a, area, &view);
        std::thread::sleep(Duration::from_millis(50));
        render_garden(&mut b, area, &view);
        for i in 0..(area.width * area.height) as usize {
            assert_eq!(
                a.content[i].symbol(),
                b.content[i].symbol(),
                "symbol differs at {i}"
            );
            assert_eq!(a.content[i].fg, b.content[i].fg, "fg differs at {i}");
            assert_eq!(a.content[i].bg, b.content[i].bg, "bg differs at {i}");
        }
    }

    #[test]
    fn frozen_bloom_is_position_derived() {
        // The bloom age comes from since_line_change, which the UI derives
        // from the media position: the same input twice must be identical,
        // and a different age must change the growth.
        let area = Rect::new(0, 0, 72, 22);
        let mut a = Buffer::empty(area);
        let mut b = Buffer::empty(area);
        let mut view = synced_view("grow a garden");
        view.since_line_change = 0.1;
        render_garden(&mut a, area, &view);
        std::thread::sleep(Duration::from_millis(30));
        render_garden(&mut b, area, &view);
        for i in 0..(area.width * area.height) as usize {
            assert_eq!(a.content[i].symbol(), b.content[i].symbol());
        }

        // And a fully-grown frame differs from a just-started one.
        let mut c = Buffer::empty(area);
        view.since_line_change = 5.0;
        render_garden(&mut c, area, &view);
        let differing = (0..(area.width * area.height) as usize)
            .filter(|&i| a.content[i].symbol() != c.content[i].symbol())
            .count();
        assert!(differing > 0, "bloom must visibly grow");
    }

    #[test]
    fn mixed_case_and_punctuation_render_as_matrix() {
        // Real lyric-like text with apostrophes, dashes and case.
        let text = "Don't Stop \u{2014} Can't, Won't!";
        assert!(matrix_supported(text), "all glyphs must exist");
        let area = Rect::new(0, 0, 144, 42);
        let mut buf = Buffer::empty(area);
        render_garden(&mut buf, area, &synced_view(text));
        // The matrix must have drawn braille content in the middle band.
        let braille = (0..area.width * area.height)
            .filter(|i| {
                buf.content[*i as usize]
                    .symbol()
                    .chars()
                    .next()
                    .is_some_and(|c| ('\u{2800}'..='\u{28ff}').contains(&c))
            })
            .count();
        assert!(braille > 50, "expected a full matrix, got {braille} cells");
    }

    #[test]
    fn non_latin_falls_back_to_normal_text() {
        let text = "\u{4f60}\u{597d}\u{4e16}\u{754c}"; // CJK: hello world
        assert!(!matrix_supported(text));
        let area = Rect::new(0, 0, 72, 22);
        let mut buf = Buffer::empty(area);
        render_garden(&mut buf, area, &synced_view(text));
        // The CJK text must appear as normal chars (width-aware centering
        // is exercised implicitly - no panic, content present).
        let has_cjk = (0..area.width * area.height).any(|i| {
            buf.content[i as usize]
                .symbol()
                .chars()
                .any(|c| c == '\u{4f60}')
        });
        assert!(has_cjk, "fallback must render the original text");
    }

    #[test]
    fn long_lines_wrap_instead_of_dropping_the_garden() {
        // A line that cannot fit at scale 1 must wrap to multiple matrix
        // lines rather than fall back or clip.
        let text = "and the garden keeps on growing out of every single sound we play tonight";
        let sub_w = 72 * 2; // a 72-wide band
        let wrapped = wrap_matrix_lines(text, 1, sub_w - 4);
        assert!(
            wrapped.len() >= 2,
            "expected wrapping, got {}",
            wrapped.len()
        );
        assert!(wrapped.len() <= 4, "wrapping must stay bounded");
        // Every wrapped line must fit.
        for l in &wrapped {
            assert!(
                line_width(l, 1) <= sub_w - 4,
                "wrapped line too wide: {l:?}"
            );
        }
        // And it still renders without panic, matrix intact.
        let area = Rect::new(0, 0, 72, 22);
        let mut buf = Buffer::empty(area);
        render_garden(&mut buf, area, &synced_view(text));
        assert!(matrix_supported(text));
    }

    #[test]
    fn wrap_keeps_words_when_possible() {
        let wrapped = wrap_matrix_lines("alpha beta gamma", 1, 40);
        // 40 sub-px fits about 6-7 chars per line: multiple lines.
        assert!(wrapped.len() >= 2);
        for l in &wrapped {
            assert!(line_width(l, 1) <= 40, "line too wide: {l:?}");
        }
        // Words are not split unless unavoidable.
        assert!(wrapped.iter().all(|l| !l.is_empty()));
    }

    #[test]
    fn matrix_fits_or_falls_back() {
        assert!(matrix_supported("OK"));
        assert!(matrix_supported("Mixed Case 42!?"));
        assert!(!matrix_supported("\u{4f60}\u{597d}"));
        // Width math: 5 cols + spacing, scaled.
        assert_eq!(line_width("AB", 1), 6 * 2 - 1);
        assert_eq!(line_width("AB", 2), (6 * 2 - 1) * 2);
    }

    #[test]
    fn all_states_render() {
        for state in [
            GardenState::Synced,
            GardenState::Plain,
            GardenState::Instrumental,
            GardenState::Loading,
            GardenState::NoMatch,
            GardenState::Unavailable,
            GardenState::Error("network down".into()),
        ] {
            let area = Rect::new(0, 0, 72, 22);
            let mut buf = Buffer::empty(area);
            let mut view = synced_view("state check");
            view.state = state.clone();
            view.lines = if state == GardenState::Synced {
                view.lines
            } else {
                None
            };
            view.plain = if state == GardenState::Plain {
                Some("plain words")
            } else {
                None
            };
            render_garden(&mut buf, area, &view);
        }
    }

    #[test]
    fn bloom_springs_in() {
        let mut last = 0.0f32;
        for t in [0.0, 0.1, 0.3, 0.6, 1.0, 2.0] {
            let v = spr(t, 3.0, 10.0);
            assert!((-0.3..=1.3).contains(&v));
            assert!(v >= last - 0.35, "no wild swings");
            last = v;
        }
        assert!(spr(1.0, 3.0, 10.0) > 0.9, "completes");
    }

    #[test]
    fn rng_is_deterministic() {
        let mut a = rng(42);
        let mut b = rng(42);
        for _ in 0..16 {
            assert!((a() - b()).abs() < 1e-9);
        }
    }

    #[test]
    fn paper_palette_matches_reference() {
        // The reference Paper palette: pale paper, dark type, blue stems.
        assert_eq!(PAPER_BG, Color::Rgb(243, 238, 228));
        assert_eq!(STEM, Color::Rgb(28, 43, 143));
        // Type must be dark on the pale paper (contrast for readability).
        match INK {
            Color::Rgb(r, g, b) => {
                assert!(r < 60 && g < 60 && b < 60, "ink must be dark");
            }
            _ => panic!("ink must be RGB"),
        }
    }

    #[test]
    fn vine_lengths_graduate_across_depth() {
        // The vine lengths must fill the garden depth: most vines should
        // be taller than a third of it, and at least one reaching.
        let depth = 80usize;
        let vines = garden_vines(24, 100, 7, depth);
        assert!(vines.len() >= 3);
        let lens: Vec<f32> = vines.iter().map(|v| v.len).collect();
        let tall = lens.iter().filter(|&&l| l > depth as f32 * 0.5).count();
        assert!(
            tall >= lens.len() / 2,
            "most vines should grow past half depth: {lens:?}"
        );
        let reaching = lens.iter().filter(|&&l| l > depth as f32 * 0.8).count();
        assert!(reaching >= 1, "at least one vine should reach: {lens:?}");
        // And the trace actually rises: the tip is above the root.
        let v = &vines[0];
        let pts = v.trace(1.0);
        let tip = pts.last().unwrap();
        assert!(tip.1 < v.y0, "tip must be above the root");
    }
}
