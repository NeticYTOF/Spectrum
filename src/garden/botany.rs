//! Botanical geometry, ported from ascii-garden's `garden.ts` (upstream
//! f519e85, MIT (c) 2026 Ridit; see NOTICE): the seeded PRNG, vine/curl/
//! weave path builders, stem/leaf/rose growth from letters, spring
//! easing and the gust sway. Typing-only dynamics (wither, recoil,
//! butterflies) are not ported. Element coordinates are in EM units
//! relative to each letter (centre x, baseline y); drawing scales them
//! by the type size S into design space.

use super::mask::{MaskGrid, Pt, Role, FLOWER};

pub const CHAOS: f32 = 0.3; // "0.3 matches the original Type Garden"

/// Upstream detail thresholds are in canvas pixels at its default cell
/// size (about 10 px wide); design units here are one cell wide, so a
/// pixel constant converts by this factor (R >= 16 px -> R >= 1.6).
const PX: f32 = 0.1;

/// Terminal cells cannot shrink with the flowers the way upstream's do
/// (`targetCh = St/17`), so a rose keeps a minimum footprint of about
/// five cells across / two-and-a-half rows to stay recognizable, and a
/// leaf about two cells. Upstream roses at its default size are larger
/// than this, so the floor only engages for small type.
const ROSE_MIN_R: f32 = 2.5;
const LEAF_MIN_L: f32 = 2.0;

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

struct GrowOpts {
    base: P,
    dir: f32,
    len: f32,
    d0: f32,
    depth: bool,
    layer0: Option<u8>,
    tip: Option<&'static str>,
    pts: Option<Vec<P>>,
}

/// The recursive grow (garden.ts grow): a stem with leaves and a tip,
/// possibly branching once more at depth 1.
#[allow(clippy::too_many_arguments)]
fn grow(r: &mut Rng, els: &mut Vec<El>, nid: &mut u32, p: &WordParams, o: GrowOpts) {
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
    let mut pts = match o.pts {
        Some(pts) => pts,
        None => {
            let mut v = vine(
                o.base,
                o.dir,
                o.len,
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
    let _ = supplied_pts;
    let start_layer = o.layer0.unwrap_or(if r.rf() < 0.5 { 0 } else { 1 });
    let segs = weave(r, start_layer);
    let dur = (320.0_f32).max(o.len * 620.0);
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
            len: (0.14 + r.rf() * 0.2) * if o.depth { 0.8 } else { 1.0 },
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
            let r_size = (0.13 + r.rf() * 0.15) * p.big * if o.depth { 0.75 } else { 1.0 };
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
            });
            *nid += 1;
            if r.rf() < 0.2 {
                let oo = ta + if r.rf() < 0.5 { 1.3 } else { -1.3 };
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
                    len: 0.2 + r.rf() * 0.2,
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
                len: 0.16 + r.rf() * 0.16,
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
                GrowOpts {
                    base: q,
                    dir: a + if sign_flip { -1.0 } else { 1.0 } * branch_mag,
                    len: o.len * len_scale,
                    d0: o.d0 + dur * u,
                    depth: true,
                    layer0: Some(layer_here),
                    tip: None,
                    pts: None,
                },
            );
        }
    }
}

