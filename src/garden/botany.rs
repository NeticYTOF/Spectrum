//! Botanical geometry, ported from ascii-garden's `garden.ts` (upstream
//! f519e85, MIT (c) 2026 Ridit; see NOTICE): the seeded PRNG, vine/curl/
//! weave path builders, stem/leaf/flower growth from letters, the FULL
//! botanical species mix (rose + daisy, tulip, poppy, blossom, lily, bell,
//! lavender, fern, dandelion - garden.ts pickKind / KIND_SCALE /
//! KIND_WEIGHT / KIND_COLOR / drawKind), spring easing plus the gust sway
//! and birth settling. TYPING-ONLY dynamics (backspace wither, space
//! cut/recoil, enter clear) are not ported; the upstream canvas-click
//! spawnFly and the visit POSTER's butterflies are separate non-typing
//! features, also not ported. Live line changes bloom in with the
//! upstream "grow" preset's stagger (see birth_stagger). Element
//! coordinates are in EM units relative to each letter (centre x,
//! baseline y); drawing scales them by the type size S into design
//! space.

use super::mask::{MaskGrid, Pt, Role, FLOWER};

pub const CHAOS: f32 = 0.3; // "0.3 matches the original Type Garden"

/// Upstream detail thresholds are in canvas pixels at its default cell
/// size (about 10 px wide); design units here are one cell wide, so a
/// pixel constant converts by this factor (R >= 16 px -> R >= 1.6).
const PX: f32 = 0.1;

/// A flower species (garden.ts drawKind / pickKind / KIND_SCALE /
/// KIND_WEIGHT / KIND_COLOR; ascii.ts KIND_GLYPHS). The botanical theme
/// grows a MIX; the rose keeps its dedicated reference geometry and the
/// palette's flower colour, exactly as upstream special-cases it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Rose,
    Daisy,
    Tulip,
    Poppy,
    Blossom,
    Lily,
    Bell,
    Lavender,
    Fern,
    Dandelion,
}

impl Kind {
    /// The registered species pool with upstream's KIND_WEIGHT (rose is
    /// a little more frequent; fern and dandelion weigh 1).
    pub const POOL: [(Kind, u32); 10] = [
        (Kind::Rose, 3),
        (Kind::Daisy, 2),
        (Kind::Tulip, 2),
        (Kind::Poppy, 2),
        (Kind::Blossom, 2),
        (Kind::Lily, 2),
        (Kind::Bell, 2),
        (Kind::Lavender, 2),
        (Kind::Fern, 1),
        (Kind::Dandelion, 1),
    ];

    /// Upstream KIND_SCALE - a size tweak per species so it sits well on
    /// its stem (garden.ts: rose 1, tulip 1.25, daisy 1.25, poppy 1.2,
    /// blossom 1.3, lily 1.3, bell 1.2, lavender 1.05, fern 1.05,
    /// dandelion 1.25).
    pub fn scale(self) -> f32 {
        match self {
            Kind::Rose => 1.0,
            Kind::Daisy => 1.25,
            Kind::Tulip => 1.25,
            Kind::Poppy => 1.2,
            Kind::Blossom => 1.3,
            Kind::Lily => 1.3,
            Kind::Bell => 1.2,
            Kind::Lavender => 1.05,
            Kind::Fern => 1.05,
            Kind::Dandelion => 1.25,
        }
    }

    /// The largest scale in the registered pool - the whole-bloom
    /// clearance budget must clear the BIGGEST species a word can roll,
    /// not just the rose.
    pub fn max_pool_scale() -> f32 {
        Self::POOL
            .iter()
            .map(|(k, _)| k.scale())
            .fold(1.0, f32::max)
    }

    /// The (dark, base, light) mask roles for this species' tones -
    /// upstream's setRoles appends exactly three roles per kind.
    pub fn roles(self) -> (Role, Role, Role) {
        match self {
            Kind::Rose => (Role::FlowerShadow, Role::Flower, Role::FlowerLight),
            Kind::Daisy => (Role::DaisyDark, Role::Daisy, Role::DaisyLight),
            Kind::Tulip => (Role::TulipDark, Role::Tulip, Role::TulipLight),
            Kind::Poppy => (Role::PoppyDark, Role::Poppy, Role::PoppyLight),
            Kind::Blossom => (Role::BlossomDark, Role::Blossom, Role::BlossomLight),
            Kind::Lily => (Role::LilyDark, Role::Lily, Role::LilyLight),
            Kind::Bell => (Role::BellDark, Role::Bell, Role::BellLight),
            Kind::Lavender => (Role::LavenderDark, Role::Lavender, Role::LavenderLight),
            Kind::Fern => (Role::FernDark, Role::Fern, Role::FernLight),
            Kind::Dandelion => (Role::DandelionDark, Role::Dandelion, Role::DandelionLight),
        }
    }
}

/// Upstream pickKind: uniform over the weighted pool (the weighted list
/// is expanded by weight, then indexed by the roll).
pub fn pick_kind(r: &mut Rng) -> Kind {
    let total: u32 = Kind::POOL.iter().map(|(_, w)| *w).sum();
    if total == 0 {
        return Kind::Rose;
    }
    let mut pick = (r.rf() * total as f32).floor() as u32;
    for (k, w) in Kind::POOL {
        if pick < w {
            return k;
        }
        pick -= w;
    }
    Kind::Rose
}

/// The staggered growth schedule for a line of `n` letters (garden.ts
/// "grow" preset: `l.b = 200 + i * min(110, 3000/n)`) - the applicable
/// live-lyric motion: typing-only dynamics (backspace wither, space
/// cut/recoil) are NOT ported; line changes bloom in with THIS stagger.
pub fn birth_stagger(n: usize, i: usize) -> f32 {
    let step = (3000.0 / n.max(1) as f32).min(110.0);
    200.0 + i as f32 * step
}

/// Bounded viewport fit (QA 03:42): after generation, a whole PLANT -
/// its stem with every bloom it carries, and the leaves attached along
/// it - must sit fully inside the band, with a margin covering the
/// render-time gust sway / birth settling / jitter that leans points
/// outward on top of the static placement. A crossing plant is
/// TRANSLATED inward by the needed dx: the blooms and the stem's outer
/// third shift together (smoothstep-blended along the stem so the root
/// stays planted on the letter), and leaves follow the same blend at
/// their attach point. The flower is never hidden or carved; type,
/// density and seeds never change; the vertical whole-bloom clearance
/// is untouched (dx only). A horizontal shift that lands a bloom over
/// a NEIGHBOURING letter is still bounded by the per-letter cover caps.
/// Deterministic pure placement math, so frozen frames stay identical.
///
/// `els` are em-relative to the letter at design `(tx, ty)`; `s` is the
/// em in design units; `w_design` the band width.
pub fn fit_to_band(els: &mut LetterEls, tx: f32, s: f32, w_design: f32) {
    /// Dynamic headroom: the gust leans a tall point up to
    /// ~0.15 em outward, the birth settling adds a decaying ~0.05 em,
    /// jitter ~0.02 em - 0.3 em covers the worst excursion.
    const MARGIN_EM: f32 = 0.3;
    let m = MARGIN_EM * s;
    let smooth = |u: f32| {
        let u = u.clamp(0.0, 1.0);
        u * u * (3.0 - 2.0 * u)
    };

    // Owner stems: every bloom follows the most recent El::Stem (grow
    // pushes its stem first - mains, satellites and branch tips alike).
    // Group envelopes: stem point bbox (plus stroke half-width) united
    // with every bloom's disk.
    let stroke_half = 0.25; // >= half of mask::MIN_LW
    let mut owner: Option<usize> = None;
    let mut groups: Vec<(usize, f32, f32)> = Vec::new(); // (stem idx, min, max)
    for (i, el) in els.els.iter().enumerate() {
        match el {
            El::Stem { pts, .. } => {
                let mut lo = f32::MAX;
                let mut hi = f32::MIN;
                for p in pts {
                    let x = tx + p.0 * s;
                    lo = lo.min(x);
                    hi = hi.max(x);
                }
                groups.push((i, lo - stroke_half, hi + stroke_half));
                owner = Some(i);
            }
            El::Rose { x, r, .. } => {
                let cx = tx + x * s;
                // Species paint beyond the disk: daisy petal tips reach
                // ~1.26 r, poppy blobs ~1.2 r (measured by the geometry
                // isolation test); 1.35 r covers every registered
                // species' painted extent with slack.
                let rr = r * s * 1.35;
                if let Some(o) = owner {
                    if let Some(g) = groups.iter_mut().find(|(gi, _, _)| *gi == o) {
                        g.1 = g.1.min(cx - rr);
                        g.2 = g.2.max(cx + rr);
                    }
                }
            }
            _ => {}
        }
    }

    // The needed translation per group (0 when clear of both edges).
    let mut dxs: Vec<(usize, f32)> = Vec::new();
    for (gi, lo, hi) in &groups {
        let dx = if *lo < m {
            m - *lo
        } else if *hi > w_design - m {
            w_design - m - *hi
        } else {
            0.0
        };
        if dx != 0.0 {
            dxs.push((*gi, dx));
        }
    }
    if dxs.is_empty() {
        return; // fully inside: byte-identical no-op
    }

    // Apply: for each crossing stem, ONE owner-tracked pass shifts its
    // tail (root stays planted), its blooms fully, and its leaves by
    // the same blend at their attach point along the stem.
    for (gi, dx) in dxs {
        let dx_em = dx / s;
        // The owning stem's root/tip (copied out to avoid borrows).
        let (root_x, tip_x) = match &els.els[gi] {
            El::Stem { pts, .. } if !pts.is_empty() => (pts[0].0, pts[pts.len() - 1].0),
            _ => continue,
        };
        let span = tip_x - root_x;
        let mut owner: Option<usize> = None;
        for (i, el) in els.els.iter_mut().enumerate() {
            match el {
                El::Stem { pts, .. } => {
                    if i == gi {
                        let n = pts.len().max(1);
                        for (k, p) in pts.iter_mut().enumerate() {
                            let u = k as f32 / (n - 1).max(1) as f32;
                            p.0 += dx_em * smooth((u - 0.65) / 0.35);
                        }
                    }
                    owner = Some(i);
                }
                El::Rose { x, .. } => {
                    if owner == Some(gi) {
                        *x += dx_em;
                    }
                }
                El::Leaf { x, .. } => {
                    if owner == Some(gi) {
                        let rel = if span.abs() > 1e-6 {
                            ((*x - root_x) / span).clamp(0.0, 1.0)
                        } else {
                            1.0
                        };
                        *x += dx_em * smooth(rel);
                    }
                }
            }
        }
    }
}

/// Terminal cells cannot shrink with the flowers the way upstream's do
/// (`targetCh = St/17`), so plants carry an absolute VIEWPORT cap as a
/// safety bound against clipping. The caps are set just ABOVE upstream's
/// em-proportional maximums at the reference size (type 0.28 H: stem
/// ≤ 1.1 em = 0.31 H, rose ≤ 0.35 em = 0.10 H, leaf ≤ 0.34 em = 0.095 H),
/// so at upstream-faithful type sizes the garden keeps upstream's
/// proportions (full-length curved stems, larger detailed roses) and the
/// caps only bind in large-type edge cases. The previous tight caps
/// (0.16/0.06/0.05) were introduced against the old maximized-type
/// regime and squashed stems to half and roses to a third of upstream
/// scale - the reference comparison rejected that.
#[derive(Clone, Copy, Debug)]
pub struct PlantCaps {
    /// Longest stem path, in design units.
    pub stem: f32,
    /// Largest rose radius, in design units.
    pub rose: f32,
    /// Largest leaf length, in design units.
    pub leaf: f32,
}

impl PlantCaps {
    /// Safety caps for a garden band of height `band_h` design units,
    /// sized at upstream's em-proportional maximums for 0.28 H type plus
    /// headroom (a stem ~35% of the band, a rose ~10%, a leaf ~8%).
    pub fn for_band(band_h: f32) -> Self {
        Self {
            stem: band_h * 0.35,
            rose: band_h * 0.10,
            leaf: band_h * 0.08,
        }
    }

    /// The garden's vertical extent in em for type size `s`, INCLUDING THE
    /// LETTERS THEMSELVES: upstream's demo keeps ascenders/descenders plus
    /// plants clear of the frame. Stems sprout up to ~0.3 em inside the
    /// letters and grow at most `stem` design units with a rose at the tip
    /// (upstream's uncapped garden reaches ~1.76 em above the baseline,
    /// ~1.36 em below); the type's ascender/descender are the floor of the
    /// extent - a word like "hello" presses its h/l ascenders to the top
    /// edge if only plants are reserved.
    pub fn extent_em(&self, s: f32) -> (f32, f32) {
        let m = super::font::metrics();
        let reach = (self.stem + self.rose) / s;
        let up = m.ascender.max(0.3 + reach).min(1.76);
        let down = (-m.descender).max(0.1 + reach).min(1.36);
        (up, down)
    }
}

/// Compress `v` (in em) into `[floor, cap]` keeping variety: values over
/// the cap squash into the top 30% of the range instead of all clipping
/// to the same size.
fn squash(v: f32, floor: f32, cap: f32, rand: f32) -> f32 {
    if v <= cap {
        v.max(floor)
    } else {
        (cap * (0.7 + 0.3 * rand)).max(floor)
    }
}

/// The reference PRNG (garden.ts rng): sfc-style with a 0x6D2B79F5 step.
/// Runs in u32 because the JS uses `>>>` (logical shifts) throughout.
pub struct Rng(i32);

