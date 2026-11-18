//! Terminal UI: event loop, layout, status bar and help overlay.

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
    Radial,
    Mirror,
    Lyrics,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Bars => "Spectrum",
            Mode::Scope => "Oscilloscope",
            Mode::Radial => "Radial",
            Mode::Mirror => "Mirror",
            Mode::Lyrics => "Lyrics Garden",
        }
    }

    fn next(self) -> Self {
        match self {
            Mode::Bars => Mode::Scope,
            Mode::Scope => Mode::Radial,
            Mode::Radial => Mode::Mirror,
            Mode::Mirror => Mode::Lyrics,
            Mode::Lyrics => Mode::Bars,
        }
    }

    fn prev(self) -> Self {
        match self {
            Mode::Bars => Mode::Lyrics,
            Mode::Scope => Mode::Bars,
            Mode::Radial => Mode::Scope,
            Mode::Mirror => Mode::Radial,
            Mode::Lyrics => Mode::Mirror,
        }
    }

    fn from_number(m: u8) -> Self {
        match m {
            1 => Mode::Bars,
            2 => Mode::Scope,
            3 => Mode::Radial,
            4 => Mode::Mirror,
            _ => Mode::Lyrics,
        }
    }
}

/// Display smoothing time constants (seconds): a fast attack keeps hits
/// crisp, a slower release lets bars settle without jitter. Applied with
/// the real frame time, so motion is frame-rate independent.
const ATTACK_TAU: f32 = 0.022;
const RELEASE_TAU: f32 = 0.14;

/// Beat-flash exponential decay rate (fraction e^-kt remaining after t).
const BEAT_FLASH_DECAY_PER_SEC: f32 = 8.0;

pub struct App {
    pub mode: Mode,
    pub help: bool,
    pub quit: bool,
    pub paused: bool,
    pub beat_flash: f32,
    pub sensitivity: f32,
    pub fall: f32,
    pub volume: f32,
    peaks: visualizers::Peaks,
    /// Attack/release-smoothed bars for the bar-based modes.
    display: Vec<f32>,
    /// Smoothed spectral flux (radial ring breathing).
    flux: f32,
    last_frame: Instant,
    scratch: visualizers::RenderScratch,
    themes: Vec<Theme>,
    theme_idx: usize,
    depth: ColorDepth,
    /// Garden bookkeeping: smoothed energy for the sway. Bloom age and
    /// line identity derive from the media position itself, so frozen
    /// demos render deterministically.
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
            peaks: visualizers::Peaks::new(),
            display: Vec::new(),
            flux: 0.0,
            last_frame: Instant::now(),
            scratch: visualizers::RenderScratch::new(),
            themes,
            theme_idx,
            depth,
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
            app.mode = Mode::from_number(m);
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

        tick_app(app, &analysis, beat, dt);
        terminal.draw(|f| draw(f, app, &analysis, audio, media, lyrics, dt))?;

        if app.quit {
            return Ok(());
        }
    }
}

/// Per-frame state shared by the live loop and headless snapshots: beat
/// flash decay.
fn tick_app(app: &mut App, analysis: &FrameAnalysis, beat: bool, dt: f32) {
    // Attack/release display smoothing.
    if app.display.len() != analysis.bars.len() {
        app.display = analysis.bars.clone();
    }
    let ka = 1.0 - (-dt / ATTACK_TAU).exp();
    let kr = 1.0 - (-dt / RELEASE_TAU).exp();
    for (d, &v) in app.display.iter_mut().zip(analysis.bars.iter()) {
        let k = if v > *d { ka } else { kr };
        *d += (v - *d) * k;
    }
    app.flux += (analysis.flux - app.flux) * (1.0 - (-dt / 0.12).exp());

    if beat {
        app.beat_flash = 1.0;
    } else {
        // Exponential decay: ~85% gone after a quarter second.
        app.beat_flash *= (-BEAT_FLASH_DECAY_PER_SEC * dt).exp();
        if app.beat_flash < 0.01 {
            app.beat_flash = 0.0;
        }
    }
}

/// What a headless snapshot renders: the real `draw` path driven by the
/// deterministic demo generator, with no terminal or audio hardware.
pub struct Snapshot<'a> {
    pub mode: u8,
    pub theme: &'a str,
    pub depth: ColorDepth,
    /// Demo cycle time (seconds) of the final frame.
    pub at: f32,
    /// `true`: the timeline is frozen at `at` (what `--freeze-at` shows);
    /// `false`: the demo plays into `at` for `warmup` seconds.
    pub frozen: bool,
    pub warmup: f32,
    pub lyric: Option<&'a str>,
    pub width: u16,
    pub height: u16,
}

/// Render one frame of the real UI into a buffer (headless QA and tests).
pub fn snapshot(s: &Snapshot<'_>) -> ratatui::buffer::Buffer {
    snapshot_timed(s).0
}

