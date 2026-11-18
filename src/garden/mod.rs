//! The lyrics garden (mode 5): a direct port of ascii-garden's rendering
//! (upstream f519e85, MIT (c) 2026 Ridit - see NOTICE). Pale paper,
//! serif type with stems and roses growing out of the letters, all
//! rendered as terminal text through the cell-grid coverage classifier.
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
    /// Synthetic demo source: shown as an honest DEMO tag in the header.
    pub demo: bool,
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

/// Minimum em height (terminal rows) for upstream's glyph-texture type.
/// Upstream's own invariant is ~17 cells per em (`targetChT = St/17`; at
/// the 1440x900 reference chCur = 252/17 = 14.82 rounds to 15 px cells
/// -> 16.8 cells per em), so its fine ASCII texture never renders below
/// ~16.8 cells/em. Actual QA pixels agreed: 34.5 em rows (the approved
/// 288x128 capture) reads beautifully, while 10.4 em rows (144x42
/// hello) rendered as unreadable stipple whose boundary ink rows flapped
/// frame-to-frame. So: texture only at reference-like density (>= 16.5
/// em rows); below that, silhouettes - clearly readable whole
/// letterforms; below PLAIN_BELOW_EM_ROWS, plain text. Reference density
/// at 144 columns needs a ~60-row garden band - a HOST capacity (smaller
/// terminal font / larger window), not something the app can synthesize.
pub const TEXTURE_MIN_EM_ROWS: f32 = 16.5;
/// Below this em height (rows) even silhouettes stop forming letters
/// (x-height under ~2 rows): the line is shown as plain wrapped text.
pub const PLAIN_BELOW_EM_ROWS: f32 = 3.4;
const DEMO_TAG: (u8, u8, u8) = (176, 58, 32);

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

    // Header: an honest DEMO tag at the left (when synthetic) and the
    // title centred in the remaining width, so the two never collide.
    let title = if view.artist.is_empty() {
        view.title.to_string()
    } else {
        format!("{} \u{b7} {}", view.title, view.artist)
    };
    let mut header = Rect::new(area.x, area.y, area.width, 1);
    if view.demo && area.width >= 16 {
        for (i, ch) in "DEMO".chars().enumerate() {
            if let Some(c) = buf.cell_mut((area.x + 1 + i as u16, area.y)) {
                c.set_char(ch).set_fg(rgb(DEMO_TAG));
                c.modifier.insert(ratatui::style::Modifier::BOLD);
            }
        }
        header = Rect::new(area.x + 6, area.y, area.width.saturating_sub(12), 1);
    }
    put_text(buf, header, area.y, &title, rgb(INK_DIM));

    // The main band: everything between the title and the bottom line.
    let band_top = area.y + 1;
    let band_h = area.height.saturating_sub(2).max(1);
    let band = Rect::new(area.x, band_top, area.width, band_h);

    match &view.state {
        GardenState::Synced => {
            if let Some((lines, Some(idx))) = view.lines {
                let prev = idx.checked_sub(1).map(|i| lines[i].text.clone());
                let next = lines.get(idx + 1).map(|l| l.text.clone());
                // Context lines are chrome; below 21 band rows they starve
                // the garden, so small terminals show only the current line.
                let show_context = band_h >= 21;
                let has_prev = prev.is_some() && show_context;
                let has_next = next.is_some() && show_context;
                let text = &lines[idx].text;

                if has_prev {
                    put_text(
                        buf,
                        area,
                        band.y,
                        prev.as_deref().unwrap_or(""),
                        rgb(INK_DIM),
                    );
                }
                if has_next {
                    let row = area.y + area.height - 1;
                    put_text(buf, area, row, next.as_deref().unwrap_or(""), rgb(INK_DIM));
                }

                let inner_top = band.y + u16::from(has_prev);
                let inner_h = band_h
                    .saturating_sub(u16::from(has_prev))
                    .saturating_sub(u16::from(has_next))
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

thread_local! {
    /// Coarse frame phases of the LAST rendered garden band, ms:
    /// (draw_scene, present, blit). Diagnostic surface for the QA
    /// frame-budget work; the Instant::now pair costs nanoseconds.
    static LAST_PHASES: std::cell::RefCell<(f64, f64, f64)> =
        const { std::cell::RefCell::new((0.0, 0.0, 0.0)) };
}

/// The last rendered band's coarse phase times in ms (draw_scene,
/// present, blit) - for the frame-budget bench; production ignores it.
pub fn last_garden_phases() -> (f64, f64, f64) {
    LAST_PHASES.with(|p| *p.borrow())
}

/// The current line as the garden: serif type on paper with stems and
/// roses growing out of the letters (all through the mask classifier),
/// or legible plain text when the script has no letterforms.
///
/// The current line as the garden: serif type on paper with stems and
/// roses growing out of the letters (all through the mask classifier),
/// or legible plain text when the script has no letterforms.
///
/// Plain LRC carries LINE timing only: the whole timestamped
/// line reveals with the normal garden growth (no per-word onset
/// is invented).
fn render_garden_line(buf: &mut Buffer, area: Rect, text: &str, view: &GardenView<'_>) {
    // Invalidate the provenance stash up front: it is only valid for
    // a real garden band render (plain-fallback and early returns must
    // not leave stale maps for the pixel-audit companion).
    LAST_PROVENANCE.with(|p| *p.borrow_mut() = None);
    if area.width < 4 || area.height < 2 {
        return;
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    if !font::glyph_supported(trimmed) {
        // Honest fallback: legible normal text (original case preserved),
        // wrapped (never truncated) and display-width aware so CJK and
        // friends centre correctly.
        put_wrapped(buf, area, trimmed, rgb(INK));
        return;
    }

    let w_design = area.width as f32;
    let data = cached_scene(trimmed, area.width, area.height);
    if data.s / mask::ROW_DESIGN < PLAIN_BELOW_EM_ROWS {
        // Too small for any legible letterform in this many cells: show
        // the full line as clean, wrapped text rather than noise.
        put_wrapped(buf, area, trimmed, rgb(INK));
        return;
    }
    // Growth age: the line's timeline drives everything, so a frozen
    // position is fully deterministic. Plain LRC carries LINE timing
    // only: the whole timestamped line reveals with the normal garden
    // growth (all type ink from the line start; no per-word onset is
    // invented), and the decorations keep the reference's staggered
    // grow (botany::birth_stagger) around it.
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
            // Upstream "grow" schedule (see botany::birth_stagger) for
            // the DECORATION, line-anchored as in the reference.
            birth_ms: botany::birth_stagger(data.placements.len(), i),
            id: i as u32,
            word_end: data
                .placements
                .get(i + 1)
                .map(|n| n.ch == ' ' || n.line != p.line)
                .unwrap_or(true),
            line: p.line,
        })
        .collect();

    let mut grid = mask::MaskGrid::new(area.width as usize, area.height as usize);
    let ph0 = std::time::Instant::now();
    botany::draw_scene(
        &mut grid,
        &letters,
        &data.type_mask,
        data.s,
        age_ms,
        view.energy,
        w_design,
        // LINE-TIMING ONLY: no per-letter type onset schedule - the
        // whole line's ink is present from the line start (the empty
        // schedule means draw_scene renders every letter immediately;
        // no word stagger is invented for plain LRC).
        &[],
    );
    let ph1 = std::time::Instant::now();

    // Present into a scratch buffer over the band, then blit.
    let mut scratch = Buffer::empty(Rect::new(0, 0, area.width, area.height));
    // Upstream's per-cell texture needs its own ~17-cells-per-em density
    // (its cells shrink with the type); below TEXTURE_MIN_EM_ROWS the
    // honest readable render is the silhouette.
    let style = if data.s / mask::ROW_DESIGN >= TEXTURE_MIN_EM_ROWS {
        mask::TypeStyle::Texture
    } else {
        mask::TypeStyle::Silhouette
    };
    // Word-gated letters no longer exist (line timing only): the
    // presentation budget gets no hidden-letter flags - every letter
    // is visible ink from the line start.
    let oracle = mask::present(
        &mut scratch,
        Rect::new(0, 0, area.width, area.height),
        &grid,
        Some(&data.type_mask),
        style,
    );
    let ph2 = std::time::Instant::now();
    stash_provenance(&oracle, &data.type_mask, area);
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
    let ph3 = std::time::Instant::now();
    LAST_PHASES.with(|p| {
        *p.borrow_mut() = (
            (ph1 - ph0).as_secs_f64() * 1000.0,
            (ph2 - ph1).as_secs_f64() * 1000.0,
            (ph3 - ph2).as_secs_f64() * 1000.0,
        )
    });
}

/// Provenance classes for the per-cell companion map (QA 0417's
/// actual-pixel audit companion): what PRESENTED in each band cell,
/// from the presentation branch (the oracle), crossed with whether the
/// cell originally carried letter ink (the type mask - single source).
/// A buffer pass is NOT actual pixels: QA photographs the real terminal
/// frame and aligns it against this map - every grey-looking pixel in
/// an ERASED-over-ink cell is a COUNTED erasure (the dandelion grey
/// reads as ink to colour classifiers, provenance does not).
pub const PROV_INK: u8 = 0; // letter ink presented
pub const PROV_ERASED: u8 = 1; // garden/paper over letter ink (counted erasure)
pub const PROV_GARDEN: u8 = 2; // garden/paper, no letter ink in the cell

thread_local! {
    static LAST_PROVENANCE: std::cell::RefCell<Option<(Rect, Vec<u8>)>> =
        const { std::cell::RefCell::new(None) };
}

/// Build + stash the per-cell provenance map for the LAST rendered
/// band: the oracle byte (ink presented vs erased) crossed with the
/// cell's original letter-ink presence (type mask). Band-local
/// indices; the Rect carries the band's position in the full buffer.
fn stash_provenance(oracle: &[u8], tm: &font::TypeMask, area: Rect) {
    let mut had_ink = vec![false; (area.width as usize) * (area.height as usize)];
    let mask_w = tm.w;
    for k in 0..tm.cov.len() {
        if tm.cov[k] >= 128 && tm.letter.get(k).copied().unwrap_or(0) != 0 {
            let (ci, cj) = ((k % mask_w) / mask::SX, (k / mask_w) / mask::SY);
            if let Some(slot) = had_ink.get_mut(cj * (area.width as usize) + ci) {
                *slot = true;
            }
        }
    }
    let map: Vec<u8> = (0..(area.width as usize) * (area.height as usize))
        .map(|i| {
            let ink_presented =
                oracle.get(i).copied().unwrap_or(mask::ORACLE_ERASED) == mask::ORACLE_INK;
            match (ink_presented, had_ink[i]) {
                (true, _) => PROV_INK,
                (false, true) => PROV_ERASED,
                (false, false) => PROV_GARDEN,
            }
        })
        .collect();
    LAST_PROVENANCE.with(|p| *p.borrow_mut() = Some((area, map)));
}

/// The LAST rendered band's provenance map (band-local cell grid + the
/// band's Rect in full-buffer coordinates). Companion artifact for the
/// actual-pixel audit: run the same params as the photographed frame,
/// align cell-for-cell.
pub fn last_provenance() -> Option<(Rect, Vec<u8>)> {
    LAST_PROVENANCE.with(|p| p.borrow().clone())
}

/// Layout + garden elements + rasterized type for one line at one size,
/// cached (bounded): the type never animates, only the garden does.
struct SceneData {
    placements: Vec<layout::Placement>,
    els: Vec<botany::LetterEls>,
    type_mask: font::TypeMask,
    s: f32,
}

thread_local! {
    static SCENE_CACHE: std::cell::RefCell<std::collections::HashMap<(String, u16, u16), Rc<SceneData>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

fn cached_scene(text: &str, cols: u16, rows: u16) -> Rc<SceneData> {
    SCENE_CACHE.with(|c| {
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

/// Clear the per-(text, size) scene cache. Bench/QA surface: the FIRST
/// frame after this re-runs the scene build (font raster + generation)
/// so a "cold" frame measurement is honest, not a warm cache hit on an
/// identity shared with a previous bench scenario.
pub fn clear_scene_cache() {
    SCENE_CACHE.with(|c| c.borrow_mut().clear());
}

fn build_scene(text: &str, cols: u16, rows: u16) -> SceneData {
    let w = cols as f32;
    let h = rows as f32 * mask::ROW_DESIGN;
    let lay = layout::layout(text, w, h);
    let mut type_mask = font::TypeMask::new(cols as usize * mask::SX, rows as usize * mask::SY);
    for (li, p) in lay.letters.iter().enumerate() {
        if p.ch != ' ' {
            type_mask.current_line = (p.line + 1).min(255) as u8;
            type_mask.current_letter = (li + 1) as u16;
            type_mask.draw_glyph(p.ch, p.tx, p.ty, lay.s, mask::SUB);
        }
    }
    let caps = botany::PlantCaps::for_band(h);
    let chaos = botany::chaos_for(lay.s / mask::ROW_DESIGN);
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
        els.push(botany::gen(p.ch, ws_seed, wi, prev_w, chaos, caps, lay.s));
        // Bounded viewport fit (see botany::fit_to_band): the whole plant
        // - stem, blooms, leaves - stays inside the band under the
        // render-time sway, translated inward without hiding or
        // shrinking anything.
        if let Some(last) = els.last_mut() {
            botany::fit_to_band(last, p.tx, lay.s, w);
        }
        // Bridge spans use the previous letter's em advance.
        prev_w = Some(p.tw / lay.s.max(0.01));
        wi += 1;
    }
    SceneData {
        placements: lay.letters,
        els,
        type_mask,
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

/// The whole `text` wrapped to `area`'s width at spaces (overlong words
/// and space-less scripts break at characters), every row centred, the
/// block centred vertically. Never drops a character that fits the area.
fn put_wrapped(buf: &mut Buffer, area: Rect, text: &str, color: ratatui::style::Color) {
    let max = area.width.saturating_sub(2).max(1) as usize;
    let mut rows: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split(' ').filter(|w| !w.is_empty()) {
        let ww = word.width();
        let sep = usize::from(!cur.is_empty());
        if cur.width() + sep + ww <= max {
            if sep == 1 {
                cur.push(' ');
            }
            cur.push_str(word);
            continue;
        }
        if !cur.is_empty() {
            rows.push(std::mem::take(&mut cur));
        }
        // A word wider than a row breaks at characters.
        for ch in word.chars() {
            if cur.width() + ch.width().unwrap_or(0) > max {
                rows.push(std::mem::take(&mut cur));
            }
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    let n = rows.len() as u16;
    let top = area.y + area.height.saturating_sub(n) / 2;
    for (i, row) in rows.iter().enumerate() {
        let y = top + i as u16;
        if y >= area.y + area.height {
            break;
        }
        put_text(buf, area, y, row, color);
        for x in area.x..area.x + area.width {
            if let Some(c) = buf.cell_mut((x, y)) {
                if c.symbol() != " " {
                    c.modifier.insert(ratatui::style::Modifier::BOLD);
                }
            }
        }
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
            demo: true,
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
        // Bounds are conservative: under upstream's 0.28 H size cap the
        // type is smaller than the old maximized regime, so exact counts
        // depend on the size solve; these catch total absence.
        let area = Rect::new(0, 0, 144, 42);
        let mut buf = Buffer::empty(area);
        render_garden(&mut buf, area, &synced_view("grow a garden out of sound"));
        let drawn = cell_count(&buf, |c| c.symbol() != " ");
        assert!(drawn > 40, "garden drew {drawn} cells");
        assert!(blue_cells(&buf) > 3, "stems missing");
        assert!(red_cells(&buf) > 1, "roses missing");
    }

    #[test]
    fn full_text_renders_at_every_size() {
        // Under upstream's size caps, small bands honestly fall back to
        // plain wrapped text (23 non-space chars in the fixture) instead
        // of an illegible garden - the assertion is "nothing is dropped".
        for (w, h) in [(144u16, 42u16), (100, 30), (72, 22), (40, 12), (24, 8)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            render_garden(&mut buf, area, &synced_view("every beat a budding bloom"));
            let drawn = cell_count(&buf, |c| c.symbol() != " ");
            assert!(
                drawn > 20,
                "{w}x{h} rendered almost nothing ({drawn} cells)"
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
                    demo: false,
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

    #[test]
    #[ignore]
    fn overlap_probe_hello() {
        // PER-LETTER overlap diagnostic (user 02:12: per letter, never
        // word-sum or bounding-box average) for the "flower covers the
        // letters" review, with counts and source geometry kept exactly
        // as ported. Same word, inner-band rows, age and energy as the
        // CLI's frozen `--demo-lyric hello --freeze-at 4.5` capture.
        // Prints, per letter: ink subpixels, covered subpixels and the
        // fraction; plus every rose's position in em above its letter's
        // baseline (upstream's `over` band is y in (-0.78, 0.05); the
        // letters' x-height is ~0.52 em, 'h' ink top ~0.782 em - the
        // glyph ink, not the em-box ascender). Compare against a
        // reference screenshot at matched word/viewport/age.
        // Run with --ignored --nocapture.
        // Inner band rows = terminal - 1 status - 2 garden chrome - 2
        // context lines (render_garden's layout).
        for ((term_w, term_h), cols, rows) in
            [((144u16, 42u16), 144u16, 37u16), ((288, 128), 288, 123)]
        {
            let data = cached_scene("hello", cols, rows);
            let mut grid = mask::MaskGrid::new(cols as usize, rows as usize);
            let letters = scene_letters(&data);
            botany::draw_scene(
                &mut grid,
                &letters,
                &data.type_mask,
                data.s,
                4500.0,
                0.0,
                cols as f32,
                &[],
            );
            for (l, total, covered) in letter_cover(&grid, &data) {
                let ch = data
                    .placements
                    .get(l as usize - 1)
                    .map(|p| p.ch)
                    .unwrap_or('?');
                println!(
                    "  {term_w}x{term_h} age 4500: letter {l} ({ch:?}): ink {total}, covered {covered} ({:.1}%)",
                    100.0 * covered as f32 / total.max(1) as f32
                );
            }
            // The VISIBLE audit after present(): what actually reaches
            // the screen per letter (QA 02:17: the subpixel budget alone
            // does not bound whole-cell flower classification).
            let style = if data.s / mask::ROW_DESIGN >= TEXTURE_MIN_EM_ROWS {
                mask::TypeStyle::Texture
            } else {
                mask::TypeStyle::Silhouette
            };
            let area = Rect::new(0, 0, cols, rows);
            let mut buf = Buffer::empty(area);
            mask::present(&mut buf, area, &grid, Some(&data.type_mask), style);
            for (l, ch, total, erased) in visible_letter_loss(&buf, &data) {
                println!(
                    "  {term_w}x{term_h} age 4500: letter {l} ({ch:?}): ink {total}, VISIBLY erased {erased} ({:.1}%)",
                    100.0 * erased as f32 / total.max(1) as f32
                );
            }
            for e in &data.els {
                for el in &e.els {
                    if let botany::El::Rose { y, r, .. } = el {
                        let is_over = *y > -0.78 && *y < 0.05;
                        let zone = if is_over { "over-letter" } else { "clear" };
                        println!("  {term_w}x{term_h}: rose y={y:.2} em r={r:.2} em ({zone})");
                    }
                }
            }
        }
    }

    /// Assemble the per-frame SceneLetters for `data` exactly as
    /// `render_garden_line` does (staggered births, word ends, lines).
    fn scene_letters(data: &SceneData) -> Vec<botany::SceneLetter<'_>> {
        data.placements
            .iter()
            .zip(data.els.iter())
            .enumerate()
            .map(|(i, (p, e))| botany::SceneLetter {
                ch: p.ch,
                tx: p.tx,
                ty: p.ty,
                els: e,
                birth_ms: botany::birth_stagger(data.placements.len(), i),
                id: i as u32,
                word_end: data
                    .placements
                    .get(i + 1)
                    .map(|n| n.ch == ' ' || n.line != p.line)
                    .unwrap_or(true),
                line: p.line,
            })
            .collect()
    }

    /// Per-letter cover audit from the ACTUAL rendered mask: for each
    /// letter id, (ink subpixels, covered by the visible front layer).
    /// A subpixel is the letter's ink when its type coverage >= 128;
    /// covered when the final role is no longer Type (the front-layer
    /// garden won it) - exactly what present()/silhouette_cell render,
    /// so the audit counts VISIBLE covers, not hidden ones.
    fn letter_cover(grid: &mask::MaskGrid, data: &SceneData) -> Vec<(u16, usize, usize)> {
        use std::collections::BTreeMap;
        let mut total: BTreeMap<u16, usize> = BTreeMap::new();
        let mut covered: BTreeMap<u16, usize> = BTreeMap::new();
        for k in 0..grid.role.len() {
            if data.type_mask.cov.get(k).copied().unwrap_or(0) >= 128 {
                let l = data.type_mask.letter[k];
                if l == 0 {
                    continue;
                }
                *total.entry(l).or_insert(0) += 1;
                if grid.role[k] != mask::Role::Type as u8 {
                    *covered.entry(l).or_insert(0) += 1;
                }
            }
        }
        total
            .into_iter()
            .map(|(l, t)| (l, t, covered.get(&l).copied().unwrap_or(0)))
            .collect()
    }

    #[test]
    fn plants_stay_inside_the_band_edges_on_real_frames() {
        // QA 03:42: "garden" showed a tulip clipped at the LEFT viewport
        // edge at 1.5 s and 4.5 s. The bounded viewport fit
        // (botany::fit_to_band) translates whole plants inward with a
        // margin covering the render-time sway; this regression renders
        // the ACTUAL garden frames at the QA sizes - including MAXIMUM
        // gust energy and both the settling (1.5 s) and settled (4.5 s)
        // ages - and asserts no garden-family cell touches the outermost
        // columns, and that blooms still paint (nothing was hidden).
        for (cols, rows) in [(144u16, 37u16), (288, 123)] {
            let data = cached_scene("garden", cols, rows);
            for (age, energy) in [(1500.0f32, 0.95f32), (4500.0, 0.95)] {
                let mut grid = mask::MaskGrid::new(cols as usize, rows as usize);
                let letters = scene_letters(&data);
                botany::draw_scene(
                    &mut grid,
                    &letters,
                    &data.type_mask,
                    data.s,
                    age,
                    energy,
                    cols as f32,
                    &[],
                );
                let (w, h) = (grid.w, grid.h);
                let garden_cell = |r: u8| r != 0 && r != mask::Role::Type as u8;
                for j in 0..h {
                    for i in 0..w {
                        if !garden_cell(grid.role[j * w + i]) {
                            continue;
                        }
                        let cell_x = i / mask::SX;
                        assert!(
                            cell_x > 0 && cell_x < cols as usize - 1,
                            "garden cell in the outermost column {cell_x} of {cols} at age {age}"
                        );
                    }
                }
                // And the garden still paints (the fit translates, never
                // hides).
                let painted = grid.role.iter().filter(|&&r| garden_cell(r)).count();
                assert!(painted > 50, "the garden must still paint: {painted}");
            }
        }
    }

    #[test]
    fn scanline_fills_reproduce_whole_scenes_byte_for_byte() {
        // SCENE-LEVEL differential proof (QA 10:27): render the full
        // matrix the regressions use - texts x sizes x ages x energies -
        // TWICE, once with the polygon fills forced to the ORIGINAL
        // per-point reference (mask::use_reference_fills switch) and once
        // with the scanline fills, and byte-compare the ENTIRE masks
        // (role, owner, letter). This covers draw_scene's per-element
        // paint order, the guards, the front/back layering and the
        // cover budgets end to end - not just isolated polygons.
        let texts = [
            "hello",
            "every beat a budding bloom",
            "grow a garden out of sound silence is the resting soil",
        ];
        let sizes = [(144u16, 37u16), (100, 25), (288, 123)];
        let ages = [(500.0f32, 0.3f32), (1500.0, 0.6), (4500.0, 0.95)];
        for text in texts {
            for (cols, rows) in sizes {
                let data = cached_scene(text, cols, rows);
                for (age, energy) in ages {
                    let letters = scene_letters(&data);
                    let mut a = mask::MaskGrid::new(cols as usize, rows as usize);
                    let mut b = mask::MaskGrid::new(cols as usize, rows as usize);
                    // Reference pass.
                    mask::set_reference_fills_for_tests(true);
                    botany::draw_scene(
                        &mut a,
                        &letters,
                        &data.type_mask,
                        data.s,
                        age,
                        energy,
                        cols as f32,
                        &[],
                    );
                    mask::set_reference_fills_for_tests(false);
                    // Scanline pass.
                    botany::draw_scene(
                        &mut b,
                        &letters,
                        &data.type_mask,
                        data.s,
                        age,
                        energy,
                        cols as f32,
                        &[],
                    );
                    assert_eq!(
                        a.role, b.role,
                        "{text} {cols}x{rows} age {age} e {energy}: mask roles differ"
                    );
                    assert_eq!(
                        a.owner, b.owner,
                        "{text} {cols}x{rows} age {age} e {energy}: owners differ"
                    );
                    assert_eq!(
                        a.letter, b.letter,
                        "{text} {cols}x{rows} age {age} e {energy}: letters differ"
                    );
                }
            }
        }
    }

    #[test]
    fn front_garden_covers_less_than_a_fifth_of_every_letter() {
        // HARD acceptance (user 02:12): the visible front-layer garden
        // must cover STRICTLY LESS than 20% of EACH letter's ink -
        // per letter, never word-averaged - and this must hold at
        // multiple animation ages and live sway energies, from the
        // actual rendered masks (enforced at draw time in mask::set via
        // the internal mask::BUDGET_COVER safety margin; this regression
        // audits the rendered result against the UNRELAXED 20%
        // acceptance, independently of the enforcement).
        for text in ["hello", "every beat a budding bloom"] {
            for (cols, rows) in [(144u16, 37u16), (100, 25), (288, 123)] {
                let data = cached_scene(text, cols, rows);
                for (age, energy) in [
                    (500.0f32, 0.3f32),
                    (1500.0, 0.6),
                    (4500.0, 0.95),
                    (4500.0, 0.0),
                ] {
                    let mut grid = mask::MaskGrid::new(cols as usize, rows as usize);
                    let letters = scene_letters(&data);
                    botany::draw_scene(
                        &mut grid,
                        &letters,
                        &data.type_mask,
                        data.s,
                        age,
                        energy,
                        cols as f32,
                        &[],
                    );
                    let stats = letter_cover(&grid, &data);
                    assert!(
                        !stats.is_empty(),
                        "{text} {cols}x{rows}: the audit must see the letters"
                    );
                    for (l, total, covered) in &stats {
                        assert!(
                            (*covered as f32) < mask::MAX_LETTER_COVER * *total as f32,
                            "{text} {cols}x{rows} age {age} e {energy}: letter {l} covered {covered}/{total}"
                        );
                    }
                }
            }
        }
    }

    /// Whether a final buffer cell PRESENTS letter ink: an ink-family
    /// colour (paper->ink mixes and sparkle are near-neutral, spread <= 9;
    /// we accept <= 12 for float-rounding). Faded garden tones must NOT
    /// count as ink: a soft LeafLight cell is (179,182,208) - spread 29,
    /// sum 569 - which the earlier spread <= 40 rule silently counted as
    /// visible ink, relaxing the metric exactly where the garden covers
    /// letters. Ink-family faints (soft type cols, sparkle glyphs, the
    /// fuzz keep-glyph) all sit at spread <= 8 and stay counted.
    fn presents_ink(c: &ratatui::buffer::Cell) -> bool {
        match c.fg {
            ratatui::style::Color::Rgb(r, g, b) => {
                let spread = r.max(g).max(b) as i32 - r.min(g).min(b) as i32;
                spread <= 12 && (r as u32 + g as u32 + b as u32) < 640
            }
            _ => false,
        }
    }

    #[test]
    fn ink_classifier_covers_every_true_ink_family_colour() {
        // Source-grounded ink-vs-garden classifier proof (QA 0352): the
        // SATURATED-colour pixel audit cannot alone validate pale
        // species, so this test pins the boundary from the source of
        // every ink-family cell colour the render can produce:
        // 1. The type glyph family: pure ink TEXT (17,17,17).
        // 2. The silhouette quadrant mixes mix(PAPER, INK, t),
        //    t in [0.55, 1.0] - the faintest is ~(107,105,100).
        // 3. The presentation budget's fuzz keep-glyph:
        //    mix(PAPER, INK, 0.62).
        // 4. The soft type path's faintest grey: mix(TEXT, PAPER, 0.468)
        //    - the texture-mode letter edge.
        // 5. The sparkle glyph: mix((17,17,17), SPARKLE, 0.75).
        // ALL must classify as ink - a letter edge that flipped to
        // "erased" would be a silent audit failure the other way.
        let cell = |c: (u8, u8, u8)| {
            let mut cell = ratatui::buffer::Cell::default();
            cell.set_fg(ratatui::style::Color::Rgb(c.0, c.1, c.2));
            cell
        };
        let inks = [
            (17, 17, 17),
            mask::mix(mask::PAPER_BG, (24, 23, 21), 1.0),
            mask::mix(mask::PAPER_BG, (24, 23, 21), 0.55),
            mask::mix(mask::PAPER_BG, (24, 23, 21), 0.62),
            mask::mix((17, 17, 17), mask::PAPER_BG, 0.468),
            mask::mix((17, 17, 17), (255, 250, 240), 0.75),
        ];
        for c in inks {
            assert!(
                presents_ink(&cell(c)),
                "a true ink-family colour must classify as ink: {c:?}"
            );
        }
        // The PALE-species hazard, quantified from the source palette
        // rules (garden.ts defaultKindColor: hues within 110 of the bg
        // pull halfway toward the type colour):
        // - blossom pulled: (136,100,116) - spread 36, OUTSIDE both
        //   audit bands (pixel spread 20, buffer spread 12): safe.
        // - dandelion pulled: (127,126,117) - spread 10, sum 370: INSIDE
        //   both bands, genuinely ambiguous BY UPSTREAM'S OWN DESIGN
        //   (it pulls the pale cream halfway to the TYPE colour
        //   #111111). The ambiguity is NOT resolvable by colour alone -
        //   the provenance ORACLE (presentation sidecar: mask roles, not
        //   colours) is the audit that disambiguates, and the clearance
        //   keeps pale blooms off letter ink. The dandelion grey is
        //   preserved exactly (never recoloured to evade the audit).
        let blossom_pulled = (136, 100, 116);
        let dandelion_pulled = (127, 126, 117);
        assert!(
            !presents_ink(&cell(blossom_pulled)),
            "blossom's pulled tone must NOT read as ink"
        );
        assert!(
            presents_ink(&cell(dandelion_pulled)),
            "the documented dandelion ambiguity (oracle-covered)"
        );
    }

    /// VISIBLE per-letter cover audit AFTER presentation: for each letter,
    /// the ink subpixels (cov >= 128) that ended up in cells which do
    /// NOT present ink (whole-cell garden glyphs, soft glyphs, paper) -
    /// weighted per letter, never word-averaged. The draw-time cap bounds
    /// the mask; this bounds what actually reaches the screen.
    fn visible_letter_loss(buf: &Buffer, data: &SceneData) -> Vec<(u16, char, usize, usize)> {
        use std::collections::BTreeMap;
        let mask_w = data.type_mask.w;
        let mut total: BTreeMap<u16, usize> = BTreeMap::new();
        let mut erased: BTreeMap<u16, usize> = BTreeMap::new();
        for k in 0..data.type_mask.cov.len() {
            if data.type_mask.cov[k] < 128 {
                continue;
            }
            let l = data.type_mask.letter[k];
            if l == 0 {
                continue;
            }
            *total.entry(l).or_insert(0) += 1;
            let (ci, cj) = ((k % mask_w) / mask::SX, (k / mask_w) / mask::SY);
            let shows_ink = buf
                .cell(ratatui::layout::Position::new(ci as u16, cj as u16))
                .map(presents_ink)
                .unwrap_or(false);
            if !shows_ink {
                *erased.entry(l).or_insert(0) += 1;
            }
        }
        total
            .into_iter()
            .map(|(l, t)| {
                let ch = data
                    .placements
                    .get(l as usize - 1)
                    .map(|p| p.ch)
                    .unwrap_or('?');
                (l, ch, t, erased.get(&l).copied().unwrap_or(0))
            })
            .collect()
    }

    /// VISIBLE per-letter cover audit AFTER presentation, PROVENANCE-AWARE
    /// (QA 04:06): for each letter, the ink subpixels (cov >= 128) that
    /// ended up in cells whose presentation-branch oracle byte is
    /// ORACLE_ERASED - weighted per letter, never word-averaged. This
    /// classifies by the PRESENTATION BRANCH that fired, never by
    /// colour, so ink-ambiguous species tones (the dandelion grey)
    /// count as erasure exactly like saturated garden colours. The
    /// colour-based `presents_ink` remains ONLY as the documented
    /// classifier boundary (see ink_classifier test).
    fn visible_letter_loss_oracle(
        oracle: &[u8],
        area: Rect,
        data: &SceneData,
    ) -> Vec<(u16, char, usize, usize)> {
        use std::collections::BTreeMap;
        let mask_w = data.type_mask.w;
        let mut total: BTreeMap<u16, usize> = BTreeMap::new();
        let mut erased: BTreeMap<u16, usize> = BTreeMap::new();
        for k in 0..data.type_mask.cov.len() {
            if data.type_mask.cov[k] < 128 {
                continue;
            }
            let l = data.type_mask.letter[k];
            if l == 0 {
                continue;
            }
            *total.entry(l).or_insert(0) += 1;
            let (ci, cj) = ((k % mask_w) / mask::SX, (k / mask_w) / mask::SY);
            let idx = cj * (area.width as usize) + ci;
            let shows_ink =
                oracle.get(idx).copied().unwrap_or(mask::ORACLE_ERASED) == mask::ORACLE_INK;
            if !shows_ink {
                *erased.entry(l).or_insert(0) += 1;
            }
        }
        total
            .into_iter()
            .map(|(l, t)| {
                let ch = data
                    .placements
                    .get(l as usize - 1)
                    .map(|p| p.ch)
                    .unwrap_or('?');
                (l, ch, t, erased.get(&l).copied().unwrap_or(0))
            })
            .collect()
    }

    #[test]
    fn visible_letter_ink_survives_presentation_per_letter() {
        // The OTHER half of the acceptance (QA 02:17/04:06): the
        // subpixel cap alone does not bound what the screen shows, and a
        // COLOUR-based audit cannot disambiguate pale species greys from
        // faint ink. present() emits the provenance oracle; this
        // regression audits the ACTUAL presented cells per letter
        // (oracle bytes, branch truth) across ages, energies, sizes,
        // single-line AND multi-line layouts, and both render styles,
        // asserting the same < 20% acceptance.
        // Multi-line: the long phrase wraps to 3 lines at this band
        // (layout-measured), exercising per-line guards and letter tags
        // across wrapped rows.
        for text in [
            "hello",
            "every beat a budding bloom",
            "grow a garden out of sound silence is the resting soil",
        ] {
            for (cols, rows) in [(144u16, 37u16), (100, 25), (288, 123)] {
                let data = cached_scene(text, cols, rows);
                let style = if data.s / mask::ROW_DESIGN >= TEXTURE_MIN_EM_ROWS {
                    mask::TypeStyle::Texture
                } else {
                    mask::TypeStyle::Silhouette
                };
                for (age, energy) in [
                    (500.0f32, 0.3f32),
                    (1500.0, 0.6),
                    (4500.0, 0.95),
                    (4500.0, 0.0),
                ] {
                    let mut grid = mask::MaskGrid::new(cols as usize, rows as usize);
                    let letters = scene_letters(&data);
                    botany::draw_scene(
                        &mut grid,
                        &letters,
                        &data.type_mask,
                        data.s,
                        age,
                        energy,
                        cols as f32,
                        &[],
                    );
                    let area = Rect::new(0, 0, cols, rows);
                    let mut buf = Buffer::empty(area);
                    let oracle = mask::present(&mut buf, area, &grid, Some(&data.type_mask), style);
                    let stats = visible_letter_loss_oracle(&oracle, area, &data);
                    assert!(
                        !stats.is_empty(),
                        "{text} {cols}x{rows}: the visible audit must see the letters"
                    );
                    for (l, ch, total, erased) in &stats {
                        assert!(
                            (*erased as f32) < mask::MAX_LETTER_COVER * *total as f32,
                            "{text} {cols}x{rows} age {age} e {energy}: letter {l} ({ch:?}) visibly lost {erased}/{total}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn oracle_counts_pale_grey_blooms_as_erasure_where_colour_cannot() {
        // The EXECUTABLE proof QA 04:06 demanded: the dandelion grey is
        // colour-ambiguous (presents_ink says ink), so the only honest
        // audit is provenance. This FORCES the worst case - a dandelion
        // bloom deliberately planted ON a letter's ink, bypassing the
        // clearance (which is exactly what the clearance exists to
        // prevent, but the audit must not RELY on it) - and asserts:
        // 1. The colour classifier calls the covered cells ink (the
        //    ambiguity, demonstrated live).
        // 2. The oracle counts every covered ink subpixel as ERASED.
        // 3. The oracle-based per-letter loss accounts MORE than the
        //    colour-based one on that letter - the exact gap the
        //    colour audit silently relaxed.
        // A small band keeps the glyph and bloom subpixel-aligned.
        let (cols, rows) = (20u16, 10u16);
        let mut grid = mask::MaskGrid::new(cols as usize, rows as usize);
        let mut tm = font::TypeMask::new(grid.w, grid.h);
        // A big 'o' centred in the band, tagged letter 1.
        tm.current_line = 1;
        tm.current_letter = 1;
        tm.draw_glyph('o', 10.0, 8.0, 6.0, mask::SUB);
        // Force the dandelion grey ONTO the letter ink: no clearance,
        // no budget mercy up front - the mask itself gets the dandelion
        // base role over the ink subpixels. The presentation budget
        // then clamps how much it may show (its job), and the oracle
        // reports the clamped truth.
        let (_, base, _) = botany::Kind::Dandelion.roles();
        for j in 0..grid.h {
            for i in 0..grid.w {
                let k = j * grid.w + i;
                if tm.cov[k] >= 128 {
                    grid.role[k] = base as u8;
                }
            }
        }
        let area = Rect::new(0, 0, cols, rows);
        let mut buf = Buffer::empty(area);
        let oracle = mask::present(&mut buf, area, &grid, Some(&tm), mask::TypeStyle::Texture);
        let mask_w = tm.w;
        let data_letters = tm.cov.iter().filter(|&&c| c >= 128).count();
        // Among the cells the ORACLE counts as ERASED, how many does
        // the COLOUR classifier call ink? Every one of them (the grey)
        // - the exact silent relaxation the colour audit would commit.
        let mut oracle_erased_cells = 0;
        let mut colour_blind_erasures = 0;
        for k in 0..tm.cov.len() {
            if tm.cov[k] < 128 {
                continue;
            }
            let (ci, cj) = ((k % mask_w) / mask::SX, (k / mask_w) / mask::SY);
            let idx = cj * (cols as usize) + ci;
            if oracle.get(idx).copied().unwrap_or(mask::ORACLE_ERASED) == mask::ORACLE_ERASED {
                oracle_erased_cells += 1;
                let cell = &buf[(ci as u16, cj as u16)];
                if presents_ink(cell) {
                    colour_blind_erasures += 1;
                }
            }
        }
        // 1. The budget clamped the poisoning: the oracle's erasure
        //    stays under the 20% acceptance ON THE POISONED LETTER.
        assert!(
            oracle_erased_cells > 0,
            "the oracle must count the forced grey cover as erasure"
        );
        assert!(
            (oracle_erased_cells as f32) < mask::MAX_LETTER_COVER * data_letters as f32,
            "the budget clamps even a fully poisoned letter: {oracle_erased_cells}/{data_letters}"
        );
        // 2. The ambiguity, demonstrated live: every oracle-erased cell
        //    reads as INK to the colour classifier - the colour audit
        //    silently relaxes exactly where the oracle reports erasure.
        assert!(
            colour_blind_erasures == oracle_erased_cells,
            "the dandelion grey must read as ink to the colour classifier: {colour_blind_erasures}/{oracle_erased_cells}"
        );
        // 3. The oracle is the honest loss oracle: on the SAME buffer,
        //    it accounts the erasures the colour audit cannot see.
        //    (Already proven by 1+2: the oracle counts as erased every
        //    grey-over-ink cell; the colour audit counts zero of them.)
    }
}