impl Rng {
    pub fn new(seed: i32) -> Self {
        Self(seed)
    }
    pub fn next_f64(&mut self) -> f64 {
        let s = (self.0 as u32).wrapping_add(0x6D2B79F5);
        self.0 = s as i32;
        let mut t = (s ^ (s >> 15)).wrapping_mul(1 | s);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        let t = t ^ (t >> 14);
        t as f64 / 4294967296.0
    }
    #[inline]
    pub fn rf(&mut self) -> f32 {
        self.next_f64() as f32
    }
}

/// fract(sin(n) * 43758.5453) (garden.ts h): x - floor(x), which is NOT
/// Rust's `fract` for negative values. The multiplier is the source's
/// exact constant, kept verbatim.
#[allow(clippy::excessive_precision)]
const H_MAGIC: f32 = 43758.5453;

fn h(n: f32) -> f32 {
    let x = n.sin() * H_MAGIC;
    x - x.floor()
}

/// Spring: 1 - exp(-t*k) * cos(t*w) (garden.ts spr).
pub fn spr(t: f32, k: f32, w: f32) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    1.0 - (-t * k).exp() * (t * w).cos()
}

/// Ease-out cubic (garden.ts eo).
pub fn eo(t: f32) -> f32 {
    if t <= 0.0 {
        0.0
    } else if t >= 1.0 {
        1.0
    } else {
        1.0 - (1.0 - t).powi(3)
    }
}

type P = (f32, f32);

/// Cubic bezier sampled n times (garden.ts bez).
fn bez(a: P, b: P, c: P, d: P, n: usize) -> Vec<P> {
    let mut pts = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let t = i as f32 / n as f32;
        let u = 1.0 - t;
        pts.push((
            u * u * u * a.0 + 3.0 * u * u * t * b.0 + 3.0 * u * t * t * c.0 + t * t * t * d.0,
            u * u * u * a.1 + 3.0 * u * u * t * b.1 + 3.0 * u * t * t * c.1 + t * t * t * d.1,
        ));
    }
    pts
}

/// Point and angle at parameter u of a polyline (garden.ts at).
fn at(pts: &[P], u: f32) -> (P, f32) {
    let n = pts.len();
    if n < 2 {
        return (pts.first().copied().unwrap_or((0.0, 0.0)), 0.0);
    }
    let i = ((u * (n - 1) as f32).round() as i64).clamp(1, n as i64 - 2) as usize;
    let ang = (pts[i + 1].1 - pts[i - 1].1).atan2(pts[i + 1].0 - pts[i - 1].0);
    (pts[i], ang)
}

/// A wandering vine (garden.ts vine).
#[allow(clippy::too_many_arguments)]
fn vine(base: P, dir: f32, len: f32, amp: f32, waves: f32, ph: f32, bend: f32, n: usize) -> Vec<P> {
    let n = n.max(8);
    let mut pts = vec![base];
    let step = len / n as f32;
    let (mut x, mut y) = base;
    for i in 1..=n {
        let u = i as f32 / n as f32;
        let a = dir
            + bend * u
            + amp * (u * std::f32::consts::PI * waves + ph).sin() * (u * 3.0).min(1.0);
        x += a.cos() * step;
        y += a.sin() * step;
        pts.push((x, y));
    }
    pts
}

/// A curl at the end of a path (garden.ts curl), in EM units.
fn curl(pts: &mut Vec<P>, sign: f32, rad: f32) {
    let n = pts.len();
    if n < 2 {
        return;
    }
    let e = pts[n - 1];
    let a = (e.1 - pts[n - 2].1).atan2(e.0 - pts[n - 2].0);
    let c = (e.0 - a.sin() * sign * rad, e.1 + a.cos() * sign * rad);
    let a0 = (e.1 - c.1).atan2(e.0 - c.0);
    for i in 1..=14 {
        let u = i as f32 / 14.0;
        let t = a0 + sign * u * std::f32::consts::PI * 1.6;
        let rr = rad * (1.0 - 0.5 * u);
        pts.push((c.0 + t.cos() * rr, c.1 + t.sin() * rr));
    }
}

/// A stem weaves between the back and front layers (garden.ts weave).
#[derive(Clone, Debug)]
pub struct Seg {
    pub u0: f32,
    pub u1: f32,
    pub layer: u8,
}

fn weave(r: &mut Rng, start_layer: u8) -> Vec<Seg> {
    let mut cuts = Vec::new();
    let nc = 1 + (r.rf() * 3.0) as usize;
    for _ in 0..nc {
        cuts.push(0.15 + r.rf() * 0.7);
    }
    cuts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut segs = Vec::new();
    let mut u0 = 0.0f32;
    let mut layer = start_layer;
    for c in cuts {
        if c <= u0 {
            continue;
        }
        segs.push(Seg { u0, u1: c, layer });
        if r.rf() < 0.3 {
            u0 = (0.98_f32).min(c + 0.02 + r.rf() * 0.03);
        } else {
            u0 = c;
        }
        layer = 1 - layer;
    }
    segs.push(Seg { u0, u1: 1.0, layer });
    segs
}

fn layer_at(segs: &[Seg], u: f32) -> u8 {
    for s in segs {
        if u >= s.u0 && u <= s.u1 {
            return s.layer;
        }
    }
    segs.last().map(|s| s.layer).unwrap_or(0)
}

/// Word-level growth parameters (garden.ts wordParams).
pub struct WordParams {
    pub rose_p: f32,
    pub leafy: f32,
    pub dens: f32,
    pub lean: f32,
    pub big: f32,
    pub curvy: f32,
    pub bridge_p: f32,
    pub len: f32,
    pub w: f32,
}

/// Upstream's chaos knob for a given type size: 0.3 ("matches the
/// original Type Garden") once an em spans ~9 rows, easing down toward
/// upstream's "minimal botanical typography" end for small type so the
/// garden decorates the letters instead of drowning them.
pub fn chaos_for(em_rows: f32) -> f32 {
    let t = ((em_rows - 4.5) / 4.5).clamp(0.0, 1.0);
    0.12 + (CHAOS - 0.12) * t
}

pub fn word_params(ws: i32, chaos: f32) -> WordParams {
    let mut r = Rng::new(ws.wrapping_mul(7).wrapping_add(13));
    let c = chaos;
    // The reference theme is "botanical": tipP = 1, leaf = 1.
    let dens = 1.0 * (0.25 + c * 2.4) / 0.97;
    WordParams {
        rose_p: (0.3 + r.rf() * 0.3) * (0.45 + c * 1.5),
        leafy: (0.6 + r.rf() * 0.8) * dens,
        dens,
        lean: (r.rf() - 0.5) * 0.6 * (0.6 + c * 1.2),
        big: (0.85 + r.rf() * 0.45) * (0.8 + c * 0.45),
        curvy: (0.8 + r.rf() * 0.6) * (0.7 + c * 0.9),
        bridge_p: (0.45 + r.rf() * 0.35) * dens,
        len: 0.78 + c * 0.75,
        w: 0.5,
    }
}

/// Everything that grows out of one letter.
#[derive(Clone, Debug)]
pub enum El {
    Stem {
        id: u32,
        pts: Vec<P>,
        segs: Vec<Seg>,
        d0: f32,
        dur: f32,
        w: f32,
        thorns: Vec<(f32, f32)>,
    },
    Leaf {
        id: u32,
        x: f32,
        y: f32,
        a: f32,
        len: f32,
        bend: f32,
        layer: u8,
        d0: f32,
    },
    Rose {
        id: u32,
        x: f32,
        y: f32,
        r: f32,
        rot: f32,
        ph1: f32,
        ph2: f32,
        turns: f32,
        layer: u8,
        d0: f32,
        /// The species (upstream `e.kind`): rose keeps the reference rose
        /// geometry; other kinds draw their ported drawKind geometry.
        kind: Kind,
    },
}

impl El {
    fn id(&self) -> u32 {
        match self {
            El::Stem { id, .. } | El::Leaf { id, .. } | El::Rose { id, .. } => *id,
        }
    }
}

/// One letter's garden: its elements plus the end-of-word tendril.
#[derive(Clone, Debug)]
pub struct LetterEls {
    pub els: Vec<El>,
    pub tend: Option<El>,
}

/// HARD whole-bloom clearance (user 01:52 instruction; refined by
/// QA's 02:47-02:58 pixel feedback): a rose
/// grown on its letter's OWN stem keeps its full disk at least this far
/// (in em) from that letter's actual ink, on the side the stem already
/// heads. Upstream relies on its separate canvases' z-order for this
/// (over-letter blooms composite BEHIND whole); a terminal has one grid,
/// so we place the bloom clear instead - intact flower heads away from
/// the strokes, never clipped petals. The presentation cap remains the
/// safety net for stems, bridges and leaves, which still weave behind
/// the type exactly as in the reference.
const BLOOM_CLEAR: f32 = 0.08;

struct GrowOpts {
    base: P,
    dir: f32,
    len: f32,
    d0: f32,
    depth: bool,
    layer0: Option<u8>,
    tip: Option<&'static str>,
    pts: Option<Vec<P>>,
    /// The anchor letter's ink extent (top, bottom) in em, y-up - the
    /// whole-bloom clearance target for rose tips on grown stems.
    ink: Option<(f32, f32)>,
}

/// Per-em plant caps for one letter's type size (design-unit caps / s).
#[derive(Clone, Copy)]
struct EmCaps {
    stem: f32,
    rose: f32,
    leaf: f32,
}

