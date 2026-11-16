//! Terminal UI: event loop, layout, status bar and help overlay.

use std::collections::VecDeque;
use std::io::{self, Stdout};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{Frame, Terminal};

use crate::analysis::{FrameAnalysis, SharedState};
use crate::audio::AudioHandle;
use crate::theme::{self, ColorDepth, Theme};
use crate::visualizers;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Bars,
    Scope,
    Gram,
    Combo,
    Mirror,
    Lyrics,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Bars => "Spectrum Analyzer",
            Mode::Scope => "Oscilloscope",
            Mode::Gram => "Spectrogram",
            Mode::Combo => "Bars + Wave",
            Mode::Mirror => "Mirrored Spectrum",
            Mode::Lyrics => "Lyrics Garden",
        }
    }

    fn next(self) -> Self {
        match self {
            Mode::Bars => Mode::Scope,
            Mode::Scope => Mode::Gram,
            Mode::Gram => Mode::Combo,
            Mode::Combo => Mode::Mirror,
            Mode::Mirror => Mode::Lyrics,
            Mode::Lyrics => Mode::Bars,
        }
    }

    fn prev(self) -> Self {
        match self {
            Mode::Bars => Mode::Lyrics,
            Mode::Scope => Mode::Bars,
            Mode::Gram => Mode::Scope,
            Mode::Combo => Mode::Gram,
            Mode::Mirror => Mode::Combo,
            Mode::Lyrics => Mode::Mirror,
        }
    }
}

/// Beat-flash exponential decay rate (fraction e^-kt remaining after t).
const BEAT_FLASH_DECAY_PER_SEC: f32 = 8.0;
/// Spectrogram scroll: one new column this often.
const GRAM_INTERVAL_SECS: f32 = 1.0 / 30.0;
/// Spectrogram history bound in columns; >= any realistic terminal width.
const GRAM_HISTORY: usize = 1024;

pub struct App {
    pub mode: Mode,
    pub help: bool,
    pub quit: bool,
    pub paused: bool,
    pub beat_flash: f32,
    pub sensitivity: f32,
    pub fall: f32,
    pub volume: f32,
    peaks: Vec<f32>,
    gram: VecDeque<Vec<f32>>,
    gram_accum: f32,
    last_frame: Instant,
    scratch: visualizers::RenderScratch,
    themes: Vec<Theme>,
    theme_idx: usize,
    depth: ColorDepth,
    /// Garden bookkeeping: the current lyric line and when it changed
    /// (drives bloom), plus smoothed energy for the sway.
    lyric_line: Option<usize>,
    lyric_changed_at: Instant,
    sway_energy: f32,
}

impl App {
    fn new(
        shared: &Arc<Mutex<SharedState>>,
        audio: &AudioHandle,
        theme_name: &str,
        depth: ColorDepth,
    ) -> Self {
        let state = shared.lock().unwrap_or_else(|e| e.into_inner());
        let themes = theme::all_with_depth(depth);
        let theme_idx = themes
            .iter()
            .position(|t| t.name == theme_name)
            .unwrap_or(0);
        let volume = if audio.is_live() { 0.0 } else { audio.volume() };
        Self {
            // Mirrored spectrum is the default composition: it fills the
            // field symmetrically, which reads best at every size.
            mode: Mode::Mirror,
            help: false,
            quit: false,
            paused: false,
            beat_flash: 0.0,
            sensitivity: state.params.sensitivity,
            fall: state.params.fall,
            volume,
            peaks: Vec::new(),
            gram: VecDeque::with_capacity(GRAM_HISTORY + 16),
            gram_accum: 0.0,
            last_frame: Instant::now(),
            scratch: visualizers::RenderScratch::new(),
            themes,
            theme_idx,
            depth,
            lyric_line: None,
            lyric_changed_at: Instant::now(),
            sway_energy: 0.0,
        }
    }

    fn theme(&self) -> &Theme {
        &self.themes[self.theme_idx]
    }
}

/// Configuration for a single `ui::run` invocation.
pub struct RunConfig<'a> {
    pub shared: &'a Arc<Mutex<SharedState>>,
    pub audio: &'a AudioHandle,
    pub media: &'a crate::media::MediaShared,
    pub lyrics: &'a crate::lyrics::LyricsShared,
    pub theme_name: &'a str,
    pub depth: ColorDepth,
    pub start_mode: Option<u8>,
    pub exit_after: Option<std::time::Duration>,
}

