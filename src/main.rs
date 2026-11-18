use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;

use spectrum::analysis::{FrameAnalysis, Params, SharedState, DEFAULT_BANDS};
use spectrum::audio::{self, CaptureSource, SampleQueue};
use spectrum::demo;
use spectrum::theme::{self, ColorDepth};
use spectrum::ui;

#[derive(Parser, Debug)]
#[command(
    name = "spectrum",
    version,
    about = "SPECTRUM \u{2014} a terminal-native audio visualizer"
)]
struct Args {
    /// Audio file to play and visualize (mp3, wav, ogg, flac)
    file: Option<PathBuf>,

    /// Visualize live audio instead of playing a file
    #[arg(short, long)]
    capture: bool,

    /// Silent synthetic demo instead of real audio (all visuals, no sound)
    #[arg(long)]
    demo: bool,

    /// Live capture source: system (Windows output loopback) or mic
    #[arg(long, default_value = "system")]
    source: String,

    /// Color theme: spectrum, fire, ice, matrix, vapor
    #[arg(short, long, default_value = "spectrum")]
    theme: String,

    /// Start visualization in this mode (1-5)
    #[arg(long)]
    mode: Option<u8>,

    /// Exit after this many seconds (for scripted QA / screenshots)
    #[arg(long)]
    exit_after: Option<f32>,

    /// With --demo: freeze the synthetic timeline at this cycle time
    /// (seconds) for deterministic, pixel-identical frames
    #[arg(long)]
    freeze_at: Option<f32>,

    /// With --demo: show this single line in the lyrics garden instead of
    /// the built-in fixture (local text only; e.g. --demo-lyric hello)
    #[arg(long)]
    demo_lyric: Option<String>,

    /// Number of frequency bands (8..=256)
    #[arg(long, default_value_t = DEFAULT_BANDS)]
    bars: usize,

    /// FFT display sensitivity (0.1..=4.0)
    #[arg(long, default_value_t = 1.0)]
    sensitivity: f32,

    /// Bar fall speed: 0.01 slow .. 0.95 instant
    #[arg(long, default_value_t = 0.12)]
    smoothing: f32,

    /// Terminal color depth: true (24-bit), 256 or 16 (both dithered)
    #[arg(long, default_value = "true")]
    colors: String,

    /// Playback volume 0.0..=1.0 for audio files (default 0.8).
    /// 0 = muted from the first sample - the QA constraint for
    /// scripted validation runs (never audible); the default behavior
    /// is unchanged.
    #[arg(long)]
    volume: Option<f32>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let inputs = [args.file.is_some(), args.capture, args.demo];
    if inputs.iter().filter(|b| **b).count() > 1 {
        bail!("pass only one of: an audio file, --capture or --demo");
    }

    // clap parses NaN/inf into f32 happily; reject them here rather than
    // letting NaN propagate into the analysis math.
    ensure_finite("sensitivity", args.sensitivity)?;
    ensure_finite("smoothing", args.smoothing)?;
    if let Some(secs) = args.exit_after {
        if !secs.is_finite() || secs <= 0.0 {
            bail!("--exit-after must be a positive number of seconds");
        }
    }
    if let Some(v) = args.volume {
        if !v.is_finite() || !(0.0..=1.0).contains(&v) {
            bail!("--volume must be a finite number in 0.0..=1.0");
        }
    }
    if args.demo_lyric.is_some() && !args.demo {
        bail!("--demo-lyric requires --demo");
    }
    if let Some(t) = args.freeze_at {
        if !args.demo {
            bail!("--freeze-at requires --demo (it pins the synthetic timeline)");
        }
        if !t.is_finite() || t < 0.0 {
            bail!("--freeze-at must be a finite number of seconds >= 0");
        }
    }

    let depth = ColorDepth::parse(&args.colors).with_context(|| {
        format!(
            "unknown color depth '{}'; expected true, 256 or 16",
            args.colors
        )
    })?;

    let theme = theme::find_with_depth(&args.theme, depth).with_context(|| {
        format!(
            "unknown theme '{}'; available themes: {}",
            args.theme,
            theme::names().join(", ")
        )
    })?;

    let start_mode = match args.mode {
        None => None,
        Some(m) if (1..=5).contains(&m) => Some(m),
        Some(m) => bail!("--mode must be 1-5, got {m}"),
    };

    // Clamp once here so every component sees consistent values.
    let bands = args.bars.clamp(8, 256);