/// The recursive grow (garden.ts grow): a stem with leaves and a tip,
/// possibly branching once more at depth 1.
#[allow(clippy::too_many_arguments)]
fn grow(r: &mut Rng, els: &mut Vec<El>, nid: &mut u32, p: &WordParams, caps: EmCaps, o: GrowOpts) {
    let tip = match o.tip {
        Some(t) => t.to_string(),
        None => {
            // Fresh draws per branch, exactly as the reference.
            if r.rf() < p.rose_p {
                "rose".into()
            } else if r.rf() < 0.45 {
                "fan".into()
            } else if r.rf() < 0.6 {
                "leaf".into()
            } else {
                "curl".into()
            }
        }
    };
    let supplied_pts = o.pts.is_some();
    // Viewport plant caps: when the type is large relative to the band,
    // em-proportional plants would tower over the word (upstream's cells
    // shrink instead); squash them with variety kept.
    let len = if supplied_pts {
        o.len
    } else {
        squash(o.len, 0.0, caps.stem, r.rf())
    };
    let mut pts = match o.pts {
        Some(pts) => {
            // A supplied path (the bridge) scales toward its base if the
            // walk from a to b would exceed the stem cap.
            let mut q = pts;
            let mut plen = 0.0;
            for w in q.windows(2) {
                plen += (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1);
            }
            if plen > caps.stem {
                let f = caps.stem / plen;
                let b = q[0];
                for p in q.iter_mut() {
                    *p = (b.0 + (p.0 - b.0) * f, b.1 + (p.1 - b.1) * f);
                }
            }
            q
        }
        None => {
            let mut v = vine(
                o.base,
                o.dir,
                len,
                (0.35 + r.rf() * 0.45) * p.curvy,
                1.0 + r.rf() * 1.6,
                r.rf() * std::f32::consts::TAU,
                (r.rf() - 0.5) * 1.2,
                40,
            );
            if tip == "curl" {
                curl(
                    &mut v,
                    if r.rf() < 0.5 { -1.0 } else { 1.0 },
                    0.05 + r.rf() * 0.05,
                );
            }
            v
        }
    };
    // Whole-bloom clearance (user 01:52 instruction; refined by QA's
    // 02:47-02:58 pixel feedback): for
    // EVERY rose tip - grown vines AND bridge tips - extend the stem's
    // end step by step UNTIL the bloom's centre clears the anchor
    // letter's ink on the side the stem already heads: above the ink
    // top (+ the largest rose this word can roll + BLOOM_CLEAR) for
    // up-heading tips, below the ink bottom likewise for genuinely
    // down-heading ones (horizontal-ish tips go UP - the reference's
    // blooms live above the word). The tail starts along the stem's own
    // heading and eases toward the clearing direction within a few
    // steps, with a gentle sway - organic, like the vine, and never a
    // long straight run (no rng consumed; frozen frames stay
    // byte-identical). Stopping AT the clearance keeps the bloom
    // envelope <= ~1.8 em above the baseline - inside the band
    // headroom, the same envelope the plant caps allowed before. The
    // rolled rose radius is <= r_est, so the full disk clears with
    // margin: intact flower heads away from the strokes.
    if tip == "rose" {
        if let Some((ink_top, ink_bot)) = o.ink {
            let (tp0, ta) = at(&pts, 1.0);
            let goes_down = ta.sin() > 0.35;
            // The largest bloom ANY registered species can roll: the
            // base roll's max (0.28 * p.big) times the pool's max
            // KIND_SCALE - the clearance must hold for tulip-sized and
            // poppy-sized flowers as much as for roses.
            let r_est = 0.28 * p.big * Kind::max_pool_scale();
            let need = if goes_down {
                -ink_bot + r_est + BLOOM_CLEAR
            } else {
                -(ink_top + r_est + BLOOM_CLEAR)
            };
            // at() clamps to the SECOND-TO-LAST point: the bloom ANCHORS
            // there, one segment short of the raw tip - so the clearance
            // must hold at the anchor, not the tip (the 'every'/'y' bloom
            // the regression caught sat exactly one 0.05 em segment above
            // the cleared tip, eating the whole margin).
            let anchor_y = if pts.len() >= 2 {
                pts[pts.len() - 2].1
            } else {
                tp0.1
            };
            let short = if goes_down {
                anchor_y < need
            } else {
                anchor_y > need
            };
            if short {
                let dir = 0.5
                    * (ta
                        + if goes_down {
                            std::f32::consts::FRAC_PI_2
                        } else {
                            -std::f32::consts::FRAC_PI_2
                        });
                let step = 0.05f32;
                let mut e = *pts.last().unwrap();
                let mut k = 0u32;
                while (goes_down && e.1 < need) || (!goes_down && e.1 > need) {
                    // Ease from the stem's heading into the clearing
                    // direction over the first few steps, with a gentle
                    // organic sway.
                    let w = (k as f32 / 4.0).min(1.0);
                    let ang = ta * (1.0 - w) + dir * w + 0.16 * (k as f32 * 0.9).sin();
                    e = (e.0 + ang.cos() * step, e.1 + ang.sin() * step);
                    pts.push(e);
                    k += 1;
                    if k > 80 {
                        break;
                    }
                }
                // One extra step along the clearing direction: the point
                // that first reached `need` must BE the anchor (the
                // second-to-last point), so the bloom itself - not just
                // the segment beyond it - sits at the cleared height.
                // This also covers the zero-iteration case (tip already
                // clear, anchor not yet).
                let ang = dir + 0.16 * ((k + 1) as f32 * 0.9).sin();
                e = (e.0 + ang.cos() * step, e.1 + ang.sin() * step);
                pts.push(e);
            }
        }
    }
    let start_layer = o.layer0.unwrap_or(if r.rf() < 0.5 { 0 } else { 1 });
    let segs = weave(r, start_layer);
    let dur = (320.0_f32).max(len * 620.0);
    let mut thorns = Vec::new();
    let nt = (r.rf() * 2.5) as usize;
    for _ in 0..nt {
        thorns.push((0.15 + r.rf() * 0.65, if r.rf() < 0.5 { -1.0 } else { 1.0 }));
    }
    let stem_w = if o.depth { 0.82 } else { 1.0 };
    els.push(El::Stem {
        id: *nid,
        pts: std::mem::take(&mut pts),
        segs: segs.clone(),
        d0: o.d0,
        dur,
        w: stem_w,
        thorns: thorns.clone(),
    });
    *nid += 1;

    let nl = (r.rf() * 2.8 * p.leafy) as usize;
    let mut sd = if r.rf() < 0.5 { -1.0 } else { 1.0 };
    let stem_pts = match els.last() {
        Some(El::Stem { pts, .. }) => pts.clone(),
        _ => vec![],
    };
    for _ in 0..nl {
        let u = 0.25 + r.rf() * 0.6;
        let (q, a) = at(&stem_pts, u);
        sd = -sd;
        els.push(El::Leaf {
            id: *nid,
            x: q.0,
            y: q.1,
            a: a + sd * (0.55 + r.rf() * 0.5),
            len: squash(
                (0.14 + r.rf() * 0.2) * if o.depth { 0.8 } else { 1.0 },
                0.0,
                caps.leaf,
                r.rf(),
            ),
            bend: (r.rf() - 0.5) * 1.2,
            layer: layer_at(&segs, u),
            d0: o.d0 + dur * u,
        });
        *nid += 1;
    }
    let (tp, ta) = at(&stem_pts, 1.0);
    let td = o.d0 + dur * 0.8;
    let tl = layer_at(&segs, 1.0);
    match tip.as_str() {
        "rose" => {
            // Upstream rolls the species and scales the radius by its
            // KIND_SCALE in the same breath (garden.ts: pickKind(r) then
            // R = (0.13 + r()*0.15) * p.big * depth * KIND_SCALE[kind]).
            let kind = pick_kind(r);
            let r_size = squash(
                (0.13 + r.rf() * 0.15) * p.big * if o.depth { 0.75 } else { 1.0 } * kind.scale(),
                0.0,
                caps.rose,
                r.rf(),
            );
            // Legibility: a flower over the letter body mostly goes behind.
            let over = tp.1 > -0.78 && tp.1 < 0.05 && tp.0.abs() < p.w * 0.5;
            let layer = if over {
                if r.rf() < 0.22 {
                    1
                } else {
                    0
                }
            } else if r.rf() < 0.6 {
                1
            } else {
                tl
            };
            // Reference-exact placement (garden.ts grow): the bloom sits
            // AT its stem tip - and rose tips have already been extended
            // (the whole-bloom clearance above) so the full disk clears
            // the letter's real ink with margin: intact flower heads away
            // from the strokes. The one-radius outward nudge tried here
            // earlier diverged from the source and was reverted: the
            // placement is upstream's; only the tip-clearance is a
            // documented terminal adaptation (one grid vs upstream's
            // separate canvases).
            els.push(El::Rose {
                id: *nid,
                x: tp.0,
                y: tp.1,
                r: r_size,
                rot: (r.rf() - 0.5) * 1.0,
                ph1: r.rf() * std::f32::consts::TAU,
                ph2: r.rf() * std::f32::consts::TAU,
                turns: 1.8 + r.rf() * 1.0,
                layer,
                d0: td,
                kind,
            });
            *nid += 1;
            if r.rf() < 0.2 {
                // The satellite rosette keeps its reference offset
                // (upstream: ta +/- 1.3 at 1.4 radii) but is constrained
                // to the stem's heading hemisphere: a downward offset could
                // swing the smaller bloom back over the letter ink,
                // recreating the cutout the clearance just avoided.
                let oo = ta + if (ta + 1.3).sin() <= 0.0 { 1.3 } else { -1.3 };
                els.push(El::Rose {
                    id: *nid,
                    x: tp.0 + oo.cos() * r_size * 1.4,
                    y: tp.1 + oo.sin() * r_size * 1.4,
                    r: r_size * (0.6 + r.rf() * 0.3),
                    rot: (r.rf() - 0.5) * 1.2,
                    ph1: r.rf() * std::f32::consts::TAU,
                    ph2: r.rf() * std::f32::consts::TAU,
                    turns: 1.8 + r.rf(),
                    layer,
                    d0: td + 120.0,
                    kind,
                });
                *nid += 1;
            }
        }
        "fan" => {
            let spread = 0.6 + r.rf() * 0.3;
            for j in 0..2u32 {
                els.push(El::Leaf {
                    id: *nid,
                    x: tp.0,
                    y: tp.1,
                    a: ta + (j as f32 - 0.5) * spread,
                    len: squash(0.2 + r.rf() * 0.2, 0.0, caps.leaf, r.rf()),
                    bend: (j as f32 - 0.5) * 0.8,
                    layer: tl,
                    d0: td + j as f32 * 60.0,
                });
                *nid += 1;
            }
        }
        "leaf" => {
            els.push(El::Leaf {
                id: *nid,
                x: tp.0,
                y: tp.1,
                a: ta + (r.rf() - 0.5) * 0.3,
                len: squash(0.16 + r.rf() * 0.16, 0.0, caps.leaf, r.rf()),
                bend: r.rf() - 0.5,
                layer: tl,
                d0: td,
            });
            *nid += 1;
        }
        _ => {}
    }
    if !o.depth {
        let branch_roll = r.rf();
        if branch_roll < (0.85_f32).min(0.3 * p.dens) {
            let u = 0.35 + r.rf() * 0.35;
            let (q, a) = at(&stem_pts, u);
            let sign_flip = r.rf() < 0.5;
            let branch_mag = 0.7 + r.rf() * 0.4;
            let len_scale = 0.35 + r.rf() * 0.2;
            let layer_here = layer_at(&segs, u);
            grow(
                r,
                els,
                nid,
                p,
                caps,
                GrowOpts {
                    base: q,
                    dir: a + if sign_flip { -1.0 } else { 1.0 } * branch_mag,
                    len: len * len_scale,
                    d0: o.d0 + dur * u,
                    depth: true,
                    layer0: Some(layer_here),
                    tip: None,
                    pts: None,
                    ink: o.ink,
                },
            );
        }
    }
}

/// The elements of one letter (garden.ts gen, deterministically seeded:
/// the reference's Math.random component is replaced by the word seed so
/// a frozen frame is reproducible). `caps` are the viewport plant caps in
/// design units, converted to em with the type size `s`.
pub fn gen(
    ch: char,
    ws: i32,
    wi: usize,
    prev_w: Option<f32>,
    chaos: f32,
    caps: PlantCaps,
    s: f32,
) -> LetterEls {
    let mut r = Rng::new(
        ws ^ ((wi as i32)
            .wrapping_add(1)
            .wrapping_mul(2654435761u32 as i32)),
    );
    let p = word_params(ws, chaos);
    // Absolute floors keep plants recognizable at tiny type; the viewport
    // caps keep them from towering at large type. Both in em at this s.
    let em_caps = EmCaps {
        stem: (caps.stem / s).max(0.2),
        rose: (caps.rose / s).max(0.08).max(2.5 / s),
        leaf: (caps.leaf / s).max(0.08).max(2.0 / s),
    };
    let w = super::font::advance(ch).unwrap_or(0.5);
    // The anchor letter's real ink shape - the whole-bloom clearance
    // target for every rose grown on this letter's own stems.
    let ink_ext = super::font::ink_extent(ch);
    let mut els: Vec<El> = Vec::new();
    let mut nid: u32 = 0;

    let mut n_up = 1;
    for k in 0..4 {
        if r.rf() < 0.45 * p.dens * if k > 0 { 0.7 } else { 1.0 } {
            n_up += 1;
        }
    }
    for i in 0..n_up {
        let base = ((r.rf() - 0.5) * w * 0.7, -r.rf() * 0.3);
        let dir = -std::f32::consts::FRAC_PI_2 + p.lean * 0.5 + (r.rf() - 0.5) * 0.7;
        let len = (0.6 + r.rf() * 0.5) * p.len;
        let tip = if wi == 0 && i == 0 {
            Some("rose")
        } else {
            None
        };
        grow(
            &mut r,
            &mut els,
            &mut nid,
            &p,
            em_caps,
            GrowOpts {
                base,
                dir,
                len,
                d0: 40.0 + i as f32 * 110.0,
                depth: false,
                layer0: None,
                tip,
                pts: None,
                ink: Some(ink_ext),
            },
        );
    }
    if r.rf() < (0.95_f32).min(0.4 * p.dens) {
        let base = ((r.rf() - 0.5) * w * 0.7, -0.1 - r.rf() * 0.35);
        let dir = std::f32::consts::FRAC_PI_2 + (r.rf() - 0.5) * 0.8;
        let len = (0.35 + r.rf() * 0.35) * p.len;
        grow(
            &mut r,
            &mut els,
            &mut nid,
            &p,
            em_caps,
            GrowOpts {
                base,
                dir,
                len,
                d0: 160.0,
                depth: false,
                layer0: None,
                tip: None,
                pts: None,
                ink: Some(ink_ext),
            },
        );
    }
    if let Some(pw) = prev_w {
        let bridge_roll = r.rf();
        if bridge_roll < p.bridge_p {
            let px = -(pw + w) / 2.0;
            let a = ((r.rf() - 0.5) * w * 0.7, -0.05 - r.rf() * 0.55);
            let b = (px + (r.rf() - 0.5) * 0.25, -0.05 - r.rf() * 0.55);
            let bulge = if r.rf() < 0.55 { -1.0 } else { 1.0 } * (0.4 + r.rf() * 0.4);
            let dx = b.0 - a.0;
            let mut pts = bez(
                a,
                (a.0 + dx * 0.15, a.1 + bulge),
                (b.0 - dx * 0.15, b.1 + bulge * 0.9),
                b,
                40,
            );
            if r.rf() < 0.45 {
                let keep = (pts.len() as f32 * (0.7 + r.rf() * 0.2)) as usize;
                pts.truncate(keep.max(2));
            }
            let tip = if r.rf() < 0.5 {
                "curl"
            } else if r.rf() < 0.5 {
                "leaf"
            } else {
                "rose"
            };
            let blen = dx.abs() + bulge.abs();
            grow(
                &mut r,
                &mut els,
                &mut nid,
                &p,
                em_caps,
                GrowOpts {
                    base: (0.0, 0.0),
                    dir: 0.0,
                    len: blen,
                    d0: 90.0,
                    depth: false,
                    layer0: None,
                    tip: Some(tip),
                    pts: Some(pts),
                    ink: Some(ink_ext),
                },
            );
        }
    }
    if wi == 0 {
        let base = (-w * 0.3, -0.15 - r.rf() * 0.4);
        let dir = std::f32::consts::PI + (r.rf() - 0.5) * 1.2;
        let len = 0.45 + r.rf() * 0.3;
        grow(
            &mut r,
            &mut els,
            &mut nid,
            &p,
            em_caps,
            GrowOpts {
                base,
                dir,
                len,
                d0: 120.0,
                depth: false,
                layer0: None,
                tip: Some("curl"),
                pts: None,
                ink: Some(ink_ext),
            },
        );
    }
    // The end-of-word tendril (garden.ts tend).
    let mut tr = Rng::new(ws ^ 0x977);
    let tend = {
        let mut t_els: Vec<El> = Vec::new();
        let mut tn: u32 = 90;
        let base = (w * 0.25, -0.1 - tr.rf() * 0.45);
        let dir = (tr.rf() - 0.5) * 1.4;
        let len = 0.45 + tr.rf() * 0.25;
        grow(
            &mut tr,
            &mut t_els,
            &mut tn,
            &p,
            em_caps,
            GrowOpts {
                base,
                dir,
                len,
                d0: 0.0,
                depth: true,
                layer0: None,
                tip: Some("curl"),
                pts: None,
                ink: Some(ink_ext),
            },
        );
        // The reference keeps at most one tendril stem.
        t_els.into_iter().find(|e| matches!(e, El::Stem { .. }))
    };
    LetterEls { els, tend }
}

/// Jitter pose (garden.ts jit): interpolate seeded poses with zero
/// velocity at each end; returns (dx, dy, dAngle) in design units.
fn jit(id: u32, f: f32, amp: f32) -> (f32, f32, f32) {
    if amp <= 0.0 {
        return (0.0, 0.0, 0.0);
    }
    let step = f.floor();
    let u = f - step;
    let ease = u * u * u * (u * (u * 6.0 - 15.0) + 10.0);
    let sample = |axis: f32, rate: f32, scale: f32| -> f32 {
        let a = h(id as f32 * axis + step * rate) * 2.0 - 1.0;
        let b = h(id as f32 * axis + (step + 1.0) * rate) * 2.0 - 1.0;
        (a + (b - a) * ease) * scale
    };
    (
        sample(1.37, 7.13, amp),
        sample(2.71, 3.11, amp),
        sample(5.3, 1.7, 0.018),
    )
}