/// Run the UI to completion. The terminal is always restored, even on
/// error or panic (see `install_panic_hook`).
pub fn run(cfg: RunConfig<'_>) -> Result<()> {
    let RunConfig {
        shared,
        audio,
        media,
        lyrics,
        theme_name,
        depth,
        start_mode,
        exit_after,
    } = cfg;
    install_panic_hook();
    enable_raw_mode()?;

    let result = (|| -> Result<()> {
        execute!(io::stdout(), EnterAlternateScreen)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        terminal.hide_cursor()?;

        let mut app = App::new(shared, audio, theme_name, depth);
        if let Some(m) = start_mode {
            app.mode = match m {
                1 => Mode::Bars,
                2 => Mode::Scope,
                3 => Mode::Gram,
                4 => Mode::Combo,
                5 => Mode::Mirror,
                _ => Mode::Lyrics,
            };
        }
        let started = Instant::now();
        let r = event_loop(
            &mut terminal,
            shared,
            audio,
            media,
            lyrics,
            &mut app,
            exit_after.map(|d| started + d),
        );

        let _ = terminal.show_cursor();
        r
    })();

    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    let _ = disable_raw_mode();
    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    shared: &Arc<Mutex<SharedState>>,
    audio: &AudioHandle,
    media: &crate::media::MediaShared,
    lyrics: &crate::lyrics::LyricsShared,
    app: &mut App,
    exit_at: Option<Instant>,
) -> Result<()> {
    loop {
        // Input: wait up to ~14 ms for a key, then drain any backlog so
        // we never lag behind a burst of events.
        if event::poll(Duration::from_millis(14))? {
            while event::poll(Duration::ZERO)? {
                if let Event::Key(key) = event::read()? {
                    on_key(app, shared, audio, key);
                }
            }
        }

        // Scripted QA hook: exit cleanly after the requested duration.
        if let Some(deadline) = exit_at {
            if Instant::now() >= deadline {
                return Ok(());
            }
        }

        // Elapsed time since the previous frame; all per-frame animation
        // decays are expressed per second and scaled by this, so a slow
        // or fast terminal changes speed, not character. Clamped so a
        // stall can't produce a visible jump.
        let now = Instant::now();
        let dt = (now - app.last_frame).as_secs_f32().clamp(0.0, 0.1);
        app.last_frame = now;

        // Pull the freshest analysis; claim pending beats so none are missed.
        let (analysis, beat) = {
            let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
            let beat = state.beats_pending > 0;
            state.beats_pending = 0;
            (Arc::clone(&state.latest), beat)
        };

        if beat {
            app.beat_flash = 1.0;
        } else {
            // Exponential decay: ~85% gone after a quarter second.
            app.beat_flash *= (-BEAT_FLASH_DECAY_PER_SEC * dt).exp();
            if app.beat_flash < 0.01 {
                app.beat_flash = 0.0;
            }
        }

        // Spectrogram history: newest column on the right, pushed at a
        // fixed wall-clock cadence regardless of frame rate.
        if app.mode == Mode::Gram {
            app.gram_accum += dt;
            if app.gram_accum >= GRAM_INTERVAL_SECS {
                app.gram_accum = 0.0;
                app.gram.push_back(analysis.spectrum.clone());
                while app.gram.len() > GRAM_HISTORY {
                    app.gram.pop_front();
                }
            }
        } else {
            app.gram_accum = GRAM_INTERVAL_SECS;
        }

        terminal.draw(|f| draw(f, app, &analysis, audio, media, lyrics, dt))?;

        if app.quit {
            return Ok(());
        }
    }
}

fn on_key(app: &mut App, shared: &Arc<Mutex<SharedState>>, audio: &AudioHandle, key: KeyEvent) {
    // Windows sends Release events too; only react to presses.
    if key.kind != KeyEventKind::Press {
        return;
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('q')) {
        app.quit = true;
        return;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.quit = true,
        KeyCode::Char('1') => set_mode(app, Mode::Bars),
        KeyCode::Char('2') => set_mode(app, Mode::Scope),
        KeyCode::Char('3') => set_mode(app, Mode::Gram),
        KeyCode::Char('4') => set_mode(app, Mode::Combo),
        KeyCode::Char('5') => set_mode(app, Mode::Mirror),
        KeyCode::Char('6') => set_mode(app, Mode::Lyrics),
        KeyCode::Tab => set_mode(app, app.mode.next()),
        KeyCode::BackTab => set_mode(app, app.mode.prev()),
        KeyCode::Char(' ') => {
            app.paused = !app.paused;
            if app.paused {
                audio.pause();
            } else {
                audio.resume();
            }
        }
        KeyCode::Up => {
            if !audio.is_live() {
                app.volume = (app.volume + 0.05).min(1.0);
                audio.set_volume(app.volume);
            }
        }
        KeyCode::Down => {
            if !audio.is_live() {
                app.volume = (app.volume - 0.05).max(0.0);
                audio.set_volume(app.volume);
            }
        }
        KeyCode::Char('+') | KeyCode::Char('=') => adjust_sensitivity(app, shared, 0.1),
        KeyCode::Char('-') | KeyCode::Char('_') => adjust_sensitivity(app, shared, -0.1),
        KeyCode::Char(']') | KeyCode::Char('.') => adjust_fall(app, shared, 0.02),
        KeyCode::Char('[') | KeyCode::Char(',') => adjust_fall(app, shared, -0.02),
        KeyCode::Char('t') => {
            app.theme_idx = (app.theme_idx + 1) % app.themes.len();
        }
        KeyCode::Char('h') | KeyCode::Char('?') => app.help = !app.help,
        _ => {}
    }
}