/// [`Snapshot`] plus the measured wall time of EVERY rendered frame
/// (analysis + tick + draw), in order: frame 0 is the COLD frame (scene
/// build + font raster + first draw), the rest are warm frames of the
/// cached scene. Diagnostic surface for the QA frame-budget work -
/// production paths never call this.
pub fn snapshot_timed(s: &Snapshot<'_>) -> (ratatui::buffer::Buffer, Vec<std::time::Duration>) {
    use ratatui::backend::TestBackend;
    let bands = crate::analysis::DEFAULT_BANDS;
    let shared = Arc::new(Mutex::new(SharedState {
        latest: Arc::new(crate::demo::synthetic_frame(s.at, bands)),
        params: crate::analysis::Params {
            sensitivity: 1.0,
            fall: 0.12,
        },
        beats_pending: 0,
    }));
    let audio = AudioHandle::for_demo();
    let media = crate::media::new_shared();
    let lyrics = crate::lyrics::new_shared();
    crate::demo::seed_demo_state(&media, &lyrics, s.at as f64, s.frozen, s.lyric);
    let mut app = App::new(&shared, &audio, s.theme, s.depth);
    app.mode = Mode::from_number(s.mode);
    let mut terminal = Terminal::new(TestBackend::new(s.width, s.height)).expect("test backend");
    let dt = 1.0 / 60.0;
    let frames = (s.warmup.max(0.0) / dt).round() as usize + 1;
    let mut times = Vec::with_capacity(frames);
    for k in 0..frames {
        let t = if s.frozen {
            s.at
        } else {
            s.at - (frames - 1 - k) as f32 * dt
        };
        let f0 = std::time::Instant::now();
        let analysis = crate::demo::synthetic_frame(t, bands);
        if !s.frozen {
            let mut m = media.lock().unwrap_or_else(|e| e.into_inner());
            m.position_s = t.max(0.0) as f64;
        }
        let beat = analysis.beat && !s.frozen;
        tick_app(&mut app, &analysis, beat, dt);
        terminal
            .draw(|f| draw(f, &mut app, &analysis, &audio, &media, &lyrics, dt))
            .expect("draw");
        times.push(f0.elapsed());
    }
    (terminal.backend().buffer().clone(), times)
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
        KeyCode::Char('3') => set_mode(app, Mode::Radial),
        KeyCode::Char('4') => set_mode(app, Mode::Mirror),
        KeyCode::Char('5') => set_mode(app, Mode::Lyrics),
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
    app.mode = mode;
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
    let accent = app.themes[theme_idx].accent();
    let lyrics_mode = mode == Mode::Lyrics;

    // Layout: the visualization owns the field. Bar modes get a quiet
    // one-row header on the theme's canvas; the garden is full-bleed paper
    // like upstream's canvas. Both end in a single status row.
    let chunks = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(area);
    let (main, status_area) = (chunks[0], chunks[1]);
    let status = if lyrics_mode {
        status_line_with(app, audio, &StatusPalette::paper(), status_area.width)
    } else {
        let canvas = visualizers::canvas(&app.themes[theme_idx]);
        status_line_with(
            app,
            audio,
            &StatusPalette::canvas(accent, &app.themes[theme_idx], canvas),
            status_area.width,
        )
    };

    let inner = if lyrics_mode {
        main
    } else {
        let theme = &app.themes[theme_idx];
        let canvas = visualizers::canvas(theme);
        visualizers::paint_canvas(f.buffer_mut(), area, theme);
        let header = Rect::new(main.x, main.y, main.width, 1);
        draw_header(
            f.buffer_mut(),
            header,
            mode,
            audio.demo,
            beat_flash,
            theme,
            canvas,
        );
        Rect::new(
            main.x,
            main.y + 1,
            main.width,
            main.height.saturating_sub(1),
        )
    };

    let App {
        themes,
        peaks,
        scratch,
        display,
        flux,
        ..
    } = app;
    let theme = &themes[theme_idx];
    let bars: &[f32] = if display.is_empty() {
        &analysis.bars
    } else {
        display
    };

    match mode {
        Mode::Bars => {
            visualizers::render_spectrum(f.buffer_mut(), inner, bars, theme, peaks, scratch, dt)
        }
        Mode::Radial => visualizers::render_radial(
            f.buffer_mut(),
            inner,
            bars,
            &analysis.waveform,
            *flux,
            theme,
            scratch,
        ),
        Mode::Mirror => visualizers::render_mirror(f.buffer_mut(), inner, bars, theme, scratch),
        Mode::Scope => visualizers::render_oscilloscope(
            f.buffer_mut(),
            inner,
            &analysis.waveform,
            theme,
            0.9,
            scratch,
            dt,
        ),
        Mode::Lyrics => render_lyrics(
            f.buffer_mut(),
            inner,
            app,
            analysis,
            media,
            lyrics,
            dt,
            audio.demo,
            audio.playback_pos_secs(),
        ),
    }

    let status_bg = if lyrics_mode {
        Color::Rgb(243, 238, 228)
    } else {
        visualizers::canvas(&app.themes[theme_idx])
    };
    {
        let buf = f.buffer_mut();
        for x in status_area.x..status_area.x + status_area.width {
            if let Some(c) = buf.cell_mut((x, status_area.y)) {
                c.set_char(' ');
                c.set_bg(status_bg);
            }
        }
    }
    f.render_widget(
        Paragraph::new(status).style(Style::new().bg(status_bg)),
        status_area,
    );
    let depth = app.depth;

    if help {
        // A quiet card: dark panel, rounded hairline border in the accent.
        let help_area = centered_rect(64, 15, area);
        let panel = Color::Rgb(16, 17, 24);
        f.render_widget(Clear, help_area);
        f.render_widget(
            Paragraph::new(help_text(accent))
                .style(Style::new().bg(panel))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(ratatui::widgets::BorderType::Rounded)
                        .border_style(Style::new().fg(accent).bg(panel))
                        .title(" keys "),
                ),
            help_area,
        );
    }

    // Every colour on screen follows the selected depth.
    theme::quantize_buffer(f.buffer_mut(), depth);
}