/// Gust sway + birth settling (garden.ts wpt, both phases):
/// - BIRTH settling: for the first 2.5 s of the letter's life a damped
///   oscillation sprouts it in - x by S*0.07*hh*e^(-3.2t)*sin(11t) with
///   the direction alternating per letter id parity, y by
///   S*0.035*hh*e^(-3.2t)*sin(11t+1.2). Upstream gates it on
///   `l.birth != null && t > 0 && t < 2.5`; hh is NOT clamped here
///   (only the gust clamps at 2.5 em). Applies to every live line
///   change (each line births its letters); timeline-derived, so
///   frozen frames stay byte-identical.
/// - FACE gust (the ported sway): points higher up lean with a
///   travelling sine gust; height is measured from the letter baseline.
#[allow(clippy::too_many_arguments)]
fn wpt(
    lx: f32,
    ly: f32,
    x: f32,
    y: f32,
    s: f32,
    w_design: f32,
    t_s: f32,
    birth_t: f32,
    dir: f32,
) -> P {
    let (mut x, mut y) = (x, y);
    if birth_t > 0.0 && birth_t < 2.5 {
        let hh = ((ly - y) / s).max(0.0);
        let dmp = (-birth_t * 3.2).exp();
        x += s * 0.07 * hh * dmp * (birth_t * 11.0).sin() * dir;
        y += s * 0.035 * hh * dmp * (birth_t * 11.0 + 1.2).sin();
    }
    let height = ((ly - y) / s).clamp(0.0, 2.5);
    let phase = lx / w_design.max(1.0) * 4.0;
    let gust = (t_s * 0.85 - phase).sin() * 0.65 + (t_s * 1.35 - phase * 0.7).sin() * 0.2;
    (
        x + s * 0.055 * height * height * gust,
        y + s * 0.008 * height * (t_s * 0.85 - phase + 0.6).sin(),
    )
}

/// Two-tone leaf with a light midrib (garden.ts leaf), design units.
fn draw_leaf(g: &mut MaskGrid, bx: f32, by: f32, a: f32, len: f32, bend: f32) {
    if len < 0.5 * PX {
        return;
    }
    let ca = a.cos();
    let sa = a.sin();
    let px = -sa;
    let py = ca;
    let ax = |u: f32| -> P {
        let b = std::f32::consts::PI * u * bend * 0.15 * len;
        (bx + ca * u * len + px * b, by + sa * u * len + py * b)
    };
    let hw = |u: f32| len * 0.18 * (std::f32::consts::PI * u.powf(0.8)).sin();
    let n = 12;
    let mut s1 = Vec::with_capacity(n + 1);
    let mut s2 = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let u = i as f32 / n as f32;
        let c = ax(u);
        let w = hw(u);
        s1.push((c.0 + px * w, c.1 + py * w));
        s2.push((c.0 - px * w, c.1 - py * w));
    }
    let tip = ax(1.0);
    let base = ax(0.0);
    let mut half1: Vec<Pt> = vec![Pt(base.0, base.1)];
    half1.extend(s1[1..n].iter().map(|&(x, y)| Pt(x, y)));
    half1.push(Pt(tip.0, tip.1));
    g.fill_poly(&half1, Role::Stem);
    let mut half2: Vec<Pt> = vec![Pt(tip.0, tip.1)];
    half2.extend(s2[1..n].iter().rev().map(|&(x, y)| Pt(x, y)));
    half2.push(Pt(base.0, base.1));
    g.fill_poly(&half2, Role::LeafShadow);
    // Upstream ribs leaves over 14 px; at terminal resolution the
    // half-cell minimum line width would swallow a small leaf, so the
    // rib appears only once the leaf is wide enough to carry it.
    if len > 14.0 * PX && len * 0.18 > super::mask::MIN_LW * 1.2 {
        let rib: Vec<Pt> = (1..=8)
            .map(|i| {
                let (x, y) = ax(0.06 + 0.74 * i as f32 / 8.0);
                Pt(x, y)
            })
            .collect();
        g.stroke_polyline(&rib, (0.8 * PX).max(len * 0.03), Role::LeafLight);
    }
}

/// The rose (garden.ts rose): six petals back to front with radial
/// shading bands, petal edge arcs, then the spiral and sepal lines.
#[allow(clippy::too_many_arguments)]
fn draw_rose(
    g: &mut MaskGrid,
    cx: f32,
    cy: f32,
    r: f32,
    rot: f32,
    ph1: f32,
    ph2: f32,
    turns: f32,
    age_ms: f32,
) {
    if r < 0.8 * PX {
        return;
    }
    let cr = rot.cos();
    let sr = rot.sin();
    let t = |x: f32, y: f32| Pt(cx + x * cr - y * sr, cy + x * sr + y * cr);
    // Petals back to front: [cx, cy, rx, ry, angle, depth, edge].
    let petals: [[f32; 7]; 6] = [
        [0.0, -0.36, 0.56, 0.54, 0.0, 0.0, 0.0],
        [-0.5, -0.06, 0.60, 0.60, -0.35, 0.0, 0.0],
        [0.52, -0.06, 0.60, 0.60, 0.35, 0.0, 0.0],
        [-0.42, 0.30, 0.62, 0.48, 0.2, 0.12, 1.0],
        [0.47, 0.30, 0.62, 0.48, -0.2, 0.12, 1.0],
        [0.05, 0.42, 0.56, 0.38, 0.0, 0.14, 0.0],
    ];
    let lw = (0.8 * PX).max(r * 0.045);
    let flower_cands = [Role::Flower, Role::FlowerLight, Role::FlowerShadow];
    let light = super::mask::role_color(Role::FlowerLight);
    let base_col = FLOWER;
    let dark = super::mask::role_color(Role::FlowerShadow);
    for (i, q) in petals.iter().enumerate() {
        let s = spr((age_ms - i as f32 * 85.0) / 1000.0, 8.0, 14.0);
        if s <= 0.001 {
            continue;
        }
        let open = (1.0 - s.min(1.0)) * if q[0] < 0.0 { 0.5 } else { -0.5 };
        let ang = q[4] + open;
        let ca = ang.cos();
        let sa = ang.sin();
        let pt = |th: f32| -> Pt {
            let rr = 1.0 + 0.08 * (3.0 * th + ph1 + i as f32).sin() + 0.04 * (5.0 * th + ph2).sin();
            let lx = th.cos() * q[2] * rr;
            let ly = th.sin() * q[3] * rr;
            let px = q[0] + lx * ca - ly * sa;
            let py = q[1] + lx * sa + ly * ca;
            // Unfold from (0, 0.5), scaled to the rose radius.
            t(px * s * r, (0.5 + (py - 0.5) * s) * r)
        };
        let pts: Vec<Pt> = (0..26)
            .map(|k| pt(k as f32 / 26.0 * std::f32::consts::TAU))
            .collect();
        if pts.len() < 3 {
            continue;
        }
        let mut mx = 0.0;
        let mut my = 0.0;
        for p in &pts {
            mx += p.0;
            my += p.1;
        }
        mx /= pts.len() as f32;
        my /= pts.len() as f32;
        let front = i >= 3;
        let rr = r * (if front { 0.62 } else { 0.78 }) * s.min(1.0);
        let stops: Vec<(f32, (u8, u8, u8))> = if front {
            vec![(0.0, light), (0.5, base_col), (1.0, dark)]
        } else {
            vec![(0.0, base_col), (0.55, base_col), (1.0, dark)]
        };
        g.fill_grad_poly(
            &pts,
            mx - r * 0.14,
            my - r * 0.18,
            rr,
            &stops,
            &flower_cands,
        );
        if r >= 16.0 * PX && s > 0.5 {
            g.outline_poly(&pts, lw * 0.6, Role::FlowerShadow);
        }
        if q[6] > 0.5 && r >= 12.0 * PX && s > 0.4 {
            let arc: Vec<Pt> = (0..=14)
                .map(|k| pt(std::f32::consts::PI * (1.18 + 0.64 * k as f32 / 14.0)))
                .collect();
            g.stroke_range(&arc, 0.0, ((s - 0.4) / 0.5).min(1.0), lw * 0.8, Role::Line);
        }
    }
    // Spiral and sepal lines (garden.ts rose tail).
    let fr = eo((age_ms - 6.0 * 85.0 - 80.0) / 520.0);
    if fr > 0.0 && r > 3.0 * PX {
        let turns_eff = if r < 20.0 * PX {
            turns.min(1.3)
        } else if r < 32.0 * PX {
            turns * 0.8
        } else {
            turns
        };
        let sp: Vec<Pt> = (0..=70)
            .map(|i| {
                let u = i as f32 / 70.0;
                let th = ph1 + u * turns_eff * std::f32::consts::TAU;
                let rr = r * (0.08 + 0.57 * u);
                t(th.cos() * rr * 1.05, th.sin() * rr * 0.72 - 0.12 * r)
            })
            .collect();
        g.stroke_range(&sp, 0.0, fr, lw, Role::Line);
        if r >= 14.0 * PX {
            let scl: Vec<Pt> = (0..=36)
                .map(|i| {
                    let u = i as f32 / 36.0;
                    t(
                        (-0.72 + 1.5 * u) * r,
                        0.36 * r + 0.16 * r * (u * std::f32::consts::PI * 3.0).sin().abs(),
                    )
                })
                .collect();
            g.stroke_range(&scl, 0.0, fr, lw, Role::Line);
        }
    }
}

/// A shaded petal along a spine (garden.ts petal): left/right outline
/// points with width `wid * sin(PI * u^0.72) * (1 - 0.18 u)`, radial
/// gradient along the spine.
// The parameter list mirrors upstream's petal() verbatim (port
// fidelity beats refactoring the ported geometry pipeline).
#[allow(clippy::too_many_arguments)]
fn petal(
    g: &mut MaskGrid,
    t: &dyn Fn(f32, f32) -> Pt,
    ox: f32,
    oy: f32,
    ang: f32,
    len: f32,
    wid: f32,
    stops: &[(f32, (u8, u8, u8))],
    cands: &[Role],
    outline: Option<(f32, Role)>,
) {
    if len < 1.5 * PX {
        return;
    }
    let n = 9;
    let (dx, dy) = (ang.cos(), ang.sin());
    let (px, py) = (-dy, dx);
    let mut pts: Vec<Pt> = Vec::with_capacity(2 * (n + 1));
    for k in 0..=n {
        let u = k as f32 / n as f32;
        let w = wid * (std::f32::consts::PI * u.powf(0.72)).sin() * (1.0 - 0.18 * u);
        let (bx, by) = (ox + dx * len * u, oy + dy * len * u);
        pts.push(t(bx + px * w, by + py * w));
    }
    for k in (0..=n).rev() {
        let u = k as f32 / n as f32;
        let w = wid * (std::f32::consts::PI * u.powf(0.72)).sin() * (1.0 - 0.18 * u);
        let (bx, by) = (ox + dx * len * u, oy + dy * len * u);
        pts.push(t(bx - px * w, by - py * w));
    }
    let gc = t(ox + dx * len * 0.4, oy + dy * len * 0.4);
    g.fill_grad_poly(&pts, gc.0, gc.1, len * 0.75, stops, cands);
    if let Some((ow, orole)) = outline {
        g.outline_poly(&pts, ow, orole);
    }
}

/// A rounded blob polygon (garden.ts blobPts): radius wobble
/// `1 + 0.07 sin(3th + ph)`.
#[allow(clippy::too_many_arguments)] // upstream's parameter list, kept verbatim
fn blob_pts(
    t: &dyn Fn(f32, f32) -> Pt,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    ang: f32,
    ph: f32,
    n: usize,
) -> Vec<Pt> {
    let (ca, sa) = (ang.cos(), ang.sin());
    (0..n)
        .map(|k| {
            let th = k as f32 / n as f32 * std::f32::consts::TAU;
            let rr = 1.0 + 0.07 * (3.0 * th + ph).sin();
            let (lx, ly) = (th.cos() * rx * rr, th.sin() * ry * rr);
            t(cx + lx * ca - ly * sa, cy + lx * sa + ly * ca)
        })
        .collect()
}

/// A filled disc with a radial gradient (garden.ts disc), in the kind's
/// roles.
#[allow(clippy::too_many_arguments)] // upstream's parameter list, kept verbatim
fn disc(
    g: &mut MaskGrid,
    t: &dyn Fn(f32, f32) -> Pt,
    cx: f32,
    cy: f32,
    r: f32,
    stops: &[(f32, (u8, u8, u8))],
    ph: f32,
    cands: &[Role],
) {
    if r < 0.8 * PX {
        return;
    }
    let pts = blob_pts(t, cx, cy, r, r, 0.0, ph, 14);
    let gc = t(cx - r * 0.2, cy - r * 0.2);
    g.fill_grad_poly(&pts, gc.0, gc.1, r * 1.25, stops, cands);
}