    let shared = Arc::new(Mutex::new(SharedState {
        latest: Arc::new(FrameAnalysis::quiet(bands)),
        params: Params {
            sensitivity: args.sensitivity,
            fall: args.smoothing,
        }
        .clamped(),
        beats_pending: 0,
    }));

    let stop = Arc::new(AtomicBool::new(false));
    let exit_after = args.exit_after.map(Duration::from_secs_f32);

    // Plain launch (NO arguments at all): the interactive menu the
    // user asked for - file selection and the existing modes live
    // INSIDE it, while every expert flag still bypasses it and
    // behaves exactly as before.
    let plain_launch = !args.capture && !args.demo && args.file.is_none();

    // The OWN-TERMINAL chooser (user 18:51): offer the app-owned
    // portable Windows Terminal ("Own terminal (Spectrum only)" -
    // reference 288x128 grid at a compact font, focus mode, no
    // shell/tabs/palette/settings in ordinary use) or the NORMAL
    // terminal with the honest warning below. Windows-only; skipped
    // when already inside the own terminal (no relaunch loop); when
    // stdin is not an interactive terminal (scripts, pipes) it
    // auto-selects NORMAL without prompting so scripted runs are
    // unchanged. Failure of the own route prints the honest error
    // and stays in the normal terminal - never a fake exit 0.
    #[cfg(windows)]
    if !spectrum::own_term::imp::in_own_terminal()
        && std::io::IsTerminal::is_terminal(&std::io::stdin())
    {
        println!("Spectrum start:");
        println!("  1) Own terminal (Spectrum only)");
        println!("  2) Normal terminal");
        println!("     lyrics garden view may glitch out");
        print!("choice: ");
        use std::io::Write as _;
        std::io::stdout().flush()?;
        let mut line = String::new();
        let chosen = if std::io::stdin().read_line(&mut line)? == 0 {
            String::from("2") // EOF: stay in the normal terminal, honestly
        } else {
            line.trim().to_string()
        };
        if chosen == "1" {
            let original: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
            match spectrum::own_term::imp::launch(&original) {
                Ok(status) => {
                    // Propagate the own-terminal window's ACTUAL exit
                    // status (nonzero included - no fake exit 0).
                    std::process::exit(status.code().unwrap_or(1));
                }
                Err(e) => {
                    println!("own terminal unavailable: {e}");
                    println!("continuing in the normal terminal - ");
                    println!("     lyrics garden view may glitch out");
                }
            }
        }
    }

    // Media + lyrics pipeline: shared snapshots the UI reads.
    let media_shared = spectrum::media::new_shared();
    let lyrics_shared = spectrum::lyrics::new_shared();
    let mut worker_threads: Vec<std::thread::JoinHandle<()>> = Vec::new();

    let result = if args.demo {
        // No audio hardware is touched at all; the demo fixture drives
        // both the analysis frames and the synthetic media session.
        let handle = audio::AudioHandle::for_demo();
        let demo_thread = demo::spawn_demo(
            shared.clone(),
            bands,
            stop.clone(),
            handle.demo_pause.clone(),
            args.freeze_at,
        );
        worker_threads.push(demo_thread);
        // The demo fixture's lyrics ship locally (no network, no
        // copyrighted content). Media + lyrics identities are seeded
        // together BEFORE the UI and the media thread start, so the
        // first rendered frame already has a matching identity.
        demo::seed_demo_state(
            &media_shared,
            &lyrics_shared,
            args.freeze_at.map(|f| f as f64).unwrap_or(0.0),
            args.freeze_at.is_some(),
            args.demo_lyric.as_deref(),
        );
        let pause = handle.demo_pause.clone().unwrap();
        let media_thread =
            demo::spawn_demo_media(media_shared.clone(), stop.clone(), pause, args.freeze_at);
        worker_threads.push(media_thread);
        let r = ui::run(spectrum::ui::RunConfig {
            shared: &shared,
            audio: &handle,
            media: &media_shared,
            lyrics: &lyrics_shared,
            theme_name: theme.name,
            depth,
            start_mode,
            exit_after,
        });
        stop.store(true, Ordering::Relaxed);
        for t in worker_threads {
            let _ = t.join();
        }
        r
    } else if plain_launch {
        run_interactive_menu(&args, &theme, depth, start_mode, bands)
    } else {
        let selection = if let Some(path) = &args.file {
            Selection::File(path.clone())
        } else {
            let source = CaptureSource::parse(&args.source).with_context(|| {
                format!(
                    "unknown capture source '{}'; expected system or mic",
                    args.source
                )
            })?;
            Selection::Capture(source)
        };
        run_visualizer(&selection, &args, &theme, depth, start_mode, bands)
    };