fn set_mode(app: &mut App, mode: Mode) {
    if app.mode != mode {
        app.mode = mode;
        app.gram.clear();
    }
}

fn adjust_sensitivity(app: &mut App, shared: &Arc<Mutex<SharedState>>, delta: f32) {
    app.sensitivity = (app.sensitivity + delta).clamp(0.1, 4.0);
    shared
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .params
        .sensitivity = app.sensitivity;
}

fn adjust_fall(app: &mut App, shared: &Arc<Mutex<SharedState>>, delta: f32) {
    app.fall = (app.fall + delta).clamp(0.01, 0.95);
    shared.lock().unwrap_or_else(|e| e.into_inner()).params.fall = app.fall;
}

fn draw(
    f: &mut Frame,
    app: &mut App,
    analysis: &FrameAnalysis,
    audio: &AudioHandle,
    media: &crate::media::MediaShared,
    lyrics: &crate::lyrics::LyricsShared,
    dt: f32,
) {
    let area = f.area();
    if area.width < 12 || area.height < 5 {
        return; // too small to render anything useful
    }

    // Copy scalars first, then split-borrow the heavy fields: the theme is
    // referenced (not cloned) while peaks/scratch stay mutable.
    let mode = app.mode;
    let theme_idx = app.theme_idx;
    let beat_flash = app.beat_flash;
    let help = app.help;

    // Quiet chrome: the visualization is the star. The border sits at a
    // low-contrast slate and only flashes toward the theme on beats; the
    // status chip keeps the single accent pop.
    let chrome = Color::Rgb(150, 152, 162);
    let slate_border = Color::Rgb(70, 74, 86);
    let accent = app.themes[theme_idx].accent();
    let beat_color = app.themes[theme_idx].beat();
    let border = theme::lerp_color(slate_border, beat_color, beat_flash);

    let chunks = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(area);
    let main = chunks[0];
    let status_area = chunks[1];

    let status = status_line(app, audio, accent, status_area.width);

    let mut title_right = vec![
        Span::raw(" "),
        Span::styled(mode.name(), Style::new().fg(chrome)),
        Span::raw(" "),
    ];
    if beat_flash > 0.5 {
        title_right.push(Span::styled("\u{266A}", Style::new().fg(beat_color)));
    }

    let mut title_left = vec![Span::styled(" SPECTRUM ", Style::new().fg(chrome))];
    if audio.demo {
        title_left.push(Span::styled(
            " DEMO ",
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    }

    let title_width = 4 + mode.name().chars().count() + if audio.demo { 6 } else { 0 };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(border))
        .title(Line::from(title_left))
        .title(if area.width as usize > title_width + 12 {
            Line::from(title_right).right_aligned()
        } else {
            Line::from("")
        });
    let inner = block.inner(main);
    f.render_widget(block, main);

    let App {
        themes,
        peaks,
        gram,
        scratch,
        ..
    } = app;
    let theme = &themes[theme_idx];

    match mode {
        Mode::Bars => visualizers::render_spectrum(
            f.buffer_mut(),
            inner,
            &analysis.bars,
            theme,
            peaks,
            scratch,
            dt,
        ),
        Mode::Combo => {
            // Integrated composition: mirrored halftone bars fill the
            // field symmetrically (no dead upper half) and the waveform
            // rides the mirror's centerline, so the trace and the bars
            // share a single visual axis instead of floating apart.
            visualizers::render_mirror(f.buffer_mut(), inner, &analysis.bars, theme, scratch);
            let half = inner.height / 2;
            let wave_h = (inner.height / 3).clamp(2, inner.height);
            // Center the band on the mirror's centerline; bounded so the
            // rect always stays inside `inner`.
            let wave_top = half
                .saturating_sub(wave_h / 2)
                .min(inner.height.saturating_sub(wave_h));
            let wave = Rect::new(inner.x, inner.y + wave_top, inner.width, wave_h);
            visualizers::render_oscilloscope(
                f.buffer_mut(),
                wave,
                &analysis.waveform,
                theme,
                1.1,
                scratch,
            );
        }
        Mode::Mirror => {
            visualizers::render_mirror(f.buffer_mut(), inner, &analysis.bars, theme, scratch)
        }
        Mode::Scope => visualizers::render_oscilloscope(
            f.buffer_mut(),
            inner,
            &analysis.waveform,
            theme,
            0.9,
            scratch,
        ),
        Mode::Gram => visualizers::render_spectrogram(f.buffer_mut(), inner, gram, theme),
        Mode::Lyrics => render_lyrics(f.buffer_mut(), inner, app, analysis, media, lyrics, dt),
    }

    f.render_widget(Paragraph::new(status), status_area);

    if help {
        let help_area = centered_rect(46, 15, area);
        f.render_widget(Clear, help_area);
        f.render_widget(
            Paragraph::new(help_text(accent)).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::new().fg(accent))
                    .title(" Help "),
            ),
            help_area,
        );
    }
}

