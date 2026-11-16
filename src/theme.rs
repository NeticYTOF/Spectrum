//! Color themes for the visualizers.
//!
//! Each theme is a set of gradient stops that get baked into a dense
//! lookup table (dim -> hot). All renderers map a 0..=1 intensity to a
//! color with [`Theme::at`].

use ratatui::style::Color;

/// Number of precomputed gradient entries per theme.
const GRADIENT_STEPS: usize = 48;

#[derive(Clone)]
pub struct Theme {
    pub name: &'static str,
    gradient: Vec<Color>,
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

    /// Mid-high accent used for UI chrome (titles, borders, status).
    pub fn accent(&self) -> Color {
        self.at(0.6)
    }

    /// Hottest color; used for the beat flash.
    pub fn beat(&self) -> Color {
        self.at(1.0)
    }
}

const THEMES: &[(&str, &[(u8, u8, u8)])] = &[
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

fn sample_stops(stops: &[(u8, u8, u8)], t: f32) -> Color {
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

/// All available themes, with gradients precomputed.
pub fn all() -> Vec<Theme> {
    THEMES
        .iter()
        .map(|(name, stops)| Theme {
            name,
            gradient: (0..GRADIENT_STEPS)
                .map(|i| sample_stops(stops, i as f32 / (GRADIENT_STEPS - 1) as f32))
                .collect(),
        })
        .collect()
}

/// Look up a theme by name (case-insensitive, surrounding whitespace ignored).
pub fn find(name: &str) -> Option<Theme> {
    let wanted = name.trim().to_lowercase();
    all().into_iter().find(|t| t.name == wanted)
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
}