    result
}

/// What the visualizer should play.
enum Selection {
    /// Live capture with the explicit source (system loopback output
    /// or microphone - chosen, never a silent fallback).
    Capture(CaptureSource),
    /// Play + visualize a local audio file. Lyrics come from the
    /// same-basename .lrc sidecar next to the file (song.mp3 ->
    /// song.lrc): local file only, line timing, no fetch, no invented
    /// word onsets. A missing or malformed sidecar shows an honest
    /// state - never guessed lyrics.
    File(std::path::PathBuf),
    /// The silent synthetic demo.
    Demo,
}

/// Load the same-basename .lrc sidecar for an audio file (song.mp3
/// -> song.lrc, SAME directory). HONEST outcomes only:
/// - found + timestamped lines -> Synced (line timing; no word onsets
///   exist in plain LRC and none are invented)
/// - found but WITHOUT timestamps -> Plain (clearly unsynced text,
///   never fake-synced)
/// - found but unreadable -> Error(reason)
/// - missing -> Error saying exactly what was expected and where
fn load_sidecar_lyrics(audio: &std::path::Path) -> spectrum::lyrics::LyricsStatus {
    let sidecar = audio.with_extension("lrc");
    let shown = sidecar.display();
    match std::fs::read_to_string(&sidecar) {
        Ok(text) => match spectrum::lrc::parse_lrc(&text) {
            Some(lines) => spectrum::lyrics::LyricsStatus::Synced(lines),
            None => {
                // No timestamped lines: still show the text, honestly
                // unsynced - never as fake-synced lines.
                let plain = text
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                if plain.is_empty() {
                    spectrum::lyrics::LyricsStatus::Error(format!(
                        "lyrics sidecar {shown} is empty"
                    ))
                } else {
                    spectrum::lyrics::LyricsStatus::Plain(plain)
                }
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            spectrum::lyrics::LyricsStatus::Error(format!(
                "no lyrics sidecar: expected {shown} next to the audio"
            ))
        }
        Err(e) => {
            spectrum::lyrics::LyricsStatus::Error(format!("lyrics sidecar {shown} unreadable: {e}"))
        }
    }
}

/// The flag-driven visualizer run (the exact behavior the expert flags
/// had before the menu existed).
fn run_visualizer(
    selection: &Selection,
    args: &Args,
    theme: &spectrum::theme::Theme,
    depth: spectrum::theme::ColorDepth,
    start_mode: Option<u8>,
    bands: usize,
) -> Result<()> {
    let shared = Arc::new(Mutex::new(SharedState {
        latest: Arc::new(FrameAnalysis::quiet(bands)),
        params: Params {
            sensitivity: args.sensitivity,
            fall: args.smoothing,
        }
        .clamped(),
        beats_pending: 0,
    }));
    let stop = Arc::new(AtomicBool::new(false));
    let exit_after = args.exit_after.map(Duration::from_secs_f32);
    let media_shared = spectrum::media::new_shared();
    let lyrics_shared = spectrum::lyrics::new_shared();
    let mut worker_threads: Vec<std::thread::JoinHandle<()>> = Vec::new();

    // QA 18:44 RECOVERY: the session console geometry/font manipulation
    // that ran here is REMOVED - the forced 288-column screen buffer
    // corrupted real host viewports (a buffer wider than the window
    // garbles the TUI). The app renders into whatever size the host
    // reports, exactly as it did before any console manipulation was
    // introduced. No rehost, no buffer/font/window calls, no global
    // settings; the honest LEFT-density limitation is reported at the
    // documentation level, not by fighting the host.

    let r = match selection {
        Selection::Demo => {
            let handle = audio::AudioHandle::for_demo();
            let demo_thread = demo::spawn_demo(
                shared.clone(),
                bands,
                stop.clone(),
                handle.demo_pause.clone(),
                args.freeze_at,
            );
            worker_threads.push(demo_thread);
            demo::seed_demo_state(
                &media_shared,
                &lyrics_shared,
                args.freeze_at.map(|f| f as f64).unwrap_or(0.0),
                args.freeze_at.is_some(),
                args.demo_lyric.as_deref(),
            );
            let pause = handle.demo_pause.clone().unwrap();
            let media_thread =
                demo::spawn_demo_media(media_shared.clone(), stop.clone(), pause, args.freeze_at);
            worker_threads.push(media_thread);
            ui::run(spectrum::ui::RunConfig {
                shared: &shared,
                audio: &handle,
                media: &media_shared,
                lyrics: &lyrics_shared,
                theme_name: theme.name,
                depth,
                start_mode,
                exit_after,
            })
        }
        Selection::Capture(_) | Selection::File(_) => {
            let queue: SampleQueue = audio::new_queue();
            let handle = match selection {
                Selection::File(path) => audio::start_file(path, queue.clone(), args.volume)?,
                Selection::Capture(source) => audio::start_capture(queue.clone(), *source)?,
                // Demo has its own arm above.
                _ => unreachable!("the outer arm excludes the other selections"),
            };
            let analyzer = spectrum::analysis::spawn_analyzer(
                handle.meta_snapshot().sample_rate,
                bands,
                queue.clone(),
                shared.clone(),
                stop.clone(),
                handle.rate_watch.clone(),
            );
            worker_threads.push(analyzer);
            if let Selection::File(path) = selection {
                // FILE PLAYBACK + SIDECAR LYRICS (user 17:15): playback
                // started above; the lyrics come from the same-basename
                // .lrc next to the audio (song.mp3 -> song.lrc), read
                // ONCE here - local file only, no fetch, no fallback
                // architecture. Plain LRC carries LINE timing only;
                // no word onsets are ever invented. Missing/malformed
                // sidecars set an HONEST error state, never guessed
                // lyrics.
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("audio")
                    .to_string();
                let seed = spectrum::media::MediaSnapshot {
                    available: true,
                    title: stem.clone(),
                    artist: "(local file)".into(),
                    album: "".into(),
                    duration_s: None,
                    position_s: 0.0,
                    playing: true,
                };
                // from_snapshot is Some for a valid seed (title and
                // artist are always set); a None falls back honestly
                // to the plain playback path (no lyrics state).
                let key = spectrum::lyrics::TrackKey::from_snapshot(&seed).unwrap_or_else(|| {
                    spectrum::lyrics::TrackKey {
                        artist: "(local file)".into(),
                        title: stem.clone(),
                        album: String::new(),
                        duration_s: None,
                    }
                });
                *media_shared.lock().unwrap_or_else(|e| e.into_inner()) = seed;
                {
                    let mut st = lyrics_shared.lock().unwrap_or_else(|e| e.into_inner());
                    st.key = Some(key);
                    st.generation = st.generation.wrapping_add(1);
                    st.status = load_sidecar_lyrics(path);
                }
                let sink = handle.sink.clone();
                let title = stem.clone();
                let media_thread = spectrum::media::spawn_synthetic_media(
                    media_shared.clone(),
                    stop.clone(),
                    move || spectrum::media::MediaSnapshot {
                        available: true,
                        title: title.clone(),
                        artist: "(local file)".into(),
                        album: "".into(),
                        duration_s: None,
                        position_s: sink
                            .as_ref()
                            .map(|s| s.get_pos().as_secs_f64())
                            .unwrap_or(0.0),
                        playing: sink.as_ref().map(|s| !s.is_paused()).unwrap_or(true),
                    },
                );
                worker_threads.push(media_thread);
                ui::run(spectrum::ui::RunConfig {
                    shared: &shared,
                    audio: &handle,
                    media: &media_shared,
                    lyrics: &lyrics_shared,
                    theme_name: theme.name,
                    depth,
                    start_mode,
                    exit_after,
                })
            } else {
                // Live capture: the SMTC media watcher drives the status
                // bar. There is no lyrics lookup anymore (no network,
                // no autofetch): the lyrics state stays Unavailable and
                // the garden shows that honestly.
                let media_thread = spectrum::media::spawn_media(media_shared.clone(), stop.clone());
                worker_threads.push(media_thread);
                ui::run(spectrum::ui::RunConfig {
                    shared: &shared,
                    audio: &handle,
                    media: &media_shared,
                    lyrics: &lyrics_shared,
                    theme_name: theme.name,
                    depth,
                    start_mode,
                    exit_after,
                })
            }
        }
    };
    stop.store(true, Ordering::Relaxed);
    for t in worker_threads {
        let _ = t.join();
    }
    r
}

/// The plain-launch INTERACTIVE MENU (user 1033): no flags, no hidden
/// behavior - live capture (system output or microphone), file
/// playback and the silent demo live INSIDE it, and after the UI exits
/// control comes BACK here. Every expert flag bypasses this menu
/// entirely and behaves exactly as before.
fn run_interactive_menu(
    args: &Args,
    theme: &spectrum::theme::Theme,
    depth: ColorDepth,
    start_mode: Option<u8>,
    bands: usize,
) -> Result<()> {
    use std::io::Write as _;
    // Inside the OWN TERMINAL's MENU profile (readable font), the
    // visualizer selections delegate to the GARDEN window (fine font)
    // so the menu text stays readable and the garden renders at the
    // reference density. The menu window stays; the garden window
    // opens, runs, closes, and control returns here.
    #[cfg(windows)]
    let in_own_menu = spectrum::own_term::imp::in_own_menu();

    // Helper: in the own-terminal menu, launch the GARDEN window with
    // the original args + the selection-specific CLI args; on failure,
    // fall back to running inline (honest, no fake success).
    #[cfg(windows)]
    fn garden_handoff(original_args: &[std::ffi::OsString], selection_args: &[&str]) -> Result<()> {
        let mut child_args: Vec<std::ffi::OsString> = original_args.to_vec();
        for a in selection_args {
            child_args.push(std::ffi::OsString::from(*a));
        }
        match spectrum::own_term::imp::launch_garden(&child_args) {
            Ok(status) => {
                // QA 19:44: the WT launcher status is the
                // LAUNCHER's exit, not proof the actual Spectrum
                // child exited cleanly. Report the launcher status
                // honestly, without claiming the garden's own exit.
                println!(
                    "garden window closed (launcher exit {}) - back to the menu",
                    status.code().unwrap_or(0)
                );
                Ok(())
            }
            Err(e) => Err(anyhow::anyhow!("garden window unavailable: {e}")),
        }
    }

    loop {
        println!();
        println!("SPECTRUM \u{2014} terminal audio visualizer");
        println!();
        println!("  1) Visualize live system audio (what Windows is playing)");
        println!("  2) Visualize live microphone input");
        println!("  3) Play + visualize an audio file (lyrics: same-name .lrc next to it)");
        println!("  4) Silent demo (no audio)");
        println!("  s) Settings & expert flags");
        println!("  q) Quit");
        println!();
        print!("choice: ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            println!();
            return Ok(());
        }

        #[cfg(windows)]
        let original_args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();

        let outcome = match line.trim() {
            "1" => {
                #[cfg(windows)]
                if in_own_menu {
                    garden_handoff(&original_args, &["--capture", "--source", "system"])
                } else {
                    run_visualizer(
                        &Selection::Capture(CaptureSource::System),
                        args,
                        theme,
                        depth,
                        start_mode,
                        bands,
                    )
                }
                #[cfg(not(windows))]
                run_visualizer(
                    &Selection::Capture(CaptureSource::System),
                    args,
                    theme,
                    depth,
                    start_mode,
                    bands,
                )
            }
            "2" => {
                #[cfg(windows)]
                if in_own_menu {
                    garden_handoff(&original_args, &["--capture", "--source", "mic"])
                } else {
                    run_visualizer(
                        &Selection::Capture(CaptureSource::Microphone),
                        args,
                        theme,
                        depth,
                        start_mode,
                        bands,
                    )
                }
                #[cfg(not(windows))]
                run_visualizer(
                    &Selection::Capture(CaptureSource::Microphone),
                    args,
                    theme,
                    depth,
                    start_mode,
                    bands,
                )
            }
            "3" => match read_path_prompt("audio file path")? {
                #[cfg(windows)]
                Some(path) => {
                    if in_own_menu {
                        let mut child_args = original_args.clone();
                        child_args.push(path.clone().into_os_string());
                        match spectrum::own_term::imp::launch_garden(&child_args) {
                            Ok(status) => {
                                // QA 19:44: launcher status, not the
                                // Spectrum child's own exit.
                                println!(
                                    "garden window closed (launcher exit {}) - back to the menu",
                                    status.code().unwrap_or(0)
                                );
                                Ok(())
                            }
                            Err(e) => {
                                // Honest fallback: run inline at the menu
                                // font (coarser garden, never fake).
                                println!("garden window unavailable: {e}");
                                println!("running in this terminal instead:");
                                run_visualizer(
                                    &Selection::File(path),
                                    args,
                                    theme,
                                    depth,
                                    start_mode,
                                    bands,
                                )
                            }
                        }
                    } else {
                        run_visualizer(
                            &Selection::File(path),
                            args,
                            theme,
                            depth,
                            start_mode,
                            bands,
                        )
                    }
                }
                #[cfg(not(windows))]
                Some(path) => run_visualizer(
                    &Selection::File(path),
                    args,
                    theme,
                    depth,
                    start_mode,
                    bands,
                ),
                None => Ok(()),
            },
            "4" => {
                #[cfg(windows)]
                if in_own_menu {
                    garden_handoff(&original_args, &["--demo"])
                } else {
                    run_visualizer(&Selection::Demo, args, theme, depth, start_mode, bands)
                }
                #[cfg(not(windows))]
                run_visualizer(&Selection::Demo, args, theme, depth, start_mode, bands)
            }
            "s" | "S" | "settings" => {
                print_settings(args);
                Ok(())
            }
            "q" | "Q" | "quit" => return Ok(()),
            _ => {
                println!("unrecognized choice - try 1, 2, 3, 4, s or q");
                Ok(())
            }
        };
        // Back to the menu after the UI exits; errors are honest and
        // non-fatal to the menu loop.
        if let Err(e) = outcome {
            println!("error: {e:#}");
        }
    }
}

/// Prompt for a path; empty input returns None (back to the menu).
/// Windows' "Copy as path" yields QUOTED paths - matching quotes are
/// stripped so pasted input just works. EOF also returns None (the
/// menu loop itself quits on a closed stdin).
fn read_path_prompt(what: &str) -> Result<Option<std::path::PathBuf>> {
    use std::io::Write as _;
    print!("{what}: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let t = line.trim().trim_matches(|c| c == '"' || c == '\'');
    if t.is_empty() {
        return Ok(None);
    }
    Ok(Some(std::path::PathBuf::from(t)))
}

/// The discoverable settings page: current defaults plus the expert
/// flags that bypass the menu entirely.
fn print_settings(args: &Args) {
    println!();
    println!("current defaults (override with flags):");
    println!(
        "  theme {} \u{b7} colors {} \u{b7} bars {} \u{b7} sensitivity {:.2} \u{b7} smoothing {:.2}",
        args.theme, args.colors, args.bars, args.sensitivity, args.smoothing
    );
    println!();
    println!("expert flags (any flag bypasses this menu entirely):");
    println!("  --theme NAME                        color theme (--help lists them)");
    println!("  --mode 1-5                          spectrum \u{b7} oscilloscope \u{b7} radial \u{b7} mirror \u{b7} lyrics garden");
    println!("  --colors true|256|16                color depth");
    println!("  --bars N                            spectrum bands (8-256)");
    println!("  --sensitivity F                     input gain");
    println!("  --smoothing F                       bar release");
    println!("  --source system|mic                 live capture source (menu: 1 or 2)");
    println!("  --demo --demo-lyric W --freeze-at S silent synthetic demo");
    println!("  --exit-after SECS                   auto-quit");
    println!("  full reference: spectrum --help");
}

/// Reject NaN/infinite values from the command line with a clear error.
fn ensure_finite(name: &str, v: f32) -> Result<f32> {
    if v.is_finite() {
        Ok(v)
    } else {
        bail!("--{name} must be a finite number, got {v}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finite_values_pass() {
        assert_eq!(ensure_finite("sensitivity", 1.5).unwrap(), 1.5);
        assert_eq!(ensure_finite("smoothing", 0.0).unwrap(), 0.0);
    }

    #[test]
    fn non_finite_values_rejected() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(ensure_finite("sensitivity", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn exit_after_validation() {
        // The clamp logic used by main; mirrored here for regression.
        fn ok(secs: Option<f32>) -> bool {
            secs.map(|s| s.is_finite() && s > 0.0).unwrap_or(true)
        }
        assert!(ok(None));
        assert!(ok(Some(5.0)));
        assert!(!ok(Some(0.0)));
        assert!(!ok(Some(-1.0)));
        assert!(!ok(Some(f32::NAN)));
    }

    // QA 1749: same-basename .lrc sidecar loader INTEGRATION tests on
    // REAL temporary files - the exact flows the file playback uses.
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "spectrum-sidecar-test-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&d).expect("temp dir");
        d
    }

    #[test]
    fn sidecar_valid_lrc_loads_synced_lines() {
        let dir = temp_dir("valid");
        let audio = dir.join("song.mp3");
        std::fs::write(&audio, b"").expect("audio placeholder");
        std::fs::write(
            dir.join("song.lrc"),
            "[00:01.00]First line\n[00:04.50]Second line\n",
        )
        .expect("sidecar");
        match load_sidecar_lyrics(&audio) {
            spectrum::lyrics::LyricsStatus::Synced(lines) => {
                assert_eq!(lines.len(), 2);
                assert_eq!(lines[0].text, "First line");
                assert!((lines[1].time_s - 4.5).abs() < 1e-9);
            }
            other => panic!("expected Synced, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecar_paths_with_spaces_dots_and_uppercase_extensions_pair() {
        let dir = temp_dir("paths");
        std::fs::create_dir_all(dir.join("My Album (2024)")).expect("dir with spaces");
        let audio = dir.join("My Album (2024)").join("Track 02 (Demo).MP3");
        std::fs::write(&audio, b"").expect("audio placeholder");
        // The sidecar: SAME basename, SAME directory, lowercase .lrc
        // extension - exactly what with_extension produces.
        std::fs::write(
            dir.join("My Album (2024)").join("Track 02 (Demo).lrc"),
            "[00:02.00]spaced dot path line\n",
        )
        .expect("sidecar");
        match load_sidecar_lyrics(&audio) {
            spectrum::lyrics::LyricsStatus::Synced(lines) => {
                assert_eq!(lines.len(), 1);
                assert_eq!(lines[0].text, "spaced dot path line");
            }
            other => panic!("expected Synced, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecar_missing_is_an_honest_error_never_guessed_lyrics() {
        let dir = temp_dir("missing");
        let audio = dir.join("song.wav");
        std::fs::write(&audio, b"").expect("audio placeholder");
        match load_sidecar_lyrics(&audio) {
            spectrum::lyrics::LyricsStatus::Error(msg) => {
                assert!(
                    msg.contains("no lyrics sidecar") && msg.contains("song.lrc"),
                    "the error must say exactly what was expected and where: {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecar_without_timestamps_is_honest_unsynced_plain_text() {
        let dir = temp_dir("plain");
        let audio = dir.join("song.flac");
        std::fs::write(&audio, b"").expect("audio placeholder");
        std::fs::write(
            dir.join("song.lrc"),
            "just some\nplain lyric text\nno timestamps\n",
        )
        .expect("sidecar");
        match load_sidecar_lyrics(&audio) {
            // Honest unsynced: never fake-synced, never guessed times.
            spectrum::lyrics::LyricsStatus::Plain(text) => {
                assert!(text.contains("plain lyric text"));
                assert!(text.contains("no timestamps"));
            }
            other => panic!("expected Plain, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecar_malformed_timestamps_never_masquerade_as_synced() {
        let dir = temp_dir("malformed");
        let audio = dir.join("song.ogg");
        std::fs::write(&audio, b"").expect("audio placeholder");
        // Only malformed stamps: the bad lines are skipped; the one
        // valid line stays synced; nothing fake-syncs, nothing panics.
        std::fs::write(
            dir.join("song.lrc"),
            "[NaN:30.0]bad\n[-1:30.0]bad\n[00:02.5]good\n",
        )
        .expect("sidecar");
        match load_sidecar_lyrics(&audio) {
            spectrum::lyrics::LyricsStatus::Synced(lines) => {
                assert_eq!(lines.len(), 1, "only the valid line survives");
                assert_eq!(lines[0].text, "good");
                assert!((lines[0].time_s - 2.5).abs() < 1e-9);
            }
            other => panic!("expected Synced with the one valid line, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecar_empty_file_is_an_honest_error() {
        let dir = temp_dir("empty");
        let audio = dir.join("song.m4a");
        std::fs::write(&audio, b"").expect("audio placeholder");
        std::fs::write(dir.join("song.lrc"), "").expect("sidecar");
        match load_sidecar_lyrics(&audio) {
            spectrum::lyrics::LyricsStatus::Error(msg) => {
                assert!(msg.contains("empty"), "clear reason: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