/// The elements of one letter (garden.ts gen, deterministically seeded:
/// the reference's Math.random component is replaced by the word seed so
/// a frozen frame is reproducible).
pub fn gen(ch: char, ws: i32, wi: usize, prev_w: Option<f32>, chaos: f32) -> LetterEls {
    let mut r = Rng::new(
        ws ^ ((wi as i32)
            .wrapping_add(1)
            .wrapping_mul(2654435761u32 as i32)),
    );
    let p = word_params(ws, chaos);
    let w = super::font::advance(ch).unwrap_or(0.5);
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
            GrowOpts {
                base,
                dir,
                len,
                d0: 40.0 + i as f32 * 110.0,
                depth: false,
                layer0: None,
                tip,
                pts: None,
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
            GrowOpts {
                base,
                dir,
                len,
                d0: 160.0,
                depth: false,
                layer0: None,
                tip: None,
                pts: None,
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
                GrowOpts {
                    base: (0.0, 0.0),
                    dir: 0.0,
                    len: blen,
                    d0: 90.0,
                    depth: false,
                    layer0: None,
                    tip: Some(tip),
                    pts: Some(pts),
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
            GrowOpts {
                base,
                dir,
                len,
                d0: 120.0,
                depth: false,
                layer0: None,
                tip: Some("curl"),
                pts: None,
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
            GrowOpts {
                base,
                dir,
                len,
                d0: 0.0,
                depth: true,
                layer0: None,
                tip: Some("curl"),
                pts: None,
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

/// Gust sway (garden.ts wpt, face portion): points higher up lean with a
/// travelling sine gust; height is measured from the letter baseline.
fn wpt(lx: f32, ly: f32, x: f32, y: f32, s: f32, w_design: f32, t_s: f32) -> P {
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
) {
    let chaos = chaos_for(s / 2.0);
    // Upstream boil: max(0.8 px, S * 0.01) scaled by chaos; the music's
    // smoothed energy modulates it gently (calm in quiet passages).
    let amp =
        (0.8 * PX).max(s * 0.01) * (0.55 + chaos * 2.4) * (0.6 + 0.8 * energy.clamp(0.0, 1.0));
    let f = age_ms / 420.0;
    let t_s = age_ms / 1000.0;

    let draw_el = |g: &mut MaskGrid, l: &SceneLetter, el: &El, age: f32, layer: u8| {
        let j = jit(el.id(), f, amp);
        let (x0, y0) = (l.tx, l.ty);
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
                let (wx, wy) = wpt(x0, y0, x0 + x * s + j.0, y0 + y * s + j.1, s, w_design, t_s);
                draw_leaf(g, wx, wy, a + j.2, (len * s).max(LEAF_MIN_L) * sc, *bend);
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
                ..
            } => {
                let a = age - d0;
                if a < 0.0 {
                    return;
                }
                let (wx, wy) = wpt(x0, y0, x0 + x * s + j.0, y0 + y * s + j.1, s, w_design, t_s);
                draw_rose(
                    g,
                    wx,
                    wy,
                    (r * s).max(ROSE_MIN_R),
                    rot + j.2,
                    *ph1,
                    *ph2,
                    *turns,
                    a,
                );
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
    if type_mask.w == g.w && type_mask.h == g.h {
        for (k, &c) in type_mask.cov.iter().enumerate() {
            if c >= 128 {
                g.role[k] = Role::Type as u8;
                g.owner[k] = type_mask.line.get(k).copied().unwrap_or(1);
            }
        }
    }
    // Front layer: each element is guarded so it may weave over its own
    // line's letters but never over another line's. Small type (an em
    // under ~8 rows) cannot afford any occlusion, so there every letter
    // is protected and the whole garden sits behind the type.
    let protect_all = s / 2.0 < 8.0;
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
        let a = gen('g', 12345, 0, Some(0.5), CHAOS);
        let b = gen('g', 12345, 0, Some(0.5), CHAOS);
        assert_eq!(format!("{:?}", a.els.len()), format!("{:?}", b.els.len()));
        assert!(!a.els.is_empty(), "a letter must grow something");
        let rose_first = gen('s', 999, 0, None, CHAOS);
        // First letter of a word starts with a rose-tipped stem.
        assert!(matches!(rose_first.els[0], El::Stem { .. }));
        // Different seeds give different gardens.
        let c = gen('g', 54321, 0, Some(0.5), CHAOS);
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
        let els = gen('o', 4242, 0, None, CHAOS);
        let letters = [SceneLetter {
            ch: 'o',
            tx: 4.0,
            ty: 6.0,
            els: &els,
            birth_ms: 0.0,
            word_end: true,
            line: 0,
        }];
        let mut g = MaskGrid::new(12, 8);
        let mut tm = super::super::font::TypeMask::new(g.w, g.h);
        tm.draw_glyph('o', 4.0, 6.0, 8.0, 4.0);
        draw_scene(&mut g, &letters, &tm, 8.0, 5000.0, 0.3, 12.0);
        let roles = g.role.clone();
        let type_n = roles.iter().filter(|&&r| r == Role::Type as u8).count();
        let stem_n = roles.iter().filter(|&&r| r == Role::Stem as u8).count();
        let flower_n = roles.iter().filter(|&&r| r == Role::Flower as u8).count();
        assert!(type_n > N_SUB, "glyph must paint, got {type_n}");
        assert!(stem_n > 0, "stems must paint");
        assert!(flower_n > 0, "the first-letter rose must bloom");
    }

    #[test]
    fn sway_moves_points_over_time() {
        let a = wpt(4.0, 6.0, 4.0, 3.0, 8.0, 12.0, 0.0);
        let b = wpt(4.0, 6.0, 4.0, 3.0, 8.0, 12.0, 1.7);
        assert!((a.0 - b.0).abs() > 1e-6 || (a.1 - b.1).abs() > 1e-6);
    }
}
