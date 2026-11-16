//! The lyrics garden (mode 6): a direct port of ascii-garden's rendering
//! (upstream f519e85, MIT (c) 2026 Ridit - see NOTICE and
//! docs/PORT-PLAN.md). Pale paper, serif type with stems and roses
//! growing out of the letters, all rendered as terminal text through
//! the cell-grid coverage classifier.
//!
//! Honest states are preserved: Loading / Unavailable / NoMatch /
//! Instrumental / Error never show stale words; unsupported scripts fall
//! back to legible plain text (original case preserved). All animation
//! derives from the media timeline, so `--freeze-at` frames are
//! byte-identical across runs.

pub mod botany;
pub mod font;
pub mod layout;
pub mod mask;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::rc::Rc;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::lrc::LrcLine;

/// The public view contract with the UI (unchanged from the previous
/// garden so `ui.rs` keeps working).
pub struct GardenView<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    /// Synced lines with the current index, when lyrics are synced.
    pub lines: Option<(&'a [LrcLine], Option<usize>)>,
    /// Unsycned plain text, if that is the state.
    pub plain: Option<&'a str>,
    pub state: GardenState,
    /// Smoothed audio energy 0..1 driving sway.
    pub energy: f32,
    /// Media time for sway phase (frozen demos stay still).
    pub time_s: f32,
    /// Seconds since the current line started (regrows the garden).
    pub since_line_change: f32,
}

pub enum GardenState {
    Synced,
    Plain,
    Instrumental,
    Loading,
    NoMatch,
    Unavailable,
    Error(String),
}

// Palette chips for the plain-text chrome (title, prev/next, states).
pub const PAPER_BG: (u8, u8, u8) = mask::PAPER_BG;
pub const INK: (u8, u8, u8) = mask::TEXT;
pub const INK_DIM: (u8, u8, u8) = (130, 122, 110);
pub const STEM: (u8, u8, u8) = mask::STEM;
pub const FLOWER: (u8, u8, u8) = mask::FLOWER;

fn rgb(c: (u8, u8, u8)) -> ratatui::style::Color {
    ratatui::style::Color::Rgb(c.0, c.1, c.2)
}

fn paper_color() -> ratatui::style::Color {
    rgb(PAPER_BG)
}

/// Render the garden into `area` of the terminal buffer.
pub fn render_garden(buf: &mut Buffer, area: Rect, view: &GardenView<'_>) {
    if area.width < 8 || area.height < 3 {
        return;
    }

    // Backdrop: pale paper over the whole area.
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut(ratatui::layout::Position::new(x, y)) {
                cell.set_bg(paper_color());
            }
        }
    }

    // Title line at the very top, understated.
    let title = if view.artist.is_empty() {
        view.title.to_string()
    } else {
        format!("{} \u{b7} {}", view.title, view.artist)
    };
    put_text(buf, area, area.y, &title, rgb(INK_DIM));

    // The main band: everything between the title and the bottom line.
    let band_top = area.y + 1;
    let band_h = area.height.saturating_sub(2).max(1);
    let band = Rect::new(area.x, band_top, area.width, band_h);

    match &view.state {
        GardenState::Synced => {
            if let Some((lines, Some(idx))) = view.lines {
                let prev = idx.checked_sub(1).map(|i| lines[i].text.clone());
                let next = lines.get(idx + 1).map(|l| l.text.clone());
                let has_prev = prev.is_some();
                let has_next = next.is_some();
                let text = &lines[idx].text;

                if let Some(p) = &prev {
                    put_text(buf, area, band.y, p, rgb(INK_DIM));
                }
                if let Some(n) = &next {
                    let row = area.y + area.height - 1;
                    put_text(buf, area, row, n, rgb(INK_DIM));
                }

                let inner_top = band.y + if has_prev { 1 } else { 0 };
                let inner_h = band_h
                    .saturating_sub(if has_prev { 1 } else { 0 })
                    .saturating_sub(if has_next { 1 } else { 0 })
                    .max(2);
                let inner = Rect::new(band.x, inner_top, band.width, inner_h);
                render_garden_line(buf, inner, text, view);
            }
        }
        GardenState::Plain => {
            if let Some(plain) = view.plain {
                put_text(buf, area, band.y, "(unsynced lyrics)", rgb(INK_DIM));
                let rows = band.y + 1..band.y + band.height;
                for (row, line) in rows.zip(plain.lines()) {
                    put_text(buf, area, row, line, rgb(INK));
                }
            }
        }
        GardenState::Instrumental => {
            let inner = Rect::new(
                band.x,
                band.y + band.height / 3,
                band.width,
                band.height / 3,
            );
            render_garden_line(buf, inner, "[instrumental]", view);
        }
        GardenState::Loading => {
            put_text(
                buf,
                area,
                band.y + band.height / 2,
                "looking for lyrics...",
                rgb(INK_DIM),
            );
        }
        GardenState::NoMatch => {
            put_text(
                buf,
                area,
                band.y + band.height / 2,
                "no matching lyrics",
                rgb(INK),
            );
        }
        GardenState::Unavailable => {
            put_text(
                buf,
                area,
                band.y + band.height / 2,
                "no media session",
                rgb(INK),
            );
        }
        GardenState::Error(e) => {
            let msg = format!("lyrics: {e}");
            put_text(buf, area, band.y + band.height / 2, &msg, rgb(INK));
        }
    }
}