/// Build the status bar for a given width. Sections are ordered by
/// priority; lower-priority ones are dropped whole (never truncated
/// mid-string, so no broken glyphs) when the width can't fit everything.
///
/// Hints are mutually exclusive: the wide hint line is only shown when
/// EVERYTHING fits; otherwise the short "h help \u{b7} q quit" tier takes
/// precedence over the description and the tuning readouts, so help and
/// quit stay discoverable at every width.
fn status_line(app: &App, audio: &AudioHandle, accent: Color, width: u16) -> Line<'static> {
    let dim = Color::Rgb(130, 130, 130);
    let meta = audio.meta_snapshot();

    // The chip is the single honest state; the DEMO banner lives in the
    // window title, so it is not repeated here.
    let state = if meta.status == "reconnecting" || meta.status == "degraded" {
        meta.status
    } else if audio.demo {
        if app.paused {
            "paused"
        } else {
            "demo"
        }
    } else if audio.is_live() {
        if audio.is_paused() {
            "paused"
        } else {
            "capturing"
        }
    } else if audio.is_finished() {
        "ended"
    } else if app.paused {
        "paused"
    } else {
        "playing"
    };
    let icon = match state {
        "playing" | "capturing" | "demo" => "\u{bb}",
        "paused" => "\u{2016}",
        _ => "\u{b7}",
    };

    let warned = meta.status == "reconnecting" || meta.status == "degraded";

    // Core sections, (span, priority): smaller priority survives first.
    let mut base: Vec<(Span<'static>, u8)> = vec![
        (
            Span::styled(
                format!("{icon} {state} "),
                Style::new().fg(accent).add_modifier(Modifier::BOLD),
            ),
            0,
        ),
        (
            Span::styled("h help \u{b7} q quit ", Style::new().fg(dim)),
            1,
        ),
        (
            Span::styled(
                format!("{} ", meta.description),
                Style::new().fg(if warned { Color::Yellow } else { Color::White }),
            ),
            2,
        ),
    ];
    if !audio.is_live() && !audio.demo {
        base.push((
            Span::styled(
                format!("vol {:>3.0}% ", app.volume * 100.0),
                Style::new().fg(dim),
            ),
            3,
        ));
    }
    base.push((
        Span::styled(
            format!("[{}:{}] ", app.theme().name, app.depth.name()),
            Style::new().fg(accent),
        ),
        4,
    ));
    base.push((
        Span::styled(
            format!("sens {:.1} \u{b7} smooth {:.2} ", app.sensitivity, app.fall),
            Style::new().fg(dim),
        ),
        5,
    ));

    let wide = Span::styled(
        "1-5 modes \u{b7} space pause \u{b7} t theme \u{b7} h help \u{b7} q quit",
        Style::new().fg(dim),
    );

    // Pass 1: replace the short hint tier with the wide hint line; if it
    // fits entirely, use it.
    let wide_pass = {
        let mut sections: Vec<(Span<'static>, u8)> =
            base.iter().filter(|(_, p)| *p != 1).cloned().collect();
        sections.push((wide, 6));
        fit_sections(sections, width)
    };
    if !wide_pass.1 {
        return Line::from(wide_pass.0);
    }

    // Pass 2: short hints at high priority; anything low-priority drops.
    let short_pass = fit_sections(base, width);
    let mut spans = short_pass.0;
    if short_pass.1 {
        let ellipsis = Span::styled("\u{2026}", Style::new().fg(dim));
        let used: usize = spans.iter().map(|s| s.width()).sum();
        if used + ellipsis.width() <= width as usize {
            spans.push(ellipsis);
        }
    }
    Line::from(spans)
}

