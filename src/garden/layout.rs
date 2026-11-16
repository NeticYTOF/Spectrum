//! Letter layout, ported from ascii-garden's `garden.ts layoutCore`
//! (upstream f519e85): proportional advances, word wrapping that never
//! drops a word, over-long words split into chunks, and a binary-search
//! size that trades letter size against line count within the band.
//!
//! All coordinates are in design space (x in cell widths, y in half-row
//! units; one terminal cell = 1.0 x 2.0).

use super::font;

/// A placed letter: centre x, baseline y, advance width, line index.
#[derive(Clone, Debug)]
pub struct Placement {
    pub ch: char,
    /// Letter centre x (design units) - the reference centers each
    /// glyph in its advance slot and draws it centered.
    pub tx: f32,
    /// Baseline y (design units).
    pub ty: f32,
    /// Advance width (design units).
    pub tw: f32,
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub letters: Vec<Placement>,
    /// Type size (design units).
    pub s: f32,
    pub wrapped: bool,
    pub n_lines: usize,
}

/// Advance of `ch` scaled by `s` (mw() in garden.ts).
fn mw(ch: char, s: f32) -> f32 {
    font::advance(ch).unwrap_or(0.5) * s
}

/// Sum of advances of `ids`, not counting trailing spaces (lineW).
fn line_w(chars: &[char], ids: &[usize], s: f32) -> f32 {
    let mut e = ids.len();
    while e > 0 && chars[ids[e - 1]] == ' ' {
        e -= 1;
    }
    ids[..e].iter().map(|&i| mw(chars[i], s)).sum()
}

