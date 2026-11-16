//! SPECTRUM - a terminal-native audio visualizer.
//!
//! Library modules: audio capture/playback, FFT analysis, themes and
//! renderers. The binary target adds the terminal UI loop (`ui`).

pub mod analysis;
pub mod audio;
pub mod theme;
pub mod ui;
pub mod visualizers;