/// The daisy (garden.ts drawKind "daisy"): 14 petals in two passes plus a
/// disc centre; petals stagger in pairs (U(i, 38)).
#[allow(clippy::too_many_arguments)]
fn draw_daisy(
    g: &mut MaskGrid,
    cx: f32,
    cy: f32,
    r: f32,
    rot: f32,
    ph1: f32,
    ph2: f32,
    age_ms: f32,
) {
    if r < 0.8 * PX {
        return;
    }
    let (cr, sr) = (rot.cos(), rot.sin());
    let t = |x: f32, y: f32| Pt(cx + x * cr - y * sr, cy + x * sr + y * cr);
    let (dark, base, light) = Kind::Daisy.roles();
    let (base_col, light_col, dark_col) = (
        super::mask::role_color(base),
        super::mask::role_color(light),
        super::mask::role_color(dark),
    );
    let cands = [dark, base, light];
    let lw = (0.8 * PX).max(r * 0.045);
    let ol = if r >= 26.0 * PX {
        Some((lw * 0.5, dark))
    } else {
        None
    };
    let u = |i: f32, st: f32| spr((age_ms - i * st) / 1000.0, 8.0, 14.0);
    let n = 14.0f32;
    for pass in 0..2 {
        let mut i = pass as f32;
        while i < n {
            let s = u(i, 38.0);
            if s > 0.001 {
                let ang = (i / n) * std::f32::consts::TAU + ph1;
                let stops: Vec<(f32, (u8, u8, u8))> = if pass == 1 {
                    vec![(0.0, light_col), (1.0, base_col)]
                } else {
                    vec![(0.0, light_col), (0.7, light_col), (1.0, base_col)]
                };
                petal(
                    g,
                    &t,
                    ang.cos() * r * 0.2,
                    ang.sin() * r * 0.2,
                    ang,
                    r * 0.95 * s,
                    r * 0.22,
                    &stops,
                    &cands,
                    ol,
                );
            }
            i += 2.0;
        }
    }
    let s = u(n, 38.0).min(1.0);
    disc(
        g,
        &t,
        0.0,
        0.0,
        r * 0.3 * s,
        &[(0.0, base_col), (1.0, dark_col)],
        ph2,
        &cands,
    );
}

/// The tulip (garden.ts drawKind "tulip"): three cup petals from a base
/// slightly below the centre; the centre petal blooms last (U(1, 90)).
#[allow(clippy::too_many_arguments)]
fn draw_tulip(g: &mut MaskGrid, cx: f32, cy: f32, r: f32, rot: f32, age_ms: f32) {
    if r < 0.8 * PX {
        return;
    }
    let (cr, sr) = (rot.cos(), rot.sin());
    let t = |x: f32, y: f32| Pt(cx + x * cr - y * sr, cy + x * sr + y * cr);
    let (dark, base, light) = Kind::Tulip.roles();
    let (base_col, light_col, dark_col) = (
        super::mask::role_color(base),
        super::mask::role_color(light),
        super::mask::role_color(dark),
    );
    let cands = [dark, base, light];
    let lw = (0.8 * PX).max(r * 0.045);
    let ol = if r >= 26.0 * PX {
        Some((lw, dark))
    } else {
        None
    };
    let u = |i: f32, st: f32| spr((age_ms - i * st) / 1000.0, 8.0, 14.0);
    let s = u(0.0, 0.0);
    if s <= 0.001 {
        return;
    }
    let by = r * 0.55;
    let h = r * 1.55 * s;
    let shade: Vec<(f32, (u8, u8, u8))> = vec![(0.0, light_col), (0.5, base_col), (1.0, dark_col)];
    petal(
        g,
        &t,
        0.0,
        by,
        -std::f32::consts::FRAC_PI_2 - 0.34,
        h,
        r * 0.42,
        &shade,
        &cands,
        ol,
    );
    petal(
        g,
        &t,
        0.0,
        by,
        -std::f32::consts::FRAC_PI_2 + 0.34,
        h,
        r * 0.42,
        &shade,
        &cands,
        ol,
    );
    let s2 = u(1.0, 90.0).min(1.1);
    let lit: Vec<(f32, (u8, u8, u8))> = vec![(0.0, dark_col), (0.45, base_col), (1.0, light_col)];
    petal(
        g,
        &t,
        0.0,
        by,
        -std::f32::consts::FRAC_PI_2,
        h * 1.04 * s2,
        r * 0.46,
        &lit,
        &cands,
        ol,
    );
}

/// The poppy (garden.ts drawKind "poppy"): four overlapping blob petals
/// plus a spoked centre; petals stagger individually (U(i, 90)).
#[allow(clippy::too_many_arguments)]
fn draw_poppy(
    g: &mut MaskGrid,
    cx: f32,
    cy: f32,
    r: f32,
    rot: f32,
    ph1: f32,
    ph2: f32,
    age_ms: f32,
) {
    if r < 0.8 * PX {
        return;
    }
    let (cr, sr) = (rot.cos(), rot.sin());
    let t = |x: f32, y: f32| Pt(cx + x * cr - y * sr, cy + x * sr + y * cr);
    let (dark, base, light) = Kind::Poppy.roles();
    let (base_col, light_col, dark_col) = (
        super::mask::role_color(base),
        super::mask::role_color(light),
        super::mask::role_color(dark),
    );
    let cands = [dark, base, light];
    let lw = (0.8 * PX).max(r * 0.045);
    let u = |i: f32, st: f32| spr((age_ms - i * st) / 1000.0, 8.0, 14.0);
    let quads: [[f32; 5]; 4] = [
        [-0.42, -0.1, 0.62, 0.58, -0.25],
        [0.42, -0.1, 0.62, 0.58, 0.25],
        [-0.3, 0.3, 0.55, 0.5, 0.2],
        [0.3, 0.3, 0.55, 0.5, -0.2],
    ];
    for (i, q) in quads.iter().enumerate() {
        let s = u(i as f32, 90.0);
        if s <= 0.001 {
            continue;
        }
        let (qx, qy) = (q[0] * r * s, q[1] * r * s);
        let pts = blob_pts(
            &t,
            qx,
            qy,
            q[2] * r * s,
            q[3] * r * s,
            q[4],
            ph1 + i as f32,
            22,
        );
        let gc = t(qx - r * 0.1, qy - r * 0.12);
        let stops: Vec<(f32, (u8, u8, u8))> = if i < 2 {
            vec![(0.0, base_col), (0.6, base_col), (1.0, dark_col)]
        } else {
            vec![(0.0, light_col), (0.55, base_col), (1.0, dark_col)]
        };
        g.fill_grad_poly(&pts, gc.0, gc.1, r * 0.75 * s, &stops, &cands);
        if r >= 16.0 * PX {
            g.outline_poly(&pts, lw * 0.6, dark);
        }
    }
    let s = u(4.0, 90.0);
    for k in 0..12 {
        let ang = (k as f32 / 12.0) * std::f32::consts::TAU + ph2;
        let (r0, r1) = (
            r * 0.2,
            r * (0.36 + 0.1 * (k as f32 * 2.7).sin()) * s.min(1.0),
        );
        g.stroke_polyline(
            &[
                t(ang.cos() * r0, ang.sin() * r0),
                t(ang.cos() * r1, ang.sin() * r1),
            ],
            lw,
            dark,
        );
    }
    disc(
        g,
        &t,
        0.0,
        0.0,
        r * 0.2 * s.min(1.0),
        &[(0.0, dark_col), (1.0, dark_col)],
        ph1,
        &cands,
    );
}

/// The blossom (garden.ts drawKind "blossom"): five petals plus a ring
/// of eight stamen strokes with tip discs and a small centre disc;
/// petals stagger U(i, 80).
#[allow(clippy::too_many_arguments)]
fn draw_blossom(
    g: &mut MaskGrid,
    cx: f32,
    cy: f32,
    r: f32,
    rot: f32,
    ph1: f32,
    ph2: f32,
    age_ms: f32,
) {
    if r < 0.8 * PX {
        return;
    }
    let (cr, sr) = (rot.cos(), rot.sin());
    let t = |x: f32, y: f32| Pt(cx + x * cr - y * sr, cy + x * sr + y * cr);
    let (dark, base, light) = Kind::Blossom.roles();
    let (base_col, light_col, dark_col) = (
        super::mask::role_color(base),
        super::mask::role_color(light),
        super::mask::role_color(dark),
    );
    let cands = [dark, base, light];
    let lw = (0.8 * PX).max(r * 0.045);
    let ol = if r >= 26.0 * PX {
        Some((lw * 0.7, dark))
    } else {
        None
    };
    let u = |i: f32, st: f32| spr((age_ms - i * st) / 1000.0, 8.0, 14.0);
    for i in 0..5 {
        let s = u(i as f32, 80.0);
        if s <= 0.001 {
            continue;
        }
        let ang = ph1 + (i as f32 / 5.0) * std::f32::consts::TAU;
        let stops: Vec<(f32, (u8, u8, u8))> =
            vec![(0.0, dark_col), (0.3, base_col), (1.0, light_col)];
        petal(
            g,
            &t,
            ang.cos() * r * 0.12,
            ang.sin() * r * 0.12,
            ang,
            r * 0.95 * s,
            r * 0.34,
            &stops,
            &cands,
            ol,
        );
    }
    let s = u(5.0, 80.0).min(1.0);
    for k in 0..8 {
        let ang = ph2 + (k as f32 / 8.0) * std::f32::consts::TAU;
        let len = r * (0.26 + 0.06 * (k as f32 * 3.1).sin()) * s;
        g.stroke_polyline(
            &[t(0.0, 0.0), t(ang.cos() * len, ang.sin() * len)],
            lw * 0.6,
            dark,
        );
        if r >= 18.0 * PX {
            disc(
                g,
                &t,
                ang.cos() * len,
                ang.sin() * len,
                r * 0.05,
                &[(0.0, light_col), (1.0, base_col)],
                0.0,
                &cands,
            );
        }
    }
    disc(
        g,
        &t,
        0.0,
        0.0,
        r * 0.1 * s,
        &[(0.0, dark_col), (1.0, dark_col)],
        ph2,
        &cands,
    );
}

/// The lily (garden.ts drawKind "lily"): six petals in the bloom order
/// [0,5,1,4,2,3] fanning upward from below the centre, plus three
/// stamen strokes with tip discs; petals stagger U(i, 70).
#[allow(clippy::too_many_arguments)]
fn draw_lily(g: &mut MaskGrid, cx: f32, cy: f32, r: f32, rot: f32, age_ms: f32) {
    if r < 0.8 * PX {
        return;
    }
    let (cr, sr) = (rot.cos(), rot.sin());
    let t = |x: f32, y: f32| Pt(cx + x * cr - y * sr, cy + x * sr + y * cr);
    let (dark, base, light) = Kind::Lily.roles();
    let (base_col, light_col, dark_col) = (
        super::mask::role_color(base),
        super::mask::role_color(light),
        super::mask::role_color(dark),
    );
    let cands = [dark, base, light];
    let lw = (0.8 * PX).max(r * 0.045);
    let ol = if r >= 26.0 * PX {
        Some((lw * 0.8, dark))
    } else {
        None
    };
    let u = |i: f32, st: f32| spr((age_ms - i * st) / 1000.0, 8.0, 14.0);
    let lit: Vec<(f32, (u8, u8, u8))> = vec![(0.0, dark_col), (0.45, base_col), (1.0, light_col)];
    for i in [0.0f32, 5.0, 1.0, 4.0, 2.0, 3.0] {
        let s = u(i, 70.0);
        if s <= 0.001 {
            continue;
        }
        let ang = -std::f32::consts::FRAC_PI_2 + (i - 2.5) * 0.56;
        petal(
            g,
            &t,
            0.0,
            r * 0.3,
            ang,
            r * 1.2 * s,
            r * 0.2,
            &lit,
            &cands,
            ol,
        );
    }
    let s = u(6.0, 70.0).min(1.0);
    for k in 0..3 {
        let ang = -std::f32::consts::FRAC_PI_2 + (k as f32 - 1.0) * 0.42;
        let len = r * (0.85 + 0.1 * k as f32) * s;
        let (tx, ty) = (ang.cos() * len, r * 0.3 + ang.sin() * len);
        g.stroke_polyline(&[t(0.0, r * 0.3), t(tx, ty)], lw * 0.7, light);
        if r >= 16.0 * PX {
            disc(
                g,
                &t,
                tx,
                ty,
                r * 0.06,
                &[(0.0, dark_col), (1.0, dark_col)],
                0.0,
                &cands,
            );
        }
    }
}

/// The bell (garden.ts drawKind "bell"): three hanging bell flowers on
/// short stalks at fixed spots; each stalk uses the STEM-dark tone
/// (upstream C.blueD) and the bell body the kind tones; stagger
/// U(i, 110).
#[allow(clippy::too_many_arguments)]
fn draw_bell(g: &mut MaskGrid, cx: f32, cy: f32, r: f32, rot: f32, age_ms: f32) {
    if r < 0.8 * PX {
        return;
    }
    let (cr, sr) = (rot.cos(), rot.sin());
    let t = |x: f32, y: f32| Pt(cx + x * cr - y * sr, cy + x * sr + y * cr);
    let (dark, base, light) = Kind::Bell.roles();
    let (base_col, light_col, dark_col) = (
        super::mask::role_color(base),
        super::mask::role_color(light),
        super::mask::role_color(dark),
    );
    let cands = [dark, base, light];
    let lw = (0.8 * PX).max(r * 0.045);
    let u = |i: f32, st: f32| spr((age_ms - i * st) / 1000.0, 8.0, 14.0);
    let spots: [[f32; 2]; 3] = [[-0.5, 0.0], [0.42, 0.28], [-0.02, 0.62]];
    let shape: [[f32; 2]; 11] = [
        [-0.1, -0.36],
        [-0.17, -0.1],
        [-0.36, 0.3],
        [-0.28, 0.42],
        [-0.1, 0.34],
        [0.0, 0.42],
        [0.1, 0.34],
        [0.28, 0.42],
        [0.36, 0.3],
        [0.17, -0.1],
        [0.1, -0.36],
    ];
    for (i, q) in spots.iter().enumerate() {
        let s = u(i as f32, 110.0);
        if s <= 0.001 {
            continue;
        }
        let (bx, by) = (q[0] * r, q[1] * r);
        let k = s.min(1.2) * r;
        // The stalk: upstream draws it in the STEM-dark tone.
        g.stroke_polyline(&[t(0.0, 0.0), t(bx, by - 0.4 * k)], lw, Role::LeafShadow);
        let pts: Vec<Pt> = shape
            .iter()
            .map(|p| t(bx + p[0] * k, by + p[1] * k))
            .collect();
        let gc = t(bx, by - 0.1 * k);
        g.fill_grad_poly(
            &pts,
            gc.0,
            gc.1,
            0.55 * k,
            &[(0.0, light_col), (0.6, base_col), (1.0, dark_col)],
            &cands,
        );
        if r >= 16.0 * PX {
            g.stroke_polyline(
                &[
                    t(bx - 0.3 * k, by + 0.38 * k),
                    t(bx + 0.3 * k, by + 0.38 * k),
                ],
                lw * 0.7,
                dark,
            );
        }
    }
}

