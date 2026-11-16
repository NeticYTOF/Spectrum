//! The lyrics garden: an ascii-garden-inspired terminal rendering.
//!
//! Visual language ported from ridit-jangra/ascii-garden (MIT, (c) 2026
//! Ridit; see NOTICE for the preserved license): a pale-paper backdrop,
//! dot-matrix text, curling blue stems growing out of the letters and
//! warm flowers blooming at their tips. The Rust implementation here is
//! original: text and garden are drawn on a sub-cell canvas (2x4 Braille
//! dots per terminal cell) with a seeded deterministic RNG, spring-eased
//! bloom on line changes and a gentle sway driven by audio energy - no
//! per-frame randomness anywhere.
//!
//! The ascii-garden motion vocabulary it borrows conceptually:
//! - `spr(t) = 1 - e^(-kt) * cos(wt)`: springy growth/bloom easing
//! - vines that curl (sinusoidal angle perturbation along the stem)
//! - blooms staged over a few hundred milliseconds, not all at once

use std::f32::consts::PI;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::lrc::LrcLine;

/// Paper-adapted palette (dark-terminal version of ascii-garden's "Paper").
const PAPER_BG: Color = Color::Rgb(26, 24, 21);
const INK: Color = Color::Rgb(226, 222, 210); // warm cream text
const INK_DIM: Color = Color::Rgb(120, 114, 104); // surrounding lyrics
const STEM: Color = Color::Rgb(90, 130, 200); // the original's #1C2B8F lifted
const STEM_LIGHT: Color = Color::Rgb(150, 185, 235);
const FLOWER: Color = Color::Rgb(245, 140, 165); // soft rose
#[allow(dead_code)]
const FLOWER_LIGHT: Color = Color::Rgb(255, 200, 215);
#[allow(dead_code)]
const FLOWER_DARK: Color = Color::Rgb(200, 95, 125);

/// Sub-cell layers, highest value wins when packing a cell's color.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Layer {
    Empty = 0,
    In = 1,
    Stem = 2,
    Leaf = 3,
    Flower = 4,
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
        s = s.wrapping_mul(0x68bc21eb).wrapping_add(0x9e3779b9);
        let t = s ^ (s >> 15);
        let t = t.wrapping_mul(t | 1);
        let t = t ^ (t.wrapping_add(t >> 7));
        let t = t ^ (t >> 16);
        (t as f32) / (u32::MAX as f32)
    }
}

/// A 3x5 dot-matrix font for the big current line (A-Z, 0-9 and common
/// punctuation). Rows are bit-3..bit-0, left to right in value bits.
fn glyph(c: char) -> Option<[u8; 5]> {
    Some(match c {
        'A' => [0b010, 0b101, 0b111, 0b101, 0b101],
        'B' => [0b110, 0b101, 0b110, 0b101, 0b110],
        'C' => [0b011, 0b100, 0b100, 0b100, 0b011],
        'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        'E' => [0b111, 0b100, 0b110, 0b100, 0b111],
        'F' => [0b111, 0b100, 0b110, 0b100, 0b100],
        'G' => [0b011, 0b100, 0b101, 0b101, 0b011],
        'H' => [0b101, 0b101, 0b111, 0b101, 0b101],
        'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
        'J' => [0b001, 0b001, 0b001, 0b101, 0b010],
        'K' => [0b101, 0b101, 0b110, 0b101, 0b101],
        'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        'M' => [0b101, 0b111, 0b111, 0b101, 0b101],
        'N' => [0b110, 0b101, 0b101, 0b101, 0b101],
        'O' => [0b010, 0b101, 0b101, 0b101, 0b010],
        'P' => [0b110, 0b101, 0b110, 0b100, 0b100],
        'Q' => [0b010, 0b101, 0b101, 0b111, 0b011],
        'R' => [0b110, 0b101, 0b110, 0b101, 0b101],
        'S' => [0b011, 0b100, 0b010, 0b001, 0b110],
        'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        'U' => [0b101, 0b101, 0b101, 0b101, 0b011],
        'V' => [0b101, 0b101, 0b101, 0b101, 0b010],
        'W' => [0b101, 0b101, 0b111, 0b111, 0b101],
        'X' => [0b101, 0b101, 0b010, 0b101, 0b101],
        'Y' => [0b101, 0b101, 0b010, 0b010, 0b010],
        'Z' => [0b111, 0b001, 0b010, 0b100, 0b111],
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b110, 0b001, 0b010, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b011, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b010, 0b010, 0b010],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        ' ' => [0; 5],
        '.' => [0, 0, 0, 0, 0b010],
        ',' => [0, 0, 0, 0b010, 0b100],
        '\'' => [0b010, 0b010, 0, 0, 0],
        '!' => [0b010, 0b010, 0b010, 0, 0b010],
        '?' => [0b110, 0b001, 0b010, 0, 0b010],
        '-' => [0, 0, 0b111, 0, 0],
        ':' => [0, 0b010, 0, 0b010, 0],
        _ => return None,
    })
}

