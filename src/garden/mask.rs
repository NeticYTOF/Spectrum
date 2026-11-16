//! Cell-grid ASCII rasterizer, ported from ascii-garden's `engine/ascii.ts`
//! (upstream f519e85, MIT (c) 2026 Ridit; see NOTICE). The garden is drawn
//! with vector calls into a hidden low-resolution mask - here a role index
//! per subpixel instead of a canvas - and every cell is then read back and
//! turned into text: mostly covered cells become a colour block with a
//! dense glyph, elongated shapes become angle glyphs (- \\ | /), and partly
//! covered cells fade into the background through the density RAMP.
//!
//! Terminal geometry: one cell = 4x8 subpixels (square subpixels at the
//! terminal's 1:2 cell aspect), replacing the canvas' SXxSY sampling.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};

/// Subpixels per cell along x and y (square at 1:2 cell aspect).
pub const SX: usize = 4;
pub const SY: usize = 8;
/// Samples per cell.
pub const N_SUB: usize = SX * SY;
/// Subpixels per design unit (square: 4 across a 1.0-wide cell, 8 down a
/// 2.0-tall one).
pub const SUB: f32 = SX as f32;
/// Minimum stroke width in design units: upstream's `minLw = cw * 0.5`
/// (half a cell) so thin stems still register as a glyph in every cell
/// they cross instead of breaking into fragments.
pub const MIN_LW: f32 = 0.5;

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
/// 5 flower shadow, 6 flower highlight, 7 leaf shadow, 8 leaf highlight.
/// Shadows get dense glyphs, highlights sparse ones, so shading reads as
/// texture. (ascii.ts roles, same order.)
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
}

pub const ROLE_COUNT: usize = 9;

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
    }
}

/// Which family a role belongs to (flower / stem / line / type).
pub const FAM: [u8; ROLE_COUNT] = [0, 1, 2, 3, 4, 1, 1, 2, 2];
/// 0 base colour, 1 shadow, 2 highlight.
pub const TONE: [u8; ROLE_COUNT] = [0, 0, 0, 0, 0, 1, 2, 1, 2];
/// Glyph family per role (ascii.ts BASE_SETS).
pub const SETS: [&str; ROLE_COUNT] = [
    "", "O0o*&", ":;+=x*", "()/~|", "#@8&%B", "@%#8&W", ".:'`,", "#%=xX", ".:'`;",
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

/// A point in design space: x in cell widths, y in half-row units
/// (one terminal cell = 1.0 x 2.0, so subpixels are square).
#[derive(Clone, Copy, Debug)]
pub struct Pt(pub f32, pub f32);

/// The mask: one role index per subpixel.
pub struct MaskGrid {
    pub w: usize,
    pub h: usize,
    pub role: Vec<u8>,
    /// Which type line (index + 1) owns each type subpixel; 0 = none.
    pub owner: Vec<u8>,
    /// While drawing a front-layer element of line `guard - 1`, other
    /// lines' letters are protected (0 = no guard). Upstream lays out one
    /// line and sends flowers over the letter body mostly behind it; with
    /// wrapped lyrics the same rule extends to every other line.
    pub guard: u8,
}

impl MaskGrid {
    pub fn new(cols: usize, rows: usize) -> Self {
        let n = cols * SX * rows * SY;
        Self {
            w: cols * SX,
            h: rows * SY,
            role: vec![0; n],
            owner: vec![0; n],
            guard: 0,
        }
    }

    pub fn clear(&mut self) {
        self.role.fill(0);
        self.owner.fill(0);
        self.guard = 0;
    }

    #[inline]
    fn set(&mut self, i: usize, j: usize, role: Role) {
        if i < self.w && j < self.h {
            let k = j * self.w + i;
            let o = self.owner[k];
            if self.guard != 0 && o != 0 && o != self.guard {
                return;
            }
            self.role[k] = role as u8;
        }
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

    /// Fill a closed polygon (even-odd point-in-polygon, design space).
    pub fn fill_poly(&mut self, pts: &[Pt], role: Role) {
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

    /// Fill a polygon with a radial shade: `stops` are [offset, colour];
    /// each sample's radial position picks a colour, which maps to the
    /// NEAREST role (the 15-bit LUT's quantisation, so shading bands read
    /// as shadow/base/highlight roles exactly as in ascii.ts fillGrad).
    pub fn fill_grad_poly(
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
    // ink toward the paper, fully covered ones stay dark.
    let on_cov = (on_sum / on_n).clamp(0.0, 1.0);
    let ink = mix(PAPER_BG, INK, (0.35 + 0.65 * on_cov).min(1.0));
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
pub fn present(
    buf: &mut Buffer,
    area: Rect,
    grid: &MaskGrid,
    type_cov: Option<&super::font::TypeMask>,
    style: TypeStyle,
) {
    let min_hits = ((N_SUB as f32 * 0.125).round() as usize).max(1); // 4 of 32
    let sec_hits = ((N_SUB as f32 * 0.17).round() as usize).max(1); // 5 of 32
    let colors: [(u8, u8, u8); ROLE_COUNT] = {
        let mut c = [(0u8, 0u8, 0u8); ROLE_COUNT];
        for (i, col) in c.iter_mut().enumerate() {
            *col = role_color(role_from_index(i));
        }
        c
    };

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
                    // Cell-local sample coordinates (ascii.ts pxs/pys).
                    let px = (i as f64 + 0.5) / SX as f64;
                    let py = (j as f64 + 0.5) / SY as f64;
                    count[r] += 1;
                    sx[r] += px;
                    sy[r] += py;
                    sxx[r] += px * px;
                    syy[r] += py * py;
                    sxy[r] += px * py;
                }
            }
            let cell = buf.cell_mut(Position::new(cx, cy)).unwrap();
            if style == TypeStyle::Silhouette && count[Role::Type as usize] > 0 {
                if let Some(tc) = type_cov {
                    let key = (hash3((cx - area.x) as i32, (cy - area.y) as i32, 17) * 4294967295.0)
                        as u32;
                    if let Some((ch, fg, bg)) = silhouette_cell(grid, tc, gi0, gj0, key, &colors) {
                        cell.set_char(ch);
                        cell.set_fg(rgb(fg));
                        cell.set_bg(rgb(bg));
                        continue;
                    }
                    // Below every quadrant threshold: a faint hairline; let
                    // the garden (if any) speak for this cell instead.
                    count[Role::Type as usize] = 0;
                }
            }
            let total: u16 = count[1..].iter().sum();
            if (total as usize) < min_hits {
                continue; // paper
            }
            let mut dom = 1;
            for k in 2..ROLE_COUNT {
                if count[k] > count[dom] {
                    dom = k;
                }
            }
            let dom_fam = FAM[dom];
            let cov = total as f32 / N_SUB as f32;
            let key = (hash3((cx - area.x) as i32, (cy - area.y) as i32, 17) * 4294967295.0) as u32;
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
                if sec != 0 {
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
            let col = if black_text {
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
            cell.set_fg(rgb(col));
            cell.set_bg(rgb(PAPER_BG));
        }
    }
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
        // 6 of 32 samples: soft ramp glyph, paper background.
        let g = grid_with(2, 1, |i, j| {
            if j == 0 && i < 6 && (i / SX) == 0 {
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
        assert_eq!(buf[(1, 0)].symbol(), " ");
    }

    #[test]
    fn sparse_samples_below_threshold_stay_paper() {
        // 3 of 32 samples < minHits 4: nothing is drawn.
        let g = grid_with(1, 1, |i, j| {
            if j == 0 && i < 3 {
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
