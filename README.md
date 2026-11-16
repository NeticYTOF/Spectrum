# SPECTRUM

A terminal-native audio visualizer written in Rust. Play a local audio file
or capture live audio and watch it as real-time FFT visualizations rendered
with Unicode blocks, braille dots and ANSI color.

```
 ▄▄                    ▄▄
 ██ ▀▀▀█▄   ▄▄▄▄▄▄▄▄▄▄ ██   ▄▄▄▄
 ████████ ▄▄███████████▀▀  ▄▄████
 ███████████████████████████████▀
```

## Features

- **Spectrum analyzer** - log-spaced frequency bars with partial-block
  tops and peak-hold caps
- **Oscilloscope** - phase-triggered braille waveform (2x4 dot
  resolution per cell)
- **Spectrogram** - scrolling frequency heatmap, newest at the right edge
- **Bars + Wave** - oscilloscope overlaid on the spectrum bars
- **Mirrored spectrum** - bars growing outward from a center line
- **Beat detection** - spectral flux drives a border/`♪` flash
- **Color themes** - spectrum, fire, ice, matrix, vapor; gradients get a
  deliberate ordered-stipple dither between steps (visible in truecolor,
  consistent in 256/16)
- **Live tuning** - sensitivity, smoothing and volume adjustable while
  running; all animation is frame-rate independent

## Install

Requires [Rust](https://rustup.rs) 1.74 or newer.

```
git clone https://github.com/NeticYTOF/Spectrum.git
cd Spectrum
cargo build --release
```

Use `cargo build --release -j 2` if you want to bound build-time memory.

A truecolor terminal is recommended (Windows Terminal, WezTerm, kitty,
iTerm2, Alacritty, modern gnome-terminal). The signature gradient stipple
renders in every depth; on 256/16-color terminals run with `--colors 256`
or `--colors 16` for the nearest-palette equivalent.

## Usage

```
# Visualize whatever Windows is playing (default system-output loopback)
spectrum --capture

# Play and visualize a file (mp3, wav, ogg vorbis, flac)
spectrum song.mp3

# Silent synthetic demo: all visuals, no audio hardware touched
spectrum --demo

# With options
spectrum song.mp3 --theme fire --bars 96 --sensitivity 1.4
spectrum --capture --source mic          # explicit microphone
spectrum --demo --mode 4 --exit-after 10  # scripted QA / screenshots
```

| Option | Default | Meaning |
|---|---|---|
| `FILE` | - | Audio file to play and visualize |
| `-c, --capture` | - | Visualize live audio (see `--source`) |
| `--demo` | off | Silent synthetic generator; no audio hardware |
| `--source NAME` | system | `system` (Windows output loopback) or `mic` |
| `--mode N` | 1 | Start in mode 1-5 (spectrum/scope/spectrogram/combo/mirror) |
| `--exit-after SECS` | - | Exit cleanly after N seconds (scripted QA) |
| `-t, --theme NAME` | spectrum | spectrum, fire, ice, matrix, vapor |
| `--bars N` | 64 | Frequency bands (8..=256) |
| `--sensitivity F` | 1.0 | FFT display gain (0.1..=4.0, must be finite) |
| `--smoothing F` | 0.12 | Bar fall speed (0.01 slow .. 0.95 instant) |
| `--colors MODE` | true | true (24-bit), 256 or 16 (all stipple-dithered) |

Capture honesty: `--source system` opens the default *output* device via
WASAPI loopback and never silently falls back to the microphone - if it
cannot open, the error says what to try (`--source mic`). While running,
the default output device is re-checked every second; switching speakers /
headphones transparently rebuilds the stream (the status bar shows the live
device and any reconnect state).

## Controls

| Key | Action |
|---|---|
| `1`-`5` | spectrum / oscilloscope / spectrogram / combo / mirror |
| `Tab` / `Shift+Tab` | cycle visualizations |
| `Space` | pause / resume audio |
| `Up` / `Down` | volume (file mode) |
| `+` / `-` | sensitivity |
| `[` / `]` | bar smoothing (fall speed) |
| `t` | cycle color theme |
| `h` / `?` | help overlay |
| `q` / `Esc` | quit |

## Platform notes

- **Windows**: `--capture` opens the default *output* device as a WASAPI
  loopback input, visualizing whatever the system is playing; it falls
  back to the microphone when loopback is unavailable.
- **Linux**: capture uses the default input device - select a PulseAudio
  or PipeWire *monitor* source to visualize system playback.
- **macOS**: only microphone capture is supported (CoreAudio exposes no
  system loopback).
- Playback uses rodio; decoding supports mp3, wav, ogg vorbis and flac.

## Development

```
cargo test                    # unit + regression tests
cargo clippy                  # lint (kept warning-free)
cargo run --example preview   # headless glyph-level check of all renderers
cargo run --release -- --demo # silent visual check without audio
cargo run -- song.mp3          # run from source
```

Screenshots in `docs/screenshots/` were captured from the real running
application (`--demo`, one window per mode/theme/depth) via
`scripts/capture_all.ps1`.

Architecture: `audio` (playback tap + capture) feeds a lock-batched sample
queue to `analysis` (Hann-windowed FFT, log bands, spectral-flux beat
detection, waveform trigger) on its own thread; `ui` renders with ratatui
using allocation-free hot paths and shared-frame `Arc` handoff.

## License

MIT