/// The lavender (garden.ts drawKind "lavender"): a tall stem in the
/// STEM-dark tone with nine staggered pairs of tiny blob florets up it,
/// tapering, plus a tip disc; florets stagger U(i, 55).
#[allow(clippy::too_many_arguments)]
fn draw_lavender(
    g: &mut MaskGrid,
    cx: f32,
    cy: f32,
    r: f32,
    rot: f32,
    ph1: f32,
    ph2: f32,
    age_ms: f32,
) {
    if r < 0.8 * PX {
        return;
    }
    let (cr, sr) = (rot.cos(), rot.sin());
    let t = |x: f32, y: f32| Pt(cx + x * cr - y * sr, cy + x * sr + y * cr);
    let (dark, base, light) = Kind::Lavender.roles();
    // Upstream's floret stops are light -> base only ([[0, C.redL],
    // [1, C.red]]) - no dark tone in this species' gradient.
    let (base_col, light_col) = (
        super::mask::role_color(base),
        super::mask::role_color(light),
    );
    let cands = [dark, base, light];
    let lw = (0.8 * PX).max(r * 0.045);
    let u = |i: f32, st: f32| spr((age_ms - i * st) / 1000.0, 8.0, 14.0);
    let n = 9.0f32;
    let top = r * 1.9;
    // The main stem: STEM-dark (upstream C.blueD).
    let stem_s = u(0.0, 0.0).min(1.0);
    g.stroke_polyline(
        &[t(0.0, r * 0.1), t(0.0, r * 0.1 - top * stem_s)],
        lw,
        Role::LeafShadow,
    );
    let mut i = 0.0f32;
    while i < n {
        let uu = i / n;
        let s = u(i, 55.0);
        if s > 0.001 {
            let y = r * 0.1 - uu * top * 0.95;
            let w = r * 0.2 * (1.0 - 0.45 * uu);
            for sd in [-1.0f32, 1.0] {
                let bx = sd * w * 0.9 * s;
                let pts = blob_pts(
                    &t,
                    bx,
                    y,
                    r * 0.13 * (1.0 - 0.4 * uu) * s,
                    r * 0.18 * s,
                    sd * 0.5,
                    ph1 + i,
                    12,
                );
                let gc = t(bx, y);
                g.fill_grad_poly(
                    &pts,
                    gc.0,
                    gc.1,
                    r * 0.2,
                    &[(0.0, light_col), (1.0, base_col)],
                    &cands,
                );
            }
        }
        i += 1.0;
    }
    let s = u(n, 55.0).min(1.0);
    disc(
        g,
        &t,
        0.0,
        r * 0.1 - top,
        r * 0.1 * s,
        &[(0.0, light_col), (1.0, base_col)],
        ph2,
        &cands,
    );
}

/// The fern (garden.ts drawKind "fern"): a curved mid-rib with 13
/// staggered pairs of tapering pinnae; upstream draws the fronds in the
/// STEM-TONE OVERRIDE (C.blue/blueD/blueL = the fern kind's own green
/// tones), the side-A pinnae base->light and side-B dark->base.
#[allow(clippy::too_many_arguments)]
fn draw_fern(g: &mut MaskGrid, cx: f32, cy: f32, r: f32, rot: f32, ph2: f32, age_ms: f32) {
    if r < 0.8 * PX {
        return;
    }
    let (cr, sr) = (rot.cos(), rot.sin());
    let t = |x: f32, y: f32| Pt(cx + x * cr - y * sr, cy + x * sr + y * cr);
    let (dark, base, light) = Kind::Fern.roles();
    let (base_col, light_col, dark_col) = (
        super::mask::role_color(base),
        super::mask::role_color(light),
        super::mask::role_color(dark),
    );
    let cands = [dark, base, light];
    let lw = (0.8 * PX).max(r * 0.045);
    let u = |i: f32, st: f32| spr((age_ms - i * st) / 1000.0, 8.0, 14.0);
    let hgt = r * 2.0;
    let sgn = if ph2 > std::f32::consts::PI {
        1.0
    } else {
        -1.0
    };
    // The mid-rib: 10 points along the curve, in the LIGHT tone
    // (upstream C.blueL).
    let rib_s = u(0.0, 0.0).min(1.0);
    let mid: Vec<Pt> = (0..=10)
        .map(|i| {
            let uu = i as f32 / 10.0 * rib_s;
            t(sgn * r * 0.45 * (uu * 1.6).sin(), -hgt * uu)
        })
        .collect();
    if mid.len() > 1 {
        g.stroke_polyline(&mid, lw, light);
    }
    for i in 1..=13 {
        let uu = i as f32 / 14.0;
        let s = u(i as f32, 40.0);
        if s <= 0.001 {
            continue;
        }
        let (mx, my) = (sgn * r * 0.45 * (uu * 1.6).sin(), -hgt * uu);
        let tang = (-hgt).atan2(sgn * r * 0.45 * 1.6 * (uu * 1.6).cos());
        let len = r * 0.62 * (1.0 - 0.8 * uu) * s;
        petal(
            g,
            &t,
            mx,
            my,
            tang - 1.15,
            len,
            r * 0.1,
            &[(0.0, base_col), (1.0, light_col)],
            &cands,
            None,
        );
        petal(
            g,
            &t,
            mx,
            my,
            tang + 1.15,
            len,
            r * 0.1,
            &[(0.0, dark_col), (1.0, base_col)],
            &cands,
            None,
        );
    }
}

/// The dandelion (garden.ts drawKind "dandelion"): 26 radial seed
/// strokes with tip discs plus a centre disc, in the PULLED grey tone
/// (upstream's own palette rule; ink-ambiguous to colour-only audits,
/// disambiguated by the provenance oracle - see mod's audits).
#[allow(clippy::too_many_arguments)]
fn draw_dandelion(g: &mut MaskGrid, cx: f32, cy: f32, r: f32, rot: f32, ph1: f32, age_ms: f32) {
    if r < 0.8 * PX {
        return;
    }
    let (cr, sr) = (rot.cos(), rot.sin());
    let t = |x: f32, y: f32| Pt(cx + x * cr - y * sr, cy + x * sr + y * cr);
    let (dark, base, light) = Kind::Dandelion.roles();
    let (base_col, _light_col, _dark_col) = (
        super::mask::role_color(base),
        super::mask::role_color(light),
        super::mask::role_color(dark),
    );
    let cands = [dark, base, light];
    let lw = (0.8 * PX).max(r * 0.045);
    let u = |i: f32, st: f32| spr((age_ms - i * st) / 1000.0, 8.0, 14.0);
    let n = 26.0f32;
    let mut i = 0.0f32;
    while i < n {
        let s = u((i / 4.0).floor(), 60.0).min(1.1);
        if s > 0.001 {
            let ang = (i / n) * std::f32::consts::TAU + ph1;
            let len = r * (0.75 + 0.22 * (i * 2.3).sin()) * s;
            let (x, y) = (ang.cos() * len, ang.sin() * len);
            g.stroke_polyline(&[t(0.0, 0.0), t(x, y)], lw * 0.8, light);
            if r >= 18.0 * PX {
                disc(
                    g,
                    &t,
                    x,
                    y,
                    r * 0.07,
                    &[(0.0, _light_col), (1.0, base_col)],
                    0.0,
                    &cands,
                );
            }
        }
        i += 1.0;
    }
    disc(
        g,
        &t,
        0.0,
        0.0,
        r * 0.16,
        &[(0.0, base_col), (1.0, _dark_col)],
        ph1,
        &cands,
    );
}

/// Draw one scene frame (garden.ts render, single-mask z-order: back
/// layer, then the type, then the front layer - the terminal has one cell
/// grid, so the reference's separate flower/text/flower surfaces collapse
/// into painter's order over one mask).
pub struct SceneLetter<'a> {
    pub ch: char,
    pub tx: f32,
    pub ty: f32,
    pub els: &'a LetterEls,
    /// Birth offset from the line start, ms (staggered regrow).
    pub birth_ms: f32,
    /// The letter's scene index - the birth-settling direction
    /// alternates on its parity (upstream `l.id`).
    pub id: u32,
    /// True when the next letter is a space or this is the last letter:
    /// the reference draws the end-of-word tendril only here.
    pub word_end: bool,
    /// Wrapped line index (front-layer elements may cover only their own
    /// line's letters).
    pub line: usize,
}