/// The layoutCore port. `w`/`h` are the band's design size; `h` is the
/// FULL design height the type may occupy (reference H).
pub fn layout_core(text: &str, w: f32, h: f32, prev_wrapped: bool) -> Layout {
    let max_w = w * 0.8;
    let s0 = h * 0.28;
    let lh = 2.1; // line height in ems
    let chars: Vec<char> = text.chars().collect();
    let ws: Vec<f32> = chars.iter().map(|&c| mw(c, 1.0)).collect();

    // Words as char-index groups; spaces belong to the preceding line.
    let mut words: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == ' ' {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            words.push(vec![i]); // the space itself
        } else {
            cur.push(i);
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }

    // A single long word cannot wrap: break it into chunks at a readable
    // size so it can sit on separate lines (reference limit math).
    let col_w = w * 0.58;
    let limit = col_w / (h * 0.1);
    let mut wi = 0;
    while wi < words.len() {
        let word: Vec<usize> = words[wi].clone();
        if word[0] < chars.len() && chars[word[0]] != ' ' {
            let total: f32 = word.iter().map(|&i| ws[i]).sum();
            if total > limit {
                let mut parts: Vec<Vec<usize>> = Vec::new();
                let mut part: Vec<usize> = Vec::new();
                let mut acc = 0.0;
                for &i in &word {
                    if acc + ws[i] > limit && !part.is_empty() {
                        parts.push(std::mem::take(&mut part));
                        acc = 0.0;
                    }
                    part.push(i);
                    acc += ws[i];
                }
                if !part.is_empty() {
                    parts.push(part);
                }
                let np = parts.len();
                words.splice(wi..wi + 1, parts);
                wi += np - 1;
            }
        }
        wi += 1;
    }

    // wrap(S): greedy word wrap; spaces belong to the preceding line and
    // never indent a fresh line.
    let wrap = |s: f32| -> Vec<Vec<usize>> {
        let mut lines: Vec<Vec<usize>> = vec![Vec::new()];
        let mut lw = 0.0;
        for word in &words {
            let is_space = chars[word[0]] == ' ';
            if is_space && lw == 0.0 {
                continue;
            }
            let ww: f32 = word.iter().map(|&i| ws[i]).sum::<f32>() * s;
            if !is_space && lw > 0.0 && lw + ww > col_w {
                lines.push(Vec::new());
                lw = 0.0;
            }
            lines.last_mut().unwrap().extend(word.iter().copied());
            lw += ww;
        }
        lines
    };

    let mut lines: Vec<Vec<usize>> = vec![(0..chars.len()).collect()];
    let unit = line_w(&chars, &lines[0], 1.0);
    let mut s = if unit > 0.0 {
        (s0).min(max_w / unit)
    } else {
        s0
    };
    let multi = words.iter().filter(|wrd| chars[wrd[0]] != ' ').count() > 1;
    let mut wrapped = multi && (s < h * 0.1 || (prev_wrapped && s < h * 0.14));
    if wrapped {
        let mut lo = h * 0.03;
        let mut hi = h * 0.15;
        for _ in 0..22 {
            let mid = (lo + hi) / 2.0;
            let ls = wrap(mid);
            let ok = ls.iter().all(|li| line_w(&chars, li, mid) <= col_w)
                && ls.len() as f32 * lh * mid <= h * 0.82;
            if ok {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        s = lo;
        lines = wrap(s);
        if lines.len() < 2 {
            wrapped = false;
        }
    }

    let n = lines.len();
    let lh_px = lh * s;
    let mut letters = Vec::with_capacity(chars.len());
    for (k, li) in lines.iter().enumerate() {
        let mut x = w / 2.0 - line_w(&chars, li, s) / 2.0;
        let y = h / 2.0 + (k as f32 - (n as f32 - 1.0) / 2.0) * lh_px + 0.33 * s;
        for &i in li {
            let cw = ws[i] * s;
            letters.push(Placement {
                ch: chars[i],
                tx: x + cw / 2.0,
                ty: y,
                tw: cw,
            });
            x += cw;
        }
    }
    Layout {
        letters,
        s,
        wrapped,
        n_lines: n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_short_line_fits_without_wrapping() {
        let text = "hello";
        let lay = layout_core(text, 144.0, 40.0, false);
        assert_eq!(lay.n_lines, 1);
        assert!(!lay.wrapped);
        assert_eq!(lay.letters.len(), 5);
        // Letters sit inside the band.
        for l in &lay.letters {
            assert!((0.0..144.0).contains(&l.tx), "letter at {}", l.tx);
        }
    }

    #[test]
    fn long_line_wraps_and_keeps_every_word() {
        // Design height = band rows * 2 (a 22-row terminal's band ~18 rows).
        let text = "grow a garden out of every single sound we play";
        let lay = layout_core(text, 72.0, 36.0, false);
        let rendered: String = lay.letters.iter().map(|l| l.ch).collect();
        for word in text.split(' ') {
            assert!(rendered.contains(word), "lost word {word:?}");
        }
        assert!(lay.wrapped);
        assert!(lay.n_lines >= 2);
    }

    #[test]
    fn one_very_long_word_splits_into_chunks() {
        let text = "unstoppably";
        let lay = layout_core(text, 40.0, 12.0, false);
        let rendered: String = lay.letters.iter().map(|l| l.ch).collect();
        assert_eq!(rendered.chars().count(), text.len());
        // Every letter still placed inside the band horizontally.
        for l in &lay.letters {
            assert!((-2.0..42.0).contains(&l.tx));
        }
    }

    #[test]
    fn tiny_terminals_never_drop_characters() {
        // The 72x22 "lost bloom" regression: the full text must render.
        let text = "every beat a budding bloom";
        let lay = layout_core(text, 70.0, 14.0, false);
        let rendered: String = lay.letters.iter().map(|l| l.ch).collect();
        for word in text.split(' ') {
            assert!(rendered.contains(word), "lost {word:?} at tiny size");
        }
    }

    #[test]
    fn empty_text_is_valid() {
        let lay = layout_core("", 40.0, 12.0, false);
        assert!(lay.letters.is_empty());
        assert_eq!(lay.n_lines, 1);
    }

    #[test]
    fn size_shrinks_for_long_lines_and_stays_positive() {
        let short = layout_core("hi", 72.0, 20.0, false);
        let long = layout_core(
            "a whole long line of lyrics here to sing",
            72.0,
            20.0,
            false,
        );
        assert!(long.s <= short.s);
        assert!(long.s > 0.0);
    }
}