/// Build the status bar for a given width. Sections are ordered by
/// priority; lower-priority ones are dropped whole (never truncated
/// mid-string, so no broken glyphs) when the width can't fit everything.
///
/// Hints are mutually exclusive: the wide hint line is only shown when
/// EVERYTHING fits; otherwise the short "h help \u{b7} q quit" tier takes
/// precedence over the description and the tuning readouts, so help and
/// quit stay discoverable at every width.
/// The quiet header for the bar modes: wordmark, mode, an honest DEMO
/// tag when the source is synthetic, and a beat dot that glows and fades
/// with the (time-based) beat flash.
fn draw_header(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    mode: Mode,
    demo: bool,
    beat_flash: f32,
    theme: &Theme,
    canvas: Color,
) {
    if area.width < 8 || area.height == 0 {
        return;
    }
    let dim = theme::lerp_color(canvas, Color::Rgb(150, 154, 168), 0.75);
    let faint = theme::lerp_color(canvas, Color::Rgb(150, 154, 168), 0.45);
    let mut x = area.x + 1;
    let end = area.x + area.width;
    let mut put = |s: &str, fg: Color, bold: bool, x: &mut u16| {
        for ch in s.chars() {
            if *x >= end {
                return;
            }
            if let Some(c) = buf.cell_mut((*x, area.y)) {
                c.set_char(ch).set_fg(fg).set_bg(canvas);
                if bold {
                    c.set_style(Style::new().add_modifier(Modifier::BOLD));
                }
            }
            *x += 1;
        }
    };
    put("SPECTRUM", dim, true, &mut x);
    put("  ", faint, false, &mut x);
    put(mode.name(), faint, false, &mut x);
    if demo {
        put("  ", faint, false, &mut x);
        put("DEMO", Color::Rgb(214, 170, 60), true, &mut x);
    }
    // Beat dot at the right edge.
    if area.width >= 24 {
        let bx = end - 2;
        let glow = theme::lerp_color(faint, theme.beat(), beat_flash.clamp(0.0, 1.0));
        if let Some(c) = buf.cell_mut((bx, area.y)) {
            c.set_char('\u{25CF}').set_fg(glow).set_bg(canvas);
        }
    }
}

/// Colours for the status row (dark chrome, or the garden's paper).
pub struct StatusPalette {
    pub accent: Color,
    pub dim: Color,
    pub text: Color,
    pub warn: Color,
}

impl StatusPalette {
    #[cfg(test)]
    fn dark(accent: Color) -> Self {
        Self {
            accent,
            dim: Color::Rgb(130, 130, 130),
            text: Color::White,
            warn: Color::Yellow,
        }
    }

    /// Quiet tones on the theme canvas.
    fn canvas(accent: Color, _theme: &Theme, canvas: Color) -> Self {
        Self {
            accent,
            dim: theme::lerp_color(canvas, Color::Rgb(150, 154, 168), 0.6),
            text: theme::lerp_color(canvas, Color::Rgb(205, 208, 218), 0.9),
            warn: Color::Rgb(226, 178, 60),
        }
    }

    fn paper() -> Self {
        Self {
            accent: Color::Rgb(28, 43, 143),
            dim: Color::Rgb(146, 138, 124),
            text: Color::Rgb(74, 70, 64),
            warn: Color::Rgb(176, 98, 18),
        }
    }
}

#[cfg(test)]
fn status_line(app: &App, audio: &AudioHandle, accent: Color, width: u16) -> Line<'static> {
    status_line_with(app, audio, &StatusPalette::dark(accent), width)
}