/// The current line as the garden: serif type on paper with stems and
/// roses growing out of the letters (all through the mask classifier),
/// or legible plain text when the script has no letterforms.
fn render_garden_line(buf: &mut Buffer, area: Rect, text: &str, view: &GardenView<'_>) {
    if area.width < 4 || area.height < 2 {
        return;
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    if !font::glyph_supported(trimmed) {
        // Honest fallback: legible normal text (original case preserved),
        // display-width aware so CJK and friends center correctly.
        put_text(buf, area, area.y + area.height / 2, trimmed, rgb(INK));
        return;
    }

    let w_design = area.width as f32;
    let data = cached_scene(trimmed, area.width, area.height);
    // Growth age: the line's timeline drives everything, so a frozen
    // position is fully deterministic. Letters bloom staggered like the
    // reference's regrow.
    let age_ms = view.since_line_change.max(0.0) * 1000.0;
    let letters: Vec<botany::SceneLetter<'_>> = data
        .placements
        .iter()
        .zip(data.els.iter())
        .enumerate()
        .map(|(i, (p, e))| botany::SceneLetter {
            ch: p.ch,
            tx: p.tx,
            ty: p.ty,
            els: e,
            birth_ms: i as f32 * 45.0,
            word_end: data
                .placements
                .get(i + 1)
                .map(|n| n.ch == ' ')
                .unwrap_or(true),
        })
        .collect();

    let mut grid = mask::MaskGrid::new(area.width as usize, area.height as usize);
    botany::draw_scene(&mut grid, &letters, data.s, age_ms, view.energy, w_design);

    // Present into a scratch buffer over the band, then blit.
    let mut scratch = Buffer::empty(Rect::new(0, 0, area.width, area.height));
    mask::present(
        &mut scratch,
        Rect::new(0, 0, area.width, area.height),
        &grid,
    );
    for row in 0..area.height {
        for col in 0..area.width {
            let src = scratch
                .cell(ratatui::layout::Position::new(col, row))
                .unwrap();
            if src.symbol() == " " {
                continue; // paper already set on the backdrop
            }
            if let Some(dst) =
                buf.cell_mut(ratatui::layout::Position::new(area.x + col, area.y + row))
            {
                *dst = src.clone();
                if dst.bg == ratatui::style::Color::Reset {
                    dst.set_bg(paper_color());
                }
            }
        }
    }
}

/// Layout + garden elements for one line at one size, cached (bounded).
struct SceneData {
    placements: Vec<layout::Placement>,
    els: Vec<botany::LetterEls>,
    s: f32,
}

fn cached_scene(text: &str, cols: u16, rows: u16) -> Rc<SceneData> {
    thread_local! {
        static CACHE: std::cell::RefCell<std::collections::HashMap<(String, u16, u16), Rc<SceneData>>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
    }
    CACHE.with(|c| {
        let mut map = c.borrow_mut();
        if map.len() > 4 {
            map.clear();
        }
        let key = (text.to_string(), cols, rows);
        map.entry(key)
            .or_insert_with(|| Rc::new(build_scene(text, cols, rows)))
            .clone()
    })
}

fn build_scene(text: &str, cols: u16, rows: u16) -> SceneData {
    let w = cols as f32;
    let h = rows as f32 * 2.0;
    let lay = layout::layout_core(text, w, h, false);
    let mut els = Vec::with_capacity(lay.letters.len());
    let mut prev_w: Option<f32> = None;
    let mut ws_seed: i32 = 0;
    let mut wi = 0usize;
    for (i, p) in lay.letters.iter().enumerate() {
        if p.ch == ' ' {
            ws_seed = 0;
            wi = 0;
            prev_w = None;
            els.push(botany::LetterEls {
                els: Vec::new(),
                tend: None,
            });
            continue;
        }
        if wi == 0 {
            // A new word begins at this letter: seed from the word text.
            let word: String = lay.letters[i..]
                .iter()
                .map(|l| l.ch)
                .take_while(|&c| c != ' ')
                .collect();
            ws_seed = hash_str(&word);
        }
        els.push(botany::gen(p.ch, ws_seed, wi, prev_w));
        // Bridge spans use the previous letter's em advance.
        prev_w = Some(p.tw / lay.s.max(0.01));
        wi += 1;
    }
    SceneData {
        placements: lay.letters,
        els,
        s: lay.s,
    }
}

fn hash_str(s: &str) -> i32 {
    // FNV-1a 32-bit, folded to i32.
    let mut h: u32 = 2166136261;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(16777619);
    }
    h as i32
}

