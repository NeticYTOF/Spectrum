//! Letter layout for the garden, ported exactly from upstream
//! (garden.ts `layoutCore`): split long words, size, wrap, place -
//! nothing else. Upstream keeps its type legible at every size by
//! shrinking its CELLS with the type (`targetChT = St/17`, floored at
//! 8 px); a terminal cannot shrink its cells, so bands too small for a
//! letterform render through the mod-level plain-text fallback (see
//! `PLAIN_BELOW_EM_ROWS`) instead of this solver inventing a bigger
//! size.
//!
//! Coordinates are design units (x in cell widths, y in design units;
//! one terminal cell = 1.0 x 1.5 - the reference's 2:3 cell, see
//! `mask::ROW_DESIGN`). Advances and kerning come from the real
//! Playfair Display font.

use super::font;

/// A placed letter: glyph-slot centre x, baseline y, advance width.
#[derive(Clone, Debug)]
pub struct Placement {
    pub ch: char,
    pub tx: f32,
    pub ty: f32,
    pub tw: f32,
    /// Which wrapped line the letter sits on.
    pub line: usize,
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub letters: Vec<Placement>,
    /// Type size: one em in design units.
    pub s: f32,
    pub n_lines: usize,
}

/// Line pitch in ems (upstream `LH = 2.1`): a lower line's garden fits
/// under the line above without covering it.
pub const LINE_PITCH: f32 = 2.1;
/// Upstream `S0`: the initial type-size cap as a fraction of the band
/// height. The reference hello runs at St = 0.28 H.
const SIZE0_FRAC: f32 = 0.28;
/// Upstream `maxW`: the most of the band width one unwrapped line may
/// use.
const WIDTH_FRAC: f32 = 0.8;
/// Upstream `colW`: the wrap column for wrapped text (narrower than a
/// single line, so a long phrase breaks into more, larger lines).
const COL_FRAC: f32 = 0.58;
/// Upstream wrapped-size window: the binary search bounds, as fractions
/// of the band height.
const WRAP_LO_FRAC: f32 = 0.03;
const WRAP_HI_FRAC: f32 = 0.15;
/// Upstream wrap height budget: wrapped lines must satisfy
/// `n * LH * S <= H * 0.82`.
const WRAP_H_FRAC: f32 = 0.82;

fn adv(c: char) -> f32 {
    font::advance(c).unwrap_or(0.5)
}

/// Width of a run of chars in em, including kerning.
fn run_w(chars: &[char]) -> f32 {
    let mut w = 0.0;
    for (i, &c) in chars.iter().enumerate() {
        w += adv(c);
        if let Some(&n) = chars.get(i + 1) {
            w += font::kern(c, n);
        }
    }
    w
}

/// Greedy wrap of `words` into lines no wider than `max_w` em.
fn wrap(words: &[Vec<char>], max_w: f32) -> Vec<Vec<usize>> {
    let space = adv(' ');
    let mut lines: Vec<Vec<usize>> = vec![Vec::new()];
    let mut lw = 0.0f32;
    for (i, wd) in words.iter().enumerate() {
        let ww = run_w(wd);
        let cur = lines.last_mut().unwrap();
        if cur.is_empty() {
            cur.push(i);
            lw = ww;
        } else if lw + space + ww <= max_w {
            cur.push(i);
            lw += space + ww;
        } else {
            lines.push(vec![i]);
            lw = ww;
        }
    }
    lines
}

fn line_width(words: &[Vec<char>], line: &[usize]) -> f32 {
    let space = adv(' ');
    let mut w = 0.0;
    for (k, &i) in line.iter().enumerate() {
        if k > 0 {
            w += space;
        }
        w += run_w(&words[i]);
    }
    w
}

