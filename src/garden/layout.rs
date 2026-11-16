//! Letter layout for the garden.
//!
//! Upstream (garden.ts `layoutCore`) keeps letters legible by shrinking
//! its CELLS with the type (`targetChT = St/17`, floored at 8 px), so a
//! letter always spans roughly 17 cells per em down to that floor. A
//! terminal cannot shrink its cells, so the same intent - its comment:
//! "a long phrase breaks into more, larger lines instead of two wide,
//! small ones" - is realized here by searching the line count that
//! maximizes the type size within the band. Words are wrapped only at
//! spaces; a single word too long for any line is split into chunks (as
//! upstream does). No character is ever dropped.
//!
//! Coordinates are design units (x in cell widths, y in half-rows; one
//! terminal cell = 1.0 x 2.0). Advances and kerning come from the real
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

/// Line pitch in ems. Upstream uses 2.1 so its garden fits between
/// lines; lyrics need the size more, and stems still weave between the
/// lines through the back/front layers.
pub const LINE_PITCH: f32 = 1.55;
/// Share of the band width a line may use.
const WIDTH_FRAC: f32 = 0.88;
/// Share of the band height the type block may use (the rest is garden
/// headroom, mostly above the type).
const HEIGHT_FRAC: f32 = 0.64;
/// Share of the free vertical space placed above the type block: the
/// garden grows mostly upward from the letters, so the headroom belongs
/// above (upstream's single line sits a third of an em below centre).
const ABOVE_FRAC: f32 = 0.74;

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

/// The narrowest wrap into at most `n` lines (binary search on width).
fn balanced(words: &[Vec<char>], n: usize) -> Vec<Vec<usize>> {
    let widest = words.iter().map(|w| run_w(w)).fold(0.0f32, f32::max);
    let total = line_width(words, &(0..words.len()).collect::<Vec<_>>());
    let (mut lo, mut hi) = (widest, total.max(widest));
    for _ in 0..28 {
        let mid = (lo + hi) / 2.0;
        if wrap(words, mid).len() <= n {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    wrap(words, hi + 1e-4)
}

/// Split any word wider than `max_em` into chunks (upstream splits words
/// that cannot wrap so they stay at a readable size).
fn split_long(words: Vec<Vec<char>>, max_em: f32) -> Vec<Vec<char>> {
    let mut out = Vec::new();
    for wd in words {
        if run_w(&wd) <= max_em || wd.len() < 2 {
            out.push(wd);
            continue;
        }
        let mut part: Vec<char> = Vec::new();
        for c in wd {
            part.push(c);
            if run_w(&part) > max_em && part.len() > 1 {
                let last = part.pop().unwrap();
                out.push(std::mem::take(&mut part));
                part.push(last);
            }
        }
        if !part.is_empty() {
            out.push(part);
        }
    }
    out
}

/// Lay out `text` in a band of `w` x `h` design units.
pub fn layout(text: &str, w: f32, h: f32) -> Layout {
    let m = font::metrics();
    let ink_h = m.ascender - m.descender; // em, one line's full ink height
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
    let max_h = h * HEIGHT_FRAC;
    let size_for = |lines: &[Vec<usize>], words: &[Vec<char>]| -> f32 {
        let n = lines.len() as f32;
        let widest = lines
            .iter()
            .map(|l| line_width(words, l))
            .fold(0.0f32, f32::max)
            .max(0.01);
        let block = (n - 1.0) * LINE_PITCH + ink_h;
        (max_w / widest).min(max_h / block)
    };

    let search = |words: &[Vec<char>]| {
        let max_lines = words.len().clamp(1, 6);
        let mut local: Option<(f32, Vec<Vec<usize>>)> = None;
        for n in 1..=max_lines {
            let lines = balanced(words, n);
            let s = size_for(&lines, words);
            // Prefer fewer lines unless more lines are clearly bigger.
            let better = match &local {
                None => true,
                Some((bs, _)) => s > *bs * 1.04,
            };
            if better {
                local = Some((s, lines));
            }
        }
        local.unwrap()
    };
    let mut chosen = search(&words);
    // When one long word forces the type below a legible em (~3 rows =
    // 6 design units), split such words into chunks (upstream does the
    // same for words that cannot wrap) and search again.
    let legible_s = 6.0f32.min(max_h / ink_h);
    let limit_em = max_w / legible_s;
    if chosen.0 < legible_s && words.iter().any(|wd| run_w(wd) > limit_em) {
        words = split_long(words, limit_em);
        chosen = search(&words);
    }
    let (s, lines) = chosen;

    let n = lines.len();
    let block_h = ((n as f32 - 1.0) * LINE_PITCH + ink_h) * s;
    let top = ((h - block_h).max(0.0)) * ABOVE_FRAC;
    let mut letters = Vec::new();
    let space = adv(' ');
    for (k, line) in lines.iter().enumerate() {
        let lw = line_width(&words, line) * s;
        let mut x = (w - lw) / 2.0;
        let baseline = top + m.ascender * s + k as f32 * LINE_PITCH * s;
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

    #[test]
    fn short_word_gets_big_type() {
        // "hello" in a 144x40 band: one line, an em of many rows.
        let lay = layout("hello", 144.0, 80.0);
        assert_eq!(lay.n_lines, 1);
        assert_eq!(rendered(&lay), "hello");
        assert!(lay.s / 2.0 >= 20.0, "em rows {}", lay.s / 2.0);
    }

    #[test]
    fn lyric_line_wraps_to_maximize_size_and_keeps_every_word() {
        let text = "every beat a budding bloom";
        let one = layout(text, 144.0, 80.0);
        for wd in text.split(' ') {
            assert!(rendered(&one).contains(wd), "lost {wd:?}");
        }
        assert!(one.n_lines >= 2, "a long line should wrap for size");
        // Bigger than a single unwrapped line would be.
        let single = 144.0 * WIDTH_FRAC / run_w(&text.chars().collect::<Vec<_>>());
        assert!(one.s > single * 1.2, "{} vs {}", one.s, single);
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
    fn overlong_word_splits_rather_than_overflowing() {
        let lay = layout("incomprehensibilities", 24.0, 40.0);
        assert!(lay.n_lines >= 2, "split expected");
        for p in &lay.letters {
            assert!(p.tx <= 24.5);
        }
    }

    #[test]
    #[ignore]
    fn print_layouts() {
        for (w, h, t) in [
            (144.0, 74.0, "every beat a budding bloom"),
            (144.0, 74.0, "hello"),
            (72.0, 34.0, "every beat a budding bloom"),
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
                l.s / 2.0,
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
