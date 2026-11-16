//! Color themes for the visualizers.
//!
//! Each theme is a set of gradient stops that get baked into a dense
//! lookup table (dim -> hot). All renderers map a 0..=1 intensity to a
//! color with [`Theme::at`] (exact) or [`Theme::at_dither`] (which adds
//! ordered dithering when the theme is built for a limited color depth).

use std::sync::OnceLock;

use ratatui::style::Color;

/// Number of precomputed gradient entries per theme.
const GRADIENT_STEPS: usize = 48;

/// One gradient color stop in RGB.
type Stop = (u8, u8, u8);

/// 4x4 Bayer matrix for ordered dithering; thresholds tile the screen.
const BAYER4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// Ordered-dither a single xterm-256 channel within the 6x6x6 cube.
fn dither_cube_channel(v: u8, th: f32) -> u8 {
    let f = v as f32 / 51.0;
    let lo = f.floor();
    if f - lo > th {
        (lo as u8 + 1).min(5)
    } else {
        lo as u8
    }
}

/// Ordered-dither threshold in [0, 1) for a screen position.
fn bayer_threshold(x: u16, y: u16) -> f32 {
    (BAYER4[(y % 4) as usize][(x % 4) as usize] as f32 + 0.5) / 16.0
}

/// Ordered-dither one RGB triple for xterm-256. Near-neutral colors use
/// the finer 24-step grayscale ramp (232..255) instead of the coarse cube,
/// which smooths the dark ends of gradients like theme shadows.
fn dither_p256(rgb: [u8; 3], th: f32) -> Color {
    let [r, g, b] = rgb;
    let spread = r.max(g).max(b).saturating_sub(r.min(g).min(b));
    if spread <= 4 {
        // Grayscale ramp: 24 entries, 8 + 10*i.
        let avg = ((r as u16 + g as u16 + b as u16) / 3) as u8;
        let f = (avg.saturating_sub(8) as f32) / 10.0;
        let lo = f.floor().min(23.0);
        let idx = if f - lo > th {
            (lo + 1.0).min(23.0)
        } else {
            lo
        };
        Color::Indexed(232 + idx as u8)
    } else {
        Color::Indexed(
            16 + 36 * dither_cube_channel(r, th)
                + 6 * dither_cube_channel(g, th)
                + dither_cube_channel(b, th),
        )
    }
}

/// Terminal color depth a theme is rendered for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorDepth {
    /// 24-bit RGB (the default; no quantization or dithering).
    True,
    /// xterm-256: nearest-palette colors plus ordered dithering.
    P256,
    /// 16 ANSI colors: nearest-palette colors plus ordered dithering.
    Ansi16,
}

impl ColorDepth {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "true" | "truecolor" | "24" => Some(ColorDepth::True),
            "256" => Some(ColorDepth::P256),
            "16" | "ansi16" => Some(ColorDepth::Ansi16),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ColorDepth::True => "true",
            ColorDepth::P256 => "256",
            ColorDepth::Ansi16 => "16",
        }
    }
}

/// The standard 16 ANSI/VGA colors, indices 0..=15.
const ANSI16_RGB: [[u8; 3]; 16] = [
    [0, 0, 0],       // 0 black
    [128, 0, 0],     // 1 maroon
    [0, 128, 0],     // 2 green
    [128, 128, 0],   // 3 olive
    [0, 0, 128],     // 4 navy
    [128, 0, 128],   // 5 purple
    [0, 128, 128],   // 6 teal
    [192, 192, 192], // 7 silver
    [128, 128, 128], // 8 gray
    [255, 0, 0],     // 9 red
    [0, 255, 0],     // 10 lime
    [255, 255, 0],   // 11 yellow
    [0, 0, 255],     // 12 blue
    [255, 0, 255],   // 13 magenta
    [0, 255, 255],   // 14 cyan
    [255, 255, 255], // 15 white
];

/// Full xterm-256 palette as RGB triples.
fn xterm_palette() -> &'static [[u8; 3]; 256] {
    static PALETTE: OnceLock<[[u8; 3]; 256]> = OnceLock::new();
    PALETTE.get_or_init(|| {
        let mut p = [[0u8; 3]; 256];
        for (i, rgb) in p.iter_mut().enumerate().take(16) {
            *rgb = ANSI16_RGB[i];
        }
        let levels = [0u8, 95, 135, 175, 215, 255];
        for r in 0..6 {
            for g in 0..6 {
                for b in 0..6 {
                    p[16 + 36 * r + 6 * g + b] = [levels[r], levels[g], levels[b]];
                }
            }
        }
        for i in 0..24u16 {
            let v = (8 + i * 10) as u8;
            p[232 + i as usize] = [v, v, v];
        }
        p
    })
}

