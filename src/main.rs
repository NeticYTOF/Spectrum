mod analysis;
mod audio;
mod theme;
mod ui;
mod visualizers;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use clap::Parser;

use crate::analysis::{FrameAnalysis, Params, SharedState, DEFAULT_BANDS};
use crate::audio::SampleQueue;

#[derive(Parser, Debug)]
#[command(
    name = "spectrum",
    version,
    about = "SPECTRUM \u{2014} a terminal-native audio visualizer"
)]
struct Args {
    /// Audio file to play and visualize (mp3, wav, ogg, flac)
    file: Option<PathBuf>,

    /// Visualize live audio instead of playing a file (system loopback on Windows)
    #[arg(short, long)]
    capture: bool,

    /// Color theme: spectrum, fire, ice, matrix, vapor
    #[arg(short, long, default_value = "spectrum")]
    theme: String,

    /// Number of frequency bands (8..=256)
    #[arg(long, default_value_t = DEFAULT_BANDS)]
    bars: usize,

    /// FFT display sensitivity (0.1..=4.0)
    #[arg(long, default_value_t = 1.0)]
    sensitivity: f32,

    /// Bar fall speed: 0.01 slow .. 0.95 instant
    #[arg(long, default_value_t = 0.12)]
    smoothing: f32,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.capture && args.file.is_some() {
        bail!("pass either an audio file or --capture, not both");
    }

    let theme = theme::find(&args.theme).with_context(|| {
        format!(
            "unknown theme '{}'; available themes: {}",
            args.theme,
            theme::names().join(", ")
        )
    })?;

    // Clamp once here so every component sees consistent values.
    let bands = args.bars.clamp(8, 256);

    let queue: SampleQueue = audio::new_queue();
    let handle = match &args.file {
        Some(path) => audio::start_file(path, queue.clone())?,
        None => audio::start_capture(queue.clone())?,
    };

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
    let analyzer = analysis::spawn_analyzer(
        handle.meta.sample_rate,
        bands,
        queue.clone(),
        shared.clone(),
        stop.clone(),
    );

    // ui::run restores the terminal on all exits, including errors.
    let result = ui::run(&shared, &handle, theme.name);

    stop.store(true, Ordering::Relaxed);
    let _ = analyzer.join();

    result
}