/// Fit sections by priority into `width`, returning the kept spans and
/// whether anything was dropped. Spans are kept or dropped whole.
fn fit_sections(mut sections: Vec<(Span<'static>, u8)>, width: u16) -> (Vec<Span<'static>>, bool) {
    sections.sort_by_key(|(_, p)| *p);
    let mut spans = Vec::new();
    let mut used = 0usize;
    let mut dropped = false;
    for (span, _) in sections {
        let w = span.width();
        if used + w <= width as usize {
            spans.push(span);
            used += w;
        } else {
            dropped = true;
        }
    }
    (spans, dropped)
}

fn help_text(accent: Color) -> Vec<Line<'static>> {
    let key = Style::new().fg(Color::White).add_modifier(Modifier::BOLD);
    let desc = Style::new().fg(Color::Rgb(170, 170, 170));
    let mk = |k: &str, d: &str| {
        Line::from(vec![
            Span::styled(format!("  {k:<14}"), key),
            Span::styled(d.to_string(), desc),
        ])
    };
    let mut lines = vec![Line::from(Span::styled(
        "  SPECTRUM \u{2014} keybindings",
        Style::new().fg(accent).add_modifier(Modifier::BOLD),
    ))];
    lines.push(Line::from(""));
    lines.extend([
        mk(
            "1 / 2 / 3 / 4 / 5 / 6",
            "spectrum \u{b7} scope \u{b7} spectrogram \u{b7} combo \u{b7} mirror (default) \u{b7} lyrics garden",
        ),
        mk("Tab / Shift+Tab", "cycle visualizations"),
        mk("Space", "pause / resume audio"),
        mk("\u{2191} / \u{2193}", "volume (file mode)"),
        mk("+ / -", "sensitivity"),
        mk("[ / ]", "bar smoothing (fall speed)"),
        mk("t", "cycle color theme"),
        mk("h / ?", "toggle this help"),
        mk("q / Esc", "quit"),
    ]);
    lines
}

fn centered_rect(w: u16, h: u16, area: Rect) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

/// Never leave the terminal broken after a panic.
fn render_lyrics(
    buf: &mut ratatui::buffer::Buffer,
    area: ratatui::layout::Rect,
    app: &mut App,
    analysis: &FrameAnalysis,
    media: &crate::media::MediaShared,
    lyrics: &crate::lyrics::LyricsShared,
    dt: f32,
) {
    let snap = media.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let state = lyrics.lock().unwrap_or_else(|e| e.into_inner()).clone();

    // Smoothed energy drives the sway (quiet passages stay still).
    let energy_now: f32 = analysis.bars.iter().sum::<f32>() / analysis.bars.len().max(1) as f32;
    app.sway_energy += (energy_now - app.sway_energy) * (1.0 - (-8.0 * dt).exp());

    // Map the lyrics state into the garden view.
    use crate::garden::GardenState;
    use crate::lyrics::LyricsStatus;

    let (garden_state, lines, plain, current) = match &state.status {
        LyricsStatus::Unavailable => (GardenState::Unavailable, None, None, None),
        LyricsStatus::Loading => (GardenState::Loading, None, None, None),
        LyricsStatus::Synced(lines) => (GardenState::Synced, Some(lines.clone()), None, {
            crate::lrc::line_at(lines, snap.position_s)
        }),
        LyricsStatus::Plain(text) => (GardenState::Plain, None, Some(text.clone()), None),
        LyricsStatus::Instrumental => (GardenState::Instrumental, None, None, None),
        LyricsStatus::NoMatch => (GardenState::NoMatch, None, None, None),
        LyricsStatus::Error(e) => (GardenState::Error(e.clone()), None, None, None),
    };

    // Track line changes for the bloom animation.
    if current != app.lyric_line {
        app.lyric_line = current;
        app.lyric_changed_at = Instant::now();
    }
    let since = app.lyric_changed_at.elapsed().as_secs_f32();

    let lines_ref = lines.as_deref();
    let view = crate::garden::GardenView {
        title: &snap.title,
        artist: &snap.artist,
        lines: lines_ref.map(|l| (l, current)),
        plain: plain.as_deref(),
        state: garden_state,
        energy: app.sway_energy.clamp(0.0, 1.0),
        time_s: snap.position_s as f32,
        since_line_change: since,
    };
    crate::garden::render_garden(buf, area, &view);
}

fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original(panic_info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo;
    use crate::theme::{self, ColorDepth};
    use crate::visualizers::{self, RenderScratch};
    use ratatui::buffer::Buffer;

    fn demo_app() -> App {
        let shared = Arc::new(Mutex::new(SharedState {
            latest: Arc::new(FrameAnalysis::quiet(64)),
            params: crate::analysis::Params {
                sensitivity: 1.0,
                fall: 0.12,
            },
            beats_pending: 0,
        }));
        App::new(
            &shared,
            &crate::audio::AudioHandle::for_demo(),
            "spectrum",
            ColorDepth::True,
        )
    }

    fn span_text(line: &Line<'_>) -> String {
        line.spans.iter().flat_map(|s| s.content.chars()).collect()
    }

    #[test]
    fn status_fits_every_width() {
        let app = demo_app();
        let audio = crate::audio::AudioHandle::for_demo();
        for w in [20u16, 24, 30, 40, 56, 60, 72, 80, 100, 120, 144, 200] {
            let line = status_line(&app, &audio, Color::White, w);
            let width: usize = line.spans.iter().map(|s| s.width()).sum();
            assert!(
                width <= w as usize,
                "status width {width} exceeds {w}: {}",
                span_text(&line)
            );
        }
    }

    #[test]
    fn status_hints_never_duplicated() {
        // Regression: at wide widths both the wide and short hint lines
        // used to render ("... q quit h help \u{b7} q quit").
        let app = demo_app();
        let audio = crate::audio::AudioHandle::for_demo();
        for w in [24u16, 30, 40, 56, 60, 72, 80, 100, 120, 144, 160, 200] {
            let text = span_text(&status_line(&app, &audio, Color::White, w));
            assert_eq!(
                text.matches("q quit").count(),
                1,
                "hint duplication at width {w}: {text}"
            );
        }
    }

    #[test]
    fn status_hint_priority_over_tuning_and_device() {
        let app = demo_app();
        let audio = crate::audio::AudioHandle::for_demo();

        // Wide: the full hint line replaces the short one, everything fits.
        let wide = status_line(&app, &audio, Color::White, 144);
        let text = span_text(&wide);
        assert!(text.contains("demo"), "chip missing: {text}");
        assert!(text.contains("synthetic"), "source missing: {text}");
        assert!(text.contains("1-5 modes"), "wide hints missing: {text}");
        assert!(!text.contains("h help \u{b7} q quit h"), "dup: {text}");
        assert_eq!(text.matches("DEMO").count(), 0, "duplicated DEMO: {text}");

        // Medium (72 cols, the reviewer's case): description and tuning
        // give way, but help/quit must survive.
        let medium = status_line(&app, &audio, Color::White, 72);
        let mtext = span_text(&medium);
        assert!(mtext.contains("demo"), "chip must survive: {mtext}");
        assert!(
            mtext.contains("h help"),
            "help/quit discoverability must outrank tuning at 72 cols: {mtext}"
        );
        assert_eq!(mtext.matches("q quit").count(), 1, "dup at 72: {mtext}");

        // Narrow: tuning drops before the hints; the chip always stays.
        let narrow = status_line(&app, &audio, Color::White, 46);
        let ntext = span_text(&narrow);
        assert!(ntext.contains("demo"), "chip must survive: {ntext}");
        assert!(
            ntext.contains("h help") || ntext.contains('\u{2026}'),
            "short hints or ellipsis expected: {ntext}"
        );
        assert!(
            !ntext.contains("1-5 modes"),
            "wide hints must drop: {ntext}"
        );
    }

    #[test]
    fn demo_modes_render_at_matched_phase() {
        // Loud (drop) phase, one buffer per mode at the reviewer's size.
        let theme = theme::find_with_depth("spectrum", ColorDepth::True).unwrap();
        let frame = demo::synthetic_frame(5.0, 64);
        let area = Rect::new(0, 0, 144, 40);

        let mut scratch = RenderScratch::default();
        let mut peaks = Vec::new();
        let mut bars_buf = Buffer::empty(area);
        visualizers::render_spectrum(
            &mut bars_buf,
            area,
            &frame.bars,
            &theme,
            &mut peaks,
            &mut scratch,
            1.0 / 60.0,
        );
        let bars_filled = nonblank(&bars_buf);
        assert!(bars_filled > 300, "bars mode too sparse: {bars_filled}");

        // Oscilloscope has a waveform during the loud phase.
        let mut scope_buf = Buffer::empty(area);
        visualizers::render_oscilloscope(
            &mut scope_buf,
            area,
            &frame.waveform,
            &theme,
            1.1,
            &mut scratch,
        );
        assert!(nonblank(&scope_buf) > 20, "scope too sparse");

        // Mirror renders around the center line.
        let mut mirror_buf = Buffer::empty(area);
        visualizers::render_mirror(&mut mirror_buf, area, &frame.bars, &theme, &mut scratch);
        assert!(nonblank(&mirror_buf) > 300, "mirror too sparse");

        // Spectrogram: push a few columns of history at the same phase.
        let mut cols = std::collections::VecDeque::new();
        for k in 0..30u32 {
            let f = demo::synthetic_frame(5.0 - k as f32 * 0.033, 64);
            cols.push_back(f.spectrum);
        }
        let mut gram_buf = Buffer::empty(area);
        visualizers::render_spectrogram(&mut gram_buf, area, &cols, &theme);
        assert!(nonblank_bg(&gram_buf) > 200, "spectrogram too sparse");
    }

    #[test]
    fn demo_silence_and_fade_render_quietly() {
        let theme = theme::find_with_depth("spectrum", ColorDepth::True).unwrap();
        let mut scratch = RenderScratch::default();
        let mut peaks = Vec::new();
        let area = Rect::new(0, 0, 80, 24);

        // Silence: an intentionally near-empty field (dynamics preserved,
        // nothing normalized away).
        let silent = demo::synthetic_frame(7.9, 64);
        assert!(silent.bars.iter().all(|v| *v <= 0.01));
        let mut buf = Buffer::empty(area);
        visualizers::render_spectrum(
            &mut buf,
            area,
            &silent.bars,
            &theme,
            &mut peaks,
            &mut scratch,
            1.0 / 60.0,
        );
        assert!(nonblank(&buf) <= 2, "silence should render almost empty");

        // Fade: clearly less filled than the loud drop.
        let fade = demo::synthetic_frame(6.8, 64);
        let mut fade_buf = Buffer::empty(area);
        visualizers::render_spectrum(
            &mut fade_buf,
            area,
            &fade.bars,
            &theme,
            &mut peaks,
            &mut scratch,
            1.0 / 60.0,
        );
        let loud = demo::synthetic_frame(5.0, 64);
        let mut loud_buf = Buffer::empty(area);
        visualizers::render_spectrum(
            &mut loud_buf,
            area,
            &loud.bars,
            &theme,
            &mut peaks,
            &mut scratch,
            1.0 / 60.0,
        );
        assert!(
            nonblank(&fade_buf) < nonblank(&loud_buf),
            "fade ({}) should be quieter than loud ({})",
            nonblank(&fade_buf),
            nonblank(&loud_buf)
        );
    }

    #[test]
    fn truecolor_dither_enriches_but_palette_stays_coherent() {
        // Same frame, same size: truecolor dithering must produce more
        // distinct colors than the 256-color rendering, while both use
        // their own coherent (non-random) palettes.
        let frame = demo::synthetic_frame(5.0, 64);
        let area = Rect::new(0, 0, 100, 30);

        let render = |theme: &crate::theme::Theme| -> usize {
            let mut scratch = RenderScratch::default();
            let mut peaks = Vec::new();
            let mut buf = Buffer::empty(area);
            visualizers::render_spectrum(
                &mut buf,
                area,
                &frame.bars,
                theme,
                &mut peaks,
                &mut scratch,
                1.0 / 60.0,
            );
            distinct_fg(&buf)
        };

        let true_theme = theme::find_with_depth("fire", ColorDepth::True).unwrap();
        let p256_theme = theme::find_with_depth("fire", ColorDepth::P256).unwrap();
        let n_true = render(&true_theme);
        let n_256 = render(&p256_theme);
        assert!(n_true >= 2, "truecolor dither must be visible: {n_true}");
        assert!(
            n_true >= n_256,
            "truecolor ({n_true}) should use at least as many colors as 256 ({n_256})"
        );
    }

    #[test]
    fn default_mode_is_mirror() {
        let app = demo_app();
        assert_eq!(app.mode, Mode::Mirror);
    }

    #[test]
    fn combo_wave_rides_the_mirror_centerline() {
        // The combo composition: mirrored bars plus a waveform band centered
        // on the mirror's centerline (integrated axis, no dead upper half).
        let theme = theme::find_with_depth("spectrum", ColorDepth::True).unwrap();
        let frame = demo::synthetic_frame(5.0, 64);
        let area = Rect::new(0, 0, 72, 22);
        let mut buf = Buffer::empty(area);
        let mut scratch = RenderScratch::default();

        // Mirror half of the field: the wave band center must match it.
        crate::visualizers::render_mirror(&mut buf, area, &frame.bars, &theme, &mut scratch);
        let half = area.height / 2;

        let wave_h = (area.height / 3).clamp(2, area.height);
        let wave_top = half
            .saturating_sub(wave_h / 2)
            .min(area.height.saturating_sub(wave_h));
        let wave = Rect::new(area.x, area.y + wave_top, area.width, wave_h);
        crate::visualizers::render_oscilloscope(
            &mut buf,
            wave,
            &frame.waveform,
            &theme,
            1.1,
            &mut scratch,
        );
        // The band straddles the centerline.
        assert!(
            wave_top <= half && wave_top + wave_h >= half,
            "wave band must straddle the centerline (top {wave_top}, h {wave_h}, half {half})"
        );
        // Braille trace present inside the band.
        let braille = (wave.x..wave.x + wave.width)
            .flat_map(|x| (wave.y..wave.y + wave.height).map(move |y| (x, y)))
            .filter(|(x, y)| {
                buf[(*x, *y)]
                    .symbol()
                    .chars()
                    .next()
                    .is_some_and(|c| ('\u{2800}'..='\u{28ff}').contains(&c))
            })
            .count();
        assert!(braille > 0, "combo must render the waveform");
        // No peak-cap markers floating in the composition.
        let caps = (0..area.width)
            .flat_map(|x| (0..area.height).map(move |y| (x, y)))
            .filter(|(x, y)| buf[(*x, *y)].symbol() == "\u{2594}")
            .count();
        assert_eq!(caps, 0, "combo must not carry detached peak caps");
    }

    #[test]
    fn all_modes_render_across_terminal_sizes() {
        // Every mode, loud + silence, across the reviewer's size ladder.
        let theme = theme::find_with_depth("spectrum", ColorDepth::True).unwrap();
        let loud = demo::synthetic_frame(5.0, 64);
        let silent = demo::synthetic_frame(7.9, 64);
        let sizes = [
            (144u16, 42u16),
            (100, 30),
            (72, 22),
            (40, 12),
            (20, 8),
            (300, 50),
        ];
        for (w, h) in sizes {
            let area = Rect::new(0, 0, w, h);
            let mut scratch = RenderScratch::default();
            let mut peaks = Vec::new();

            // Mirror (default), bars, scope.
            let mut buf = Buffer::empty(area);
            visualizers::render_mirror(&mut buf, area, &loud.bars, &theme, &mut scratch);
            assert!(nonblank(&buf) > 0, "mirror {w}x{h} empty at loud");
            let mut buf = Buffer::empty(area);
            visualizers::render_spectrum(
                &mut buf,
                area,
                &loud.bars,
                &theme,
                &mut peaks,
                &mut scratch,
                1.0 / 60.0,
            );
            assert!(nonblank(&buf) > 0, "bars {w}x{h} empty at loud");
            let mut buf = Buffer::empty(area);
            visualizers::render_oscilloscope(
                &mut buf,
                area,
                &loud.waveform,
                &theme,
                1.0,
                &mut scratch,
            );
            assert!(nonblank(&buf) > 0, "scope {w}x{h} empty at loud");

            // Silence is intentionally quiet everywhere.
            let mut buf = Buffer::empty(area);
            visualizers::render_mirror(&mut buf, area, &silent.bars, &theme, &mut scratch);
            assert!(
                nonblank(&buf) <= (w as usize),
                "mirror {w}x{h} not quiet at silence"
            );
        }
    }

    fn nonblank(buf: &Buffer) -> usize {
        (0..buf.area.width * buf.area.height)
            .filter(|i| buf.content[*i as usize].symbol() != " ")
            .count()
    }

    fn nonblank_bg(buf: &Buffer) -> usize {
        (0..buf.area.width * buf.area.height)
            .filter(|i| buf.content[*i as usize].bg != ratatui::style::Color::Reset)
            .count()
    }

    fn distinct_fg(buf: &Buffer) -> usize {
        let mut set = std::collections::HashSet::new();
        for i in 0..(buf.area.width * buf.area.height) {
            set.insert(buf.content[i as usize].fg);
        }
        set.len()
    }
}
