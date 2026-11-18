//! Cell-grid ASCII rasterizer, ported from ascii-garden's `engine/ascii.ts`
//! (upstream f519e85, MIT (c) 2026 Ridit; see NOTICE). The garden is drawn
//! with vector calls into a hidden low-resolution mask - here a role index
//! per subpixel instead of a canvas - and every cell is then read back and
//! turned into text: mostly covered cells become a colour block with a
//! dense glyph, elongated shapes become angle glyphs (- \\ | /), and partly
//! covered cells fade into the background through the density RAMP.
//!
//! Terminal geometry: one cell = 4x6 subpixels (square subpixels at the
//! reference's 2:3 cell aspect - upstream's cw 10 px x ch 15 px, its sx
//! 4 x sy 6), replacing the canvas' sampling with the same subpixel size.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};

/// Subpixels per cell along x and y (square at the 2:3 cell aspect:
/// upstream's own sx=4, sy=6 over its 10x15 px cells).
pub const SX: usize = 4;
pub const SY: usize = 6;
/// One terminal row in design units: the reference's cells are 10x15 px
/// (a 2:3 aspect, measured on the running upstream surface - all three
/// layers are exactly 144 cols x 60 rows at 1440x900), so a row is
/// 1.5x the width of a column. Every design-unit <-> row conversion
/// goes through this constant; the previous 2.0 assumed 1:2 cells and
/// rendered the word too wide/squashed (QA-measured on actual pixels).
pub const ROW_DESIGN: f32 = 1.5;
/// Samples per cell.
pub const N_SUB: usize = SX * SY;
/// Moment coordinates are in reference CELL-PIXEL units, not normalized
/// fractions: upstream's `axis()` regularizer (+0.35) is calibrated to
/// pixel-scale variances (`pxs = (i+0.5)*cw/sx`, cw ~ 10 px). With 0..1
/// normalized coordinates the variances are ~100x smaller, the 0.35
/// dominates, and every ratio lands far below `lineish = 1.9` - nothing
/// ever becomes a line glyph, so stems render as ramp dots instead of
/// continuous |/\ segments and solid-rose secondary cells pick 'o'
/// instead of line glyphs (both observed in actual terminal frames).
/// Our design cell is 1.0 x 1.5 (the reference's 2:3 cell, 10x15 px);
/// scaled to upstream's pixel units that is 10 x 15.
const MOMENT_CW: f64 = 10.0;
const MOMENT_CH: f64 = 15.0;
/// Subpixels per design unit (square: 4 across a 1.0-wide cell, 6 down
/// a 1.5-tall one).
pub const SUB: f32 = SX as f32;
/// Minimum stroke width in design units: upstream's `minLw = cw * 0.5`
/// (half a cell) so thin stems still register as a glyph in every cell
/// they cross instead of breaking into fragments.
pub const MIN_LW: f32 = 0.5;

/// HARD user-facing acceptance: the visible front-layer garden may
/// cover STRICTLY LESS than this fraction of EACH letter's ink (the
/// letter's ink = its type-mask subpixels at cov >= 128) - per letter,
/// never word-averaged. The AUDITS assert this bound; it is the user's
/// acceptance and must not be relaxed.
pub const MAX_LETTER_COVER: f32 = 0.2;

/// INTERNAL enforcement budget: 2 safety-margin points below
/// MAX_LETTER_COVER. Both enforcement layers (the draw-time subpixel
/// cap and the presentation budget) enforce THIS, because glyph
/// rasterization and antialiasing edges add garden-coloured pixels
/// beyond what the cell-level buffer audit can measure: QA's pixel
/// audit measured 20.192% on the second 'l' of hello at 4.5 s against
/// a ~19.9% buffer bound (+0.33 pp at saturation). Enforcing at 18%
/// keeps the ACTUAL PIXELS strictly under the 20% acceptance at every
/// frame. The audits stay at MAX_LETTER_COVER - enforcement is
/// conservative, measurement is not.
pub const BUDGET_COVER: f32 = 0.18;

/// The reference's "Paper" palette row plus tone derivatives
/// (shadow = mix toward black 0.46, light = mix toward white 0.42).
pub const PAPER_BG: (u8, u8, u8) = (243, 238, 228);
pub const FLOWER: (u8, u8, u8) = (255, 59, 31);
pub const STEM: (u8, u8, u8) = (28, 43, 143);
pub const FLOWER_LINE: (u8, u8, u8) = (255, 210, 196);
pub const TEXT: (u8, u8, u8) = (17, 17, 17);