fn dist2(rgb: [u8; 3], other: [u8; 3]) -> f32 {
    let dr = rgb[0] as f32 - other[0] as f32;
    let dg = rgb[1] as f32 - other[1] as f32;
    let db = rgb[2] as f32 - other[2] as f32;
    dr * dr + dg * dg + db * db
}

fn to_rgb(color: Color) -> [u8; 3] {
    match color {
        Color::Rgb(r, g, b) => [r, g, b],
        _ => [255, 255, 255],
    }
}

/// Nearest xterm-256 palette index for an RGB color.
fn nearest_xterm(rgb: [u8; 3]) -> u8 {
    let palette = xterm_palette();
    let mut best = 0usize;
    let mut best_d = f32::INFINITY;
    for (i, cand) in palette.iter().enumerate() {
        let d = dist2(rgb, *cand);
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best as u8
}

/// The two nearest ANSI-16 indices (distinct) and the mix weight of the
/// second: w is the fraction of the second color an ideal blend would use.
fn nearest_ansi16_pair(rgb: [u8; 3]) -> (u8, u8, f32) {
    let mut order: [(f32, usize); 16] = [(0.0, 0); 16];
    for (i, cand) in ANSI16_RGB.iter().enumerate() {
        order[i] = (dist2(rgb, *cand), i);
    }
    order.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let (d1, i1) = order[0];
    let (d2, i2) = order[1];
    let w = if d1 + d2 <= f32::EPSILON {
        0.0
    } else {
        (d1 / (d1 + d2)).clamp(0.0, 1.0)
    };
    (i1 as u8, i2 as u8, w)
}

#[derive(Clone)]
pub struct Theme {
    pub name: &'static str,
    depth: ColorDepth,
    stops: &'static [Stop],
    /// Depth-quantized gradient lookup (nearest palette color per step).
    gradient: Vec<Color>,
    /// For Ansi16: per-step (primary, secondary, mix weight) so dithering
    /// can pick between the two nearest palette entries. Empty otherwise.
    dither_pairs: Vec<(u8, u8, f32)>,
}

impl Theme {
    /// Color at intensity `t` (0.0 = dim, 1.0 = hot). Clamped, panic-free.
    pub fn at(&self, t: f32) -> Color {
        if self.gradient.is_empty() {
            return Color::White;
        }
        let last = self.gradient.len() - 1;
        let idx = (t.clamp(0.0, 1.0) * last as f32).round() as usize;
        self.gradient[idx.min(last)]
    }

    /// Color at intensity `t` for the screen position (x, y), with ordered
    /// dithering applied for limited color depths. True-color themes
    /// return the exact gradient color, unchanged.
    pub fn at_dither(&self, t: f32, x: u16, y: u16) -> Color {
        match self.depth {
            ColorDepth::True => self.at(t),
            ColorDepth::P256 => {
                let rgb = to_rgb(sample_stops(self.stops, t));
                dither_p256(rgb, bayer_threshold(x, y))
            }
            ColorDepth::Ansi16 => {
                let step = self.gradient_step(t);
                let (c1, c2, w) = self.dither_pairs[step.min(self.dither_pairs.len() - 1)];
                let th = bayer_threshold(x, y);
                Color::Indexed(if w > th { c2 } else { c1 })
            }
        }
    }

    fn gradient_step(&self, t: f32) -> usize {
        let last = self.gradient.len().saturating_sub(1);
        (t.clamp(0.0, 1.0) * last as f32).round() as usize
    }

    /// Mid-high accent used for UI chrome (titles, borders, status).
    pub fn accent(&self) -> Color {
        self.at(0.6)
    }

    /// Hottest color; used for the beat flash.
    pub fn beat(&self) -> Color {
        self.at(1.0)
    }
}

const THEMES: &[(&str, &[Stop])] = &[
    (
        "spectrum",
        &[
            (8, 8, 32),
            (30, 30, 140),
            (20, 160, 230),
            (40, 220, 120),
            (240, 230, 80),
            (250, 130, 30),
            (255, 60, 60),
            (255, 220, 200),
        ],
    ),
    (
        "fire",
        &[
            (12, 4, 4),
            (110, 10, 0),
            (220, 45, 0),
            (255, 120, 10),
            (255, 205, 60),
            (255, 250, 230),
        ],
    ),
    (
        "ice",
        &[
            (4, 8, 16),
            (8, 50, 110),
            (0, 120, 220),
            (70, 210, 240),
            (225, 250, 255),
        ],
    ),
    (
        "matrix",
        &[
            (0, 12, 0),
            (0, 70, 18),
            (0, 150, 40),
            (90, 255, 130),
            (235, 255, 235),
        ],
    ),
    (
        "vapor",
        &[(24, 6, 40), (130, 20, 190), (255, 80, 200), (120, 240, 255)],
    ),
];

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

fn sample_stops(stops: &[Stop], t: f32) -> Color {
    if stops.is_empty() {
        return Color::White;
    }
    if stops.len() == 1 {
        let (r, g, b) = stops[0];
        return Color::Rgb(r, g, b);
    }
    let x = t.clamp(0.0, 1.0) * (stops.len() - 1) as f32;
    let i = x.floor() as usize;
    let f = x - i as f32;
    let j = (i + 1).min(stops.len() - 1);
    let (r1, g1, b1) = stops[i];
    let (r2, g2, b2) = stops[j];
    Color::Rgb(lerp_u8(r1, r2, f), lerp_u8(g1, g2, f), lerp_u8(b1, b2, f))
}

fn build_theme(name: &'static str, stops: &'static [Stop], depth: ColorDepth) -> Theme {
    let gradient: Vec<Color> = (0..GRADIENT_STEPS)
        .map(|i| {
            let rgb = to_rgb(sample_stops(stops, i as f32 / (GRADIENT_STEPS - 1) as f32));
            match depth {
                ColorDepth::True => Color::Rgb(rgb[0], rgb[1], rgb[2]),
                ColorDepth::P256 => Color::Indexed(nearest_xterm(rgb)),
                ColorDepth::Ansi16 => Color::Indexed(nearest_ansi16_pair(rgb).0),
            }
        })
        .collect();
    let dither_pairs = match depth {
        ColorDepth::Ansi16 => (0..GRADIENT_STEPS)
            .map(|i| {
                let rgb = to_rgb(sample_stops(stops, i as f32 / (GRADIENT_STEPS - 1) as f32));
                nearest_ansi16_pair(rgb)
            })
            .collect(),
        _ => Vec::new(),
    };
    Theme {
        name,
        depth,
        stops,
        gradient,
        dither_pairs,
    }
}

/// All available themes at true color depth, gradients precomputed.
/// Kept for tests and external tooling.
#[cfg(test)]
#[allow(dead_code)]
pub fn all() -> Vec<Theme> {
    all_with_depth(ColorDepth::True)
}

/// All available themes quantized for the given color depth.
pub fn all_with_depth(depth: ColorDepth) -> Vec<Theme> {
    THEMES
        .iter()
        .map(|(name, stops)| build_theme(name, stops, depth))
        .collect()
}

/// Look up a theme by name (case-insensitive, surrounding whitespace ignored).
#[cfg(test)]
#[allow(dead_code)]
pub fn find(name: &str) -> Option<Theme> {
    find_with_depth(name, ColorDepth::True)
}

/// Look up a theme by name for the given color depth.
pub fn find_with_depth(name: &str, depth: ColorDepth) -> Option<Theme> {
    let wanted = name.trim().to_lowercase();
    all_with_depth(depth).into_iter().find(|t| t.name == wanted)
}

/// Names of all themes, for error messages and help output.
pub fn names() -> Vec<&'static str> {
    THEMES.iter().map(|(n, _)| *n).collect()
}