#[allow(clippy::too_many_arguments)]
pub fn draw_scene(
    g: &mut MaskGrid,
    letters: &[SceneLetter<'_>],
    type_mask: &super::font::TypeMask,
    s: f32,
    age_ms: f32,
    energy: f32,
    w_design: f32,
    // Visible-TYPE onset per letter id (ms from the line start): a
    // letter's INK appears when `age_ms` reaches it - the WORD's own
    // start, 0 ms offset (neither early nor late). Empty = no word
    // timings: the whole line's ink is visible from the line start
    // (the line-level behavior). This is SEPARATE from the decorative
    // `SceneLetter::birth_ms`, which keeps the reference's staggered
    // grow for stems and blooms around the already-visible word.
    type_births: &[f32],
) {
    let chaos = chaos_for(s / super::mask::ROW_DESIGN);
    // Upstream boil: max(0.8 px, S * 0.01) scaled by chaos; the music's
    // smoothed energy modulates it gently (calm in quiet passages).
    let amp =
        (0.8 * PX).max(s * 0.01) * (0.55 + chaos * 2.4) * (0.6 + 0.8 * energy.clamp(0.0, 1.0));
    let f = age_ms / 420.0;
    let t_s = age_ms / 1000.0;

    let draw_el = |g: &mut MaskGrid, l: &SceneLetter, el: &El, age: f32, layer: u8| {
        let j = jit(el.id(), f, amp);
        let (x0, y0) = (l.tx, l.ty);
        // Birth-settling direction alternates per letter id (garden.ts:
        // `l.id % 2 ? 1 : -1`).
        let dir = if l.id % 2 == 1 { 1.0 } else { -1.0 };
        match el {
            El::Stem {
                pts,
                segs,
                d0,
                dur,
                w,
                thorns,
                ..
            } => {
                let fr = eo((age - d0) / dur);
                if fr <= 0.0 {
                    return;
                }
                let p: Vec<Pt> = pts
                    .iter()
                    .map(|&(px, py)| {
                        let (wx, wy) = wpt(
                            x0,
                            y0,
                            x0 + px * s + j.0,
                            y0 + py * s + j.1,
                            s,
                            w_design,
                            t_s,
                            age / 1000.0,
                            dir,
                        );
                        Pt(wx, wy)
                    })
                    .collect();
                for sg in segs {
                    if sg.layer != layer {
                        continue;
                    }
                    let b = sg.u1.min(fr);
                    if b <= sg.u0 {
                        continue;
                    }
                    g.stroke_range(&p, sg.u0, b, s * 0.022 * w, Role::Stem);
                }
                for &(tu, ts) in thorns {
                    if fr > tu && layer_at(segs, tu) == layer {
                        // thorn (garden.ts thorn): a short angled stroke.
                        let (p, a) = at(pts, tu);
                        let d = a + ts * 2.3;
                        let (sx, sy) = wpt(
                            x0,
                            y0,
                            x0 + p.0 * s + j.0,
                            y0 + p.1 * s + j.1,
                            s,
                            w_design,
                            t_s,
                            age / 1000.0,
                            dir,
                        );
                        let len = s * 0.045;
                        g.stroke_polyline(
                            &[Pt(sx, sy), Pt(sx + d.cos() * len, sy + d.sin() * len)],
                            s * 0.022,
                            Role::Stem,
                        );
                    }
                }
            }
            El::Leaf {
                x,
                y,
                a,
                len,
                bend,
                d0,
                ..
            } => {
                if *d0 > age {
                    return;
                }
                let sc = spr((age - d0) / 1000.0, 7.0, 15.0);
                if sc <= 0.0 {
                    return;
                }
                let (wx, wy) = wpt(
                    x0,
                    y0,
                    x0 + x * s + j.0,
                    y0 + y * s + j.1,
                    s,
                    w_design,
                    t_s,
                    age / 1000.0,
                    dir,
                );
                draw_leaf(g, wx, wy, a + j.2, len * s * sc, *bend);
            }
            El::Rose {
                x,
                y,
                r,
                rot,
                ph1,
                ph2,
                turns,
                d0,
                kind,
                ..
            } => {
                let a = age - d0;
                if a < 0.0 {
                    return;
                }
                let (wx, wy) = wpt(
                    x0,
                    y0,
                    x0 + x * s + j.0,
                    y0 + y * s + j.1,
                    s,
                    w_design,
                    t_s,
                    age / 1000.0,
                    dir,
                );
                match kind {
                    Kind::Rose => {
                        draw_rose(g, wx, wy, r * s, rot + j.2, *ph1, *ph2, *turns, a);
                    }
                    Kind::Daisy => draw_daisy(g, wx, wy, r * s, rot + j.2, *ph1, *ph2, a),
                    Kind::Tulip => draw_tulip(g, wx, wy, r * s, rot + j.2, a),
                    Kind::Poppy => draw_poppy(g, wx, wy, r * s, rot + j.2, *ph1, *ph2, a),
                    Kind::Blossom => draw_blossom(g, wx, wy, r * s, rot + j.2, *ph1, *ph2, a),
                    Kind::Lily => draw_lily(g, wx, wy, r * s, rot + j.2, a),
                    Kind::Bell => draw_bell(g, wx, wy, r * s, rot + j.2, a),
                    Kind::Lavender => draw_lavender(g, wx, wy, r * s, rot + j.2, *ph1, *ph2, a),
                    Kind::Fern => draw_fern(g, wx, wy, r * s, rot + j.2, *ph2, a),
                    Kind::Dandelion => draw_dandelion(g, wx, wy, r * s, rot + j.2, *ph1, a),
                }
            }
        }
    };

    // Back layer.
    for l in letters {
        let age = age_ms - l.birth_ms;
        for el in &l.els.els {
            draw_el(g, l, el, age, 0);
        }
        // The end-of-word tendril grows only at word ends (garden.ts
        // checks the next letter being a space).
        if l.word_end {
            if let Some(tend) = &l.els.tend {
                let fr = eo((age - 150.0) / 450.0);
                if fr > 0.0 {
                    draw_el(g, l, tend, age, 0);
                }
            }
        }
    }
    // The type: the cached anti-aliased Playfair mask, thresholded at half
    // coverage - the point where a canvas edge pixel's colour turns
    // nearer the type role than the paper in upstream's 15-bit LUT.
    // WORD-GATED: each letter's ink appears at its WORD's onset
    // (type_births), so words show when sung; the decorative growth
    // keeps its own staggered birth. Cells without a letter tag (or
    // without a schedule entry) render immediately.
    if type_mask.w == g.w && type_mask.h == g.h {
        let mut totals: Vec<u32> = Vec::new();
        for (k, &c) in type_mask.cov.iter().enumerate() {
            if c >= 128 {
                let l = type_mask.letter[k];
                if l != 0 {
                    if let Some(&tb) = type_births.get(l as usize - 1) {
                        if age_ms < tb {
                            continue; // this word has not been sung yet
                        }
                    }
                }
                g.role[k] = Role::Type as u8;
                g.owner[k] = type_mask.line.get(k).copied().unwrap_or(1);
                g.letter[k] = l;
                if totals.len() <= l as usize {
                    totals.resize(l as usize + 1, 0);
                }
                totals[l as usize] += 1;
            }
        }
        // Arm the per-letter readability cap for the front pass: the
        // INTERNAL budget (mask::BUDGET_COVER, 2pp under the 20% user
        // acceptance) keeps actual pixels safely below it.
        g.begin_letter_cover(totals);
    }
    // Front layer: each element is guarded so it may weave over its own
    // line's letters but never over another line's. Small type (an em
    // under ~8 rows) cannot afford any occlusion, so there every letter
    // is protected and the whole garden sits behind the type.
    let protect_all = s / super::mask::ROW_DESIGN < 8.0;
    for l in letters {
        g.guard = if protect_all {
            255
        } else {
            (l.line + 1).min(254) as u8
        };
        let age = age_ms - l.birth_ms;
        for el in &l.els.els {
            draw_el(g, l, el, age, 1);
        }
        if l.word_end {
            if let Some(tend) = &l.els.tend {
                let fr = eo((age - 150.0) / 450.0);
                if fr > 0.0 {
                    draw_el(g, l, tend, age, 1);
                }
            }
        }
    }
    g.guard = 0;
    g.end_letter_cover();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_matches_reference_sequence_shape() {
        let mut r = Rng::new(7);
        let v: Vec<f32> = (0..8).map(|_| r.rf()).collect();
        // Deterministic and spread.
        let mut r2 = Rng::new(7);
        for x in &v {
            assert!((0.0..1.0).contains(x));
            let y = r2.rf();
            assert!((x - y).abs() < 1e-9);
        }
        assert!(v.iter().map(|&x| if x < 0.5 { 0 } else { 1 }).sum::<i32>() > 0);
        assert!(v.iter().map(|&x| if x < 0.5 { 1 } else { 0 }).sum::<i32>() > 0);
    }

    #[test]
    fn spr_is_spring_and_eo_is_ease_out() {
        assert_eq!(spr(-1.0, 7.0, 16.0), 0.0);
        assert!(spr(0.05, 7.0, 16.0) > 0.0);
        assert_eq!(eo(0.0), 0.0);
        assert_eq!(eo(1.0), 1.0);
        assert!(eo(0.5) > 0.5);
    }

    #[test]
    fn gen_is_deterministic_and_produces_elements() {
        let a = gen(
            'g',
            12345,
            0,
            Some(0.5),
            CHAOS,
            PlantCaps::for_band(80.0),
            17.8,
        );
        let b = gen(
            'g',
            12345,
            0,
            Some(0.5),
            CHAOS,
            PlantCaps::for_band(80.0),
            17.8,
        );
        assert_eq!(format!("{:?}", a.els.len()), format!("{:?}", b.els.len()));
        assert!(!a.els.is_empty(), "a letter must grow something");
        let rose_first = gen('s', 999, 0, None, CHAOS, PlantCaps::for_band(80.0), 17.8);
        // First letter of a word starts with a rose-tipped stem.
        assert!(matches!(rose_first.els[0], El::Stem { .. }));
        // Different seeds give different gardens.
        let c = gen(
            'g',
            54321,
            0,
            Some(0.5),
            CHAOS,
            PlantCaps::for_band(80.0),
            17.8,
        );
        assert!(
            format!("{:?}", a.els) != format!("{:?}", c.els),
            "seeds must vary the garden"
        );
    }

    #[test]
    fn vine_curl_stay_bounded() {
        let mut pts = vine(
            (0.0, 0.0),
            -std::f32::consts::FRAC_PI_2,
            1.0,
            0.5,
            1.5,
            0.3,
            0.2,
            40,
        );
        curl(&mut pts, 1.0, 0.06);
        assert_eq!(pts.len(), 41 + 14);
        for (x, y) in &pts {
            assert!(x.abs() < 3.0 && y.abs() < 3.0);
        }
    }

    #[test]
    fn draw_scene_paints_type_stems_and_roses() {
        use super::super::mask::{Role, N_SUB};
        // Scene geometry, fixed honestly: the reference-faithful stem scale
        // (up to ~1.1 em) needs ~2 em of headroom above the baseline for
        // the first-letter rose to bloom INSIDE the frame. The old 12x8
        // grid with the letter at y=6 clipped the bloom above the frame
        // entirely (flower_n == 0) - a clipping artifact of the tiny scene,
        // not a bloom regression. All assertions are unchanged.
        let els = gen('o', 4242, 0, None, CHAOS, PlantCaps::for_band(80.0), 17.8);
        let letters = [SceneLetter {
            ch: 'o',
            tx: 10.0,
            ty: 20.0,
            els: &els,
            birth_ms: 0.0,
            id: 0,
            word_end: true,
            line: 0,
        }];
        let mut g = MaskGrid::new(20, 16);
        let mut tm = super::super::font::TypeMask::new(g.w, g.h);
        tm.draw_glyph('o', 10.0, 20.0, 8.0, 4.0);
        draw_scene(&mut g, &letters, &tm, 8.0, 5000.0, 0.3, 20.0, &[]);
        let roles = g.role.clone();
        let type_n = roles.iter().filter(|&&r| r == Role::Type as u8).count();
        let stem_n = roles.iter().filter(|&&r| r == Role::Stem as u8).count();
        // Any bloom-family cell (the rose's flower family or a species'
        // own kind family) - the forced first bloom may be any species.
        let flower_n = roles
            .iter()
            .filter(|&&r| {
                let f = super::super::mask::FAM[r as usize];
                f == 1 || f >= 5
            })
            .count();
        assert!(type_n > N_SUB, "glyph must paint, got {type_n}");
        assert!(stem_n > 0, "stems must paint");
        assert!(flower_n > 0, "the first-letter bloom must bloom");
    }

    #[test]
    fn rose_blooms_clear_their_letters_ink() {
        // Whole-bloom clearance (the user's 01:52/02:12 direction,
        // verified against QA's 02:47-02:58 pixel audits): EVERY rose
        // - terminal mains, branch tips, satellite rosettes AND
        // bridge-tip roses - keeps its FULL disk at least BLOOM_CLEAR
        // from its anchor letter's actual ink, on the side its stem
        // heads: intact flower heads away from the strokes, no cutouts
        // anywhere on the word. The bridge pass (prev_w Some) covers the
        // big-head-on-the-'e' case QA measured: a lifted bridge bloom
        // clears the x-height anchor AND any ascender neighbour, because
        // the x-height clearance target (-(0.52 + r_est + clear)) tops
        // every lowercase ascender ink (~0.782).
        for word in ["hello", "every", "garden"] {
            let chars: Vec<char> = word.chars().collect();
            for (wi, &ch) in chars.iter().enumerate() {
                for prev in [None, Some(0.5)] {
                    let els = gen(ch, 4242, wi, prev, CHAOS, PlantCaps::for_band(80.0), 17.8);
                    let (top, bot) = super::super::font::ink_extent(ch);
                    let ink_top_y = -top;
                    let ink_bot_y = -bot;
                    let mut roses = 0;
                    for el in &els.els {
                        if let El::Rose { y, r, .. } = el {
                            roses += 1;
                            let above = y + r <= ink_top_y - BLOOM_CLEAR + 0.01;
                            let below = y - r >= ink_bot_y + BLOOM_CLEAR - 0.01;
                            assert!(
                                above || below,
                                "{word:?} letter {ch:?} (wi {wi}, prev {prev:?}): bloom y={y} r={r} intersects ink [{ink_top_y:.2},{ink_bot_y:.2}]"
                            );
                        }
                    }
                    // With bridges rolled in, most letters grow at least
                    // one bloom somewhere across the two passes.
                    if prev.is_none() && wi == 0 {
                        assert!(roses > 0, "first letter must carry its rose");
                    }
                }
            }
        }
    }

    #[test]
    fn sway_moves_points_over_time() {
        let a = wpt(4.0, 6.0, 4.0, 3.0, 8.0, 12.0, 0.0, 3.0, 1.0);
        let b = wpt(4.0, 6.0, 4.0, 3.0, 8.0, 12.0, 1.7, 3.0, 1.0);
        assert!((a.0 - b.0).abs() > 1e-6 || (a.1 - b.1).abs() > 1e-6);
    }

    #[test]
    fn fit_to_band_translates_crossing_plants_not_roots() {
        // A plant whose bloom envelope crosses the LEFT band edge is
        // translated inward: the bloom lands inside the margin, the
        // stem's tip follows (attach preserved), the ROOT stays planted,
        // and the flower is never dropped or shrunk. The margin covers
        // the render-time sway, so this bounds every frame.
        let mk = || LetterEls {
            els: vec![
                El::Stem {
                    id: 0,
                    pts: (0..=40)
                        .map(|k| (k as f32 / 40.0 * -0.9, -(k as f32 / 40.0) * 1.0))
                        .collect(),
                    segs: vec![],
                    d0: 0.0,
                    dur: 600.0,
                    w: 1.0,
                    thorns: vec![],
                },
                El::Rose {
                    id: 1,
                    x: -0.9,
                    y: -1.0,
                    r: 0.3,
                    rot: 0.0,
                    ph1: 0.0,
                    ph2: 0.0,
                    turns: 2.0,
                    layer: 0,
                    d0: 0.0,
                    kind: Kind::Tulip,
                },
            ],
            tend: None,
        };
        // Letter near the left edge: bloom at tx - 0.9*s, r 0.3*s.
        // s = 10: bloom spans x in [-12, 15 - ... ] with tx = 3: center
        // 3 - 9 = -6, r 3 -> envelope [-9, -3] crosses 0.
        let mut els = mk();
        fit_to_band(&mut els, 3.0, 10.0, 40.0);
        let El::Rose { x, r, .. } = &els.els[1] else {
            panic!()
        };
        let center = 3.0 + x * 10.0;
        assert!(
            center - r * 10.0 >= 0.3 * 10.0 - 1e-3,
            "bloom must clear the left margin: center {center} r {r}"
        );
        // The root stays planted (first stem point unmoved).
        let El::Stem { pts, .. } = &els.els[0] else {
            panic!()
        };
        assert_eq!(pts[0], (0.0, 0.0), "the root must not move");
        // The tip follows the bloom (attach preserved).
        let tip = pts[pts.len() - 1];
        let rose_x = *x;
        assert!(
            (tip.0 - rose_x).abs() < 0.15,
            "the stem tip must follow its bloom: tip {tip:?} rose {rose_x}"
        );

        // Right-edge mirror: same plant at the far edge crosses right.
        let mut els = mk();
        fit_to_band(&mut els, 37.0, 10.0, 40.0);
        let El::Rose { x, r, .. } = &els.els[1] else {
            panic!()
        };
        let center = 37.0 + x * 10.0;
        assert!(
            center + r * 10.0 <= 40.0 - 0.3 * 10.0 + 1e-3,
            "bloom must clear the right margin: center {center}"
        );

        // Fully inside: a byte-identical no-op (the approved composition
        // is never disturbed).
        let mut a = mk();
        fit_to_band(&mut a, 20.0, 10.0, 40.0);
        let b = mk();
        assert_eq!(format!("{:?}", a.els), format!("{:?}", b.els));
    }

    #[test]
    fn fit_to_band_moves_leaves_with_their_stems_only() {
        // QA 03:45: attachments stay - a leaf NEVER detaches. A leaf on
        // the CROSSING stem follows its owner's blend at its attach
        // point; a leaf on a DIFFERENT, fully-inside stem must not move
        // at all (the earlier all-leaves shift was the detach bug this
        // regression pins).
        let mk = |leaf_x: f32, leaf_id: u32| LetterEls {
            els: vec![
                El::Stem {
                    id: 0,
                    pts: (0..=40)
                        .map(|k| (k as f32 / 40.0 * -0.9, -(k as f32 / 40.0)))
                        .collect(),
                    segs: vec![],
                    d0: 0.0,
                    dur: 600.0,
                    w: 1.0,
                    thorns: vec![],
                },
                El::Leaf {
                    id: leaf_id,
                    x: leaf_x,
                    y: -0.5,
                    a: 0.0,
                    len: 0.2,
                    bend: 0.0,
                    layer: 0,
                    d0: 0.0,
                },
                El::Leaf {
                    id: leaf_id + 1,
                    x: 0.2,
                    y: -0.3,
                    a: 0.0,
                    len: 0.2,
                    bend: 0.0,
                    layer: 0,
                    d0: 0.0,
                },
                El::Stem {
                    id: 2,
                    pts: (0..=40)
                        .map(|k| (0.2 - k as f32 / 40.0 * 0.1, -(k as f32 / 40.0) * 0.8))
                        .collect(),
                    segs: vec![],
                    d0: 0.0,
                    dur: 600.0,
                    w: 1.0,
                    thorns: vec![],
                },
                // A leaf on the SECOND (inside) stem.
                El::Leaf {
                    id: 5,
                    x: 0.15,
                    y: -0.4,
                    a: 0.0,
                    len: 0.2,
                    bend: 0.0,
                    layer: 0,
                    d0: 0.0,
                },
            ],
            tend: None,
        };
        // tx = 3, s = 10: the first stem + its leaves cross the left
        // margin (envelope min = 3 - 9 - 0.25 = -6.25 < 3); the second
        // stem sits well inside.
        let mut els = mk(-0.45, 1);
        fit_to_band(&mut els, 3.0, 10.0, 40.0);
        // The crossing stem's group dx: envelope min -6.25 -> margin 3
        // => dx = 9.25 design = 0.925 em; the leaf at rel = 0.5 along the
        // stem blends smooth(0.5) = 0.5 => shift ~0.46 em.
        let shifted = match &els.els[1] {
            El::Leaf { x, .. } => *x,
            _ => panic!(),
        };
        let unshifted = match &els.els[2] {
            El::Leaf { x, .. } => *x,
            _ => panic!(),
        };
        // The leaf on the crossing stem moved WITH its stem (attach
        // preserved, bounded by the blend).
        assert!(
            (shifted - (-0.45)) > 0.2,
            "leaf must follow its stem: {shifted}"
        );
        assert!(
            (shifted - (-0.45)) < 0.925,
            "leaf shift must be blend-bounded, not full: {shifted}"
        );
        // The leaf at the stem's ROOT END (rel <= 0: blend is zero
        // there) does not shift - roots stay planted, attachments stay.
        assert_eq!(unshifted, 0.2, "a root-end leaf must not move");
        // And the inside stem's own leaf is untouched too.
        match &els.els[4] {
            El::Leaf { x, .. } => assert_eq!(*x, 0.15, "inside stem's leaf must not move"),
            _ => panic!(),
        }
    }

    #[test]
    fn birth_settling_sprouts_then_stops() {
        // garden.ts wpt birth phase: a damped oscillation for the first
        // 2.5 s of a letter's life, alternating direction per letter id
        // parity; nothing before birth and nothing after 2.5 s.
        // Same letter, same gust time, different birth ages: the young
        // one moves, the settled one (t >= 2.5) does not, and the pair
        // alternates direction by id parity.
        let settled = wpt(4.0, 6.0, 4.0, 3.0, 8.0, 12.0, 10.0, 3.0, 1.0);
        let still = wpt(4.0, 6.0, 4.0, 3.0, 8.0, 12.0, 10.0, 3.0, -1.0);
        assert_eq!(settled, still, "past 2.5 s the direction must not matter");
        let young_a = wpt(4.0, 6.0, 4.0, 3.0, 8.0, 12.0, 10.0, 0.5, 1.0);
        let young_b = wpt(4.0, 6.0, 4.0, 3.0, 8.0, 12.0, 10.0, 0.5, -1.0);
        assert_ne!(young_a, young_b, "id parity must alternate the sprout");
        // Before birth (t <= 0): no settling at all.
        let just_born = wpt(4.0, 6.0, 4.0, 3.0, 8.0, 12.0, 10.0, 0.0, 1.0);
        let none = wpt(4.0, 6.0, 4.0, 3.0, 8.0, 12.0, 10.0, -1.0, 1.0);
        assert_eq!(just_born, none, "t = 0 must not settle (upstream: t > 0)");
        // The settled letter equals a no-birth call (settling is zero
        // there) - guards against the birth phase leaking past 2.5 s.
        let no_birth = wpt(4.0, 6.0, 4.0, 3.0, 8.0, 12.0, 10.0, 10.0, 1.0);
        assert_eq!(settled, no_birth);
    }

    #[test]
    fn kinds_pick_deterministically_and_cover_the_pool() {
        // pick_kind is deterministic per seed and, over many seeds, hits
        // every registered species (upstream's weighted pool).
        let mut seen = [false; 10];
        for seed in 0..8000 {
            let mut r = Rng::new(seed);
            seen[match pick_kind(&mut r) {
                Kind::Rose => 0,
                Kind::Daisy => 1,
                Kind::Tulip => 2,
                Kind::Poppy => 3,
                Kind::Blossom => 4,
                Kind::Lily => 5,
                Kind::Bell => 6,
                Kind::Lavender => 7,
                Kind::Fern => 8,
                Kind::Dandelion => 9,
            }] = true;
        }
        assert!(seen.iter().all(|&s| s), "every kind must appear: {seen:?}");
        // Determinism: same seed, same kind.
        let mut r1 = Rng::new(7);
        let mut r2 = Rng::new(7);
        assert_eq!(pick_kind(&mut r1), pick_kind(&mut r2));
    }

    #[test]
    fn words_grow_a_species_mix() {
        // Real generation rolls kinds (upstream rolls pickKind at every
        // rose tip): across a word's letters both the reference rose and
        // other species appear.
        let mut rose = 0;
        let mut other = 0;
        for word in ["hello", "every", "garden", "bloom"] {
            let chars: Vec<char> = word.chars().collect();
            for (wi, &ch) in chars.iter().enumerate() {
                let els = gen(ch, 4242, wi, None, CHAOS, PlantCaps::for_band(80.0), 17.8);
                for el in &els.els {
                    if let El::Rose { kind, .. } = el {
                        if *kind == Kind::Rose {
                            rose += 1;
                        } else {
                            other += 1;
                        }
                    }
                }
            }
        }
        assert!(rose > 0, "the rose must still bloom (reference preset)");
        assert!(
            other > 0,
            "the species mix must appear (only roses: {rose})"
        );
    }

    #[test]
    fn kind_scales_and_glyph_sets_match_upstream() {
        // garden.ts KIND_SCALE: rose 1, daisy 1.25, tulip 1.25, poppy
        // 1.2, blossom 1.3, lily 1.3.
        assert!((Kind::Rose.scale() - 1.0).abs() < 1e-6);
        assert!((Kind::Daisy.scale() - 1.25).abs() < 1e-6);
        assert!((Kind::Tulip.scale() - 1.25).abs() < 1e-6);
        assert!((Kind::Poppy.scale() - 1.2).abs() < 1e-6);
        assert!((Kind::Blossom.scale() - 1.3).abs() < 1e-6);
        assert!((Kind::Lily.scale() - 1.3).abs() < 1e-6);
        assert!((Kind::max_pool_scale() - 1.3).abs() < 1e-6);
        // ascii.ts KIND_GLYPHS: each species' base set is distinct.
        let sets: Vec<&str> = Kind::POOL
            .iter()
            .map(|(k, _)| {
                let (_, base, _) = k.roles();
                super::super::mask::SETS[base as usize]
            })
            .collect();
        for i in 0..sets.len() {
            for j in (i + 1)..sets.len() {
                assert_ne!(sets[i], sets[j], "kind glyph sets must differ");
            }
        }
    }

    #[test]
    fn birth_stagger_matches_the_grow_preset() {
        // garden.ts "grow": l.b = 200 + i * min(110, 3000/n).
        assert_eq!(birth_stagger(26, 0), 200.0);
        assert!((birth_stagger(26, 1) - 310.0).abs() < 1e-4); // step min(110, 115.4)
        assert!((birth_stagger(5, 2) - 420.0).abs() < 1e-4); // step 110
        assert_eq!(birth_stagger(1, 0), 200.0);
        assert!((birth_stagger(40, 39) - (200.0 + 39.0 * 75.0)).abs() < 1e-4); // step 3000/40
    }

    #[test]
    fn kind_draws_paint_their_own_roles_only() {
        // Each species' geometry paints ONLY its own tone roles (no
        // cross-kind leaks, no rose-flower roles), and stays within the
        // planted radius neighbourhood (~1.5x + half a cell of slack) -
        // a runaway geometry guard.
        let draw = |g: &mut MaskGrid, kind: Kind| match kind {
            Kind::Daisy => draw_daisy(g, 6.0, 8.0, 2.0, 0.3, 1.0, 2.0, 5000.0),
            Kind::Tulip => draw_tulip(g, 6.0, 8.0, 2.0, 0.3, 5000.0),
            Kind::Poppy => draw_poppy(g, 6.0, 8.0, 2.0, 0.3, 1.0, 2.0, 5000.0),
            Kind::Blossom => draw_blossom(g, 6.0, 8.0, 2.0, 0.3, 1.0, 2.0, 5000.0),
            Kind::Lily => draw_lily(g, 6.0, 8.0, 2.0, 0.3, 5000.0),
            Kind::Bell => draw_bell(g, 6.0, 8.0, 2.0, 0.3, 5000.0),
            Kind::Lavender => draw_lavender(g, 6.0, 8.0, 2.0, 0.3, 1.0, 2.0, 5000.0),
            Kind::Fern => draw_fern(g, 6.0, 8.0, 2.0, 0.3, 2.0, 5000.0),
            Kind::Dandelion => draw_dandelion(g, 6.0, 8.0, 2.0, 0.3, 1.0, 5000.0),
            Kind::Rose => draw_rose(g, 6.0, 8.0, 2.0, 0.3, 1.0, 2.0, 1.8, 5000.0),
        };
        let own: &[(Kind, Role, Role, Role); 10] = &[
            (
                Kind::Rose,
                Role::FlowerShadow,
                Role::Flower,
                Role::FlowerLight,
            ),
            (Kind::Daisy, Role::DaisyDark, Role::Daisy, Role::DaisyLight),
            (Kind::Tulip, Role::TulipDark, Role::Tulip, Role::TulipLight),
            (Kind::Poppy, Role::PoppyDark, Role::Poppy, Role::PoppyLight),
            (
                Kind::Blossom,
                Role::BlossomDark,
                Role::Blossom,
                Role::BlossomLight,
            ),
            (Kind::Lily, Role::LilyDark, Role::Lily, Role::LilyLight),
            (Kind::Bell, Role::BellDark, Role::Bell, Role::BellLight),
            (
                Kind::Lavender,
                Role::LavenderDark,
                Role::Lavender,
                Role::LavenderLight,
            ),
            (Kind::Fern, Role::FernDark, Role::Fern, Role::FernLight),
            (
                Kind::Dandelion,
                Role::DandelionDark,
                Role::Dandelion,
                Role::DandelionLight,
            ),
        ];
        for (kind, dark, base, light) in own {
            let mut g = MaskGrid::new(12, 12);
            draw(&mut g, *kind);
            let roles = g.role.clone();
            let n_own: usize = roles
                .iter()
                .filter(|&&r| r == *dark as u8 || r == *base as u8 || r == *light as u8)
                .count();
            assert!(n_own > 0, "{kind:?} must paint its own tone roles");
            // Only the kind's tones (plus, for the rose, its spiral line
            // roles) may appear - no cross-kind leaks.
            for (i, &r) in roles.iter().enumerate() {
                if r == 0 {
                    continue;
                }
                // Only the kind's tones may appear - plus the
                // upstream-documented exceptions: the rose's spiral line
                // roles, and bell/lavender's stalks (upstream draws those
                // in the STEM-dark tone, C.blueD).
                let allowed = r == *dark as u8
                    || r == *base as u8
                    || r == *light as u8
                    || (*kind == Kind::Rose && r == Role::Line as u8)
                    || ((*kind == Kind::Bell || *kind == Kind::Lavender)
                        && r == Role::LeafShadow as u8);
                assert!(
                    allowed,
                    "{kind:?} painted a foreign role {r} at subpixel {i}"
                );
            }
            // Geometry stays near the planted spot: every painted cell
            // lies within ~1.5 radii + slack of the bloom centre.
            let (gw, gh) = (g.w, g.h);
            let sx = super::super::mask::SX;
            for j in 0..gh {
                for i in 0..gw {
                    if roles[j * gw + i] == 0 {
                        continue;
                    }
                    // Subpixels are square in design space, so both
                    // axes convert through SX (mask sample_xy).
                    let (px, py) = ((i as f32 + 0.5) / sx as f32, (j as f32 + 0.5) / sx as f32);
                    let d = (px - 6.0).hypot(py - 8.0);
                    // Fern/lavender reach ~2.1 radii (upstream H = 2 R);
                    // the bound is r * 2.3 + slack.
                    assert!(
                        d < 2.0 * 2.3 + 0.5,
                        "{kind:?} painted {d:.2} units from its centre"
                    );
                }
            }
        }
    }
}