fn status_line_with(
    app: &App,
    audio: &AudioHandle,
    pal: &StatusPalette,
    width: u16,
) -> Line<'static> {
    let accent = pal.accent;
    let dim = pal.dim;
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
                Style::new().fg(if warned { pal.warn } else { pal.text }),
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
            "1 / 2 / 3 / 4 / 5",
            "spectrum \u{b7} scope \u{b7} radial \u{b7} mirror (default) \u{b7} lyrics garden",
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
#[allow(clippy::too_many_arguments)]
fn render_lyrics(
    buf: &mut ratatui::buffer::Buffer,
    area: ratatui::layout::Rect,
    app: &mut App,
    analysis: &FrameAnalysis,
    media: &crate::media::MediaShared,
    lyrics: &crate::lyrics::LyricsShared,
    dt: f32,
    demo: bool,
    file_pos_s: Option<f64>,
) {
    let snap = media.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let state = lyrics.lock().unwrap_or_else(|e| e.into_inner()).clone();

    // Smoothed energy drives the sway (quiet passages stay still).
    let energy_now: f32 = analysis.bars.iter().sum::<f32>() / analysis.bars.len().max(1) as f32;
    app.sway_energy += (energy_now - app.sway_energy) * (1.0 - (-8.0 * dt).exp());

    use crate::garden::GardenState;
    use crate::lyrics::LyricsStatus;

    // Identity gate: the lyrics state is only rendered while the CURRENT
    // media identity matches it. A track change clears instantly (the
    // watcher also resets the state, but this gate guarantees old words
    // never display under a new song even while a lookup is pending).
    let media_key = crate::lyrics::TrackKey::from_snapshot(&snap);
    let identity_ok = media_key.is_some() && state.key == media_key;

    // The playback clock for word-level reveal: rodio's Sink::get_pos
    // (file playback) refreshes internally at ~5 ms - MUCH fresher than
    // the 10 Hz media snapshot, so words land within the frame budget
    // instead of the media poll quantization. Capture (SMTC clock) and
    // the demo (synthetic clock) have no sink and keep the snapshot
    // position. Pause freezes get_pos; the file media thread's identity
    // fields are unaffected.
    let pos_now = file_pos_s.unwrap_or(snap.position_s);

    let (garden_state, lines, plain, current, since) = if !identity_ok {
        // No session, or the state belongs to a previous track: show the
        // honest per-media state, never stale words.
        if media_key.is_none() {
            (GardenState::Unavailable, None, None, None, 5.0)
        } else {
            (GardenState::Loading, None, None, None, 0.0)
        }
    } else {
        match &state.status {
            LyricsStatus::Unavailable => (GardenState::Unavailable, None, None, None, 5.0),
            LyricsStatus::Loading => (GardenState::Loading, None, None, None, 0.0),
            LyricsStatus::Synced(lines) => {
                let idx = crate::lrc::line_at(lines, pos_now);
                // Bloom age derives from the track timeline (position -
                // line start), so a frozen demo freezes the bloom too;
                // line/track identity changes reset it inherently because
                // the new line's start time anchors the age again.
                let since = match idx {
                    Some(i) => ((pos_now - lines[i].time_s).max(0.0)) as f32,
                    None => 5.0,
                };
                (GardenState::Synced, Some(lines.clone()), None, idx, since)
            }
            LyricsStatus::Plain(text) => (GardenState::Plain, None, Some(text.clone()), None, 5.0),
            LyricsStatus::Instrumental => (GardenState::Instrumental, None, None, None, 5.0),
            LyricsStatus::NoMatch => (GardenState::NoMatch, None, None, None, 5.0),
            LyricsStatus::Error(e) => (GardenState::Error(e.clone()), None, None, None, 5.0),
        }
    };

    let lines_ref = lines.as_deref();
    let view = crate::garden::GardenView {
        title: &snap.title,
        artist: &snap.artist,
        lines: lines_ref.map(|l| (l, current)),
        plain: plain.as_deref(),
        state: garden_state,
        energy: app.sway_energy.clamp(0.0, 1.0),
        time_s: pos_now as f32,
        since_line_change: since,
        demo,
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

    /// Full-path regression: the demo seeding main.rs performs must pass
    /// the REAL identity gate in render_lyrics (it used to leave the
    /// lyrics key None, so the CLI showed "looking for lyrics..." forever).
    #[test]
    fn demo_seed_passes_identity_gate_and_renders_garden() {
        let media = crate::media::new_shared();
        let lyrics = crate::lyrics::new_shared();
        demo::seed_demo_state(&media, &lyrics, 4.5, true, None);
        let analysis = FrameAnalysis::quiet(64);
        for (w, h) in [(144u16, 42u16), (100, 30), (72, 22)] {
            let mut app = demo_app();
            let area = ratatui::layout::Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            render_lyrics(
                &mut buf, area, &mut app, &analysis, &media, &lyrics, 0.016, true, None,
            );
            let text: String = buf.content().iter().map(|c| c.symbol()).collect();
            assert!(
                !text.contains("looking for lyrics"),
                "{w}x{h}: stuck in Loading"
            );
            let mut ink = 0;
            let mut stem = 0;
            let mut flower = 0;
            let mut drawn = 0;
            let dark = |c: Color| matches!(c, Color::Rgb(r, g, b) if (r as u16 + g as u16 + b as u16) < 150);
            for c in buf.content() {
                if c.symbol() != " " {
                    drawn += 1;
                }
                // Type ink: upstream's texture glyphs or silhouette blocks
                // (dark fg on a quadrant, or dark fill on a solid cell).
                if dark(c.fg) || dark(c.bg) {
                    ink += 1;
                }
                match c.fg {
                    Color::Rgb(r, _g, b)
                        if b as i32 > r as i32 + 40 && b as i32 > _g as i32 + 20 =>
                    {
                        stem += 1;
                    }
                    Color::Rgb(r, _g, b) if r as i32 > 170 && r as i32 > b as i32 + 40 => {
                        flower += 1;
                    }
                    _ => {}
                }
            }
            assert!(drawn > 20, "{w}x{h}: garden drew {drawn} cells");
            assert!(ink > 0, "{w}x{h}: no lyric ink drawn");
            if (w, h) == (72, 22) {
                // Under upstream's size caps the fixture at this size
                // (~2.9 em rows) falls below the legible floor, so the
                // honest render is the PLAIN-TEXT fallback: every word
                // present, no plants. (With the 2:3 cell aspect the
                // width-capped em is taller: 144x42 ~5.9 em rows and
                // 100x30 ~4.1 em rows - both real silhouette gardens.)
                for word in "every beat a budding bloom".split(' ') {
                    assert!(text.contains(word), "{w}x{h} plain fallback lost {word:?}");
                }
            } else {
                assert!(stem > 0, "{w}x{h}: no stems drawn");
                assert!(flower > 0, "{w}x{h}: no flowers drawn");
            }
        }
    }

    /// The gate itself must stay strict: a real track change with no
    /// matching lyrics state still shows Loading, never stale fixture words.
    #[test]
    fn identity_gate_still_blocks_mismatched_state() {
        let media = crate::media::new_shared();
        let lyrics = crate::lyrics::new_shared();
        demo::seed_demo_state(&media, &lyrics, 4.5, true, None);
        {
            let mut m = media.lock().unwrap();
            m.title = "Some Other Song".into();
        }
        let mut app = demo_app();
        let area = ratatui::layout::Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        render_lyrics(
            &mut buf,
            area,
            &mut app,
            &FrameAnalysis::quiet(64),
            &media,
            &lyrics,
            0.016,
            true,
            None, // demo: no file sink, the snapshot clock drives the view
        );
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("looking for lyrics"), "gate relaxed");
    }

    /// The FILE playback clock (rodio's ~5 ms Sink::get_pos, passed in
    /// per frame) must drive the garden when present - the media
    /// snapshot's 10 Hz position is only the fallback for capture/demo.
    /// This is the word-level reveal clock: pause freezes it (same
    /// position, byte-identical render), and a later position selects
    /// the later line.
    #[test]
    fn file_clock_drives_the_garden_when_present() {
        let media = crate::media::new_shared();
        let lyrics = crate::lyrics::new_shared();
        // Demo fixture lines: 0.2, 1.8, 3.4, 5.0, 6.6 - at 4.5 s the
        // current line is "every beat a budding bloom" (3.4), at 6.0 s
        // it is "silence is the resting soil" (5.0).
        demo::seed_demo_state(&media, &lyrics, 4.5, true, None);
        let analysis = FrameAnalysis::quiet(64);
        let area = ratatui::layout::Rect::new(0, 0, 144, 42);

        let mut snap_clock = Buffer::empty(area);
        render_lyrics(
            &mut snap_clock,
            area,
            &mut demo_app(),
            &analysis,
            &media,
            &lyrics,
            0.016,
            true,
            None,
        );
        let mut file_clock_same = Buffer::empty(area);
        render_lyrics(
            &mut file_clock_same,
            area,
            &mut demo_app(),
            &analysis,
            &media,
            &lyrics,
            0.016,
            true,
            Some(4.5),
        );
        assert_eq!(
            snap_clock.content(),
            file_clock_same.content(),
            "an equal file clock must render exactly the snapshot clock"
        );

        let mut file_clock_later = Buffer::empty(area);
        render_lyrics(
            &mut file_clock_later,
            area,
            &mut demo_app(),
            &analysis,
            &media,
            &lyrics,
            0.016,
            true,
            Some(6.0),
        );
        assert_ne!(
            file_clock_same.content(),
            file_clock_later.content(),
            "the file clock must drive line selection (later line expected)"
        );
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
        let mut peaks = visualizers::Peaks::new();
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
        let bars_filled = bar_cells(&bars_buf);
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
            1.0 / 60.0,
        );
        assert!(braille_cells(&scope_buf) > 40, "scope too sparse");

        // Radial: petals and ring.
        let mut radial_buf = Buffer::empty(area);
        visualizers::render_radial(
            &mut radial_buf,
            area,
            &frame.bars,
            &frame.waveform,
            frame.flux,
            &theme,
            &mut scratch,
        );
        assert!(
            braille_cells(&radial_buf) + quadrant_cells(&radial_buf) > 150,
            "radial too sparse"
        );

        // Mirror renders around the center line.
        let mut mirror_buf = Buffer::empty(area);
        visualizers::render_mirror(&mut mirror_buf, area, &frame.bars, &theme, &mut scratch);
        assert!(nonblank(&mirror_buf) > 300, "mirror too sparse");
    }

    /// The mode mapping is explicit and part of the contract: keys 1-5,
    /// Tab cycling through all five, and the numbers resolve to distinct
    /// modes (the waterfall was removed; nothing maps to it).
    #[test]
    fn mode_mapping_is_explicit() {
        assert_eq!(Mode::from_number(1), Mode::Bars);
        assert_eq!(Mode::from_number(2), Mode::Scope);
        assert_eq!(Mode::from_number(3), Mode::Radial);
        assert_eq!(Mode::from_number(4), Mode::Mirror);
        assert_eq!(Mode::from_number(5), Mode::Lyrics);
        // Out-of-range falls back to the garden (the CLI validates 1-5).
        assert_eq!(Mode::from_number(6), Mode::Lyrics);

        // Tab cycles through every mode exactly once, back to the start.
        let mut m = Mode::Bars;
        let mut seen = vec![m];
        for _ in 0..4 {
            m = m.next();
            seen.push(m);
        }
        assert_eq!(seen.len(), 5, "five distinct modes: {seen:?}");
        assert_eq!(m.next(), Mode::Bars, "cycle closes");
        // prev inverts next.
        for mode in [
            Mode::Bars,
            Mode::Scope,
            Mode::Radial,
            Mode::Mirror,
            Mode::Lyrics,
        ] {
            assert_eq!(mode.next().prev(), mode, "prev must invert next");
        }
        // Names are unique and non-empty.
        let names: Vec<&str> = [
            Mode::Bars,
            Mode::Scope,
            Mode::Radial,
            Mode::Mirror,
            Mode::Lyrics,
        ]
        .iter()
        .map(|m| m.name())
        .collect();
        let unique: std::collections::HashSet<_> = names.iter().copied().collect();
        assert_eq!(unique.len(), 5, "mode names must be unique: {names:?}");
    }

    #[test]
    fn demo_silence_and_fade_render_quietly() {
        let theme = theme::find_with_depth("spectrum", ColorDepth::True).unwrap();
        let mut scratch = RenderScratch::default();
        let mut peaks = visualizers::Peaks::new();
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
        assert!(bar_cells(&buf) <= 2, "silence should render almost empty");

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
            bar_cells(&fade_buf) < bar_cells(&loud_buf),
            "fade ({}) should be quieter than loud ({})",
            bar_cells(&fade_buf),
            bar_cells(&loud_buf)
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
            let mut peaks = visualizers::Peaks::new();
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
    fn every_mode_draws_through_the_real_ui_path() {
        // The headless snapshot runs the real `draw`: every mode must
        // produce content, the garden must not be stuck in Loading, and
        // the bar modes must carry the quiet header with an honest DEMO.
        for mode in 1..=5u8 {
            let buf = snapshot(&Snapshot {
                mode,
                theme: "spectrum",
                depth: ColorDepth::True,
                at: 4.5,
                frozen: false,
                warmup: 1.0,
                lyric: None,
                width: 100,
                height: 30,
            });
            let text: String = buf.content().iter().map(|c| c.symbol()).collect();
            assert!(!text.contains("looking for lyrics"), "mode {mode} loading");
            assert!(text.contains("DEMO"), "mode {mode} lost the DEMO label");
            let drawn = buf.content().iter().filter(|c| c.symbol() != " ").count();
            assert!(drawn > 60, "mode {mode} drew only {drawn} cells");
        }
    }

    #[test]
    #[ignore]
    fn frame_cost_probe() {
        // Average draw cost per frame at 144x42 (release build recommended).
        for mode in 1..=5u8 {
            let frames = 120.0;
            let t0 = std::time::Instant::now();
            let _ = snapshot(&Snapshot {
                mode,
                theme: "spectrum",
                depth: ColorDepth::True,
                at: 4.5,
                frozen: false,
                warmup: (frames - 1.0) / 60.0,
                lyric: None,
                width: 144,
                height: 42,
            });
            let per = t0.elapsed().as_secs_f64() * 1000.0 / frames as f64;
            println!("mode {mode}: {per:.2} ms/frame");
        }
    }

    /// Cell-level garden geometry: which band rows carry letters (dark
    /// ink) and which carry plants (saturated colour). Exact buffer cells.
    fn garden_rows(buf: &Buffer, w: u16, h: u16) -> (Vec<u16>, Vec<u16>) {
        let ink = |c: &ratatui::buffer::Cell| {
            let dark = |x: Color| matches!(x, Color::Rgb(r, g, b) if (r as u16 + g as u16 + b as u16) < 150);
            dark(c.fg) || dark(c.bg)
        };
        let plant = |c: &ratatui::buffer::Cell| {
            let sat = |x: Color| match x {
                Color::Rgb(r, g, b) => (r as i32 - g as i32).abs().max((b as i32 - g as i32).abs()),
                _ => 0,
            };
            (sat(c.fg) > 40 || sat(c.bg) > 40) && !ink(c)
        };
        let mut letter_rows = Vec::new();
        let mut plant_rows = Vec::new();
        for y in 2..h.saturating_sub(1) {
            let mut letters = 0;
            let mut plants = 0;
            for x in 0..w {
                let c = &buf[(x, y)];
                if c.symbol() != " " {
                    if ink(c) {
                        letters += 1;
                    } else if plant(c) {
                        plants += 1;
                    }
                }
            }
            if letters >= 4 {
                letter_rows.push(y);
            }
            if plants >= 2 {
                plant_rows.push(y);
            }
        }
        (letter_rows, plant_rows)
    }

    /// The lyrics garden via the real UI path (mode 5 after the
    /// waterfall removal).
    fn snap_garden(lyric: Option<&str>, w: u16, h: u16) -> Buffer {
        snapshot(&Snapshot {
            mode: 5,
            theme: "spectrum",
            depth: ColorDepth::True,
            at: 4.5,
            frozen: false,
            warmup: 1.5,
            lyric,
            width: w,
            height: h,
        })
    }

    /// The exact `--demo --mode 5 --demo-lyric hello --freeze-at 4.5` path.
    fn snap_garden_frozen(lyric: Option<&str>, w: u16, h: u16) -> Buffer {
        snapshot(&Snapshot {
            mode: 5,
            theme: "spectrum",
            depth: ColorDepth::True,
            at: 4.5,
            frozen: true,
            warmup: 0.0,
            lyric,
            width: w,
            height: h,
        })
    }

    #[test]
    fn garden_keeps_breathing_room_at_the_edges() {
        // Regression: plants used to tower past the band and clip at the
        // viewport top, and a plant-only fit calculation pressed h/l
        // ASCENDERS against the top edge (plants fit but the letters did
        // not). Both the ink (letters) and the plants must stay clear of
        // the first and last rows of the band.
        for (w, h) in [(144u16, 42u16), (72, 22)] {
            for frozen in [false, true] {
                for lyric in [Some("hello"), None] {
                    let buf = if frozen {
                        snap_garden_frozen(lyric, w, h)
                    } else {
                        snap_garden(lyric, w, h)
                    };
                    let (letter_rows, plant_rows) = garden_rows(&buf, w, h);
                    for (name, rows) in [("letters", &letter_rows), ("plants", &plant_rows)] {
                        if let (Some(&top), Some(&bot)) = (rows.first(), rows.last()) {
                            assert!(
                                top >= 3,
                                "{w}x{h} {name} at row {top} (frozen={frozen}, clipped top)"
                            );
                            assert!(
                                bot <= h - 3,
                                "{w}x{h} {name} at row {bot} of {h} (frozen={frozen}, clipped bottom)"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn frozen_hello_keeps_ascenders_off_the_top_edge() {
        // The exact reported path: --demo-lyric hello --freeze-at 4.5.
        // "hello" has tall h/l ascenders; the fit must reserve the full
        // glyph bounds, so the first ink row stays clear of the band top.
        // The single-line custom lyric has no prev row, so the band starts
        // at row 1: the ascenders must keep at least two rows below it.
        for (w, h) in [(144u16, 42u16), (72, 22)] {
            let buf = snap_garden_frozen(Some("hello"), w, h);
            let (letter_rows, _) = garden_rows(&buf, w, h);
            let top = letter_rows.first().copied().unwrap_or(0);
            assert!(
                top >= 3,
                "{w}x{h}: ascenders pressed at row {top} (band starts at 1)"
            );
            // The word also must not sit in the bottom quarter.
            let bot = letter_rows.last().copied().unwrap_or(0);
            assert!(bot <= h - 3, "{w}x{h}: letters touch the bottom at {bot}");
        }
    }

    #[test]
    fn frozen_and_live_lay_out_the_same_type() {
        // Layout is a pure function of text and size: the frozen CLI path
        // and a live render at the same demo time must place the type
        // identically. The comparison runs in the SILENCE section
        // (7.95 s): during the loud drop the live garden sways with the
        // music BY DESIGN (frozen plants hold still), and a swayed flower
        // covering or revealing a boundary ascender cell flips the
        // ink-row extents by 2 (measured: hello 144x42 (17..23) frozen
        // vs (15..23) live at 4.5 s) - that is layering noise, not a
        // placement difference. At rest both paths must agree, and any
        // real placement divergence shows up there just the same.
        for (w, h) in [(144u16, 42u16), (72, 22)] {
            for lyric in [Some("hello"), None] {
                let spec = |frozen: bool| Snapshot {
                    mode: 5,
                    theme: "spectrum",
                    depth: ColorDepth::True,
                    at: 7.95,
                    frozen,
                    warmup: if frozen { 0.0 } else { 1.5 },
                    lyric,
                    width: w,
                    height: h,
                };
                let a = snapshot(&spec(true));
                let b = snapshot(&spec(false));
                let (la, _) = garden_rows(&a, w, h);
                let (lb, _) = garden_rows(&b, w, h);
                let extent = |r: &[u16]| {
                    (
                        r.first().copied().unwrap_or(0),
                        r.last().copied().unwrap_or(0),
                    )
                };
                let (ta, ba) = extent(&la);
                let (tb, bb) = extent(&lb);
                assert!(
                    ta.abs_diff(tb) <= 1 && ba.abs_diff(bb) <= 1,
                    "{w}x{h} frozen/live placement differs: ({ta}..{ba}) vs ({tb}..{bb}) ({lyric:?})"
                );
                // And both actually drew the word.
                assert!(
                    !la.is_empty() && !lb.is_empty(),
                    "{w}x{h} no type ({lyric:?})"
                );
            }
        }
    }

    #[test]
    fn garden_type_caps_at_the_upstream_reference_size() {
        // The reference comparison gave exact upstream demo metrics: the
        // word is NARROW and centered (St = 0.28 H), not maximized
        // full-width. At 144x42 the band is ~40 rows, so the em must be
        // about 0.28 x 40 = 11.2 rows of ink - previously it filled nearly
        // the whole height, which the reviewer rejected.
        let (w, h) = (144u16, 42u16);
        let buf = snap_garden(Some("hello"), w, h);
        let (letter_rows, _) = garden_rows(&buf, w, h);
        let ink_rows = letter_rows.len();
        assert!(
            (5..=18).contains(&ink_rows),
            "hello ink spans {ink_rows} rows; upstream St=0.28H implies ~11"
        );
        // The word sits inside the middle of the band, not pressed high.
        let top = letter_rows.first().copied().unwrap_or(0);
        let bot = letter_rows.last().copied().unwrap_or(0);
        assert!(top >= 5, "word too high: ink starts at row {top}");
        assert!(bot <= h - 6, "word too low: ink ends at row {bot}");
    }

    #[test]
    fn small_terminal_shows_every_word_honestly() {
        // Under upstream's size caps a 72x22 band (~19 usable rows) gives
        // the fixture line an em under the legible floor, so it renders as
        // wrapped plain text - honest, never dropped. "hello" still gets
        // a garden (its single word fits 0.28 H).
        let (w, h) = (72u16, 22u16);

        // Fixture: plain fallback, but every word present.
        let buf = snap_garden(None, w, h);
        let text: String = buf.content().iter().map(|c| c.symbol()).collect::<String>();
        for word in "every beat a budding bloom".split(' ') {
            assert!(text.contains(word), "{w}x{h} lost {word:?}");
        }

        // hello: a real garden with plants.
        let buf = snap_garden(Some("hello"), w, h);
        let (letter_rows, plant_rows) = garden_rows(&buf, w, h);
        assert!(!letter_rows.is_empty(), "hello type missing at {w}x{h}");
        assert!(!plant_rows.is_empty(), "hello garden missing at {w}x{h}");
    }

    #[test]
    #[ignore]
    fn garden_geometry_probe() {
        // Cell-level geometry of the garden band: where the letters sit,
        // where plants reach, and the breathing margins. Exact buffer cells,
        // no pixel thresholds. Run with --ignored --nocapture.
        let probe = |lyric: Option<&str>, w: u16, h: u16| {
            let buf = snap_garden(lyric, w, h);
            let (letter_rows, plant_rows) = garden_rows(&buf, w, h);
            let first = |v: &[u16]| v.first().copied().unwrap_or(u16::MAX);
            let last = |v: &[u16]| v.last().copied().unwrap_or(0);
            println!(
                "{w}x{h} {:?}: letter rows {}..{}  plant rows {}..{}  plant-above {}",
                lyric.map(|_| "custom").unwrap_or("fixture"),
                first(&letter_rows),
                last(&letter_rows),
                first(&plant_rows),
                last(&plant_rows),
                letter_rows.first().copied().unwrap_or(0) as i32
                    - plant_rows.first().copied().unwrap_or(0) as i32
            );
        };
        for (w, h) in [(144u16, 42u16), (72, 22)] {
            probe(Some("hello"), w, h);
            probe(None, w, h);
        }
    }

    #[test]
    #[ignore]
    fn motion_probe() {
        // Frame-to-frame change rate per mode (live, 60 fps): flicker or
        // jitter shows up as a high rate of cells changing between two
        // consecutive frames. Prints; run with --ignored --nocapture.
        for mode in 1..=5u8 {
            let snap = |at: f32| {
                snapshot(&Snapshot {
                    mode,
                    theme: "spectrum",
                    depth: ColorDepth::True,
                    at,
                    frozen: false,
                    warmup: 1.5,
                    lyric: None,
                    width: 100,
                    height: 30,
                })
            };
            for at in [2.0f32, 4.5, 6.8, 7.9] {
                let a = snap(at);
                let b = snap(at + 1.0 / 60.0);
                let changed = a
                    .content()
                    .iter()
                    .zip(b.content().iter())
                    .filter(|(x, y)| x != y)
                    .count();
                let lit = a.content().iter().filter(|c| c.symbol() != " ").count();
                println!(
                    "mode {mode} t={at:.2}: changed {changed:4} of {} cells (lit {lit})",
                    a.content().len()
                );
            }
        }
    }

    #[test]
    fn snapshot_is_deterministic_when_frozen() {
        let s = Snapshot {
            mode: 5,
            theme: "spectrum",
            depth: ColorDepth::True,
            at: 4.5,
            frozen: true,
            warmup: 0.5,
            lyric: None,
            width: 72,
            height: 22,
        };
        let a = snapshot(&s);
        let b = snapshot(&s);
        assert_eq!(a.content(), b.content());
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
            let mut peaks = visualizers::Peaks::new();

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
                1.0 / 60.0,
            );
            assert!(braille_cells(&buf) > 0, "scope {w}x{h} empty at loud");
            let mut buf = Buffer::empty(area);
            visualizers::render_radial(
                &mut buf,
                area,
                &loud.bars,
                &loud.waveform,
                loud.flux,
                &theme,
                &mut scratch,
            );
            assert!(
                braille_cells(&buf) + quadrant_cells(&buf) > 0,
                "radial {w}x{h} empty at loud"
            );

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

    /// Cells carrying a bar glyph (partial or full block).
    fn bar_cells(buf: &Buffer) -> usize {
        buf.content()
            .iter()
            .filter(|c| {
                c.symbol()
                    .chars()
                    .next()
                    .is_some_and(|ch| ('\u{2581}'..='\u{2588}').contains(&ch))
            })
            .count()
    }

    /// Cells carrying a quadrant/half block (silhouettes, petals).
    fn quadrant_cells(buf: &Buffer) -> usize {
        buf.content()
            .iter()
            .filter(|c| {
                c.symbol()
                    .chars()
                    .next()
                    .is_some_and(|ch| ('\u{2580}'..='\u{259F}').contains(&ch))
            })
            .count()
    }

    /// Cells carrying a lit braille glyph.
    fn braille_cells(buf: &Buffer) -> usize {
        buf.content()
            .iter()
            .filter(|c| {
                c.symbol()
                    .chars()
                    .next()
                    .is_some_and(|ch| ('\u{2801}'..='\u{28FF}').contains(&ch))
            })
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
