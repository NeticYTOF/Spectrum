//! The garden's type: genuine Playfair Display (the reference's
//! `700 "Playfair Display", Georgia, serif`), rasterized from the
//! embedded, UNMODIFIED variable font with the weight axis set to 700 at
//! runtime - no derived font file is produced. Font licence: SIL OFL 1.1,
//! "Copyright 2017 The Playfair Display Project Authors", Reserved Font
//! Name "Playfair Display" (assets/fonts/playfair-display/OFL.txt).
//!
//! Glyphs rasterize with anti-aliased coverage into a [`TypeMask`] at the
//! garden's subpixel resolution; advances and kerning come from the font
//! (garden.ts `mw()` measures the same font with `measureText`).

use std::sync::OnceLock;

use ab_glyph::{point, Font, FontRef, GlyphId, PxScale, VariableFont};

static FONT_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/playfair-display/PlayfairDisplay[wght].ttf");

/// The reference draws type at weight 700.
const WEIGHT: f32 = 700.0;

fn font() -> &'static FontRef<'static> {
    static FONT: OnceLock<FontRef<'static>> = OnceLock::new();
    FONT.get_or_init(|| {
        let mut f = FontRef::try_from_slice(FONT_BYTES).expect("embedded Playfair Display");
        f.set_variation(b"wght", WEIGHT);
        f
    })
}

fn upem() -> f32 {
    font().units_per_em().unwrap_or(1000.0)
}

/// The glyph for `c`, or None when the font has no glyph (notdef).
fn glyph_id(c: char) -> Option<GlyphId> {
    let id = font().glyph_id(c);
    if id.0 == 0 {
        None
    } else {
        Some(id)
    }
}

/// Advance of `c` in em units (garden.ts `mw`, which also widens the
/// space by 1.4). None when the font lacks the glyph.
pub fn advance(c: char) -> Option<f32> {
    let id = glyph_id(c)?;
    let a = font().h_advance_unscaled(id) / upem();
    Some(if c == ' ' { a * 1.4 } else { a })
}

/// Kerning between two glyphs, in em units.
pub fn kern(a: char, b: char) -> f32 {
    match (glyph_id(a), glyph_id(b)) {
        (Some(x), Some(y)) => font().kern_unscaled(x, y) / upem(),
        _ => 0.0,
    }
}

/// Whether every character of `text` has a real Playfair glyph (Latin,
/// Latin extended, Cyrillic, Vietnamese...). Anything else falls back to
/// honest plain text.
pub fn glyph_supported(text: &str) -> bool {
    text.chars()
        .all(|c| c.is_whitespace() || glyph_id(c).is_some())
}

/// Font metrics in em units.
pub struct Metrics {
    pub x_height: f32,
    pub cap_height: f32,
    pub ascender: f32,
    pub descender: f32,
}

/// The glyph's actual INK extent in em, y-up: (top, bottom) - measured
/// per glyph from the embedded Playfair (the 'h' ink top is ~0.782, NOT
/// the em-box ascender ~0.96; x-height letters ~0.52; descenders go
/// negative). Used for whole-bloom clearance: flowers must clear the
/// letter's real ink shape, not a bounding-box guess.
pub fn ink_extent(c: char) -> (f32, f32) {
    match glyph_id(c) {
        Some(id) => {
            let u = upem();
            font()
                .outline(id)
                .map(|o| {
                    (
                        o.bounds.min.y.max(o.bounds.max.y) / u,
                        o.bounds.min.y.min(o.bounds.max.y) / u,
                    )
                })
                .unwrap_or((0.517, -0.2))
        }
        None => (0.517, -0.2),
    }
}

pub fn metrics() -> Metrics {
    let f = font();
    let u = upem();
    let top = |c: char| {
        glyph_id(c)
            .and_then(|id| f.outline(id))
            .map(|o| o.bounds.min.y.max(o.bounds.max.y) / u)
            .unwrap_or(0.5)
    };
    let bottom = |c: char| {
        glyph_id(c)
            .and_then(|id| f.outline(id))
            .map(|o| o.bounds.min.y.min(o.bounds.max.y) / u)
            .unwrap_or(-0.2)
    };
    Metrics {
        x_height: top('x'),
        cap_height: top('H'),
        ascender: top('h'),
        descender: bottom('g'),
    }
}

/// Anti-aliased type coverage at subpixel resolution (0..=255 each).
#[derive(Clone)]
pub struct TypeMask {
    pub w: usize,
    pub h: usize,
    pub cov: Vec<u8>,
    /// Owning line (index + 1) of each covered subpixel.
    pub line: Vec<u8>,
    /// Owning LETTER (scene index + 1; 0 = none) of each covered
    /// subpixel - per-letter identity, separate from the line tag: the
    /// line-based owner cannot audit EACH letter's cover budget.
    pub letter: Vec<u16>,
    /// Line tag applied by subsequent `draw_glyph` calls.
    pub current_line: u8,
    /// Letter tag applied by subsequent `draw_glyph` calls.
    pub current_letter: u16,
}

