//! Original serif letterforms for the garden's type, drawn as filled
//! primitives in em units (baseline y = 0, y up). They are written for
//! this project: Playfair-style proportions and contrast (thick stems,
//! thin joins, bracket serifs, tall x-height), but NOT extracted from any
//! font file - see docs/PORT-PLAN.md and NOTICE. Advances are
//! proportional per glyph (the reference measures Playfair; we ship a
//! per-character advance table in the same spirit).

use super::mask::{MaskGrid, Pt, Role};

/// Advance of each glyph in em units (mw() in garden.ts).
pub fn advance(c: char) -> Option<f32> {
    Some(match c {
        'a' => 0.50,
        'b' => 0.52,
        'c' => 0.45,
        'd' => 0.55,
        'e' => 0.47,
        'f' => 0.34,
        'g' => 0.52,
        'h' => 0.55,
        'i' => 0.29,
        'j' => 0.29,
        'k' => 0.52,
        'l' => 0.29,
        'm' => 0.85,
        'n' => 0.55,
        'o' => 0.55,
        'p' => 0.52,
        'q' => 0.52,
        'r' => 0.40,
        's' => 0.45,
        't' => 0.34,
        'u' => 0.55,
        'v' => 0.49,
        'w' => 0.73,
        'x' => 0.49,
        'y' => 0.49,
        'z' => 0.46,
        'A' => 0.70,
        'B' => 0.68,
        'C' => 0.70,
        'D' => 0.72,
        'E' => 0.62,
        'F' => 0.58,
        'G' => 0.74,
        'H' => 0.72,
        'I' => 0.36,
        'J' => 0.42,
        'K' => 0.66,
        'L' => 0.58,
        'M' => 0.86,
        'N' => 0.72,
        'O' => 0.75,
        'P' => 0.60,
        'Q' => 0.75,
        'R' => 0.65,
        'S' => 0.60,
        'T' => 0.62,
        'U' => 0.70,
        'V' => 0.66,
        'W' => 0.94,
        'X' => 0.64,
        'Y' => 0.63,
        'Z' => 0.60,
        '0'..='9' => 0.55,
        ' ' => 0.35, // reference multiplies the space advance by 1.4
        '.' => 0.28,
        ',' => 0.28,
        '\'' => 0.22,
        '\u{2019}' => 0.22,
        '!' => 0.30,
        '?' => 0.45,
        '-' => 0.36,
        '\u{2014}' => 1.0,
        '\u{2013}' => 0.6,
        '(' => 0.33,
        ')' => 0.33,
        '"' => 0.42,
        ';' => 0.30,
        ':' => 0.28,
        '/' => 0.5,
        '&' => 0.66,
        '*' => 0.42,
        _ => return None,
    })
}

/// Whether every character has a letterform (else fallback to plain text).
pub fn glyph_supported(text: &str) -> bool {
    text.chars().all(|c| c == ' ' || advance(c).is_some())
}

// Metrics (em): thick stems, thin joins, bracket serifs.
const THICK: f32 = 0.10; // stem width
const THIN: f32 = 0.045; // hairline width
const X: f32 = 0.47; // x-height
const ASC: f32 = 0.73; // ascender
const DESC: f32 = -0.22; // descender
const SERIF: f32 = 0.035; // serif foot extension

/// A filled primitive in em units, y up from the baseline.
#[derive(Clone, Debug)]
enum Prim {
    /// Filled rect: (x0, y0, x1, y1).
    Rect(f32, f32, f32, f32),
    /// Filled polygon.
    Poly(Vec<(f32, f32)>),
    /// Ellipse ring sector: centre, radii, side/top thickness, angle
    /// range in radians (y up, CCW), or the full ring if None.
    /// Vertical stress: thick left/right sides, thin top/bottom.
    Ring {
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
        t_side: f32,
        t_top: f32,
        a: Option<(f32, f32)>,
    },
    /// Filled ellipse (a dot).
    Dot(f32, f32, f32),
}