/// Centered plain text within `area`, display-width aware, truncated
/// safely (never wider than the area).
fn put_text(buf: &mut Buffer, area: Rect, row: u16, text: &str, color: ratatui::style::Color) {
    if row < area.y || row >= area.y + area.height {
        return;
    }
    let width = area.width as usize;
    let line = truncate_to_width(text, width);
    let w = line.width();
    let x = area.x + ((width.saturating_sub(w)) / 2) as u16;
    let mut cx = x;
    for ch in line.chars() {
        if cx >= area.x + area.width {
            break;
        }
        if let Some(cell) = buf.cell_mut(ratatui::layout::Position::new(cx, row)) {
            cell.set_char(ch);
            cell.set_fg(color);
        }
        cx += ch.width().unwrap_or(0) as u16;
    }
}

/// Truncate a string to a display width without splitting a character
/// half-way (drop chars until they fit).
fn truncate_to_width(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synced_view(text: &str) -> GardenView<'static> {
        let lines: &'static [LrcLine] = Box::leak(
            vec![LrcLine {
                time_s: 0.0,
                text: text.to_string(),
            }]
            .into_boxed_slice(),
        );
        GardenView {
            title: "Garden of Sound",
            artist: "Spectrum Fixture",
            lines: Some((lines, Some(0))),
            plain: None,
            state: GardenState::Synced,
            energy: 0.3,
            time_s: 4.5,
            since_line_change: 4.5,
        }
    }

    fn cell_count(buf: &Buffer, pred: impl Fn(&ratatui::buffer::Cell) -> bool) -> usize {
        buf.content().iter().filter(|c| pred(c)).count()
    }

    /// Blue-dominant cells: stems (solid fill, soft fade, leaf shadow).
    fn blue_cells(buf: &Buffer) -> usize {
        cell_count(buf, |c| {
            matches!(c.fg, ratatui::style::Color::Rgb(r, g, b) if b as i32 > r as i32 + 40 && b as i32 > g as i32 + 20)
                || matches!(c.bg, ratatui::style::Color::Rgb(r, g, b) if b as i32 > r as i32 + 40 && b as i32 > g as i32 + 20)
        })
    }

    /// Red-dominant cells: rose petals and their fills.
    fn red_cells(buf: &Buffer) -> usize {
        cell_count(buf, |c| {
            matches!(c.fg, ratatui::style::Color::Rgb(r, g, b) if r as i32 > 170 && r as i32 > b as i32 + 40 && (g as i32) < (r as i32))
                || matches!(c.bg, ratatui::style::Color::Rgb(r, g, b) if r as i32 > 170 && r as i32 > b as i32 + 40 && (g as i32) < (r as i32))
        })
    }

    #[test]
    fn garden_renders_type_stems_and_roses() {
        let area = Rect::new(0, 0, 144, 42);
        let mut buf = Buffer::empty(area);
        render_garden(&mut buf, area, &synced_view("grow a garden out of sound"));
        let drawn = cell_count(&buf, |c| c.symbol() != " ");
        assert!(drawn > 60, "garden drew {drawn} cells");
        assert!(blue_cells(&buf) > 8, "stems missing");
        assert!(red_cells(&buf) > 3, "roses missing");
    }

    #[test]
    fn full_text_renders_at_every_size() {
        for (w, h) in [(144u16, 42u16), (100, 30), (72, 22), (40, 12), (24, 8)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            render_garden(&mut buf, area, &synced_view("every beat a budding bloom"));
            assert!(
                cell_count(&buf, |c| c.symbol() != " ") > 30,
                "{w}x{h} rendered almost nothing"
            );
        }
    }

    #[test]
    fn mixed_case_and_punctuation_render() {
        let area = Rect::new(0, 0, 144, 42);
        let mut buf = Buffer::empty(area);
        render_garden(
            &mut buf,
            area,
            &synced_view("Don't Stop \u{2014} Can't, Won't! (q&a) 42?"),
        );
        assert!(cell_count(&buf, |c| c.symbol() != " ") > 50);
    }

    #[test]
    fn non_latin_falls_back_to_plain_text() {
        let area = Rect::new(0, 0, 72, 22);
        let mut buf = Buffer::empty(area);
        render_garden(
            &mut buf,
            area,
            &synced_view("\u{4f60}\u{597d}\u{4e16}\u{754c}"),
        );
        assert!(
            buf.content().iter().any(|c| c.symbol() == "\u{4f60}"),
            "CJK fallback text missing"
        );
    }

    #[test]
    fn frozen_frame_is_deterministic_across_wall_clock() {
        let area = Rect::new(0, 0, 144, 42);
        let mut a = Buffer::empty(area);
        render_garden(&mut a, area, &synced_view("silence is the resting soil"));
        std::thread::sleep(std::time::Duration::from_millis(30));
        let mut b = Buffer::empty(area);
        render_garden(&mut b, area, &synced_view("silence is the resting soil"));
        for (ca, cb) in a.content().iter().zip(b.content().iter()) {
            assert_eq!(ca.symbol(), cb.symbol());
            assert_eq!(ca.fg, cb.fg);
            assert_eq!(ca.bg, cb.bg);
        }
    }

    #[test]
    fn growth_advances_with_line_age() {
        // A young line grows a subset of a mature line's garden: more
        // cells appear as the line ages (regrows on every line change).
        let area = Rect::new(0, 0, 144, 42);
        let text = "every beat a budding bloom";
        let mut young = Buffer::empty(area);
        let mut mature = Buffer::empty(area);
        let mut v = synced_view(text);
        v.since_line_change = 0.05;
        render_garden(&mut young, area, &v);
        v.since_line_change = 4.0;
        render_garden(&mut mature, area, &v);
        assert!(
            cell_count(&mature, |c| c.symbol() != " ") > cell_count(&young, |c| c.symbol() != " "),
            "the garden must grow with the line"
        );
    }

    #[test]
    fn loading_and_unavailable_states_render_honestly() {
        let area = Rect::new(0, 0, 72, 22);
        for (state, expect) in [
            (GardenState::Loading, "looking for lyrics"),
            (GardenState::Unavailable, "no media session"),
            (GardenState::NoMatch, "no matching lyrics"),
        ] {
            let mut buf = Buffer::empty(area);
            render_garden(
                &mut buf,
                area,
                &GardenView {
                    title: "",
                    artist: "",
                    lines: None,
                    plain: None,
                    state,
                    energy: 0.0,
                    time_s: 1.0,
                    since_line_change: 1.0,
                },
            );
            let text: String = buf
                .content()
                .iter()
                .flat_map(|c| c.symbol().chars())
                .collect();
            assert!(text.contains(expect), "expected {expect:?} in {text:?}");
        }
    }
}
