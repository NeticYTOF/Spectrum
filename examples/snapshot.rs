//! Headless QA snapshot: renders one frame of the REAL UI (`ui::snapshot`,
//! the same `draw` path as the binary) and rasterizes the cells to a BMP
//! the way a VTE-class terminal paints them: per-cell background, glyphs
//! from a real monospace font, and block/quadrant/braille elements drawn
//! geometrically (VTE draws those itself rather than from the font).
//!
//! This is an approximation of a terminal, not a terminal: font, hinting
//! and cell metrics differ from any given emulator.
//!
//! cargo run --release --example snapshot -- --mode 6 --size 144x42 \
//!     --at 4.5 --out shot.bmp [--lyric hello] [--font path.ttf] [--live]

use ab_glyph::{point, Font, FontVec, PxScale, ScaleFont};
use ratatui::style::Color;

const CW: usize = 10;
const CH: usize = 20;
const DEFAULT_BG: (u8, u8, u8) = (18, 18, 20);
const DEFAULT_FG: (u8, u8, u8) = (204, 204, 204);

fn rgb(c: Color, fallback: (u8, u8, u8)) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Reset => fallback,
        Color::Black => (0, 0, 0),
        Color::White => (229, 229, 229),
        Color::Gray => (190, 190, 190),
        Color::DarkGray => (110, 110, 110),
        Color::Yellow => (229, 229, 16),
        Color::Indexed(i) => indexed(i),
        _ => fallback,
    }
}