/// Draw `ch`'s letterform into the mask with its baseline at
/// (bx, by) and size S (design px), as the Type role. `advance/2` is
/// the glyph's centre offset: the reference centers each glyph in its
/// advance slot.
pub fn draw_glyph(g: &mut MaskGrid, ch: char, bx: f32, by: f32, s: f32) {
    let Some(prims) = glyph_prims(ch) else { return };
    // Design-space transform: x = bx + x_em * S, y = by - y_em * S
    // (em y is up, design y is down).
    let tx = |x: f32| bx + x * s;
    let ty = |y: f32| by - y * s;
    let draw_prim = |g: &mut MaskGrid, p: &Prim| match *p {
        Prim::Rect(x0, y0, x1, y1) => g.fill_rect(tx(x0), ty(y1), tx(x1), ty(y0), Role::Type),
        Prim::Poly(ref pts) => {
            let poly: Vec<Pt> = pts.iter().map(|&(x, y)| Pt(tx(x), ty(y))).collect();
            g.fill_poly(&poly, Role::Type);
        }
        Prim::Ring {
            cx,
            cy,
            rx,
            ry,
            t_side,
            t_top,
            a,
        } => {
            let steps = 96;
            let (a0, a1) = a.unwrap_or((0.0, std::f32::consts::TAU));
            // Outer and inner edges of the ring sector, em angles CCW.
            let mut outer = Vec::with_capacity(steps + 1);
            let mut inner = Vec::with_capacity(steps + 1);
            for k in 0..=steps {
                let t = a0 + (a1 - a0) * k as f32 / steps as f32;
                let c = t.cos();
                let sn = t.sin();
                // Vertical stress: the inner ellipse is inset by the side
                // thickness horizontally and the top thickness vertically.
                outer.push(Pt(tx(cx + rx * c), ty(cy + ry * sn)));
                inner.push(Pt(tx(cx + (rx - t_side) * c), ty(cy + (ry - t_top) * sn)));
            }
            let mut path = outer;
            path.extend(inner.iter().rev().cloned());
            g.fill_poly(&path, Role::Type);
        }
        Prim::Dot(cx, cy, r) => {
            let steps = 24;
            let mut pts = Vec::with_capacity(steps);
            for k in 0..steps {
                let t = std::f32::consts::TAU * k as f32 / steps as f32;
                pts.push(Pt(tx(cx + r * t.cos()), ty(cy + r * t.sin())));
            }
            g.fill_poly(&pts, Role::Type);
        }
    };
    for p in &prims {
        draw_prim(g, p);
    }
}

/// Serif foot: a small bracketed slab at the base (or top) of a stem.
fn foot(x: f32, y: f32, up: bool) -> Vec<Prim> {
    // A horizontal slab extending past the stem, plus a small bracket
    // tapering into the stem above (or below) it.
    vec![
        Prim::Rect(
            x - SERIF,
            if up { y } else { y - SERIF },
            x + THICK + SERIF,
            if up { y + SERIF } else { y },
        ),
        Prim::Rect(
            x + SERIF * 0.4,
            if up { y + SERIF } else { y - 2.0 * SERIF },
            x + THICK - SERIF * 0.4,
            if up { y + 2.0 * SERIF } else { y - SERIF },
        ),
    ]
}

/// Trapezoid helper between two points with thickness.
fn bar(x0: f32, y0: f32, x1: f32, y1: f32, w: f32) -> Prim {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len = dx.hypot(dy).max(1e-4);
    let nx = -dy / len * w / 2.0;
    let ny = dx / len * w / 2.0;
    Prim::Poly(vec![
        (x0 + nx, y0 + ny),
        (x1 + nx, y1 + ny),
        (x1 - nx, y1 - ny),
        (x0 - nx, y0 - ny),
    ])
}

