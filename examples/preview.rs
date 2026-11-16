//! Headless visual preview: renders every visualization into ratatui
//! buffers with synthetic input and prints the resulting cell contents,
//! so output can be inspected without an interactive terminal.
//!
//! Run with: cargo run --example preview
//! (also simulates resizing by re-rendering with shared scratch buffers)

use std::collections::VecDeque;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use spectrum::theme::{self, ColorDepth};
use spectrum::visualizers::{self, RenderScratch};

fn synthetic_bars(n: usize) -> Vec<f32> {
    // Bass-heavy curve with a mid bump and sparkle tail - enough shape
    // to judge the log-spaced band mapping visually.
    (0..n)
        .map(|i| {
            let t = i as f32 / (n - 1).max(1) as f32;
            let bass = (-t * 6.0).exp();
            let mid = 0.7 * (-((t - 0.45) * 8.0).powi(2)).exp();
            let air = 0.25 * (-((t - 0.85) * 10.0).powi(2)).exp();
            (bass * 0.9 + mid + air).clamp(0.0, 1.0)
        })
        .collect()
}

fn synthetic_wave(len: usize) -> Vec<f32> {
    let mut v = Vec::with_capacity(len);
    for i in 0..len {
        let t = i as f32 / 44_100.0;
        let s = (2.0 * std::f32::consts::PI * 220.0 * t).sin() * 0.55
            + (2.0 * std::f32::consts::PI * 55.0 * t).sin() * 0.35;
        v.push(s);
    }
    v
}

fn luminance(color: ratatui::style::Color) -> f32 {
    match color {
        ratatui::style::Color::Rgb(r, g, b) => {
            (r as u16 + g as u16 + b as u16) as f32 / (3.0 * 255.0)
        }
        _ => 0.0,
    }
}

fn dump(title: &str, buf: &Buffer) {
    println!("--- {title} ---");
    let area = buf.area();
    for y in 0..area.height {
        let mut row = String::new();
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            let sym = cell.symbol();
            if sym != " " {
                row.push_str(sym);
            } else {
                // Spaces with a background color (spectrogram cells) are
                // shown as density characters so the heatmap is visible.
                let lum = luminance(cell.bg);
                row.push(if lum > 0.66 {
                    '#'
                } else if lum > 0.33 {
                    '+'
                } else if lum > 0.05 {
                    '-'
                } else {
                    '.'
                });
            }
        }
        println!("|{row}|");
    }
    println!();
}

fn render_mode(
    mode: &str,
    area: Rect,
    theme: &theme::Theme,
    scratch: &mut RenderScratch,
) -> Buffer {
    let bars = synthetic_bars(64);
    let wave = synthetic_wave(768);
    let mut buf = Buffer::empty(area);
    let mut peaks = Vec::new();
    match mode {
        "spectrum" => visualizers::render_spectrum(
            &mut buf,
            area,
            &bars,
            theme,
            &mut peaks,
            scratch,
            1.0 / 60.0,
        ),
        "oscilloscope" => {
            visualizers::render_oscilloscope(&mut buf, area, &wave, theme, 0.9, scratch)
        }
        "spectrogram" => {
            let mut cols: VecDeque<Vec<f32>> = VecDeque::new();
            for f in 0..area.width.min(24) {
                let shift = (f % 8) as f32 / 8.0;
                let col: Vec<f32> = bars
                    .iter()
                    .skip((f % 16) as usize)
                    .map(|v| (v * (1.0 - shift)).clamp(0.0, 1.0))
                    .collect();
                cols.push_back(col);
            }
            visualizers::render_spectrogram(&mut buf, area, &cols, theme)
        }
        "combo" => {
            visualizers::render_spectrum(
                &mut buf,
                area,
                &bars,
                theme,
                &mut peaks,
                scratch,
                1.0 / 60.0,
            );
            let wave_h = (area.height / 2).clamp(2, area.height);
            let wave_area = Rect::new(
                area.x,
                area.y + (area.height - wave_h) / 2,
                area.width,
                wave_h,
            );
            visualizers::render_oscilloscope(&mut buf, wave_area, &wave, theme, 0.9, scratch)
        }
        "mirror" => visualizers::render_mirror(&mut buf, area, &bars, theme, scratch),
        "lyrics" => {
            // The garden with the demo fixture (deterministic, frozen).
            let lines = spectrum::demo::demo_lyric_lines();
            let view = spectrum::garden::GardenView {
                title: spectrum::demo::DEMO_TRACK_TITLE,
                artist: spectrum::demo::DEMO_TRACK_ARTIST,
                lines: Some((&lines, Some(2))),
                plain: None,
                state: spectrum::garden::GardenState::Synced,
                energy: 0.5,
                time_s: 3.6,
                since_line_change: 2.0,
            };
            spectrum::garden::render_garden(&mut buf, area, &view);
        }
        _ => unreachable!(),
    }
    buf
}

fn main() {
    let theme = theme::find_with_depth("spectrum", ColorDepth::True).expect("theme");

    println!("SPECTRUM visual preview (dots = empty cells)\n");

    let modes = [
        "spectrum",
        "oscilloscope",
        "spectrogram",
        "combo",
        "mirror",
        "lyrics",
    ];

    for &mode in &modes {
        // Normal terminal size.
        let mut scratch = RenderScratch::new();
        let buf = render_mode(mode, Rect::new(0, 0, 72, 16), &theme, &mut scratch);
        dump(&format!("{mode} @ 72x16 (normal)"), &buf);

        // Small terminal.
        let buf = render_mode(mode, Rect::new(0, 0, 26, 7), &theme, &mut scratch);
        dump(&format!("{mode} @ 26x7 (small)"), &buf);
    }

    // Resize sequence with shared scratch buffers: large -> small -> wide,
    // all through the same peaks/scratch state like a real session.
    let mut scratch = RenderScratch::new();
    let sizes = [(120u16, 30u16), (24, 6), (100, 12)];
    let bars = synthetic_bars(64);
    for &(w, h) in &sizes {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        let mut peaks = Vec::new();
        visualizers::render_spectrum(
            &mut buf,
            area,
            &bars,
            &theme,
            &mut peaks,
            &mut scratch,
            1.0 / 60.0,
        );
        let nonblank = (0..area.width * area.height)
            .filter(|i| buf.content[*i as usize].symbol() != " ")
            .count();
        println!("resize {w}x{h}: {nonblank} non-blank cells");
    }

    // Silence: bars decayed to near zero render an (almost) empty field
    // with only peak caps and a flat baseline.
    let area = Rect::new(0, 0, 60, 10);
    let mut buf = Buffer::empty(area);
    let mut peaks = vec![0.0; 60];
    let silent = vec![0.02; 64];
    let mut scratch2 = RenderScratch::new();
    visualizers::render_spectrum(
        &mut buf,
        area,
        &silent,
        &theme,
        &mut peaks,
        &mut scratch2,
        1.0 / 60.0,
    );
    dump("spectrum @ silence (decayed)", &buf);
}