/// Total dot-matrix width of a text (sub-pixels), or None if any glyph
/// is unsupported (caller falls back to normal text). Lowercase maps to
/// its uppercase glyph, standard for dot-matrix fonts.
fn matrix_width(text: &str) -> Option<usize> {
    let mut w = 0usize;
    let mut count = 0usize;
    for c in text.chars() {
        glyph(to_matrix_char(c))?;
        w += 4; // 3 columns + 1 spacing
        count += 1;
    }
    if count == 0 {
        return None;
    }
    Some(w - 1) // no trailing space
}

/// Lowercase letters use their uppercase dot-matrix glyph.
fn to_matrix_char(c: char) -> char {
    if c.is_ascii_lowercase() {
        c.to_ascii_uppercase()
    } else {
        c
    }
}

/// Draw dot-matrix text at (x0, y0) as In-layer dots. Returns the width.
fn draw_matrix(canvas: &mut DotCanvas, text: &str, x0: i32, y0: i32, grow: f32) {
    let mut x = x0;
    for (i, c) in text.chars().enumerate() {
        let g = glyph(to_matrix_char(c)).unwrap_or([0; 5]);
        // Stagger the letters' growth like ascii-garden's typed reveal.
        let letter_grow = spr(grow * 2.0 - i as f32 * 0.06, 6.0, 12.0);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..3 {
                if bits & (1 << (2 - col)) != 0 {
                    let fx = col as f32 + 1.0 - letter_grow;
                    if fx >= 0.0 {
                        canvas.set(x + col, y0 + row as i32, Layer::In);
                    }
                }
            }
        }
        x += 4;
    }
}

/// One vine: a curling stem with leaves and a flower at the tip, ported
/// from ascii-garden's `vine` + `grow` concepts. Draws only up to `grow`
/// of its length; the flower blooms at the very end.
struct Vine {
    /// Start point in sub-pixels (relative to canvas).
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
    /// Whether the tip carries a flower.
    flower: bool,
    /// Flower radius (sub-pixels).
    flower_r: f32,
    /// Leaf positions along the stem (0..1) and sides.
    leaves: Vec<(f32, f32)>,
}