fn indexed(i: u8) -> (u8, u8, u8) {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
        (127, 127, 127),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (92, 92, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    if i < 16 {
        return BASE[i as usize];
    }
    if i >= 232 {
        let v = 8 + (i - 232) * 10;
        return (v, v, v);
    }
    let k = i - 16;
    let lv = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
    (lv(k / 36), lv((k / 6) % 6), lv(k % 6))
}

struct Img {
    w: usize,
    h: usize,
    px: Vec<(u8, u8, u8)>,
}

impl Img {
    fn fill(&mut self, x0: usize, y0: usize, w: usize, h: usize, c: (u8, u8, u8)) {
        for y in y0..(y0 + h).min(self.h) {
            for x in x0..(x0 + w).min(self.w) {
                self.px[y * self.w + x] = c;
            }
        }
    }
    fn blend(&mut self, x: usize, y: usize, c: (u8, u8, u8), a: f32) {
        if x >= self.w || y >= self.h {
            return;
        }
        let p = &mut self.px[y * self.w + x];
        let m = |s: u8, d: u8| (d as f32 + (s as f32 - d as f32) * a).round() as u8;
        *p = (m(c.0, p.0), m(c.1, p.1), m(c.2, p.2));
    }
    fn save_bmp(&self, path: &str) -> std::io::Result<()> {
        let row = (self.w * 3).div_ceil(4) * 4;
        let size = 54 + row * self.h;
        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&(size as u32).to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&(self.w as i32).to_le_bytes());
        out.extend_from_slice(&(self.h as i32).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(&[0u8; 24]);
        for y in (0..self.h).rev() {
            let start = out.len();
            for x in 0..self.w {
                let (r, g, b) = self.px[y * self.w + x];
                out.extend_from_slice(&[b, g, r]);
            }
            while out.len() - start < row {
                out.push(0);
            }
        }
        std::fs::write(path, out)
    }
}

/// Geometric block elements / braille, as VTE draws them. Returns false
/// when the char is not one of them.
fn draw_geometric(img: &mut Img, x0: usize, y0: usize, ch: char, fg: (u8, u8, u8)) -> bool {
    let c = ch as u32;
    let (hw, hh) = (CW / 2, CH / 2);
    let q = |img: &mut Img, tl: bool, tr: bool, bl: bool, br: bool| {
        if tl {
            img.fill(x0, y0, hw, hh, fg);
        }
        if tr {
            img.fill(x0 + hw, y0, CW - hw, hh, fg);
        }
        if bl {
            img.fill(x0, y0 + hh, hw, CH - hh, fg);
        }
        if br {
            img.fill(x0 + hw, y0 + hh, CW - hw, CH - hh, fg);
        }
    };
    match c {
        0x2580 => q(img, true, true, false, false),
        0x2584 => q(img, false, false, true, true),
        0x2588 => q(img, true, true, true, true),
        0x258C => q(img, true, false, true, false),
        0x2590 => q(img, false, true, false, true),
        0x2596 => q(img, false, false, true, false),
        0x2597 => q(img, false, false, false, true),
        0x2598 => q(img, true, false, false, false),
        0x2599 => q(img, true, false, true, true),
        0x259A => q(img, true, false, false, true),
        0x259B => q(img, true, true, true, false),
        0x259C => q(img, true, true, false, true),
        0x259D => q(img, false, true, false, false),
        0x259E => q(img, false, true, true, false),
        0x259F => q(img, false, true, true, true),
        0x2581..=0x2587 => {
            // Lower eighths.
            let n = (c - 0x2580) as usize;
            let h = CH * n / 8;
            img.fill(x0, y0 + CH - h, CW, h, fg);
        }
        0x2589..=0x258F => {
            // Left eighths (7/8 .. 1/8).
            let n = (0x2590 - c) as usize;
            img.fill(x0, y0, CW * n / 8, CH, fg);
        }
        0x2800..=0x28FF => {
            let bits = c - 0x2800;
            // Dot order: 1 2 3 7 (left col), 4 5 6 8 (right col).
            let map = [
                (0, 0),
                (0, 1),
                (0, 2),
                (1, 0),
                (1, 1),
                (1, 2),
                (0, 3),
                (1, 3),
            ];
            for (k, &(cx, cy)) in map.iter().enumerate() {
                if bits & (1 << k) != 0 {
                    let px = x0 + cx * hw + hw / 2;
                    let py = y0 + cy * (CH / 4) + CH / 8;
                    for dy in 0..2 {
                        for dx in 0..2 {
                            img.fill(px - 1 + dx, py - 1 + dy, 1, 1, fg);
                        }
                    }
                }
            }
        }
        _ => return false,
    }
    true
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |k: &str| {
        args.iter()
            .position(|a| a == k)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let mode: u8 = get("--mode").and_then(|v| v.parse().ok()).unwrap_or(6);
    let (w, h) = get("--size")
        .and_then(|v| {
            let (a, b) = v.split_once('x')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .unwrap_or((144u16, 42u16));
    let at: f32 = get("--at").and_then(|v| v.parse().ok()).unwrap_or(4.5);
    let out = get("--out").unwrap_or_else(|| "snapshot.bmp".into());
    let lyric = get("--lyric");
    let theme = get("--theme").unwrap_or_else(|| "spectrum".into());
    let depth =
        spectrum::theme::ColorDepth::parse(&get("--colors").unwrap_or_else(|| "true".into()))
            .unwrap_or(spectrum::theme::ColorDepth::True);
    let live = args.iter().any(|a| a == "--live");
    let font_path = get("--font").unwrap_or_else(|| {
        if cfg!(windows) {
            "C:\\Windows\\Fonts\\consola.ttf".into()
        } else {
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf".into()
        }
    });

    let buf = spectrum::ui::snapshot(&spectrum::ui::Snapshot {
        mode,
        theme: &theme,
        depth,
        at,
        frozen: !live,
        warmup: if live { 2.0 } else { 3.0 },
        lyric: lyric.as_deref(),
        width: w,
        height: h,
    });

    let font = FontVec::try_from_vec(std::fs::read(&font_path).expect("monospace font"))
        .expect("parse font");
    // Size the font so its line height fills the cell (as terminals do).
    let px = PxScale::from(
        CH as f32 * font.height_unscaled() / (font.ascent_unscaled() - font.descent_unscaled()),
    );
    let sf = font.as_scaled(px);
    let ascent = sf.ascent();

    let mut img = Img {
        w: w as usize * CW,
        h: h as usize * CH,
        px: vec![DEFAULT_BG; w as usize * CW * h as usize * CH],
    };
    for y in 0..h {
        for x in 0..w {
            let cell = &buf[(x, y)];
            let bg = rgb(cell.bg, DEFAULT_BG);
            let fg = rgb(cell.fg, DEFAULT_FG);
            let (x0, y0) = (x as usize * CW, y as usize * CH);
            img.fill(x0, y0, CW, CH, bg);
            let sym = cell.symbol();
            let Some(ch) = sym.chars().next() else {
                continue;
            };
            if ch == ' ' {
                continue;
            }
            if draw_geometric(&mut img, x0, y0, ch, fg) {
                continue;
            }
            let id = font.glyph_id(ch);
            let adv = sf.h_advance(id);
            let gx = x0 as f32 + (CW as f32 - adv) / 2.0;
            let glyph = id.with_scale_and_position(px, point(gx, y0 as f32 + ascent));
            if let Some(o) = font.outline_glyph(glyph) {
                let b = o.px_bounds();
                o.draw(|gx, gy, c| {
                    let xx = b.min.x as i64 + gx as i64;
                    let yy = b.min.y as i64 + gy as i64;
                    if xx >= 0 && yy >= 0 {
                        img.blend(xx as usize, yy as usize, fg, c);
                    }
                });
            }
        }
    }
    img.save_bmp(&out).expect("write bmp");
    println!("wrote {out} ({}x{} px, {w}x{h} cells)", img.w, img.h);
}
