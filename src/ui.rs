//! Terminal UI: event loop, layout, status bar and help overlay.

use std::collections::VecDeque;
use std::io::{self, Stdout};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
use crate::theme::{self, Theme};
use crate::visualizers;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Bars,
    Scope,
    Gram,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Bars => "Spectrum Analyzer",
            Mode::Scope => "Oscilloscope",
            Mode::Gram => "Spectrogram",
        }
    }

    fn next(self) -> Self {
        match self {
            Mode::Bars => Mode::Scope,
            Mode::Scope => Mode::Gram,
            Mode::Gram => Mode::Bars,
        }
    }

    fn prev(self) -> Self {
        match self {
            Mode::Bars => Mode::Gram,
            Mode::Scope => Mode::Bars,
            Mode::Gram => Mode::Scope,
        }
    }
}

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
    gram_tick: u32,
    themes: Vec<Theme>,
    theme_idx: usize,
}

impl App {
    fn new(shared: &Arc<Mutex<SharedState>>, audio: &AudioHandle, theme_name: &str) -> Self {
        let state = shared.lock().unwrap_or_else(|e| e.into_inner());
        let themes = theme::all();
        let theme_idx = themes
            .iter()
            .position(|t| t.name == theme_name)
            .unwrap_or(0);
        let volume = if audio.is_live() { 0.0 } else { audio.volume() };
        Self {
            mode: Mode::Bars,
            help: false,
            quit: false,
            paused: false,
            beat_flash: 0.0,
            sensitivity: state.params.sensitivity,
            fall: state.params.fall,
            volume,
            peaks: Vec::new(),
            gram: VecDeque::new(),
            gram_tick: 0,
            themes,
            theme_idx,
        }
    }

    fn theme(&self) -> &Theme {
        &self.themes[self.theme_idx]
    }
}

/// Run the UI to completion. The terminal is always restored, even on
/// error or panic (see `install_panic_hook`).
pub fn run(shared: &Arc<Mutex<SharedState>>, audio: &AudioHandle, theme_name: &str) -> Result<()> {
    install_panic_hook();
    enable_raw_mode()?;

    let result = (|| -> Result<()> {
        execute!(io::stdout(), EnterAlternateScreen)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        terminal.hide_cursor()?;

        let mut app = App::new(shared, audio, theme_name);
        let r = event_loop(&mut terminal, shared, audio, &mut app);

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
    app: &mut App,
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
            app.beat_flash *= 0.88;
            if app.beat_flash < 0.01 {
                app.beat_flash = 0.0;
            }
        }

        // Spectrogram history: newest column on the right.
        app.gram_tick = app.gram_tick.wrapping_add(1);
        if app.mode == Mode::Gram && app.gram_tick % 2 == 0 {
            app.gram.push_back(analysis.spectrum.clone());
            while app.gram.len() > 512 {
                app.gram.pop_front();
            }
        }

        terminal.draw(|f| draw(f, app, &analysis, audio))?;

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

fn draw(f: &mut Frame, app: &mut App, analysis: &FrameAnalysis, audio: &AudioHandle) {
    let area = f.area();
    if area.width < 12 || area.height < 5 {
        return; // too small to render anything useful
    }

    // Clone once per frame so all later `&mut app.*` borrows stay disjoint.
    let theme = app.themes[app.theme_idx].clone();
    let accent = theme.accent();
    let beat_color = theme.beat();
    let border = theme::lerp_color(accent, beat_color, app.beat_flash);

    let chunks = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(area);
    let main = chunks[0];
    let status_area = chunks[1];

    let mut title_right = vec![
        Span::raw(" "),
        Span::styled(app.mode.name(), Style::new().fg(accent)),
        Span::raw(" "),
    ];
    if app.beat_flash > 0.5 {
        title_right.push(Span::styled("\u{266A}", Style::new().fg(beat_color)));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(border))
        .title(Line::from(vec![Span::styled(
            " SPECTRUM ",
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
        )]))
        .title(Line::from(title_right).right_aligned());
    let inner = block.inner(main);
    f.render_widget(block, main);

    match app.mode {
        Mode::Bars => visualizers::render_spectrum(
            f.buffer_mut(),
            inner,
            &analysis.bars,
            &theme,
            &mut app.peaks,
        ),
        Mode::Scope => {
            visualizers::render_oscilloscope(f.buffer_mut(), inner, &analysis.waveform, &theme, 0.9)
        }
        Mode::Gram => visualizers::render_spectrogram(f.buffer_mut(), inner, &app.gram, &theme),
    }

    let status = status_line(app, audio, accent, analysis.flux);
    f.render_widget(Paragraph::new(status), status_area);

    if app.help {
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

fn status_line(app: &App, audio: &AudioHandle, accent: Color, flux: f32) -> Line<'static> {
    let dim = Color::Rgb(130, 130, 130);
    let state = if audio.is_live() {
        if app.paused {
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
        "playing" | "capturing" => "\u{bb}",
        "paused" => "\u{2016}",
        _ => "\u{b7}",
    };

    let mut spans = vec![
        Span::styled(
            format!(" {icon} {state} "),
            Style::new().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} ", audio.meta.description),
            Style::new().fg(Color::White),
        ),
    ];
    if !audio.is_live() {
        spans.push(Span::styled(
            format!("vol {:>3.0}% ", app.volume * 100.0),
            Style::new().fg(dim),
        ));
    }
    spans.push(Span::styled(
        format!("sens {:.1} ", app.sensitivity),
        Style::new().fg(dim),
    ));
    spans.push(Span::styled(
        format!("smooth {:.2} ", app.fall),
        Style::new().fg(dim),
    ));
    spans.push(Span::styled(
        format!("flux {:.2} ", flux),
        Style::new().fg(dim),
    ));
    spans.push(Span::styled(
        format!("[{}] ", app.theme().name),
        Style::new().fg(accent),
    ));
    spans.push(Span::styled(
        "1/2/3 modes \u{b7} space pause \u{b7} t theme \u{b7} h help \u{b7} q quit",
        Style::new().fg(dim),
    ));
    Line::from(spans)
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
        mk("1 / 2 / 3", "switch visualization"),
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
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original(panic_info);
    }));
}