impl Vine {
    /// Trace the stem up to `u` in 0..=1, returning the points.
    fn trace(&self, u: f32) -> Vec<(f32, f32)> {
        let n = 28;
        let steps = ((n as f32) * u.clamp(0.0, 1.0)).round() as usize;
        let mut pts = Vec::with_capacity(steps + 1);
        let mut x = self.x0;
        let mut y = self.y0;
        let step = self.len / n as f32;
        for i in 0..=steps {
            let f = i as f32 / n as f32;
            let a = self.dir
                + self.bend * f
                + self.amp * (f * PI * self.waves + self.phase).sin() * f.min(1.0);
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
        // The flower blooms when the stem finishes.
        if self.flower && u >= 0.999 {
            let bloom = spr((grow - 1.0).max(0.0) * 1.6, 7.0, 12.0);
            let tip = pts.last().copied().unwrap_or((self.x0, self.y0));
            let r = self.flower_r * bloom.max(0.05);
            let (cx, cy) = tip;
            // Petal ring + center: a rose-ish cluster in the original's
            // spirit, scaled to sub-pixel resolution.
            let mut r2 = rng((cx as i32 as u32)
                .wrapping_mul(7919)
                .wrapping_add(cy as i32 as u32));
            let petals = 5;
            for p in 0..petals {
                let ang = p as f32 / petals as f32 * PI * 2.0 + r2() * 0.4;
                let px = cx + ang.cos() * r * 0.8;
                let py = cy + ang.sin() * r * 0.55;
                canvas.set(px.round() as i32, py.round() as i32, Layer::Flower);
                canvas.set(
                    (px + ang.cos() * 0.9).round() as i32,
                    (py + ang.sin() * 0.9).round() as i32,
                    Layer::Flower,
                );
            }
            canvas.set(cx.round() as i32, cy.round() as i32, Layer::Flower);
            canvas.set(
                (cx + r2() - 0.5).round() as i32,
                (cy - r * 0.8).round() as i32,
                Layer::Flower,
            );
        }
    }
}

/// Grow a garden of vines from deterministic letter positions.
fn garden_vines(text: &str, seed: u32, chaos: f32) -> Vec<Vine> {
    let mut r = rng(seed);
    let count = (1.0 + chaos * 5.0).round() as usize;
    let mut vines = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vines;
    }
    for _ in 0..count {
        let letter = ((r() * chars.len() as f32) as usize).min(chars.len() - 1);
        let x0 = letter as f32 * 4.0 + r() * 2.0; // somewhere in the letter
        let dir = -PI / 2.0 + (r() - 0.5) * 0.9;
        let len = 6.0 + r() * (8.0 + chaos * 16.0);
        let amp = 0.3 + r() * 0.6;
        let waves = 1.0 + r() * 1.6;
        let phase = r() * 2.0 * PI;
        let bend = (r() - 0.5) * 0.8;
        let flower = r() < 0.75;
        let flower_r = 1.8 + r() * 1.4;
        let leaves = if r() < 0.6 {
            vec![(0.3 + r() * 0.4, if r() < 0.5 { 1.0 } else { -1.0 })]
        } else {
            vec![]
        };
        vines.push(Vine {
            x0,
            y0: 4.0, // from the letter's top row
            dir,
            len,
            amp,
            waves,
            phase,
            bend,
            flower,
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
            let mut counts = [0usize; 5];
            for row in 0..4 {
                for col in 0..2 {
                    let l = canvas.get((sx + col) as i32, (sy + row) as i32);
                    if l == Layer::Empty {
                        continue;
                    }
                    bits |= braille_bit(col, row);
                    counts[l as usize] += 1;
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

/// What the garden should show this frame.
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
    /// Seconds since the current line changed (drives bloom).
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

    // Section layout (cell rows): title, prev line, the garden band, then
    // the next lines under the matrix.
    let title_y = area.y;
    let garden_rows = area
        .height
        .saturating_sub(2)
        .min(area.height / 2 + 2)
        .max(4);
    let garden_y = area.y + 1.min(area.height / 3);
    let garden_h = garden_rows;

    // Backdrop over the whole area.
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut(ratatui::layout::Position::new(x, y)) {
                cell.set_bg(PAPER_BG);
            }
        }
    }

    // Title line: "title - artist" (or the fixture name).
    let title = if view.artist.is_empty() {
        view.title.to_string()
    } else {
        format!("{} \u{b7} {}", view.title, view.artist)
    };
    put_text(buf, area, title_y, &title, INK_DIM);

    let garden_area = Rect::new(
        area.x,
        garden_y,
        area.width,
        garden_h.min(area.height - garden_y),
    );

    match &view.state {
        GardenState::Synced => {
            if let Some((lines, Some(idx))) = view.lines {
                let prev = idx.checked_sub(1).map(|i| lines[i].text.clone());
                let next = lines.get(idx + 1).map(|l| l.text.clone());
                let text = &lines[idx].text;

                // Prev line above the garden (dim, understated).
                if let Some(p) = prev {
                    put_text(buf, area, garden_y, &p, INK_DIM);
                }
                // Next lines below the matrix.
                let matrix_bottom = garden_area.y + garden_area.height;
                if let Some(n) = next {
                    let row = matrix_bottom.min(area.y + area.height - 1);
                    put_text(buf, area, row, &n, INK_DIM);
                }

                render_matrix_band(
                    buf,
                    Rect::new(
                        garden_area.x,
                        garden_area.y + 1,
                        garden_area.width,
                        garden_area.height.saturating_sub(2).max(2),
                    ),
                    text,
                    view,
                );
            }
        }
        GardenState::Plain => {
            if let Some(plain) = view.plain {
                let rows = garden_area.y + 1..garden_area.y + garden_area.height;
                for (row, line) in rows.zip(plain.lines()) {
                    put_text(buf, area, row, line, INK);
                }
                put_text(buf, area, garden_area.y, "(unsynced lyrics)", INK_DIM);
            }
        }
        GardenState::Instrumental => {
            render_matrix_band(buf, garden_area, "[instrumental]", view);
        }
        GardenState::Loading => {
            put_text(
                buf,
                area,
                garden_area.y + garden_area.height / 2,
                "looking for lyrics...",
                INK_DIM,
            );
        }
        GardenState::NoMatch => {
            put_text(
                buf,
                area,
                garden_area.y + garden_area.height / 2,
                "no matching lyrics",
                INK_DIM,
            );
        }
        GardenState::Unavailable => {
            put_text(
                buf,
                area,
                garden_area.y + garden_area.height / 2,
                "no media session",
                INK_DIM,
            );
        }
        GardenState::Error(e) => {
            let msg = format!("lyrics: {e}");
            put_text(
                buf,
                area,
                garden_area.y + garden_area.height / 2,
                &msg,
                INK_DIM,
            );
        }
    }
}

/// The dot-matrix band: big braille letters with the garden growing from
/// them, or a styled-text fallback when it does not fit.
fn render_matrix_band(buf: &mut Buffer, area: Rect, text: &str, view: &GardenView<'_>) {
    if area.width < 4 || area.height < 2 {
        return;
    }
    let sub_w = area.width as usize * 2;
    // Matrix needs: width sub-px + margin, height 5 sub-rows + garden above
    // (garden grows up from the letters).
    let fit = matrix_width(text);
    let fits = fit.map(|w| w + 4 <= sub_w).unwrap_or(false);

    if fits {
        let mw = fit.unwrap() as i32;
        let x0 = (sub_w as i32 - mw) / 2;
        // Letters sit low in the band; the garden fills the space above.
        let band_h = area.height as i32 * 4;
        let garden_depth = (band_h - 10).max(6); // sub-rows above the letters
        let letters_y = area.height as i32 * 4 - 7; // bottom-ish
        let _ = garden_depth;

        let mut canvas = DotCanvas::new(sub_w, area.height as usize * 4);
        let grow = ease_out(view.since_line_change / 0.7).max(0.05);
        draw_matrix(&mut canvas, text, x0, letters_y, grow);

        // Sway: gentle, energy-driven; frozen time still sways only if the
        // clock moves - frozen frames stay still because time_s is pinned.
        let sway_phase = view.time_s * 0.6;
        let sway_amp = (0.4 + view.energy * 1.8).min(2.2);

        // Vines from deterministic letter tops, clipped into the band.
        let seed = text_len_seed(text) ^ (letters_y as u32).wrapping_mul(2654435761);
        let chaos = 0.35;
        let vines = garden_vines(text, seed, chaos);
        for (vi, v) in vines.iter().enumerate() {
            let stagger = (vi as f32) * 0.09;
            let vgrow = spr(
                (view.since_line_change - 0.15 - stagger).max(-0.1),
                2.6,
                9.0,
            ) + spr((view.since_line_change - 1.0).max(-0.1), 1.2, 6.0);
            let vgrow = vgrow.clamp(0.0, 1.6);
            let sway = (sway_phase + v.x0 * 0.15).sin() * sway_amp * 0.35;
            // Translate vine coordinates into the band: letter tops are at
            // letters_y, vines grow upward from there.
            let mut vv = Vine {
                y0: letters_y as f32 - 1.0,
                x0: x0 as f32 + v.x0,
                ..v.clone_vine()
            };
            vv.dir = v.dir;
            vv.len = v.len.min(garden_depth as f32 - 2.0);
            vv.draw(&mut canvas, vgrow, sway);
        }
        present(&canvas, buf, area);
    } else {
        // Fallback: styled normal text, centered, no clipping.
        let line = truncate_to_width(text, area.width as usize - 2);
        let text_w = line.chars().count() as u16;
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
    }
}

impl Vine {
    fn clone_vine(&self) -> Vine {
        Vine {
            x0: self.x0,
            y0: self.y0,
            dir: self.dir,
            len: self.len,
            amp: self.amp,
            waves: self.waves,
            phase: self.phase,
            bend: self.bend,
            flower: self.flower,
            flower_r: self.flower_r,
            leaves: self.leaves.clone(),
        }
    }
}

fn text_len_seed(text: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for b in text.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}

/// Write a dim text line centered-clipped into `area` at cell row `row`.
fn put_text(buf: &mut Buffer, area: Rect, row: u16, text: &str, color: Color) {
    if row < area.y || row >= area.y + area.height {
        return;
    }
    let width = area.width as usize;
    let line: String = text.chars().take(width).collect();
    let x = area.x + ((width - line.chars().count()) / 2) as u16;
    for (i, ch) in line.chars().enumerate() {
        if let Some(cell) = buf.cell_mut(ratatui::layout::Position::new(x + i as u16, row)) {
            cell.set_char(ch);
            cell.set_fg(color);
            cell.set_bg(PAPER_BG);
        }
    }
}

fn truncate_to_width(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lrc::LrcLine;

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
    fn garden_is_deterministic_for_frozen_inputs() {
        let area = Rect::new(0, 0, 80, 24);
        let mut a = Buffer::empty(area);
        let mut b = Buffer::empty(area);
        let view = synced_view("every beat a budding bloom");
        render_garden(&mut a, area, &view);
        render_garden(&mut b, area, &view);
        for i in 0..(area.width * area.height) {
            assert_eq!(
                a.content[i as usize].symbol(),
                b.content[i as usize].symbol(),
                "frame {i} differs"
            );
        }
    }

    #[test]
    fn matrix_fits_or_falls_back() {
        // A short line fits in a wide area.
        assert!(matrix_width("OK").unwrap() <= 200);
        // Unsupported scripts fall back (no glyph).
        assert!(matrix_width("\u{4f60}\u{597d}").is_none());
        // A very long line reports wider than any terminal (caller checks).
        assert!(matrix_width(&"A".repeat(81)).unwrap() > 320);
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
        // Growth easing is monotone-ish and bounded.
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
}