/// Linear interpolation between two colors (falls back to a hard switch
/// for non-RGB colors).
pub fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            Color::Rgb(lerp_u8(r1, r2, t), lerp_u8(g1, g2, t), lerp_u8(b1, b2, t))
        }
        _ => {
            if t < 0.5 {
                a
            } else {
                b
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_covers_extremes() {
        let t = find("fire").expect("fire theme exists");
        assert_eq!(t.gradient.len(), GRADIENT_STEPS);
        assert_eq!(t.at(0.0), Color::Rgb(12, 4, 4));
        assert_eq!(t.at(1.0), Color::Rgb(255, 250, 230));
    }

    #[test]
    fn at_clamps_out_of_range() {
        let t = find("ice").unwrap();
        let lo = t.at(-1.0);
        let hi = t.at(2.0);
        assert_eq!(lo, t.at(0.0));
        assert_eq!(hi, t.at(1.0));
    }

    #[test]
    fn find_is_case_insensitive() {
        assert!(find("FIRE").is_some());
        assert!(find("  Fire ").is_some());
        assert!(find("nope").is_none());
    }

    #[test]
    fn lerp_midpoint() {
        let a = Color::Rgb(0, 0, 0);
        let b = Color::Rgb(100, 200, 50);
        assert_eq!(lerp_color(a, b, 0.5), Color::Rgb(50, 100, 25));
    }

    #[test]
    fn p256_dither_uses_ramp_for_neutral_grays() {
        // A dark neutral gray below the cube's floor dithers in the
        // grayscale ramp (232..255), not the cube.
        let lo = dither_p256([10, 10, 10], 0.1);
        let hi = dither_p256([10, 10, 10], 0.9);
        assert!(matches!(lo, Color::Indexed(i) if i >= 232), "{lo:?}");
        assert!(matches!(hi, Color::Indexed(i) if i >= 232), "{hi:?}");
        assert_ne!(lo, hi, "dithering must vary with threshold");

        // Mid neutral gray: both ends stay in the ramp.
        let a = dither_p256([139, 138, 137], 0.2);
        assert!(matches!(a, Color::Indexed(i) if i >= 232), "{a:?}");

        // Tinted colors stay in the cube (16..232).
        let tinted = dither_p256([200, 60, 30], 0.5);
        assert!(
            matches!(tinted, Color::Indexed(i) if (16..232).contains(&i)),
            "{tinted:?}"
        );
    }

    #[test]
    fn bayer_thresholds_span_unit_range() {
        let mut min = 1.0f32;
        let mut max = 0.0f32;
        for y in 0..4u16 {
            for x in 0..4u16 {
                let th = bayer_threshold(x, y);
                min = min.min(th);
                max = max.max(th);
            }
        }
        assert!(min >= 0.0 && max < 1.0);
        assert!(max - min > 0.8, "thresholds should tile the range");
    }

    #[test]
    fn depth_parse_and_names() {
        assert_eq!(ColorDepth::parse("TRUE"), Some(ColorDepth::True));
        assert_eq!(ColorDepth::parse("truecolor"), Some(ColorDepth::True));
        assert_eq!(ColorDepth::parse("256"), Some(ColorDepth::P256));
        assert_eq!(ColorDepth::parse("ansi16"), Some(ColorDepth::Ansi16));
        assert_eq!(ColorDepth::parse("nope"), None);
    }

    #[test]
    fn truecolor_at_dither_is_exact() {
        let t = find("fire").unwrap();
        for i in 0..=10 {
            let tt = i as f32 / 10.0;
            assert_eq!(t.at_dither(tt, 0, 0), t.at(tt));
            assert_eq!(t.at_dither(tt, 123, 45), t.at(tt));
        }
    }

    #[test]
    fn xterm_nearest_matches_known_colors() {
        // Pure red is exactly ANSI bright red (index 9) and also exists in
        // the cube at 196; the exact palette entry wins.
        assert_eq!(nearest_xterm([255, 0, 0]), 9);
        assert_eq!(nearest_xterm([255, 215, 0]), 220, "cube entry for gold");
        // Pure black maps to the base ANSI black (index 0).
        assert_eq!(nearest_xterm([0, 0, 0]), 0);
        // Grays: exact VGA gray (128,128,128) is index 8 (the ramp's
        // 244 also holds 128; the earlier index wins). Off-palette grays
        // use the grayscale ramp (232..255).
        assert_eq!(nearest_xterm([128, 128, 128]), 8);
        assert_eq!(nearest_xterm([139, 139, 139]), 245, "nearest ramp 138");
        assert_eq!(nearest_xterm([120, 120, 120]), 243, "nearest ramp 118");
    }

    #[test]
    fn p256_gradient_is_indexed_and_dither_stays_in_cube() {
        let t = find_with_depth("fire", ColorDepth::P256).unwrap();
        for i in 0..GRADIENT_STEPS {
            assert!(matches!(t.gradient[i], Color::Indexed(_)));
        }
        // Dither output always lands inside the cube (16..=231).
        for i in 0..=16 {
            match t.at_dither(i as f32 / 16.0, 3, 7) {
                Color::Indexed(idx) => assert!((16..232).contains(&idx), "idx {idx}"),
                other => panic!("expected Indexed, got {other:?}"),
            }
        }
    }

    #[test]
    fn ansi16_dither_only_uses_ansi_indices() {
        let t = find_with_depth("fire", ColorDepth::Ansi16).unwrap();
        for i in 0..=16 {
            for &(x, y) in &[(0u16, 0u16), (1, 1), (2, 3), (3, 0), (5, 7)] {
                match t.at_dither(i as f32 / 16.0, x, y) {
                    Color::Indexed(idx) => assert!(idx < 16, "idx {idx}"),
                    other => panic!("expected Indexed, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn ansi16_dither_varies_with_position() {
        // Some intensity must dither to different palette entries at
        // different positions, otherwise the pattern is not applied.
        let t = find_with_depth("vapor", ColorDepth::Ansi16).unwrap();
        let mut seen = std::collections::HashSet::new();
        for y in 0..4u16 {
            for x in 0..4u16 {
                if let Color::Indexed(idx) = t.at_dither(0.35, x, y) {
                    seen.insert(idx);
                }
            }
        }
        assert!(seen.len() >= 2, "expected dither variance, got {seen:?}");
    }
}