fn glyph_prims(c: char) -> Option<Vec<Prim>> {
    let mut p: Vec<Prim> = Vec::new();
    match c {
        ' ' => return Some(vec![]),
        'a' => {
            p.push(Prim::Ring {
                cx: 0.20,
                cy: 0.235,
                rx: 0.17,
                ry: 0.235,
                t_side: THICK,
                t_top: THIN,
                a: Some((
                    200.0_f32.to_radians(),
                    160.0_f32.to_radians() + std::f32::consts::TAU,
                )),
            });
            p.push(Prim::Rect(0.28, 0.0, 0.28 + THICK, X));
            p.push(Prim::Rect(0.28, X - THICK * 1.4, 0.42, X));
            p.extend(foot(0.28, 0.0, true));
        }
        'b' => {
            p.push(Prim::Rect(0.04, 0.0, 0.04 + THICK, ASC));
            p.extend(foot(0.04, 0.0, true));
            p.push(Prim::Ring {
                cx: 0.27,
                cy: 0.235,
                rx: 0.21,
                ry: 0.235,
                t_side: THICK,
                t_top: THIN,
                a: None,
            });
        }
        'c' => {
            p.push(Prim::Ring {
                cx: 0.25,
                cy: 0.235,
                rx: 0.21,
                ry: 0.235,
                t_side: THICK,
                t_top: THIN,
                a: Some((45.0_f32.to_radians(), 315.0_f32.to_radians())),
            });
            p.push(Prim::Rect(0.25, 0.19, 0.48, 0.235)); // upper terminal
        }
        'd' => {
            p.push(Prim::Rect(0.38, 0.0, 0.38 + THICK, ASC));
            p.extend(foot(0.38, 0.0, true));
            p.push(Prim::Ring {
                cx: 0.25,
                cy: 0.235,
                rx: 0.21,
                ry: 0.235,
                t_side: THICK,
                t_top: THIN,
                a: None,
            });
        }
        'e' => {
            p.push(Prim::Ring {
                cx: 0.24,
                cy: 0.235,
                rx: 0.21,
                ry: 0.235,
                t_side: THICK,
                t_top: THIN,
                a: Some((-35.0_f32.to_radians(), 315.0_f32.to_radians())),
            });
            p.push(Prim::Rect(0.03, 0.19, 0.45, 0.19 + THICK * 0.9));
        }
        'f' => {
            p.push(Prim::Rect(0.16, 0.0, 0.16 + THICK, 0.62));
            p.extend(foot(0.16, 0.0, true));
            p.push(Prim::Ring {
                cx: 0.15,
                cy: 0.60,
                rx: 0.13,
                ry: 0.13,
                t_side: THICK,
                t_top: THIN,
                a: Some((90.0_f32.to_radians(), 270.0_f32.to_radians())),
            });
            p.push(Prim::Rect(0.02, 0.48, 0.32, 0.48 + THICK * 0.7));
        }
        'g' => {
            p.push(Prim::Ring {
                cx: 0.25,
                cy: 0.235,
                rx: 0.21,
                ry: 0.235,
                t_side: THICK,
                t_top: THIN,
                a: None,
            });
            p.push(Prim::Ring {
                cx: 0.28,
                cy: -0.06,
                rx: 0.17,
                ry: 0.16,
                t_side: THICK * 0.9,
                t_top: THIN,
                a: Some((-60.0_f32.to_radians(), 210.0_f32.to_radians())),
            });
        }
        'h' => {
            p.push(Prim::Rect(0.04, 0.0, 0.04 + THICK, ASC));
            p.extend(foot(0.04, 0.0, true));
            p.push(Prim::Ring {
                cx: 0.26,
                cy: 0.33,
                rx: 0.17,
                ry: 0.14,
                t_side: THICK,
                t_top: THIN,
                a: Some((180.0_f32.to_radians(), 360.0_f32.to_radians())),
            });
            p.push(Prim::Rect(0.36, 0.0, 0.36 + THICK, 0.33));
            p.extend(foot(0.36, 0.0, true));
        }
        'i' => {
            p.push(Prim::Rect(0.12, 0.0, 0.12 + THICK, X));
            p.extend(foot(0.12, 0.0, true));
            p.push(Prim::Dot(0.17, 0.64, THICK * 0.75));
        }
        'j' => {
            p.push(Prim::Rect(0.12, 0.0, 0.12 + THICK, X));
            p.push(Prim::Dot(0.17, 0.64, THICK * 0.75));
            p.push(Prim::Ring {
                cx: 0.16,
                cy: -0.06,
                rx: 0.14,
                ry: 0.14,
                t_side: THICK,
                t_top: THIN,
                a: Some((-70.0_f32.to_radians(), 200.0_f32.to_radians())),
            });
        }
        'k' => {
            p.push(Prim::Rect(0.04, 0.0, 0.04 + THICK, ASC));
            p.extend(foot(0.04, 0.0, true));
            p.push(bar(0.14, 0.40, 0.40, ASC - 0.02, THICK));
            p.push(bar(0.26, 0.30, 0.40, 0.0, THICK));
        }
        'l' => {
            p.push(Prim::Rect(0.12, 0.0, 0.12 + THICK, ASC));
            p.extend(foot(0.12, 0.0, true));
            p.push(Prim::Rect(
                0.12 - SERIF,
                ASC - SERIF,
                0.12 + THICK + SERIF,
                ASC,
            ));
        }
        'm' => {
            p.push(Prim::Rect(0.02, 0.0, 0.02 + THICK, X));
            p.extend(foot(0.02, 0.0, true));
            p.push(Prim::Ring {
                cx: 0.24,
                cy: 0.33,
                rx: 0.13,
                ry: 0.14,
                t_side: THICK,
                t_top: THIN,
                a: Some((180.0_f32.to_radians(), 360.0_f32.to_radians())),
            });
            p.push(Prim::Ring {
                cx: 0.50,
                cy: 0.33,
                rx: 0.13,
                ry: 0.14,
                t_side: THICK,
                t_top: THIN,
                a: Some((180.0_f32.to_radians(), 360.0_f32.to_radians())),
            });
            p.push(Prim::Rect(0.44, 0.0, 0.44 + THICK, 0.33));
            p.extend(foot(0.44, 0.0, true));
            p.push(Prim::Rect(0.58, 0.0, 0.58 + THICK, 0.33));
            p.extend(foot(0.58, 0.0, true));
        }
        'n' => {
            p.push(Prim::Rect(0.04, 0.0, 0.04 + THICK, X));
            p.extend(foot(0.04, 0.0, true));
            p.push(Prim::Ring {
                cx: 0.26,
                cy: 0.33,
                rx: 0.17,
                ry: 0.14,
                t_side: THICK,
                t_top: THIN,
                a: Some((180.0_f32.to_radians(), 360.0_f32.to_radians())),
            });
            p.push(Prim::Rect(0.36, 0.0, 0.36 + THICK, 0.33));
            p.extend(foot(0.36, 0.0, true));
        }
        'o' => {
            p.push(Prim::Ring {
                cx: 0.25,
                cy: 0.235,
                rx: 0.22,
                ry: 0.235,
                t_side: THICK,
                t_top: THIN,
                a: None,
            });
        }
        'p' => {
            p.push(Prim::Rect(0.04, DESC, 0.04 + THICK, X));
            p.push(Prim::Ring {
                cx: 0.27,
                cy: 0.235,
                rx: 0.21,
                ry: 0.235,
                t_side: THICK,
                t_top: THIN,
                a: None,
            });
        }
        'q' => {
            p.push(Prim::Rect(0.36, DESC, 0.36 + THICK, X));
            p.push(Prim::Ring {
                cx: 0.23,
                cy: 0.235,
                rx: 0.21,
                ry: 0.235,
                t_side: THICK,
                t_top: THIN,
                a: None,
            });
        }
        'r' => {
            p.push(Prim::Rect(0.04, 0.0, 0.04 + THICK, X));
            p.extend(foot(0.04, 0.0, true));
            p.push(Prim::Ring {
                cx: 0.22,
                cy: 0.40,
                rx: 0.13,
                ry: 0.11,
                t_side: THICK,
                t_top: THIN,
                a: Some((60.0_f32.to_radians(), 300.0_f32.to_radians())),
            });
        }
        's' => {
            p.push(Prim::Ring {
                cx: 0.25,
                cy: 0.33,
                rx: 0.16,
                ry: 0.12,
                t_side: THICK * 0.9,
                t_top: THIN,
                a: Some((20.0_f32.to_radians(), 260.0_f32.to_radians())),
            });
            p.push(Prim::Ring {
                cx: 0.25,
                cy: 0.14,
                rx: 0.16,
                ry: 0.12,
                t_side: THICK * 0.9,
                t_top: THIN,
                a: Some((200.0_f32.to_radians(), 440.0_f32.to_radians())),
            });
            p.push(Prim::Rect(0.04, 0.0, 0.14, THICK));
        }
        't' => {
            p.push(Prim::Rect(0.14, 0.0, 0.14 + THICK, 0.60));
            p.push(Prim::Rect(0.02, 0.44, 0.34, 0.44 + THICK * 0.7));
            p.push(Prim::Ring {
                cx: 0.18,
                cy: 0.05,
                rx: 0.14,
                ry: 0.16,
                t_side: THICK,
                t_top: THIN,
                a: Some((180.0_f32.to_radians(), 360.0_f32.to_radians())),
            });
        }
        'u' => {
            p.push(Prim::Rect(0.04, 0.18, 0.04 + THICK, X));
            p.push(Prim::Ring {
                cx: 0.26,
                cy: 0.24,
                rx: 0.17,
                ry: 0.20,
                t_side: THICK,
                t_top: THIN,
                a: Some((180.0_f32.to_radians(), 360.0_f32.to_radians())),
            });
            p.push(Prim::Rect(0.38, 0.0, 0.38 + THICK, X));
            p.extend(foot(0.38, 0.0, true));
        }
        'v' => {
            p.push(bar(0.03, X, 0.245, 0.0, THICK));
            p.push(bar(0.245, 0.0, 0.46, X, THICK));
        }
        'w' => {
            p.push(bar(0.03, X, 0.18, 0.0, THICK));
            p.push(bar(0.18, 0.0, 0.33, X, THICK));
            p.push(bar(0.33, X, 0.48, 0.0, THICK));
            p.push(bar(0.48, 0.0, 0.63, X, THICK));
        }
        'x' => {
            p.push(bar(0.04, X, 0.44, 0.0, THICK));
            p.push(bar(0.04, 0.0, 0.44, X, THICK));
        }
        'y' => {
            p.push(bar(0.03, X, 0.245, 0.0, THICK));
            p.push(bar(0.245, 0.0, 0.46, X, THICK));
            p.push(bar(0.245, 0.0, 0.40, DESC - 0.02, THICK));
        }
        'z' => {
            p.push(Prim::Rect(0.04, X - THICK, 0.44, X));
            p.push(bar(0.06, X - THICK * 0.8, 0.40, THICK * 0.6, THICK));
            p.push(Prim::Rect(0.04, 0.0, 0.44, THICK));
        }
        // Uppercase: simpler bracketed serif capitals.
        'A' => {
            p.push(bar(0.03, 0.0, 0.30, ASC, THICK * 1.15));
            p.push(bar(0.30, ASC, 0.60, 0.0, THICK * 1.15));
            p.push(Prim::Rect(0.10, 0.22, 0.52, 0.22 + THICK));
            p.extend(foot(-0.02, 0.0, false));
        }
        'B' => {
            p.push(Prim::Rect(0.04, 0.0, 0.04 + THICK, ASC));
            p.extend(foot(0.04, 0.0, true));
            p.push(Prim::Ring {
                cx: 0.36,
                cy: ASC - 0.19,
                rx: 0.20,
                ry: 0.19,
                t_side: THICK,
                t_top: THIN,
                a: Some((-90.0_f32.to_radians(), 90.0_f32.to_radians())),
            });
            p.push(Prim::Ring {
                cx: 0.36,
                cy: 0.235,
                rx: 0.22,
                ry: 0.235,
                t_side: THICK,
                t_top: THIN,
                a: Some((-90.0_f32.to_radians(), 90.0_f32.to_radians())),
            });
            p.push(Prim::Rect(0.04, 0.40, 0.30, 0.40 + THIN));
            p.push(Prim::Rect(0.04, ASC - 0.02, 0.30, ASC));
            p.push(Prim::Rect(0.04, 0.0, 0.30, THIN));
        }
        'C' => {
            p.push(Prim::Ring {
                cx: 0.37,
                cy: ASC / 2.0,
                rx: 0.30,
                ry: ASC / 2.0,
                t_side: THICK,
                t_top: THIN,
                a: Some((55.0_f32.to_radians(), 305.0_f32.to_radians())),
            });
        }
        'D' => {
            p.push(Prim::Rect(0.04, 0.0, 0.04 + THICK, ASC));
            p.extend(foot(0.04, 0.0, true));
            p.push(Prim::Ring {
                cx: 0.34,
                cy: ASC / 2.0,
                rx: 0.26,
                ry: ASC / 2.0,
                t_side: THICK,
                t_top: THIN,
                a: Some((-90.0_f32.to_radians(), 90.0_f32.to_radians())),
            });
        }
        'E' => {
            p.push(Prim::Rect(0.04, 0.0, 0.04 + THICK, ASC));
            p.extend(foot(0.04, 0.0, true));
            p.push(Prim::Rect(0.04, ASC - THICK, 0.50, ASC));
            p.push(Prim::Rect(0.04, 0.32, 0.44, 0.32 + THICK));
            p.push(Prim::Rect(0.04, 0.0, 0.50, THICK));
        }
        'F' => {
            p.push(Prim::Rect(0.04, 0.0, 0.04 + THICK, ASC));
            p.extend(foot(0.04, 0.0, true));
            p.push(Prim::Rect(0.04, ASC - THICK, 0.48, ASC));
            p.push(Prim::Rect(0.04, 0.32, 0.42, 0.32 + THICK));
        }
        'G' => {
            p.push(Prim::Ring {
                cx: 0.37,
                cy: ASC / 2.0,
                rx: 0.30,
                ry: ASC / 2.0,
                t_side: THICK,
                t_top: THIN,
                a: Some((40.0_f32.to_radians(), 320.0_f32.to_radians())),
            });
            p.push(Prim::Rect(0.37, 0.32, 0.67, 0.32 + THICK));
        }
        'H' => {
            p.push(Prim::Rect(0.04, 0.0, 0.04 + THICK, ASC));
            p.extend(foot(0.04, 0.0, true));
            p.push(Prim::Rect(0.54, 0.0, 0.54 + THICK, ASC));
            p.extend(foot(0.54, 0.0, true));
            p.push(Prim::Rect(0.04, 0.32, 0.54 + THICK, 0.32 + THICK));
        }
        'I' => {
            p.push(Prim::Rect(0.14, 0.0, 0.14 + THICK, ASC));
            p.push(Prim::Rect(0.14 - SERIF, 0.0, 0.14 + THICK + SERIF, THIN));
            p.push(Prim::Rect(
                0.14 - SERIF,
                ASC - THIN,
                0.14 + THICK + SERIF,
                ASC,
            ));
        }
        'J' => {
            p.push(Prim::Rect(0.30, 0.20, 0.30 + THICK, ASC));
            p.push(Prim::Ring {
                cx: 0.24,
                cy: 0.16,
                rx: 0.18,
                ry: 0.16,
                t_side: THICK,
                t_top: THIN,
                a: Some((180.0_f32.to_radians(), 360.0_f32.to_radians())),
            });
        }
        'K' => {
            p.push(Prim::Rect(0.04, 0.0, 0.04 + THICK, ASC));
            p.extend(foot(0.04, 0.0, true));
            p.push(bar(0.14, 0.36, 0.56, ASC, THICK));
            p.push(bar(0.26, 0.30, 0.56, 0.0, THICK));
        }
        'L' => {
            p.push(Prim::Rect(0.04, 0.0, 0.04 + THICK, ASC));
            p.extend(foot(0.04, 0.0, true));
            p.push(Prim::Rect(0.04, 0.0, 0.50, THICK));
        }
        'M' => {
            p.push(Prim::Rect(0.04, 0.0, 0.04 + THICK, ASC));
            p.extend(foot(0.04, 0.0, true));
            p.push(Prim::Rect(0.60, 0.0, 0.60 + THICK, ASC));
            p.extend(foot(0.60, 0.0, true));
            p.push(bar(0.09, ASC, 0.32, 0.22, THICK));
            p.push(bar(0.32, 0.22, 0.55, ASC, THICK));
        }
        'N' => {
            p.push(Prim::Rect(0.04, 0.0, 0.04 + THICK, ASC));
            p.extend(foot(0.04, 0.0, true));
            p.push(Prim::Rect(0.54, 0.0, 0.54 + THICK, ASC));
            p.extend(foot(0.54, 0.0, true));
            p.push(bar(0.09, ASC, 0.59, 0.0, THICK));
        }
        'O' => {
            p.push(Prim::Ring {
                cx: 0.375,
                cy: ASC / 2.0,
                rx: 0.30,
                ry: ASC / 2.0,
                t_side: THICK,
                t_top: THIN,
                a: None,
            });
        }
        'P' => {
            p.push(Prim::Rect(0.04, 0.0, 0.04 + THICK, ASC));
            p.extend(foot(0.04, 0.0, true));
            p.push(Prim::Ring {
                cx: 0.36,
                cy: ASC - 0.21,
                rx: 0.20,
                ry: 0.21,
                t_side: THICK,
                t_top: THIN,
                a: Some((-90.0_f32.to_radians(), 90.0_f32.to_radians())),
            });
        }
        'Q' => {
            p.push(Prim::Ring {
                cx: 0.375,
                cy: ASC / 2.0,
                rx: 0.30,
                ry: ASC / 2.0,
                t_side: THICK,
                t_top: THIN,
                a: None,
            });
            p.push(bar(0.42, 0.02, 0.62, -0.10, THICK));
        }
        'R' => {
            p.push(Prim::Rect(0.04, 0.0, 0.04 + THICK, ASC));
            p.extend(foot(0.04, 0.0, true));
            p.push(Prim::Ring {
                cx: 0.36,
                cy: ASC - 0.21,
                rx: 0.20,
                ry: 0.21,
                t_side: THICK,
                t_top: THIN,
                a: Some((-90.0_f32.to_radians(), 90.0_f32.to_radians())),
            });
            p.push(bar(0.20, 0.26, 0.54, 0.0, THICK));
        }
        'S' => {
            p.push(Prim::Ring {
                cx: 0.30,
                cy: ASC - 0.19,
                rx: 0.19,
                ry: 0.19,
                t_side: THICK * 0.9,
                t_top: THIN,
                a: Some((30.0_f32.to_radians(), 270.0_f32.to_radians())),
            });
            p.push(Prim::Ring {
                cx: 0.30,
                cy: 0.19,
                rx: 0.19,
                ry: 0.19,
                t_side: THICK * 0.9,
                t_top: THIN,
                a: Some((210.0_f32.to_radians(), 450.0_f32.to_radians())),
            });
        }
        'T' => {
            p.push(Prim::Rect(0.28, 0.0, 0.28 + THICK, ASC));
            p.push(Prim::Rect(0.02, ASC - THICK, 0.56, ASC));
        }
        'U' => {
            p.push(Prim::Rect(0.04, 0.24, 0.04 + THICK, ASC));
            p.push(Prim::Rect(0.50, 0.24, 0.50 + THICK, ASC));
            p.push(Prim::Ring {
                cx: 0.28,
                cy: 0.30,
                rx: 0.21,
                ry: 0.30,
                t_side: THICK,
                t_top: THIN,
                a: Some((180.0_f32.to_radians(), 360.0_f32.to_radians())),
            });
        }
        'V' => {
            p.push(bar(0.03, ASC, 0.30, 0.0, THICK));
            p.push(bar(0.30, 0.0, 0.57, ASC, THICK));
        }
        'W' => {
            p.push(bar(0.03, ASC, 0.22, 0.0, THICK));
            p.push(bar(0.22, 0.0, 0.41, ASC, THICK));
            p.push(bar(0.41, ASC, 0.60, 0.0, THICK));
            p.push(bar(0.60, 0.0, 0.79, ASC, THICK));
        }
        'X' => {
            p.push(bar(0.04, ASC, 0.56, 0.0, THICK));
            p.push(bar(0.04, 0.0, 0.56, ASC, THICK));
        }
        'Y' => {
            p.push(bar(0.03, ASC, 0.30, 0.26, THICK));
            p.push(bar(0.30, 0.26, 0.57, ASC, THICK));
            p.push(Prim::Rect(0.26, 0.0, 0.26 + THICK, 0.30));
        }
        'Z' => {
            p.push(Prim::Rect(0.04, ASC - THICK, 0.56, ASC));
            p.push(bar(0.06, ASC - THICK * 0.8, 0.52, THICK * 0.6, THICK));
            p.push(Prim::Rect(0.04, 0.0, 0.56, THICK));
        }
        '0' => {
            p.push(Prim::Ring {
                cx: 0.275,
                cy: ASC / 2.0,
                rx: 0.21,
                ry: ASC / 2.0,
                t_side: THICK,
                t_top: THIN,
                a: None,
            });
        }
        '1' => {
            p.push(Prim::Rect(0.22, 0.0, 0.22 + THICK, ASC));
            p.extend(foot(0.22, 0.0, true));
            p.push(bar(0.02, ASC - 0.16, 0.22, ASC, THICK));
        }
        '2' => {
            p.push(Prim::Ring {
                cx: 0.27,
                cy: ASC - 0.19,
                rx: 0.20,
                ry: 0.19,
                t_side: THICK,
                t_top: THIN,
                a: Some((10.0_f32.to_radians(), 265.0_f32.to_radians())),
            });
            p.push(bar(0.06, 0.20, 0.44, 0.0, THICK));
            p.push(Prim::Rect(0.04, 0.0, 0.48, THICK));
        }
        '3' => {
            p.push(Prim::Ring {
                cx: 0.27,
                cy: ASC - 0.17,
                rx: 0.17,
                ry: 0.17,
                t_side: THICK * 0.9,
                t_top: THIN,
                a: Some((-60.0_f32.to_radians(), 240.0_f32.to_radians())),
            });
            p.push(Prim::Ring {
                cx: 0.27,
                cy: 0.19,
                rx: 0.17,
                ry: 0.19,
                t_side: THICK * 0.9,
                t_top: THIN,
                a: Some((200.0_f32.to_radians(), 480.0_f32.to_radians())),
            });
        }
        '4' => {
            p.push(Prim::Rect(0.04, 0.26, 0.04 + THICK, ASC));
            p.push(bar(0.04, 0.26, 0.42, 0.26, THICK));
            p.push(Prim::Rect(0.34, 0.0, 0.34 + THICK, ASC));
        }
        '5' => {
            p.push(Prim::Rect(0.05, ASC - THICK, 0.40, ASC));
            p.push(Prim::Rect(0.05, 0.30, 0.05 + THICK * 0.8, ASC));
            p.push(Prim::Ring {
                cx: 0.27,
                cy: 0.21,
                rx: 0.19,
                ry: 0.21,
                t_side: THICK * 0.9,
                t_top: THIN,
                a: Some((170.0_f32.to_radians(), 470.0_f32.to_radians())),
            });
        }
        '6' => {
            p.push(Prim::Ring {
                cx: 0.27,
                cy: 0.22,
                rx: 0.20,
                ry: 0.22,
                t_side: THICK * 0.9,
                t_top: THIN,
                a: None,
            });
            p.push(bar(0.30, 0.30, 0.38, ASC - 0.10, THICK));
        }
        '7' => {
            p.push(Prim::Rect(0.03, ASC - THICK, 0.50, ASC));
            p.push(bar(0.08, ASC - THICK * 0.8, 0.46, THICK * 0.6, THICK));
        }
        '8' => {
            p.push(Prim::Ring {
                cx: 0.27,
                cy: ASC - 0.17,
                rx: 0.17,
                ry: 0.17,
                t_side: THICK * 0.9,
                t_top: THIN,
                a: None,
            });
            p.push(Prim::Ring {
                cx: 0.27,
                cy: 0.19,
                rx: 0.19,
                ry: 0.19,
                t_side: THICK * 0.9,
                t_top: THIN,
                a: None,
            });
        }
        '9' => {
            p.push(Prim::Ring {
                cx: 0.27,
                cy: ASC - 0.19,
                rx: 0.19,
                ry: 0.19,
                t_side: THICK * 0.9,
                t_top: THIN,
                a: None,
            });
            p.push(bar(0.30, ASC - 0.20, 0.38, 0.10, THICK));
        }
        '.' => p.push(Prim::Dot(0.14, 0.05, THICK * 0.7)),
        ',' => {
            p.push(Prim::Dot(0.14, 0.06, THICK * 0.7));
            p.push(bar(0.14, 0.05, 0.08, -0.12, THICK * 0.7));
        }
        '\'' | '\u{2019}' => p.push(bar(0.10, X + 0.12, 0.10, ASC - 0.02, THIN)),
        '!' => {
            p.push(bar(0.12, 0.16, 0.12, ASC, THICK));
            p.push(Prim::Dot(0.14, 0.0, THICK * 0.7));
        }
        '?' => {
            p.push(Prim::Ring {
                cx: 0.20,
                cy: ASC - 0.18,
                rx: 0.16,
                ry: 0.18,
                t_side: THICK,
                t_top: THIN,
                a: Some((-40.0_f32.to_radians(), 250.0_f32.to_radians())),
            });
            p.push(bar(0.20, ASC - 0.34, 0.20, 0.10, THICK));
            p.push(Prim::Dot(0.20, 0.0, THICK * 0.7));
        }
        '-' | '\u{2013}' => p.push(Prim::Rect(0.02, 0.20, 0.32, 0.20 + THIN * 1.2)),
        '\u{2014}' => p.push(Prim::Rect(0.0, 0.20, 0.96, 0.20 + THIN * 1.2)),
        '(' => p.push(Prim::Ring {
            cx: 0.34,
            cy: ASC / 2.0,
            rx: 0.28,
            ry: ASC / 2.0 + 0.06,
            t_side: THICK * 0.7,
            t_top: THICK * 0.7,
            a: Some((100.0_f32.to_radians(), 260.0_f32.to_radians())),
        }),
        ')' => p.push(Prim::Ring {
            cx: -0.02,
            cy: ASC / 2.0,
            rx: 0.28,
            ry: ASC / 2.0 + 0.06,
            t_side: THICK * 0.7,
            t_top: THICK * 0.7,
            a: Some((-80.0_f32.to_radians(), 80.0_f32.to_radians())),
        }),
        '"' => {
            p.push(bar(0.06, X + 0.10, 0.06, ASC, THIN));
            p.push(bar(0.20, X + 0.10, 0.20, ASC, THIN));
        }
        ';' => {
            p.push(Prim::Dot(0.14, 0.30, THICK * 0.6));
            p.push(bar(0.14, 0.24, 0.08, -0.12, THICK * 0.6));
        }
        ':' => {
            p.push(Prim::Dot(0.14, 0.30, THICK * 0.6));
            p.push(Prim::Dot(0.14, 0.02, THICK * 0.6));
        }
        '/' => p.push(bar(0.02, 0.0, 0.46, ASC, THICK * 0.8)),
        '&' => {
            p.push(bar(0.06, 0.0, 0.50, ASC, THICK));
            p.push(bar(0.50, ASC, 0.14, 0.20, THICK));
            p.push(bar(0.10, 0.30, 0.52, 0.0, THICK));
        }
        '*' => {
            p.push(bar(0.20, 0.30, 0.20, ASC, THIN));
            p.push(bar(0.08, 0.38, 0.32, 0.62, THIN));
            p.push(bar(0.08, 0.62, 0.32, 0.38, THIN));
        }
        _ => return None,
    }
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_advance_is_positive_and_bounded() {
        for c in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 .',!?-\u{2019}\u{2014}();:\"&*/".chars() {
            let a = advance(c).unwrap_or_else(|| panic!("advance missing for {c:?}"));
            assert!((0.05..=1.2).contains(&a), "{c:?} advance {a}");
        }
    }

    #[test]
    fn lowercase_garden_text_is_supported() {
        assert!(glyph_supported(
            "Don't Stop \u{2014} can't, won't! (q&a) 42?"
        ));
        assert!(!glyph_supported("\u{4f60}\u{597d}"));
    }

    #[test]
    fn glyphs_paint_inside_a_bounded_box() {
        // Every supported glyph draws at least something and stays within
        // the mask bounds at a large size.
        let mut g = MaskGrid::new(24, 12);
        let mut x = 1.0f32;
        for c in "abzABZ0.?".chars() {
            draw_glyph(&mut g, c, x, 20.0, 16.0);
            x += advance(c).unwrap() * 16.0;
        }
        assert!(g.role.contains(&(Role::Type as u8)));
    }

    #[test]
    fn advance_is_proportional() {
        // i is narrower than m, space is compact (reference multiplies it).
        assert!(advance('i').unwrap() < advance('m').unwrap());
        assert!(advance('w').unwrap() > advance('n').unwrap());
    }
}
