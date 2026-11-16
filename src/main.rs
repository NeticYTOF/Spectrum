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
}

fn main() -> Result<()> {
    let args = Args::parse();

    let inputs = [args.file.is_some(), args.capture, args.demo];
    if inputs.iter().filter(|b| **b).count() > 1 {
        bail!("pass only one of: an audio file, --capture, or --demo");
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
        Some(m) if (1..=6).contains(&m) => Some(m),
        Some(m) => bail!("--mode must be 1-6, got {m}"),
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
        let pause = handle.demo_pause.clone().unwrap();
        let media_thread =
            demo::spawn_demo_media(media_shared.clone(), stop.clone(), pause, args.freeze_at);
        worker_threads.push(media_thread);
        // The demo fixture's lyrics ship locally (no network, no
        // copyrighted content): seed the lyrics state directly.
        {
            let mut st = lyrics_shared.lock().unwrap();
            st.status = spectrum::lyrics::LyricsStatus::Synced(demo::demo_lyric_lines());
        }
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
    } else {
        let queue: SampleQueue = audio::new_queue();
        let handle = if let Some(path) = &args.file {
            audio::start_file(path, queue.clone())?
        } else {
            let source = CaptureSource::parse(&args.source).with_context(|| {
                format!(
                    "unknown capture source '{}'; expected system or mic",
                    args.source
                )
            })?;
            audio::start_capture(queue.clone(), source)?
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
        let media_thread = spectrum::media::spawn_media(media_shared.clone(), stop.clone());
        worker_threads.push(media_thread);
        let lyrics_thread = spectrum::lyrics::spawn_lyrics(
            media_shared.clone(),
            lyrics_shared.clone(),
            stop.clone(),
        );
        worker_threads.push(lyrics_thread);
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
    };

    result
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
}