impl TypeMask {
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            cov: vec![0; w * h],
            line: vec![0; w * h],
            letter: vec![0; w * h],
            current_line: 1,
            current_letter: 0,
        }
    }

    #[inline]
    pub fn at(&self, i: usize, j: usize) -> u8 {
        if i < self.w && j < self.h {
            self.cov[j * self.w + i]
        } else {
            0
        }
    }

    /// Rasterize `ch` centred on `cx` (design units) with its baseline at
    /// `by`, at type size `s` (em in design units). `sub` is subpixels
    /// per design unit. Coverage accumulates with max so overlapping
    /// glyphs never punch holes.
    pub fn draw_glyph(&mut self, ch: char, cx: f32, by: f32, s: f32, sub: f32) {
        let Some(id) = glyph_id(ch) else { return };
        let f = font();
        let em_px = s * sub;
        // ab_glyph's PxScale is the font height (ascent - descent); scale
        // it so one em is exactly `em_px` subpixels.
        let px = em_px * f.height_unscaled() / upem();
        let adv = f.h_advance_unscaled(id) / upem() * em_px;
        let origin = point(cx * sub - adv / 2.0, by * sub);
        let glyph = id.with_scale_and_position(PxScale::from(px), origin);
        let Some(outlined) = f.outline_glyph(glyph) else {
            return;
        };
        let bounds = outlined.px_bounds();
        let (bx, byy) = (bounds.min.x.floor() as i64, bounds.min.y.floor() as i64);
        let w = self.w as i64;
        let h = self.h as i64;
        let tag = self.current_line;
        outlined.draw(|x, y, c| {
            let xi = bx + x as i64;
            let yi = byy + y as i64;
            if xi < 0 || yi < 0 || xi >= w || yi >= h {
                return;
            }
            let k = (yi * w + xi) as usize;
            let v = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
            if v > self.cov[k] {
                self.cov[k] = v;
                self.line[k] = tag;
                self.letter[k] = self.current_letter;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playfair_metrics_are_the_real_font() {
        let m = metrics();
        // Measured from the embedded file: x-height 517/1000, caps 708.
        assert!((m.x_height - 0.517).abs() < 0.02, "x {}", m.x_height);
        assert!((m.cap_height - 0.708).abs() < 0.02, "cap {}", m.cap_height);
        assert!(m.ascender > m.cap_height);
        assert!(m.descender < -0.1);
    }

    #[test]
    fn ink_extent_tracks_each_glyphs_real_shape() {
        // Per-glyph ink, with GENUINE font relationships (no fabricated
        // magnitudes): 'h' ink tops ABOVE cap height but BELOW the
        // em-box ascender (measured 0.782); x-height letters top near
        // the x-height; 'g' dips below the baseline. The whole-bloom
        // clearance targets THESE per-letter shapes, not one bound.
        let m = metrics();
        let (h_top, h_bot) = ink_extent('h');
        let (x_top, _) = ink_extent('x');
        let (g_top, g_bot) = ink_extent('g');
        let (o_top, o_bot) = ink_extent('o');
        // 'h' ink (ascender) vs the font's own measured metrics.
        assert!(
            (h_top - m.ascender).abs() < 0.005,
            "h ink top {h_top} vs metrics ascender {}",
            m.ascender
        );
        assert!(
            h_top > m.cap_height,
            "ascender h {h_top} must top caps {}",
            m.cap_height
        );
        assert!(
            h_top < 0.85,
            "h ink top {h_top} is ink, not the em-box ascender"
        );
        assert!(
            (x_top - m.x_height).abs() < 0.02,
            "x top {x_top} vs x-height {}",
            m.x_height
        );
        assert!((o_top - m.x_height).abs() < 0.05, "o top {o_top}");
        assert!(g_bot < -0.1, "descender g bottom {g_bot}");
        assert!(o_bot > -0.06 && o_bot < 0.02, "o bottom {o_bot}");
        assert!(g_top < h_top, "g must not top the ascender h");
        assert!(h_bot > -0.06, "h sits on the baseline: bottom {h_bot}");
    }

    #[test]
    fn mixed_case_latin_cyrillic_supported_cjk_not() {
        assert!(glyph_supported(
            "Don't Stop \u{2014} Can't, Won't! (q&a) 42?"
        ));
        assert!(glyph_supported(
            "caf\u{e9} \u{416}\u{438}\u{437}\u{43d}\u{44c}"
        ));
        assert!(!glyph_supported("\u{4f60}\u{597d}"));
    }

    #[test]
    fn advances_are_proportional_and_space_widened() {
        assert!(advance('i').unwrap() < advance('m').unwrap());
        let sp = advance(' ').unwrap();
        assert!((sp - 0.233 * 1.4).abs() < 0.02, "space {sp}");
    }

    #[test]
    fn glyph_rasterizes_with_antialiased_coverage() {
        // An 'o' at 8 design units em, 4 subpixels per unit: a ring with
        // solid ink, partial edge coverage and an empty counter.
        let mut m = TypeMask::new(64, 64);
        m.draw_glyph('o', 8.0, 12.0, 8.0, 4.0);
        let full = m.cov.iter().filter(|&&c| c == 255).count();
        let partial = m.cov.iter().filter(|&&c| c > 0 && c < 255).count();
        assert!(full > 40, "solid ink {full}");
        assert!(partial > 20, "anti-aliased edge {partial}");
        // The counter (centre at x-height/2 above baseline) is empty.
        let cx = 32usize;
        let cy = ((12.0 - 8.0 * 0.26) * 4.0) as usize;
        assert_eq!(m.at(cx, cy), 0, "counter of o must be open");
    }
}