/// Split any word wider than `max_em` em into chunks no wider than
/// `max_em`. This is upstream's FIRST preprocessing step: a word too
/// wide for the wrap column at the minimum wrapped size becomes several
/// wrap-able words BEFORE any sizing happens (measured on the running
/// reference: `incomprehensibilities` at W24 H40 splits at the limit
/// 3.48 em and wraps into 4 lines). Accumulator-based per-character
/// advances exactly like upstream - no kerning inside a chunk - and
/// nothing is ever dropped: a chunk is only closed once non-empty.
fn split_long(words: Vec<Vec<char>>, max_em: f32) -> Vec<Vec<char>> {
    let mut out = Vec::new();
    for wd in words {
        let mut part: Vec<char> = Vec::new();
        let mut acc = 0.0f32;
        for &c in &wd {
            let cw = adv(c);
            if acc + cw > max_em && !part.is_empty() {
                out.push(std::mem::take(&mut part));
                acc = 0.0;
            }
            part.push(c);
            acc += cw;
        }
        if !part.is_empty() {
            out.push(part);
        }
    }
    out
}

/// Lay out `text` in a band of `w` x `h` design units, exactly like
/// upstream `layoutCore`:
///
/// 1. split long words (limit `colW / (0.1 H)`), unconditionally, before
///    the multi-word detection;
/// 2. one-line size `S = min(0.28 H, 0.8 W / unit)`;
/// 3. when several words remain AND that size is below `0.1 H`, binary
///    search the LARGEST `S` in `[0.03 H, 0.15 H]` whose wrap into the
///    `0.58 W` column fits every line and satisfies
///    `n * LH * S <= 0.82 H` (22 iterations, then re-wrap at the
///    converged bound; a wrap that does not split falls back to the
///    single line);
/// 4. place with the pure upstream formula - the block's centre at the
///    band's centre, shifted a third of an em down - with no span
///    clamping and no recentering: the reference's one-line baseline is
///    exactly `0.5 H + 0.33 St`.
pub fn layout(text: &str, w: f32, h: f32) -> Layout {
    let mut words: Vec<Vec<char>> = text
        .split(' ')
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().collect())
        .collect();
    if words.is_empty() {
        return Layout {
            letters: Vec::new(),
            s: h * 0.3,
            n_lines: 1,
        };
    }
    let max_w = w * WIDTH_FRAC;
    let col_w = w * COL_FRAC;
    let s0 = h * SIZE0_FRAC;

    // 1. Split long words FIRST - upstream runs this before deciding
    //    whether the text is multi-word at all.
    let limit_em = col_w / (h * 0.1);
    words = split_long(words, limit_em);

    // 2. One-line size from the whole unit (words joined by spaces).
    let unit: f32 = {
        let space = adv(' ');
        let mut u = 0.0f32;
        for (i, wd) in words.iter().enumerate() {
            if i > 0 {
                u += space;
            }
            u += run_w(wd);
        }
        u
    };
    let single_s = if unit > 0.0 {
        (max_w / unit).min(s0)
    } else {
        s0
    };
    let all = vec![(0..words.len()).collect::<Vec<_>>()];

    // 3. Wrap search. Upstream probes only the midpoints - it does not
    //    pre-check the low bound - converges for 22 iterations and then
    //    re-wraps at the converged size; a wrap that does not actually
    //    split is dropped (the text stays one line).
    let (s, lines) = if words.len() < 2 || single_s >= h * 0.1 {
        (single_s, all)
    } else {
        let fits = |mid: f32| -> bool {
            let ls = wrap(&words, col_w / mid);
            ls.iter()
                .all(|l| line_width(&words, l) * mid <= col_w + 1e-4)
                && ls.len() as f32 * LINE_PITCH * mid <= h * WRAP_H_FRAC
        };
        let (mut lo, mut hi) = (h * WRAP_LO_FRAC, h * WRAP_HI_FRAC);
        for _ in 0..22 {
            let mid = 0.5 * (lo + hi);
            if fits(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let ls = wrap(&words, col_w / lo);
        if ls.len() >= 2 {
            (lo, ls)
        } else {
            (single_s, all)
        }
    };

    // 4. Pure upstream placement: the block's middle sits at the band's
    //    middle with a third-of-an-em downward shift - the reference's
    //    one-line baseline is exactly 0.5 H + 0.33 St. No span
    //    recentering, no edge clamping: content that overflows a small
    //    band clips in the raster mask, like upstream's canvas.
    let n = lines.len();
    let top = h / 2.0 - (n as f32 - 1.0) * LINE_PITCH * s / 2.0 + 0.33 * s;
    let mut letters = Vec::new();
    let space = adv(' ');
    for (k, line) in lines.iter().enumerate() {
        let lw = line_width(&words, line) * s;
        let mut x = (w - lw) / 2.0;
        let baseline = top + k as f32 * LINE_PITCH * s;
        for (wi, &idx) in line.iter().enumerate() {
            if wi > 0 {
                let sw = space * s;
                letters.push(Placement {
                    ch: ' ',
                    tx: x + sw / 2.0,
                    ty: baseline,
                    tw: sw,
                    line: k,
                });
                x += sw;
            }
            let wd = &words[idx];
            for (ci, &c) in wd.iter().enumerate() {
                let a = adv(c) * s;
                let kn = wd.get(ci + 1).map(|&nx| font::kern(c, nx)).unwrap_or(0.0) * s;
                letters.push(Placement {
                    ch: c,
                    tx: x + a / 2.0,
                    ty: baseline,
                    tw: a + kn,
                    line: k,
                });
                x += a + kn;
            }
        }
    }
    Layout {
        letters,
        s,
        n_lines: n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(l: &Layout) -> String {
        l.letters.iter().map(|p| p.ch).collect()
    }

    /// One baseline per wrapped line, in order (a line's letters share it).
    fn baselines(l: &Layout) -> Vec<f32> {
        let mut ys: Vec<f32> = Vec::new();
        for p in &l.letters {
            if !ys.iter().any(|y| (y - p.ty).abs() < 0.01) {
                ys.push(p.ty);
            }
        }
        ys
    }

    #[test]
    fn hello_matches_the_measured_upstream_reference() {
        // Measured on the running reference (layoutCore, W=144, H=80):
        // S = 22.4 = the 0.28 H cap, one line, baseline 47.392 =
        // 0.5 H + 0.33 St. The size is cap-bound (not width-bound), so
        // this pins the placement formula and the cap without depending
        // on exact glyph metrics.
        let lay = layout("hello", 144.0, 80.0);
        assert_eq!(lay.n_lines, 1);
        assert_eq!(rendered(&lay), "hello");
        assert!(
            (lay.s - 22.4).abs() < 0.02,
            "size {} vs measured 22.4",
            lay.s
        );
        let b = baselines(&lay);
        assert_eq!(b.len(), 1, "one line, one baseline");
        assert!(
            (b[0] - 47.392).abs() < 0.05,
            "baseline {} vs measured 47.392",
            b[0]
        );
        // Narrow and centred: a fraction of the band width, not
        // full-width.
        let ink_w: f32 = lay.letters.iter().map(|p| p.tw).sum();
        assert!(
            ink_w < 144.0 * WIDTH_FRAC,
            "hello wider than 0.8 W: {ink_w}"
        );
    }

    #[test]
    fn measured_sizes_and_wraps_match_the_running_reference() {
        // Measured upstream at W=144 H=80: the fixture stays ONE line
        // (S = 8.80277174 = 0.8 W / unit, above the 0.1 H wrap
        // threshold), while the long phrase wraps to exactly 3 lines at
        // S = 9.66041336 (the 3->4 line boundary). Both sizes are
        // width-bound, so they cross-check our real-font advances and
        // kerning against upstream's measureText of the same Playfair.
        let text = "every beat a budding bloom";
        let one = layout(text, 144.0, 80.0);
        for wd in text.split(' ') {
            assert!(rendered(&one).contains(wd), "lost {wd:?}");
        }
        assert_eq!(one.n_lines, 1, "fixture fits one line at this aspect");
        assert!(
            (one.s - 8.802_772).abs() < 0.05,
            "fixture size {} vs measured 8.80277174",
            one.s
        );

        let long_text = "grow a garden out of sound silence is the resting soil";
        let two = layout(long_text, 144.0, 80.0);
        for wd in long_text.split(' ') {
            assert!(rendered(&two).contains(wd), "lost {wd:?}");
        }
        assert_eq!(two.n_lines, 3, "long phrase wraps to 3 lines, measured");
        assert!(
            (two.s - 9.660_413).abs() < 0.05,
            "phrase size {} vs measured 9.66041336",
            two.s
        );
    }

    #[test]
    fn overlong_word_splits_first_and_wraps_like_upstream() {
        // The measured upstream counterexample: `incomprehensibilities`
        // at W24 H40 SPLITS FIRST (limit = colW / (0.1 H) = 3.48 em)
        // and then wraps like any multi-word text - 4 lines at
        // S = 3.9047619 (the 0.82 H height budget binding at exactly 4
        // lines), baselines 8.98857/17.18857/25.38857/33.58857 from the
        // pure placement formula. All 21 characters survive; nothing
        // overflows the band.
        let lay = layout("incomprehensibilities", 24.0, 40.0);
        let got: String = rendered(&lay).chars().filter(|c| *c != ' ').collect();
        assert_eq!(got, "incomprehensibilities", "dropped characters");
        assert_eq!(lay.n_lines, 4, "must split and wrap to 4 lines");
        assert!(
            (lay.s - 3.904_761_9).abs() < 0.01,
            "size {} vs measured 3.9047619",
            lay.s
        );
        let b = baselines(&lay);
        assert_eq!(b.len(), 4, "one baseline per wrapped line");
        for (y, want) in b.iter().zip([8.98857, 17.18857, 25.38857, 33.58857]) {
            assert!((y - want).abs() < 0.05, "baseline {y} vs measured {want}");
        }
        for p in &lay.letters {
            assert!(
                p.tx >= -0.5 && p.tx <= 24.5,
                "letter at {} outside band",
                p.tx
            );
        }
    }

    #[test]
    fn narrow_terminals_never_drop_characters() {
        for (w, h) in [(72.0, 36.0), (40.0, 20.0), (24.0, 12.0)] {
            let text = "Don't Stop \u{2014} Can't, Won't! every beat a budding bloom";
            let lay = layout(text, w, h);
            let got: String = rendered(&lay).chars().filter(|c| *c != ' ').collect();
            let want: String = text.chars().filter(|c| *c != ' ').collect();
            assert_eq!(got, want, "{w}x{h} dropped characters");
        }
    }

    #[test]
    fn letters_stay_inside_the_band() {
        for (w, h) in [(144.0, 80.0), (72.0, 36.0), (30.0, 14.0)] {
            let lay = layout("silence is the resting soil", w, h);
            for p in &lay.letters {
                assert!(p.tx >= -0.5 && p.tx <= w + 0.5, "x {} in {w}", p.tx);
                assert!(p.ty >= 0.0 && p.ty <= h + 0.5, "y {} in {h}", p.ty);
            }
        }
    }

    #[test]
    #[ignore]
    fn print_layouts() {
        for (w, h, t) in [
            (144.0, 80.0, "every beat a budding bloom"),
            (144.0, 80.0, "hello"),
            (72.0, 36.0, "every beat a budding bloom"),
            (72.0, 36.0, "hello"),
            (24.0, 40.0, "incomprehensibilities"),
        ] {
            let l = layout(t, w, h);
            let ys: Vec<f32> = l
                .letters
                .iter()
                .filter(|p| p.ch != ' ')
                .map(|p| p.ty)
                .collect();
            println!(
                "{w}x{h} {t:?}: s={:.2} em_rows={:.2} lines={} baselines {:?}",
                l.s,
                l.s / crate::garden::mask::ROW_DESIGN,
                l.n_lines,
                ys.iter().fold(Vec::<f32>::new(), |mut v, y| {
                    if !v.iter().any(|q| (q - y).abs() < 0.01) {
                        v.push(*y);
                    }
                    v
                })
            );
        }
    }

    #[test]
    fn empty_text_is_valid() {
        assert!(layout("", 40.0, 12.0).letters.is_empty());
    }
}