pub fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let l = |x: u8, y: u8| {
        (x as f32 + (y as f32 - x as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (l(a.0, b.0), l(a.1, b.1), l(a.2, b.2))
}

pub const BLACK: (u8, u8, u8) = (0, 0, 0);
pub const WHITE: (u8, u8, u8) = (255, 255, 255);
const SPARKLE: (u8, u8, u8) = (255, 250, 240);

/// Roles: 0 background, 1 flower, 2 stem/leaf, 3 flower line, 4 type,
/// 5 flower shadow, 6 flower highlight, 7 leaf shadow, 8 leaf highlight,
/// then PER-KIND tone roles (dark/base/light, one family per species) -
/// the same shape upstream's setRoles() appends for each flower kind
/// (ascii.ts KIND_GLYPHS: three glyph sets per species, [shadow, base,
/// highlight], so each flower has a texture of its own as well as a
/// colour). Species colors: garden.ts KIND_COLOR; tones() is the same
/// mix we use everywhere (dark = black 0.46, light = white 0.42).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Role {
    Bg = 0,
    Flower = 1,
    Stem = 2,
    Line = 3,
    Type = 4,
    FlowerShadow = 5,
    FlowerLight = 6,
    LeafShadow = 7,
    LeafLight = 8,
    DaisyDark = 9,
    Daisy = 10,
    DaisyLight = 11,
    TulipDark = 12,
    Tulip = 13,
    TulipLight = 14,
    PoppyDark = 15,
    Poppy = 16,
    PoppyLight = 17,
    BlossomDark = 18,
    Blossom = 19,
    BlossomLight = 20,
    LilyDark = 21,
    Lily = 22,
    LilyLight = 23,
    BellDark = 24,
    Bell = 25,
    BellLight = 26,
    LavenderDark = 27,
    Lavender = 28,
    LavenderLight = 29,
    FernDark = 30,
    Fern = 31,
    FernLight = 32,
    DandelionDark = 33,
    Dandelion = 34,
    DandelionLight = 35,
}

pub const ROLE_COUNT: usize = 36;

/// Each registered flower species' base colour (garden.ts KIND_COLOR;
/// the rose uses the palette's flower colour - our Role::Flower).
/// Blossom is the source's PULLED tone on Paper: #FFB6D6 sits 59 from
/// the background, so defaultKindColor pulls it halfway toward the
/// type colour - ((255+17)/2, (182+17)/2, (214+17)/2) = (136,100,116).
/// Lily #E27BFF is 119 from the bg (>= 110) and keeps its own hue.
pub const DAISY: (u8, u8, u8) = (255, 225, 77);
pub const TULIP: (u8, u8, u8) = (255, 138, 31);
pub const POPPY: (u8, u8, u8) = (255, 59, 107);
pub const BLOSSOM: (u8, u8, u8) = (136, 100, 116);
pub const LILY: (u8, u8, u8) = (226, 123, 255);
pub const BELL: (u8, u8, u8) = (95, 178, 255);
pub const LAVENDER: (u8, u8, u8) = (154, 120, 255);
pub const FERN: (u8, u8, u8) = (69, 212, 131);
/// Dandelion is the second PULLED tone: #EDEBD8 (237,235,216) is 13.9
/// from the bg, so defaultKindColor pulls it halfway toward the TYPE
/// colour #111111 (17,17,17): (127, 126, 117) - spread 10, sum 370.
/// Upstream's own design; the grey is INK-AMBIGUOUS to colour-only
/// audits (see mod's ink_classifier test and the provenance oracle).
pub const DANDELION: (u8, u8, u8) = (127, 126, 117);

/// Colours of each role on the Paper palette.
pub fn role_color(r: Role) -> (u8, u8, u8) {
    match r {
        Role::Bg => PAPER_BG,
        Role::Flower => FLOWER,
        Role::Stem => STEM,
        Role::Line => FLOWER_LINE,
        Role::Type => TEXT,
        Role::FlowerShadow => mix(FLOWER, BLACK, 0.46),
        Role::FlowerLight => mix(FLOWER, WHITE, 0.42),
        Role::LeafShadow => mix(STEM, BLACK, 0.46),
        Role::LeafLight => mix(STEM, WHITE, 0.42),
        Role::DaisyDark => mix(DAISY, BLACK, 0.46),
        Role::Daisy => DAISY,
        Role::DaisyLight => mix(DAISY, WHITE, 0.42),
        Role::TulipDark => mix(TULIP, BLACK, 0.46),
        Role::Tulip => TULIP,
        Role::TulipLight => mix(TULIP, WHITE, 0.42),
        Role::PoppyDark => mix(POPPY, BLACK, 0.46),
        Role::Poppy => POPPY,
        Role::PoppyLight => mix(POPPY, WHITE, 0.42),
        Role::BlossomDark => mix(BLOSSOM, BLACK, 0.46),
        Role::Blossom => BLOSSOM,
        Role::BlossomLight => mix(BLOSSOM, WHITE, 0.42),
        Role::LilyDark => mix(LILY, BLACK, 0.46),
        Role::Lily => LILY,
        Role::LilyLight => mix(LILY, WHITE, 0.42),
        Role::BellDark => mix(BELL, BLACK, 0.46),
        Role::Bell => BELL,
        Role::BellLight => mix(BELL, WHITE, 0.42),
        Role::LavenderDark => mix(LAVENDER, BLACK, 0.46),
        Role::Lavender => LAVENDER,
        Role::LavenderLight => mix(LAVENDER, WHITE, 0.42),
        Role::FernDark => mix(FERN, BLACK, 0.46),
        Role::Fern => FERN,
        Role::FernLight => mix(FERN, WHITE, 0.42),
        Role::DandelionDark => mix(DANDELION, BLACK, 0.46),
        Role::Dandelion => DANDELION,
        Role::DandelionLight => mix(DANDELION, WHITE, 0.42),
    }
}

/// Which family a role belongs to (flower / stem / line / type, then one
/// per species - upstream's setRoles gives each kind its own family id
/// starting at 5, so kind tones group for the axis math and read as
/// distinct secondary families).
pub const FAM: [u8; ROLE_COUNT] = [
    0, 1, 2, 3, 4, 1, 1, 2, 2, 5, 5, 5, 6, 6, 6, 7, 7, 7, 8, 8, 8, 9, 9, 9, 10, 10, 10, 11, 11, 11,
    12, 12, 12, 13, 13, 13,
];
/// 0 base colour, 1 shadow, 2 highlight.
pub const TONE: [u8; ROLE_COUNT] = [
    0, 0, 0, 0, 0, 1, 2, 1, 2, 1, 0, 2, 1, 0, 2, 1, 0, 2, 1, 0, 2, 1, 0, 2, 1, 0, 2, 1, 0, 2, 1, 0,
    2, 1, 0, 2,
];
/// Glyph family per role (ascii.ts BASE_SETS + KIND_GLYPHS).
pub const SETS: [&str; ROLE_COUNT] = [
    "", "O0o*&", ":;+=x*", "()/~|", "#@8&%B", "@%#8&W", ".:'`,", "#%=xX", ".:'`;",
    // daisy / tulip / poppy (KIND_GLYPHS: shadow, base, highlight)
    "@08O", "o*:+.", "'`.,;", "HM#%W", "Yy+*=", ".'`:", "&@%#8", "O0o*^", "`'.,:",
    // blossom / lily
    "%&$@8", "*+x:;", ".'`,-", "W#M%@", "&Zz+=", "`.'~:",
    // bell / lavender / fern / dandelion
    "B#8%@", "nu+=:", ".'`;", "!|#I1", "li!:;", ".'`,:", "X%#/Y", "/V+x*", ".:'`;", "O@0&#",
    "o*+:.", "'`.,-",
];
const RAMP: &[u8] = b".,':;+=";

/// Cheap integer hash -> [0, 1). (ascii.ts hash3; JS `>>>` is a LOGICAL
/// shift, so this runs entirely in u32 - an arithmetic i32 shift would
/// skew the distribution.)
pub fn hash3(x: i32, y: i32, z: i32) -> f64 {
    let mut h =
        (x.wrapping_mul(374761393) ^ y.wrapping_mul(668265263) ^ z.wrapping_mul(1442695041)) as u32;
    h = (h ^ (h >> 13)).wrapping_mul(1274126177);
    h ^= h >> 16;
    h as f64 / 4294967296.0
}

/// The line glyph closest to a screen-space angle (y down). (ascii.ts.)
pub fn line_glyph(angle: f64) -> char {
    let mut a = angle % std::f64::consts::PI;
    if a < 0.0 {
        a += std::f64::consts::PI;
    }
    let pi = std::f64::consts::PI;
    if a < pi / 8.0 || a >= 7.0 * pi / 8.0 {
        '-'
    } else if a < 3.0 * pi / 8.0 {
        '\\'
    } else if a < 5.0 * pi / 8.0 {
        '|'
    } else {
        '/'
    }
}

/// A glyph colour that reads against a block of `fill`. (ascii.ts onFill.)
fn on_fill(f: (u8, u8, u8)) -> (u8, u8, u8) {
    let lum = (0.3 * f.0 as f32 + 0.59 * f.1 as f32 + 0.11 * f.2 as f32) / 255.0;
    if lum > 0.55 {
        mix(f, BLACK, 0.58)
    } else {
        (
            (f.0 as f32 * 1.45 + 55.0).min(255.0) as u8,
            (f.1 as f32 * 1.45 + 55.0).min(255.0) as u8,
            (f.2 as f32 * 1.45 + 55.0).min(255.0) as u8,
        )
    }
}

/// The role whose colour is nearest to `c` (the 15-bit LUT's semantics).
fn nearest_role(c: (u8, u8, u8), candidates: &[Role]) -> Role {
    let mut best = candidates[0];
    let mut bd = f32::MAX;
    for &r in candidates {
        let q = role_color(r);
        let d = (c.0 as f32 - q.0 as f32).powi(2)
            + (c.1 as f32 - q.1 as f32).powi(2)
            + (c.2 as f32 - q.2 as f32).powi(2);
        if d < bd {
            bd = d;
            best = r;
        }
    }
    best
}

/// A point in design space: x in cell widths, y in design units (one
/// terminal cell = 1.0 x [`ROW_DESIGN`], so subpixels are square).
#[derive(Clone, Copy, Debug)]
pub struct Pt(pub f32, pub f32);

/// The mask: one role index per subpixel.
pub struct MaskGrid {
    pub w: usize,
    pub h: usize,
    pub role: Vec<u8>,
    /// Which type line (index + 1) owns each type subpixel; 0 = none.
    pub owner: Vec<u8>,
    /// Which LETTER (scene index + 1; 0 = not type) owns each type
    /// subpixel - copied from the type layer when it is applied, so
    /// the per-letter cover cap and audit can target EACH letter (the
    /// line-based `owner` cannot).
    pub letter: Vec<u16>,
    /// While drawing a front-layer element of line `guard - 1`, other
    /// lines' letters are protected (0 = no guard). Upstream lays out one
    /// line and sends flowers over the letter body mostly behind it; with
    /// wrapped lyrics the same rule extends to every other line.
    pub guard: u8,
    /// Front-pass per-letter cover budget (see [`BUDGET_COVER`]).
    cover: Option<CoverCap>,
}

/// Per-letter ink totals and covered-so-far counts for the cover cap.
struct CoverCap {
    totals: Vec<u32>,
    covered: Vec<u32>,
    max_frac: f32,
}

#[cfg(test)]
thread_local! {
    /// Differential-test switch: when set, the polygon fills delegate to
    /// the ORIGINAL per-point reference implementations, so a test can
    /// render the SAME scene/matrix twice (reference vs scanline) and
    /// byte-compare the results - proof, not comment claims.
    static USE_REFERENCE_FILLS: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn use_reference_fills() -> bool {
    USE_REFERENCE_FILLS.with(|f| f.get())
}

/// Differential-test switch setter (QA 10:27): forces the polygon fills
/// onto the ORIGINAL per-point reference implementations so whole
/// scenes can be rendered twice and byte-compared. Test-only.
#[cfg(test)]
pub fn set_reference_fills_for_tests(on: bool) {
    USE_REFERENCE_FILLS.with(|f| f.set(on));
}

impl MaskGrid {
    pub fn new(cols: usize, rows: usize) -> Self {
        let n = cols * SX * rows * SY;
        Self {
            w: cols * SX,
            h: rows * SY,
            role: vec![0; n],
            owner: vec![0; n],
            letter: vec![0; n],
            guard: 0,
            cover: None,
        }
    }

    pub fn clear(&mut self) {
        self.role.fill(0);
        self.owner.fill(0);
        self.letter.fill(0);
        self.guard = 0;
        self.cover = None;
    }

    #[inline]
    fn set(&mut self, i: usize, j: usize, role: Role) {
        if i < self.w && j < self.h {
            let k = j * self.w + i;
            let o = self.owner[k];
            if self.guard != 0 && o != 0 && o != self.guard {
                return;
            }
            // Per-letter cover cap (armed for the front pass): the
            // FIRST overwrite of a letter's ink subpixel counts against
            // that letter's budget; a sample that would reach the
            // internal BUDGET_COVER is dropped, so the letter keeps
            // strictly more than 82% of its ink while the flower keeps
            // everything it draws outside the letter.
            if self.role[k] == Role::Type as u8 {
                let l = self.letter[k] as usize;
                if l != 0 {
                    if let Some(cap) = self.cover.as_mut() {
                        let tot = cap.totals[l] as f32;
                        if (cap.covered[l] + 1) as f32 >= cap.max_frac * tot {
                            return;
                        }
                        cap.covered[l] += 1;
                    }
                }
            }
            self.role[k] = role as u8;
        }
    }

    /// Arm the per-letter cover cap for the front pass: `totals` is the
    /// ink-subpixel count per letter id (index 0 unused). Enforces the
    /// INTERNAL budget (see [`BUDGET_COVER`]).
    pub fn begin_letter_cover(&mut self, totals: Vec<u32>) {
        let covered = vec![0; totals.len()];
        self.cover = Some(CoverCap {
            totals,
            covered,
            max_frac: BUDGET_COVER,
        });
    }

    /// Disarm the per-letter cover cap.
    pub fn end_letter_cover(&mut self) {
        self.cover = None;
    }

    /// The cover budget counters (covered-so-far per letter id), for
    /// the differential byte-equivalence proof; empty when not armed.
    #[cfg(test)]
    pub fn cover_counters(&self) -> Vec<u32> {
        self.cover
            .as_ref()
            .map(|c| c.covered.clone())
            .unwrap_or_default()
    }

    fn bbox_sub(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> (usize, usize, usize, usize) {
        let to_i = |x: f32| (x * SX as f32).floor().clamp(0.0, self.w as f32) as usize;
        let to_j = |y: f32| (y * SX as f32).floor().clamp(0.0, self.h as f32) as usize; // square subpixels
        let i0 = to_i(x0);
        let j0 = to_j(y0);
        let i1 = (to_i(x1) + 1).min(self.w).max(i0);
        let j1 = (to_j(y1) + 1).min(self.h).max(j0);
        (i0, j0, i1, j1)
    }

    #[inline]
    fn sample_xy(i: usize, j: usize) -> (f32, f32) {
        // Subpixel centres in design space.
        ((i as f32 + 0.5) / SX as f32, (j as f32 + 0.5) / SX as f32)
    }

    /// Fill an axis-aligned rectangle (design space).
    pub fn fill_rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, role: Role) {
        let (i0, j0, i1, j1) = self.bbox_sub(x0, y0, x1.max(x0), y1.max(y0));
        for j in j0..j1 {
            for i in i0..i1 {
                let (px, py) = Self::sample_xy(i, j);
                if px >= x0 && px <= x1 && py >= y0 && py <= y1 {
                    self.set(i, j, role);
                }
            }
        }
    }

    /// Even-odd edge x-crossings of a closed polygon with the horizontal
    /// line at `py` - EXACTLY the original per-point crossing arithmetic
    /// (same expression, same f32 ops), computed once per row instead of
    /// once per subpixel. The spanning rule matches point_in_poly.
    fn row_crossings(pts: &[Pt], py: f32, out: &mut Vec<f32>) {
        out.clear();
        let n = pts.len();
        let mut k = n - 1;
        for i in 0..n {
            let (xi, yi) = (pts[i].0, pts[i].1);
            let (xk, yk) = (pts[k].0, pts[k].1);
            if (yi > py) != (yk > py) {
                out.push((xk - xi) * (py - yi) / (yk - yi) + xi);
            }
            k = i;
        }
    }

    /// Fill a closed polygon (even-odd, design space) - SCANLINE: edge
    /// crossings are computed once per subpixel row (identical f32
    /// crossing arithmetic), then the sorted pair spans decide each
    /// candidate subpixel with the ORIGINAL comparison orientation
    /// (!(px < lo) && (px < hi) = the per-point crossing parity), so the
    /// fill is byte-identical to the per-point reference
    /// (differential-tested). O(rows x edges + filled) instead of
    /// O(subpixels x edges).
    pub fn fill_poly(&mut self, pts: &[Pt], role: Role) {
        #[cfg(test)]
        if use_reference_fills() {
            return self.fill_poly_reference(pts, role);
        }
        if pts.len() < 3 {
            return;
        }
        let (x0, y0, x1, y1) = poly_bbox(pts);
        let (i0, j0, i1, j1) = self.bbox_sub(x0, y0, x1, y1);
        let mut xs: Vec<f32> = Vec::with_capacity(pts.len());
        for j in j0..j1 {
            let py = (j as f32 + 0.5) / SX as f32;
            Self::row_crossings(pts, py, &mut xs);
            if xs.len() < 2 {
                continue;
            }
            xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let mut m = 0;
            while m + 1 < xs.len() {
                self.fill_row_span(j, i0, i1, xs[m], xs[m + 1], |g, i, j| {
                    g.set(i, j, role);
                });
                m += 2;
            }
        }
    }

    /// Fill a polygon with a radial shade (per-point reference for the
    /// semantics) - SCANLINE interior test; the gradient per FILLED
    /// subpixel is identical to the reference.
    pub fn fill_grad_poly(
        &mut self,
        pts: &[Pt],
        cx: f32,
        cy: f32,
        r: f32,
        stops: &[(f32, (u8, u8, u8))],
        candidates: &[Role],
    ) {
        #[cfg(test)]
        if use_reference_fills() {
            return self.fill_grad_poly_reference(pts, cx, cy, r, stops, candidates);
        }
        if pts.len() < 3 || !r.is_finite() || r <= 0.0 {
            return;
        }
        let (x0, y0, x1, y1) = poly_bbox(pts);
        let (i0, j0, i1, j1) = self.bbox_sub(x0, y0, x1, y1);
        let mut xs: Vec<f32> = Vec::with_capacity(pts.len());
        for j in j0..j1 {
            let py = (j as f32 + 0.5) / SX as f32;
            Self::row_crossings(pts, py, &mut xs);
            if xs.len() < 2 {
                continue;
            }
            xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let mut m = 0;
            while m + 1 < xs.len() {
                let (lo, hi) = (xs[m], xs[m + 1]);
                self.fill_row_span(j, i0, i1, lo, hi, |g, i, j| {
                    let (px, py) = Self::sample_xy(i, j);
                    let t = (((px - cx).powi(2) + (py - cy).powi(2)).sqrt() / r).clamp(0.0, 1.0);
                    let col = color_at(stops, t);
                    g.set(i, j, nearest_role(col, candidates));
                });
                m += 2;
            }
        }
    }

    /// Fill the subpixels of row `j` whose centre lies in the span
    /// `[lo, hi)` - decided with the EXACT original comparison
    /// orientation (!(px < lo) && (px < hi)), over a candidate range
    /// widened by one subpixel on each side so float boundary cases
    /// are decided by the comparisons, never by the range arithmetic.
    /// `paint` receives every subpixel the original per-point rule would
    /// fill (and only those).
    fn fill_row_span(
        &mut self,
        j: usize,
        i0: usize,
        i1: usize,
        lo: f32,
        hi: f32,
        paint: impl Fn(&mut Self, usize, usize),
    ) {
        let cand_lo = ((lo * SX as f32 - 0.5).ceil() as isize - 1).max(i0 as isize);
        let cand_hi = ((hi * SX as f32 - 0.5).ceil() as isize + 1).min(i1 as isize);
        for i in cand_lo.max(0)..cand_hi.max(0) {
            let (px, _) = Self::sample_xy(i as usize, j);
            if px >= lo && px < hi {
                paint(self, i as usize, j);
            }
        }
    }

    /// The ORIGINAL per-point even-odd fills, kept EXACTLY as they were
    /// as differential-test references: the scanline fills above must be
    /// byte-identical to these on every polygon the garden draws.
    #[cfg(test)]
    pub fn fill_poly_reference(&mut self, pts: &[Pt], role: Role) {
        if pts.len() < 3 {
            return;
        }
        let (x0, y0, x1, y1) = poly_bbox(pts);
        let (i0, j0, i1, j1) = self.bbox_sub(x0, y0, x1, y1);
        for j in j0..j1 {
            for i in i0..i1 {
                let (px, py) = Self::sample_xy(i, j);
                if point_in_poly(px, py, pts) {
                    self.set(i, j, role);
                }
            }
        }
    }

    #[cfg(test)]
    pub fn fill_grad_poly_reference(
        &mut self,
        pts: &[Pt],
        cx: f32,
        cy: f32,
        r: f32,
        stops: &[(f32, (u8, u8, u8))],
        candidates: &[Role],
    ) {
        if pts.len() < 3 || !r.is_finite() || r <= 0.0 {
            return;
        }
        let (x0, y0, x1, y1) = poly_bbox(pts);
        let (i0, j0, i1, j1) = self.bbox_sub(x0, y0, x1, y1);
        for j in j0..j1 {
            for i in i0..i1 {
                let (px, py) = Self::sample_xy(i, j);
                if !point_in_poly(px, py, pts) {
                    continue;
                }
                let t = (((px - cx).powi(2) + (py - cy).powi(2)).sqrt() / r).clamp(0.0, 1.0);
                let col = color_at(stops, t);
                self.set(i, j, nearest_role(col, candidates));
            }
        }
    }

    /// Stroke an open polyline with round caps (design space). The
    /// terminal replacement for canvas lineWidth strokes.
    pub fn stroke_polyline(&mut self, pts: &[Pt], w: f32, role: Role) {
        if pts.len() < 2 || w <= 0.0 {
            return;
        }
        let half = w.max(MIN_LW) / 2.0;
        // Segment by segment over each segment's own (padded) box: the
        // union of round-capped segments is exactly the stroked polyline,
        // at a cost proportional to the stroke's area rather than to its
        // bounding box times its segment count.
        for seg in pts.windows(2) {
            let (a, b) = (seg[0], seg[1]);
            let (i0, j0, i1, j1) = self.bbox_sub(
                a.0.min(b.0) - half,
                a.1.min(b.1) - half,
                a.0.max(b.0) + half,
                a.1.max(b.1) + half,
            );
            for j in j0..j1 {
                for i in i0..i1 {
                    let (px, py) = Self::sample_xy(i, j);
                    if dist_to_polyline(px, py, seg) <= half {
                        self.set(i, j, role);
                    }
                }
            }
        }
    }

    /// Stroke along a closed polygon's outline (petal rims).
    pub fn outline_poly(&mut self, pts: &[Pt], w: f32, role: Role) {
        if pts.len() < 3 {
            return;
        }
        let mut closed: Vec<Pt> = pts.to_vec();
        closed.push(pts[0]);
        self.stroke_polyline(&closed, w, role);
    }

    /// Stroke only pts[a..=b] of a longer polyline (strokeRange, t in 0..1).
    pub fn stroke_range(&mut self, pts: &[Pt], a: f32, b: f32, w: f32, role: Role) {
        if pts.len() < 2 || b <= a {
            return;
        }
        let n = (pts.len() - 1) as f32;
        let ia = (a * n).floor().max(0.0) as usize;
        let ib = ((b * n).ceil() as usize).min(pts.len() - 1);
        if ib <= ia {
            return;
        }
        let slice = &pts[ia..=ib];
        self.stroke_polyline(slice, w, role);
    }
}

fn poly_bbox(pts: &[Pt]) -> (f32, f32, f32, f32) {
    let mut x0 = f32::MAX;
    let mut y0 = f32::MAX;
    let mut x1 = f32::MIN;
    let mut y1 = f32::MIN;
    for p in pts {
        x0 = x0.min(p.0);
        y0 = y0.min(p.1);
        x1 = x1.max(p.0);
        y1 = y1.max(p.1);
    }
    (x0, y0, x1, y1)
}

#[cfg(test)] // scanline fills replaced the per-point rule; the
             // reference implementations below keep it for the differential test
fn point_in_poly(x: f32, y: f32, pts: &[Pt]) -> bool {
    let mut inside = false;
    let n = pts.len();
    let mut k = n - 1;
    for i in 0..n {
        let (xi, yi) = (pts[i].0, pts[i].1);
        let (xk, yk) = (pts[k].0, pts[k].1);
        if ((yi > y) != (yk > y)) && (x < (xk - xi) * (y - yi) / (yk - yi) + xi) {
            inside = !inside;
        }
        k = i;
    }
    inside
}

fn dist_to_polyline(x: f32, y: f32, pts: &[Pt]) -> f32 {
    let mut best = f32::MAX;
    for w in pts.windows(2) {
        let (ax, ay) = (w[0].0, w[0].1);
        let (bx, by) = (w[1].0, w[1].1);
        let dx = bx - ax;
        let dy = by - ay;
        let len2 = dx * dx + dy * dy;
        let t = if len2 <= f32::EPSILON {
            0.0
        } else {
            (((x - ax) * dx + (y - ay) * dy) / len2).clamp(0.0, 1.0)
        };
        let px = ax + t * dx;
        let py = ay + t * dy;
        best = best.min((x - px).hypot(y - py));
    }
    best
}

fn color_at(stops: &[(f32, (u8, u8, u8))], t: f32) -> (u8, u8, u8) {
    if stops.is_empty() {
        return PAPER_BG;
    }
    if t <= stops[0].0 {
        return stops[0].1;
    }
    for w in stops.windows(2) {
        if t <= w[1].0 {
            let span = w[1].0 - w[0].0;
            let u = if span <= 0.0 {
                0.0
            } else {
                (t - w[0].0) / span
            };
            return mix(w[0].1, w[1].1, u);
        }
    }
    stops[stops.len() - 1].1
}

/// How the type layer is turned into cells.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeStyle {
    /// Upstream's per-cell glyph texture (ascii.ts present), used when a
    /// letter spans enough cells for the texture to form strokes.
    Texture,
    /// Continuous silhouettes from 2x2 quadrant blocks over the same
    /// anti-aliased Playfair coverage, with upstream's type glyph family
    /// as a faint ink-on-ink texture in solid cells. Used for smaller
    /// type, where one glyph per cell cannot draw a stroke.
    Silhouette,
}

/// Near-black ink for silhouette type (upstream text #111111, a touch
/// softer so solid letters don't read as holes in the paper).
pub const INK: (u8, u8, u8) = (24, 23, 21);

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

type Rgb = (u8, u8, u8);

/// One silhouette cell, or None when no quadrant carries type. Quadrant
/// coverage only counts subpixels whose FINAL role is type, so front-layer
/// flowers still occlude the letters exactly as upstream layers them.
fn silhouette_cell(
    grid: &MaskGrid,
    type_cov: &super::font::TypeMask,
    gi0: usize,
    gj0: usize,
    key: u32,
    colors: &[(u8, u8, u8); ROLE_COUNT],
) -> Option<(char, Rgb, Rgb)> {
    let mut cov = [0f32; 4];
    let mut other = [[0u16; ROLE_COUNT]; 4];
    for j in 0..SY {
        for i in 0..SX {
            let (gi, gj) = (gi0 + i, gj0 + j);
            if gi >= grid.w || gj >= grid.h {
                continue;
            }
            let q = (i / (SX / 2)) + 2 * (j / (SY / 2));
            let r = grid.role[gj * grid.w + gi] as usize;
            if r == Role::Type as usize {
                cov[q] += type_cov.at(gi, gj) as f32 / 255.0;
            } else {
                other[q][r] += 1;
            }
        }
    }
    let per_q = (SX / 2 * SY / 2) as f32;
    let mut bits = 0u8;
    let (mut on_sum, mut on_n) = (0f32, 0f32);
    for (q, c) in cov.iter_mut().enumerate() {
        *c /= per_q;
        if *c >= 0.4 {
            bits |= 1 << q;
            on_sum += *c;
            on_n += 1.0;
        }
    }
    if bits == 0 {
        return None;
    }
    // Anti-aliasing through colour: partially covered quadrants pull the
    // ink toward the paper, fully covered ones stay dark. The floor is
    // high (0.55): at small em sizes contrast keeps the letters readable,
    // which matters more than a soft edge there.
    let on_cov = (on_sum / on_n).clamp(0.0, 1.0);
    let ink = mix(PAPER_BG, INK, (0.55 + 0.45 * on_cov).min(1.0));
    if bits == 0b1111 {
        // Solid: ink fill, upstream's type glyph family as a faint texture.
        let set = SETS[Role::Type as usize];
        let ch = set
            .chars()
            .nth(((key >> 8) % set.chars().count() as u32) as usize)
            .unwrap_or('#');
        return Some((ch, mix(ink, PAPER_BG, 0.13), ink));
    }
    // The off quadrants show whatever is behind the type there: paper, or
    // the dominant garden role's fill.
    let mut tally = [0u16; ROLE_COUNT];
    for (q, o) in other.iter().enumerate() {
        if bits & (1 << q) == 0 {
            for (k, n) in o.iter().enumerate() {
                tally[k] += n;
            }
        }
    }
    let mut dom = 0;
    for k in 1..ROLE_COUNT {
        if tally[k] > tally[dom] {
            dom = k;
        }
    }
    let back = if dom == 0 || tally[dom] < 4 {
        PAPER_BG
    } else {
        mix(colors[dom], PAPER_BG, 0.2)
    };
    Some((quadrant_char(bits), ink, back))
}

/// Read the mask back and turn it into terminal cells (ascii.ts present()).
/// Solid cells get a fill background plus a dense glyph; soft cells get a
/// faded glyph on the paper; the texture is stable per cell (hash3), the
/// geometry supplies the animation. With [`TypeStyle::Silhouette`] the
/// type layer is drawn as continuous quadrant silhouettes instead.
///
/// Returns the PROVENANCE ORACLE: one byte per cell of `area`
/// (row-major) - [`ORACLE_INK`] where the cell presents letter ink,
/// [`ORACLE_ERASED`] where it renders garden or paper. This is the
/// authoritative visible-loss truth for audits: it classifies by the
/// PRESENTATION BRANCH that fired (role provenance), never by colour,
/// so ink-ambiguous species tones (the dandelion grey) count as
/// erasure exactly like saturated garden colours. Production callers
/// ignore the return; the audits consume it.
pub const ORACLE_INK: u8 = 1;
pub const ORACLE_ERASED: u8 = 0;
pub fn present(
    buf: &mut Buffer,
    area: Rect,
    grid: &MaskGrid,
    type_cov: Option<&super::font::TypeMask>,
    style: TypeStyle,
) -> Vec<u8> {
    let mut oracle = vec![ORACLE_ERASED; (area.width as usize) * (area.height as usize)];
    let or_idx = |row: u16, col: u16| (row as usize) * (area.width as usize) + (col as usize);
    let min_hits = ((N_SUB as f32 * 0.125).round() as usize).max(1); // 3 of 24
    let sec_hits = ((N_SUB as f32 * 0.17).round() as usize).max(1); // 4 of 24
    let colors: [(u8, u8, u8); ROLE_COUNT] = {
        let mut c = [(0u8, 0u8, 0u8); ROLE_COUNT];
        for (i, col) in c.iter_mut().enumerate() {
            *col = role_color(role_from_index(i));
        }
        c
    };

    // Per-letter PRESENTATION budget (the user-facing acceptance minus a
    // safety margin, applied per cell in deterministic row-major order):
    // a cell that renders as anything but ink erases its ORIGINAL letter
    // ink - allowed only while EVERY letter it touches stays strictly
    // under BUDGET_COVER (the internal budget, 2pp under the 20%
    // acceptance, so rasterization/AA edge bleed cannot push actual
    // pixels over it). Fuzzy edge cells (ink below min_hits, or too thin
    // for any quadrant) are handled by the same budget, not by an
    // ink-amount heuristic: when a letter's visible budget is spent, its
    // remaining ink is KEPT (quadrant cell, or a faint ink glyph).
    let mut ink_total: Vec<u32> = Vec::new();
    let mut erased: Vec<u32> = Vec::new();
    if let Some(tc) = type_cov {
        let n = tc.cov.len().min(tc.letter.len());
        for k in 0..n {
            if tc.cov[k] >= 128 {
                let l = tc.letter[k] as usize;
                if l != 0 {
                    while ink_total.len() <= l {
                        ink_total.push(0);
                        erased.push(0);
                    }
                    ink_total[l] += 1;
                }
            }
        }
    }

    for row in 0..area.height {
        for col in 0..area.width {
            let cx = area.x + col;
            let cy = area.y + row;
            if cx >= buf.area().width || cy >= buf.area().height {
                continue;
            }
            let mut count = [0u16; ROLE_COUNT];
            let mut sx = [0f64; ROLE_COUNT];
            let mut sy = [0f64; ROLE_COUNT];
            let mut sxx = [0f64; ROLE_COUNT];
            let mut syy = [0f64; ROLE_COUNT];
            let mut sxy = [0f64; ROLE_COUNT];
            let gi0 = (cx - area.x) as usize * SX;
            let gj0 = (cy - area.y) as usize * SY;
            for j in 0..SY {
                let gy = gj0 + j;
                if gy >= grid.h {
                    break;
                }
                for i in 0..SX {
                    let gi = gi0 + i;
                    if gi >= grid.w {
                        break;
                    }
                    let r = grid.role[gy * grid.w + gi] as usize;
                    if r == 0 {
                        continue;
                    }
                    // Cell-local sample coordinates in reference
                    // CELL-PIXEL units (ascii.ts pxs/pys: (i+0.5)*cw/sx).
                    // See MOMENT_CW/MOMENT_CH above for why the scale
                    // matters: the axis regularizer (+0.35) is calibrated
                    // to pixel-scale variances.
                    let px = (i as f64 + 0.5) / SX as f64 * MOMENT_CW;
                    let py = (j as f64 + 0.5) / SY as f64 * MOMENT_CH;
                    count[r] += 1;
                    sx[r] += px;
                    sy[r] += py;
                    sxx[r] += px * px;
                    syy[r] += py * py;
                    sxy[r] += px * py;
                }
            }
            // This cell's ORIGINAL letter ink, per letter, straight from
            // the type mask (independent of what the garden did to the
            // mask at draw time - the visible budget audits what reaches
            // the screen, not the subpixel bookkeeping).
            let mut cell_ink: Vec<(u16, u32)> = Vec::new();
            if let Some(tc) = type_cov {
                for j in 0..SY {
                    let gy = gj0 + j;
                    if gy >= grid.h {
                        break;
                    }
                    for i in 0..SX {
                        let gi = gi0 + i;
                        if gi >= grid.w {
                            break;
                        }
                        let k = gy * grid.w + gi;
                        if tc.cov.get(k).copied().unwrap_or(0) >= 128 {
                            let l = tc.letter.get(k).copied().unwrap_or(0);
                            if l != 0 {
                                match cell_ink.iter_mut().find(|e| e.0 == l) {
                                    Some(e) => e.1 += 1,
                                    None => cell_ink.push((l, 1)),
                                }
                            }
                        }
                    }
                }
            }
            let cell = buf.cell_mut(Position::new(cx, cy)).unwrap();
            let type_n = count[Role::Type as usize];
            let key = (hash3((cx - area.x) as i32, (cy - area.y) as i32, 17) * 4294967295.0) as u32;
            // Silhouette mode keeps its approved early path: any ink
            // tries the quadrant rendering first; below every quadrant
            // threshold the faint ink stops blocking the garden.
            if style == TypeStyle::Silhouette && type_n > 0 {
                if let Some(tc) = type_cov {
                    if let Some((ch, fg, bg)) = silhouette_cell(grid, tc, gi0, gj0, key, &colors) {
                        cell.set_char(ch);
                        cell.set_fg(rgb(fg));
                        cell.set_bg(rgb(bg));
                        oracle[or_idx(row, col)] = ORACLE_INK;
                        continue;
                    }
                    count[Role::Type as usize] = 0;
                }
            }
            let mut dom = 1;
            for k in 2..ROLE_COUNT {
                if count[k] > count[dom] {
                    dom = k;
                }
            }
            let total: u16 = count[1..].iter().sum();
            // The presentation budget: rendering this cell as garden or
            // paper erases its ink, and is allowed only while EVERY
            // letter in the cell stays strictly under BUDGET_COVER (the
            // internal safety-margin budget). Cells that present ink
            // anyway (the quadrant cell above, or
            // the classifier's own type cell) never touch the budget.
            let type_dom = dom == Role::Type as usize;
            if !cell_ink.is_empty() && !(type_dom && (total as usize) >= min_hits) {
                let budget_ok = cell_ink.iter().all(|&(l, w)| {
                    let li = l as usize;
                    ((erased[li] + w) as f32) < BUDGET_COVER * ink_total[li] as f32
                });
                if budget_ok {
                    for &(l, w) in &cell_ink {
                        erased[l as usize] += w;
                    }
                } else {
                    // Keep the ink: the quadrant cell when it can carry
                    // it, otherwise a faint but clearly ink-coloured
                    // glyph so fuzzy letter edges survive presentation.
                    if let Some(tc) = type_cov {
                        if let Some((ch, fg, bg)) =
                            silhouette_cell(grid, tc, gi0, gj0, key, &colors)
                        {
                            cell.set_char(ch);
                            cell.set_fg(rgb(fg));
                            cell.set_bg(rgb(bg));
                            oracle[or_idx(row, col)] = ORACLE_INK;
                            continue;
                        }
                    }
                    let w_all = cell_ink.iter().map(|e| e.1).sum::<u32>();
                    let frac = w_all as f32 / N_SUB as f32;
                    let ri = (((frac - 0.02).max(0.0) / 0.5) * RAMP.len() as f32)
                        .floor()
                        .clamp(0.0, (RAMP.len() - 1) as f32) as usize;
                    cell.set_char(RAMP[ri] as char);
                    cell.set_fg(rgb(mix(PAPER_BG, INK, 0.62)));
                    cell.set_bg(rgb(PAPER_BG));
                    oracle[or_idx(row, col)] = ORACLE_INK;
                    continue;
                }
            }
            if (total as usize) < min_hits {
                continue; // paper
            }
            let dom_fam = FAM[dom];
            let cov = total as f32 / N_SUB as f32;
            let roll = hash3(key as i32, 0, 7);
            let base = colors[dom];
            let black_text = dom_fam == 4 && base == (0, 0, 0);

            // Principal axis of the dominant family (ascii.ts axis()).
            let mut n = 0u16;
            let mut ax = 0f64;
            let mut ay = 0f64;
            let mut axx = 0f64;
            let mut ayy = 0f64;
            let mut axy = 0f64;
            for k in 1..ROLE_COUNT {
                if FAM[k] != dom_fam {
                    continue;
                }
                n += count[k];
                ax += sx[k];
                ay += sy[k];
                axx += sxx[k];
                ayy += syy[k];
                axy += sxy[k];
            }
            let (theta, ratio) = if n == 0 {
                (0.0, 1.0)
            } else {
                let nn = n as f64;
                let mx = ax / nn;
                let my = ay / nn;
                let cxx = axx / nn - mx * mx;
                let cyy = ayy / nn - my * my;
                let cxy = axy / nn - mx * my;
                let half = (cxx - cyy) / 2.0;
                let rad = (half * half + cxy * cxy).sqrt();
                let mean = (cxx + cyy) / 2.0;
                (
                    0.5 * (2.0 * cxy).atan2(cxx - cyy),
                    (mean + rad) / (mean - rad + 0.35),
                )
            };
            let lineish = if dom_fam == 4 { 1.5 } else { 1.9 };
            let solid = cov >= 0.8 || (cov >= 0.58 && ratio < 2.3);

            if solid {
                // A second family in the cell, e.g. a swirl line over petals.
                let mut sec = 0;
                for k in 1..ROLE_COUNT {
                    if FAM[k] != dom_fam
                        && count[k] as usize >= sec_hits
                        && (sec == 0 || count[k] > count[sec])
                    {
                        sec = k;
                    }
                }
                let fill;
                let mut glyph_col;
                let mut ch;
                if dom_fam == 4 {
                    fill = if black_text {
                        base
                    } else {
                        mix(base, PAPER_BG, 0.82)
                    };
                    glyph_col = base;
                    oracle[or_idx(row, col)] = ORACLE_INK;
                } else {
                    let tn = TONE[dom];
                    fill = mix(
                        base,
                        PAPER_BG,
                        match tn {
                            2 => 0.05,
                            1 => 0.3,
                            _ => 0.2,
                        },
                    );
                    glyph_col = on_fill(fill);
                }
                let set = SETS[dom];
                ch = set
                    .chars()
                    .nth(((key >> 8) % set.chars().count() as u32) as usize)
                    .unwrap_or('#');
                // A secondary family in the cell, e.g. a swirl line over
                // petals - but NEVER on the type: the presentation budget
                // bypasses type cells on the assumption they present ink,
                // and a garden-coloured glyph here would erase the letter
                // at presentation (the exact hole behind the 23.01%
                // audit failure: solid type cells crossed by >= sec_hits
                // garden subpixels swapped their ink glyph for a garden
                // swirl). Letter cells keep their ink glyph; the swirl
                // stays on garden-dominant cells.
                if sec != 0 && dom_fam != 4 {
                    glyph_col = colors[sec];
                    ch = if ratio > 1.6 { line_glyph(theta) } else { 'o' };
                }
                if roll < 0.045 && !black_text {
                    glyph_col = mix(glyph_col, SPARKLE, 0.75);
                }
                cell.set_char(ch);
                cell.set_fg(rgb(glyph_col));
                cell.set_bg(rgb(fill));
                continue;
            }

            if cov < 0.1 {
                continue;
            }
            let f = (0.62 - cov).clamp(0.0, 0.5) * 0.9;
            let colr = if black_text {
                base
            } else {
                mix(base, PAPER_BG, f)
            };
            let g = if ratio > lineish {
                line_glyph(theta)
            } else {
                RAMP[(((cov - 0.1) / 0.5 * RAMP.len() as f32)
                    .floor()
                    .clamp(0.0, RAMP.len() as f32 - 1.0)) as usize] as char
            };
            cell.set_char(g);
            cell.set_fg(rgb(colr));
            cell.set_bg(rgb(PAPER_BG));
            if dom == Role::Type as usize {
                oracle[or_idx(row, col)] = ORACLE_INK;
            }
        }
    }
    oracle
}

fn role_from_index(i: usize) -> Role {
    match i {
        1 => Role::Flower,
        2 => Role::Stem,
        3 => Role::Line,
        4 => Role::Type,
        5 => Role::FlowerShadow,
        6 => Role::FlowerLight,
        7 => Role::LeafShadow,
        8 => Role::LeafLight,
        9 => Role::DaisyDark,
        10 => Role::Daisy,
        11 => Role::DaisyLight,
        12 => Role::TulipDark,
        13 => Role::Tulip,
        14 => Role::TulipLight,
        15 => Role::PoppyDark,
        16 => Role::Poppy,
        17 => Role::PoppyLight,
        18 => Role::BlossomDark,
        19 => Role::Blossom,
        20 => Role::BlossomLight,
        21 => Role::LilyDark,
        22 => Role::Lily,
        23 => Role::LilyLight,
        24 => Role::BellDark,
        25 => Role::Bell,
        26 => Role::BellLight,
        27 => Role::LavenderDark,
        28 => Role::Lavender,
        29 => Role::LavenderLight,
        30 => Role::FernDark,
        31 => Role::Fern,
        32 => Role::FernLight,
        33 => Role::DandelionDark,
        34 => Role::Dandelion,
        35 => Role::DandelionLight,
        _ => Role::Bg,
    }
}

fn rgb(c: (u8, u8, u8)) -> ratatui::style::Color {
    ratatui::style::Color::Rgb(c.0, c.1, c.2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid_with(cols: usize, rows: usize, fill: impl Fn(usize, usize) -> u8) -> MaskGrid {
        let mut g = MaskGrid::new(cols, rows);
        for j in 0..g.h {
            for i in 0..g.w {
                g.role[j * g.w + i] = fill(i, j);
            }
        }
        g
    }

    #[test]
    fn line_glyph_quantizes_angles() {
        assert_eq!(line_glyph(0.0), '-');
        assert_eq!(line_glyph(std::f64::consts::FRAC_PI_2), '|');
        assert_eq!(line_glyph(std::f64::consts::FRAC_PI_4), '\\');
        assert_eq!(line_glyph(-std::f64::consts::FRAC_PI_4), '/');
    }

    #[test]
    fn hash3_is_deterministic_and_spread() {
        let a = hash3(3, 4, 17);
        assert_eq!(a, hash3(3, 4, 17));
        assert!((0.0..1.0).contains(&a));
        // Over many keys the output must spread across [0, 1).
        let n: i32 = 512;
        let buckets = (0..n).filter(|k| hash3(*k, k * 31, 7) < 0.5).count() as i32;
        assert!(
            (n / 4..n * 3 / 4).contains(&buckets),
            "hash3 should spread, got {buckets}/{n}"
        );
    }

    #[test]
    fn tones_match_reference_mixes() {
        // shadow = mix toward black 0.46, light = toward white 0.42.
        assert_eq!(mix(FLOWER, BLACK, 0.46), role_color(Role::FlowerShadow));
        assert_eq!(mix(FLOWER, WHITE, 0.42), role_color(Role::FlowerLight));
    }

    #[test]
    fn kind_colours_match_garden_ts_kind_color() {
        // garden.ts KIND_COLOR: daisy #FFE14D, tulip #FF8A1F, poppy
        // #FF3B6B, blossom #FFB6D6, lily #E27BFF (the rose uses the
        // palette flower colour - FLOWER). Blossom's base is the source
        // PULLED tone on Paper (defaultKindColor: within 110 of the
        // bg pulls halfway toward the type colour):
        // ((255+17)/2, (182+17)/2, (214+17)/2).
        assert_eq!(DAISY, (255, 225, 77));
        assert_eq!(TULIP, (255, 138, 31));
        assert_eq!(POPPY, (255, 59, 107));
        assert_eq!(BLOSSOM, (136, 100, 116), "pulled halfway to text");
        assert_eq!(LILY, (226, 123, 255), "119 from bg: keeps its hue");
        assert_eq!(BELL, (95, 178, 255));
        assert_eq!(LAVENDER, (154, 120, 255));
        assert_eq!(FERN, (69, 212, 131));
        assert_eq!(DANDELION, (127, 126, 117), "13.9 from bg: pulled grey");
        // Kind tones use the reference tones() mixes, like every role.
        assert_eq!(mix(DAISY, BLACK, 0.46), role_color(Role::DaisyDark));
        assert_eq!(mix(DAISY, WHITE, 0.42), role_color(Role::DaisyLight));
        assert_eq!(mix(TULIP, BLACK, 0.46), role_color(Role::TulipDark));
        assert_eq!(mix(TULIP, WHITE, 0.42), role_color(Role::TulipLight));
        assert_eq!(mix(POPPY, BLACK, 0.46), role_color(Role::PoppyDark));
        assert_eq!(mix(POPPY, WHITE, 0.42), role_color(Role::PoppyLight));
        assert_eq!(mix(BLOSSOM, BLACK, 0.46), role_color(Role::BlossomDark));
        assert_eq!(mix(BLOSSOM, WHITE, 0.42), role_color(Role::BlossomLight));
        assert_eq!(mix(LILY, BLACK, 0.46), role_color(Role::LilyDark));
        assert_eq!(mix(LILY, WHITE, 0.42), role_color(Role::LilyLight));
        assert_eq!(mix(BELL, BLACK, 0.46), role_color(Role::BellDark));
        assert_eq!(mix(BELL, WHITE, 0.42), role_color(Role::BellLight));
        assert_eq!(mix(LAVENDER, BLACK, 0.46), role_color(Role::LavenderDark));
        assert_eq!(mix(LAVENDER, WHITE, 0.42), role_color(Role::LavenderLight));
        assert_eq!(mix(FERN, BLACK, 0.46), role_color(Role::FernDark));
        assert_eq!(mix(FERN, WHITE, 0.42), role_color(Role::FernLight));
        assert_eq!(mix(DANDELION, BLACK, 0.46), role_color(Role::DandelionDark));
        assert_eq!(
            mix(DANDELION, WHITE, 0.42),
            role_color(Role::DandelionLight)
        );
    }

    #[test]
    fn registered_kind_tones_stay_outside_the_ink_band_except_the_documented_dandelion_grey() {
        // Classifier integrity (QA 03:35/0438): the visible audit's
        // presents_ink accepts spread <= 12 && sum < 640; the pixel audit
        // uses spread 20. LOAD-BEARING for the BUFFER audit: every
        // registered NON-GREY kind tone must stay outside the buffer
        // band so a kind-coloured cell over letter ink counts as
        // erasure, never as ink. The PIXEL band (20) is QA's
        // classifier: blossom dark lands at spread 19 (inside 20) -
        // flagged for QA's pixel audit, harmless here because the
        // whole-bloom clearance keeps blossoms off letter ink and the
        // buffer band (12) still reads it as garden.
        //
        // The ONE exception, already landed (no longer future):
        // dandelion's hue #EDEBD8 is 13.9 from the Paper background, so
        // upstream's pull-toward-type rule makes it a GREY
        // (127,126,117) - spread 10, sum 370 - which BOTH colour
        // classifiers call ink. Upstream's own palette design,
        // preserved (never recoloured). The visible-loss audit is
        // PROVENANCE-BASED: the presentation oracle (branch sidecar,
        // see mod) counts dandelion cells as erasure regardless of
        // colour - proven live by the forced-grey oracle regression -
        // and the whole-bloom clearance keeps pale blooms off letter
        // ink. See mod's ink_classifier + oracle tests.
        for role in [
            Role::DaisyDark,
            Role::Daisy,
            Role::DaisyLight,
            Role::TulipDark,
            Role::Tulip,
            Role::TulipLight,
            Role::PoppyDark,
            Role::Poppy,
            Role::PoppyLight,
            Role::BlossomDark,
            Role::Blossom,
            Role::BlossomLight,
            Role::LilyDark,
            Role::Lily,
            Role::LilyLight,
            Role::BellDark,
            Role::Bell,
            Role::BellLight,
            Role::LavenderDark,
            Role::Lavender,
            Role::LavenderLight,
            Role::FernDark,
            Role::Fern,
            Role::FernLight,
        ] {
            let (r, g, b) = role_color(role);
            let spread = r.max(g).max(b) as i32 - r.min(g).min(b) as i32;
            let sum = r as u32 + g as u32 + b as u32;
            assert!(
                !(spread <= 12 && sum < 640),
                "{role:?} would classify as ink: spread {spread} sum {sum}"
            );
        }
        // The DANDELION greys are the documented INK-AMBIGUOUS pair
        // (base pulled (127,126,117): spread 10, sum 370; its dark
        // mix ~ (69,68,63) is darker still): the colour-only
        // classifiers CANNOT separate them from faint ink. This is
        // upstream's own palette design and is NOT recoloured. The
        // disambiguation is provenance-based: the visible-loss audit
        // uses the presentation sidecar oracle (mask roles, not
        // colours), and the clearance keeps pale blooms off letter
        // ink. See mod's ink_classifier test and the oracle regression.
        let (r, g, b) = role_color(Role::Dandelion);
        assert_eq!((r, g, b), (127, 126, 117), "the pulled grey, preserved");
        let spread = r.max(g).max(b) as i32 - r.min(g).min(b) as i32;
        let sum = r as u32 + g as u32 + b as u32;
        assert!(
            spread <= 12 && sum < 640,
            "dandelion base is ink-ambiguous BY DESIGN (oracle-covered)"
        );
    }

    #[test]
    fn solid_cell_gets_fill_and_glyph() {
        // A cell fully covered by stems: solid, fill mixed 0.2 toward bg,
        // glyph from the stem family.
        let g = grid_with(2, 1, |i, j| {
            if (i / SX) == 0 && (j / SY) == 0 {
                Role::Stem as u8
            } else {
                0
            }
        });
        let area = Rect::new(0, 0, 2, 1);
        let mut buf = Buffer::empty(area);
        present(&mut buf, area, &g, None, TypeStyle::Texture);
        let cell = &buf[(0, 0)];
        assert_ne!(cell.symbol(), " ");
        let stem_mix = mix(STEM, PAPER_BG, 0.2);
        assert_eq!(
            cell.bg,
            ratatui::style::Color::Rgb(stem_mix.0, stem_mix.1, stem_mix.2)
        );
        assert_eq!(buf[(1, 0)].symbol(), " ");
    }

    #[test]
    fn soft_partial_coverage_gets_ramp_glyph_no_fill() {
        // A square-ish quarter blob (2x4 subpixels, low elongation): soft
        // RAMP glyph on the paper, no fill. With the moment scale fixed
        // (see MOMENT_CW), thin full-width stripes are no longer ramps -
        // upstream renders those as line glyphs (its pixel-scale axis
        // ratio is ~25 for a single-row stripe).
        let g = grid_with(2, 1, |i, j| {
            if (i / 2 == 0) && (j == 2 || j == 3) && (i / SX) == 0 {
                Role::Flower as u8
            } else {
                0
            }
        });
        let area = Rect::new(0, 0, 2, 1);
        let mut buf = Buffer::empty(area);
        present(&mut buf, area, &g, None, TypeStyle::Texture);
        let cell = &buf[(0, 0)];
        assert_ne!(cell.symbol(), " ");
        assert_eq!(cell.bg, ratatui::style::Color::Rgb(243, 238, 228));
        let ramp: &[char] = &['.', ',', ':', ';', '+', '='];
        assert!(
            ramp.contains(&cell.symbol().chars().next().unwrap()),
            "square blob should be a ramp glyph, got {}",
            cell.symbol()
        );
        assert_eq!(buf[(1, 0)].symbol(), " ");
    }

    #[test]
    fn thin_stripes_render_as_line_glyphs() {
        // Regression for the moment-scale bug: with 0..1 normalized
        // sample coordinates, the axis regularizer (0.35) dominated and
        // EVERY cell fell below `lineish` - stems rendered as scattered
        // ramp dots instead of upstream's continuous | / \ segments and
        // rose secondary cells picked 'o' instead of line glyphs. In the
        // reference's cell-pixel units (MOMENT_CW/MOMENT_CH), a vertical
        // half-width stroke has ratio ~5 and a horizontal one ~4 - both
        // above lineish (1.9), exactly as upstream behaves.

        // Vertical: a stem covering half the cell width, full height.
        let g = grid_with(1, 1, |i, _j| {
            if i == 1 || i == 2 {
                Role::Stem as u8
            } else {
                0
            }
        });
        let area = Rect::new(0, 0, 1, 1);
        let mut buf = Buffer::empty(area);
        present(&mut buf, area, &g, None, TypeStyle::Texture);
        let ch = buf[(0, 0)].symbol().chars().next().unwrap();
        assert_eq!(ch, '|', "vertical stem cell must be a line glyph");

        // Horizontal: one subpixel row across the full width.
        let g = grid_with(1, 1, |_i, j| if j == 4 { Role::Stem as u8 } else { 0 });
        let mut buf = Buffer::empty(area);
        present(&mut buf, area, &g, None, TypeStyle::Texture);
        let ch = buf[(0, 0)].symbol().chars().next().unwrap();
        assert_eq!(ch, '-', "horizontal stroke cell must be a line glyph");
    }

    #[test]
    fn guard_protects_other_lines_letters() {
        // The per-line guard: while drawing a front-layer element of line 2
        // (guard = 2), subpixels owned by line 1's type must survive.
        let mut g = MaskGrid::new(4, 1);
        // Line 1 owns the left half as type.
        for j in 0..g.h {
            for i in 0..g.w / 2 {
                let k = j * g.w + i;
                g.role[k] = Role::Type as u8;
                g.owner[k] = 1;
            }
        }
        // A line-2 flower sweeps across the whole row.
        g.guard = 2;
        g.fill_rect(0.0, 0.0, 3.9, 1.9, Role::Flower);
        g.guard = 0;
        for j in 0..g.h {
            for i in 0..g.w / 2 {
                assert_eq!(g.role[j * g.w + i], Role::Type as u8, "line 1 covered");
            }
            for i in g.w / 2..g.w {
                assert_eq!(g.role[j * g.w + i], Role::Flower as u8);
            }
        }
        // With no guard set, the same flower would cover everything.
        g.fill_rect(0.0, 0.0, 3.9, 1.9, Role::Flower);
        assert!(g.role.iter().all(|&r| r == Role::Flower as u8));
    }

    #[test]
    fn sparse_samples_below_threshold_stay_paper() {
        // INVARIANT, not a hardcoded count: one hit below present()'s
        // coverage threshold (`round(N*0.125)`, see min_hits) draws
        // nothing at any N_SUB. The old fixture hardcoded 3 hits - below
        // the threshold when N_SUB was 32 (min 4), but equal to it after
        // the 2:3 cell aspect change (N_SUB 24 -> min 3), so the thin
        // stripe legitimately rendered as a '-' line glyph.
        let min_hits = ((N_SUB as f32 * 0.125).round() as usize).max(1);
        let below = min_hits.saturating_sub(1);
        assert!(below < min_hits, "fixture must be below the threshold");
        let g = grid_with(1, 1, move |i, j| {
            if j == 0 && i < below {
                Role::Flower as u8
            } else {
                0
            }
        });
        let area = Rect::new(0, 0, 1, 1);
        let mut buf = Buffer::empty(area);
        present(&mut buf, area, &g, None, TypeStyle::Texture);
        assert_eq!(buf[(0, 0)].symbol(), " ");
    }

    #[test]
    fn stroke_polyline_hits_centerline() {
        let mut g = MaskGrid::new(4, 1);
        g.stroke_polyline(&[Pt(0.5, 0.5), Pt(3.5, 0.5)], 0.3, Role::Stem);
        let covered = g.role.iter().filter(|&r| *r == Role::Stem as u8).count();
        assert!(covered >= SX, "stem should mark a row, got {covered}");
    }

    #[test]
    fn fill_poly_fills_interior_not_outside() {
        let mut g = MaskGrid::new(2, 2);
        let pts = [
            Pt(0.25, 0.25),
            Pt(1.75, 0.25),
            Pt(1.75, 1.75),
            Pt(0.25, 1.75),
        ];
        g.fill_poly(&pts, Role::Flower);
        // The sample at (i=4, j=3) is x=1.125, y=0.875: inside.
        let inside = g.role[3 * g.w + 4];
        assert_eq!(inside, Role::Flower as u8, "interior must be filled");
        // A corner sample outside stays background.
        assert_eq!(g.role[0], 0);
    }

    #[test]
    fn scanline_fills_are_byte_identical_to_the_per_point_reference() {
        // The performance fix (row-crossing scanline fills) MUST be
        // pixel-identical to the original per-point even-odd rule. This
        // differential test runs both on a battery of representative
        // polygons - petal outlines, blobs, thin wedges, a
        // self-intersecting star, off-grid and degenerate placements,
        // and deterministic pseudo-random polygons - and asserts the
        // masks are byte-identical, including the cover-cap and guard
        // paths exercised through set().
        let mut seed = 0x1234_5678u64;
        let mut rand = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            // Normalize to [0, 1): the high bits of xorshift64 (the low
            // bits are weak).
            (seed >> 40) as f32 / (1u64 << 24) as f32
        };
        let mut battery: Vec<Vec<Pt>> = Vec::new();
        // Petal-like outline (the petal() shape at a few angles).
        for ang in [0.0f32, 0.7, 1.9, -2.4] {
            let (dx, dy) = (ang.cos(), ang.sin());
            let (px, py) = (-dy, dx);
            let mut pts = Vec::new();
            for k in 0..=9 {
                let u = k as f32 / 9.0;
                let w = 0.6f32 * (std::f32::consts::PI * u.powf(0.72)).sin() * (1.0 - 0.18 * u);
                pts.push(Pt(2.0 + dx * 1.5 * u + px * w, 2.0 + dy * 1.5 * u + py * w));
            }
            for k in (0..=9).rev() {
                let u = k as f32 / 9.0;
                let w = 0.6f32 * (std::f32::consts::PI * u.powf(0.72)).sin() * (1.0 - 0.18 * u);
                pts.push(Pt(2.0 + dx * 1.5 * u - px * w, 2.0 + dy * 1.5 * u - py * w));
            }
            battery.push(pts);
        }
        // Blob (blob_pts-like wobble).
        let mut blob = Vec::new();
        for k in 0..22 {
            let th = k as f32 / 22.0 * std::f32::consts::TAU;
            let rr = 1.0 + 0.07 * (3.0 * th).sin();
            blob.push(Pt(3.0 + th.cos() * 1.2 * rr, 2.5 + th.sin() * 0.9 * rr));
        }
        battery.push(blob);
        // Thin wedge, triangle, self-intersecting star.
        battery.push(vec![Pt(0.5, 0.5), Pt(5.5, 0.6), Pt(0.6, 0.7)]);
        battery.push(vec![Pt(1.0, 1.0), Pt(5.0, 1.0), Pt(3.0, 4.0)]);
        battery.push(vec![
            Pt(3.0, 0.3),
            Pt(3.6, 2.4),
            Pt(5.7, 2.4),
            Pt(4.0, 3.8),
            Pt(4.7, 5.9),
            Pt(3.0, 4.5),
            Pt(1.3, 5.9),
            Pt(2.0, 3.8),
            Pt(0.3, 2.4),
            Pt(2.4, 2.4),
        ]);
        // Off-grid: straddling all four edges.
        battery.push(vec![
            Pt(-1.0, -1.0),
            Pt(6.0, -1.0),
            Pt(6.0, 6.0),
            Pt(-1.0, 6.0),
        ]);
        // Degenerate collinear (empty area).
        battery.push(vec![Pt(1.0, 1.0), Pt(2.0, 2.0), Pt(3.0, 3.0)]);
        // Deterministic pseudo-random polygons.
        for _ in 0..12 {
            let n = 3 + (rand() * 10.0) as usize;
            let cx = 1.0 + rand() * 3.0;
            let cy = 1.0 + rand() * 3.0;
            battery.push(
                (0..n)
                    .map(|k| {
                        let th = k as f32 / n as f32 * std::f32::consts::TAU;
                        let r = 0.3 + rand() * 2.0;
                        Pt(cx + th.cos() * r, cy + th.sin() * r * (0.4 + rand()))
                    })
                    .collect(),
            );
        }

        // Extreme float boundary cases: vertices exactly on subpixel
        // sample rows, a crossing exactly at a subpixel centre, a
        // sub-subpixel polygon between samples, huge coordinates that
        // the bbox clamp must handle identically.
        let s = 1.0 / SX as f32; // sample spacing; centres at (i+0.5)*s
        battery.push(vec![
            // Horizontal edges exactly AT sample rows (py == yi for
            // several rows: the spanning rule must skip identically).
            Pt(1.0, 2.5 * s),
            Pt(5.0, 2.5 * s),
            Pt(5.0, 4.5 * s),
            Pt(1.0, 4.5 * s),
        ]);
        battery.push(vec![
            // A vertex x exactly at a subpixel centre (crossing == px
            // boundary case).
            Pt(2.5 * s, 0.5),
            Pt(2.5 * s, 4.0),
            Pt(6.0, 2.0),
        ]);
        battery.push(vec![
            // Entirely between two sample rows: nothing fills.
            Pt(2.0, 2.0 + 0.3 * s),
            Pt(3.0, 2.0 + 0.3 * s),
            Pt(3.0, 2.0 + 0.7 * s),
            Pt(2.0, 2.0 + 0.7 * s),
        ]);
        battery.push(vec![
            // Huge coordinates, off-grid everywhere: identical clamping.
            Pt(-1e8, -1e8),
            Pt(1e8, -1e8),
            Pt(1e8, 1e8),
            Pt(-1e8, 1e8),
        ]);

        let stops = [
            (0.0, (255, 59, 31)),
            (0.5, (137, 32, 17)),
            (1.0, (255, 141, 125)),
        ];
        let cands = [Role::FlowerShadow, Role::Flower, Role::FlowerLight];
        for pts in &battery {
            // Plain fill: roles AND owner AND letter arrays compared
            // (fills only write roles; owner/letter must be untouched
            // by BOTH implementations - byte equivalence, not comment
            // claims).
            let mut a = MaskGrid::new(6, 6);
            a.fill_poly(pts, Role::Stem);
            let mut b = MaskGrid::new(6, 6);
            b.fill_poly_reference(pts, Role::Stem);
            assert_eq!(a.role, b.role, "fill_poly role mismatch on {pts:?}");
            assert_eq!(a.owner, b.owner, "fill_poly owner mismatch on {pts:?}");
            assert_eq!(a.letter, b.letter, "fill_poly letter mismatch on {pts:?}");

            // Cover-cap armed: compare roles AND the budget COUNTERS
            // (snapshot before end_letter_cover drops them) - the
            // per-row ascending-i paint order preserves the exact
            // budget increment sequence.
            let mut a = MaskGrid::new(6, 6);
            a.role[0] = Role::Type as u8;
            a.letter[0] = 1; // subpixel 0 is letter 1's ink
            a.begin_letter_cover(vec![0, 24]);
            a.fill_poly(pts, Role::Stem);
            let a_counters = a.cover_counters();
            a.end_letter_cover();
            let mut b = MaskGrid::new(6, 6);
            b.role[0] = Role::Type as u8;
            b.letter[0] = 1;
            b.begin_letter_cover(vec![0, 24]);
            b.fill_poly_reference(pts, Role::Stem);
            let b_counters = b.cover_counters();
            b.end_letter_cover();
            assert_eq!(a.role, b.role, "fill_poly cover-cap mismatch on {pts:?}");
            assert_eq!(
                a_counters, b_counters,
                "fill_poly budget counters diverged on {pts:?}"
            );

            // Front-layer GUARD active over another line's owned ink:
            // both implementations must skip identically.
            let mut a = MaskGrid::new(6, 6);
            a.role[0] = Role::Type as u8;
            a.owner[0] = 1; // line 1 owns subpixel 0
            a.guard = 2; // a line-2 element sweeps across
            a.fill_poly(pts, Role::Stem);
            a.guard = 0;
            let mut b = MaskGrid::new(6, 6);
            b.role[0] = Role::Type as u8;
            b.owner[0] = 1;
            b.guard = 2;
            b.fill_poly_reference(pts, Role::Stem);
            b.guard = 0;
            assert_eq!(a.role, b.role, "fill_poly guard mismatch on {pts:?}");
            assert_eq!(a.owner, b.owner, "fill_poly guard owner mismatch");

            // Gradient fill.
            let mut a = MaskGrid::new(6, 6);
            a.fill_grad_poly(pts, 3.0, 3.0, 2.5, &stops, &cands);
            let mut b = MaskGrid::new(6, 6);
            b.fill_grad_poly_reference(pts, 3.0, 3.0, 2.5, &stops, &cands);
            assert_eq!(a.role, b.role, "fill_grad_poly mismatch on {pts:?}");
            assert_eq!(a.owner, b.owner, "fill_grad owner mismatch");
            assert_eq!(a.letter, b.letter, "fill_grad letter mismatch");
        }
    }

    #[test]
    fn fill_grad_maps_to_nearest_stop_role() {
        let mut g = MaskGrid::new(2, 2);
        let pts = [Pt(0.1, 0.05), Pt(1.9, 0.05), Pt(1.9, 3.95), Pt(0.1, 3.95)];
        g.fill_grad_poly(
            &pts,
            1.0,
            2.0,
            1.6,
            &[
                (0.0, role_color(Role::FlowerLight)),
                (1.0, role_color(Role::Flower)),
            ],
            &[Role::Flower, Role::FlowerLight],
        );
        let light = g
            .role
            .iter()
            .filter(|&&r| r == Role::FlowerLight as u8)
            .count();
        let base = g.role.iter().filter(|&&r| r == Role::Flower as u8).count();
        assert!(light > 0 && base > 0, "both bands expected: {light}/{base}");
    }
}
