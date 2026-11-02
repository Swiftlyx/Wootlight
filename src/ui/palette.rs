//! Material 3 tonal color roles built from one seed color.
//!
//! Hue and chroma come from OKLCH, tone is CIE L*, so every role keeps its contrast for any seed in
//! both themes. The seed is the visualizer's accent color, softened to a pastel.

use std::f64::consts::PI;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };

    pub fn rgb8(r: u8, g: u8, b: u8) -> Self {
        Self { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: 1.0 }
    }

    pub fn from_rgb_f32(rgb: [f32; 3]) -> Self {
        Self::rgb8(rgb[0].clamp(0.0, 255.0) as u8, rgb[1].clamp(0.0, 255.0) as u8, rgb[2].clamp(0.0, 255.0) as u8)
    }

    pub fn alpha(self, a: f32) -> Self {
        Self { a: self.a * a, ..self }
    }

    /// Springs may overshoot; colors must not.
    pub fn mix(self, to: Color, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        Self {
            r: self.r + (to.r - self.r) * t,
            g: self.g + (to.g - self.g) * t,
            b: self.b + (to.b - self.b) * t,
            a: self.a + (to.a - self.a) * t,
        }
    }
}

/// No seed is used with more OKLCH chroma than this, so even a neon pick turns into a calm color.
pub const MAX_SEED_CHROMA: f64 = 0.10;
/// Used for rainbow schemes, which have no single accent.
pub const NEUTRAL_SEED: [f32; 3] = [150.0, 130.0, 230.0];

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    pub primary: Color,
    pub on_primary: Color,
    pub secondary: Color,
    pub secondary_container: Color,
    pub on_secondary_container: Color,
    pub tertiary: Color,
    pub error: Color,
    pub surface: Color,
    pub surface_container: Color,
    pub surface_container_high: Color,
    pub surface_container_highest: Color,
    pub on_surface: Color,
    pub on_surface_variant: Color,
    pub outline: Color,
    pub outline_variant: Color,
}

fn to_linear(c: f64) -> f64 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn from_linear(c: f64) -> f64 {
    if c <= 0.0031308 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// OKLab after Björn Ottosson's published matrices.
fn to_oklab([r, g, b]: [f64; 3]) -> [f64; 3] {
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

fn from_oklab([l_, a, b]: [f64; 3]) -> [f64; 3] {
    let l = (l_ + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m = (l_ - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s = (l_ - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    [
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    ]
}

fn luminance([r, g, b]: [f64; 3]) -> f64 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// CIE L* (Material "tone") to relative luminance.
fn tone_to_y(tone: f64) -> f64 {
    let f = (tone + 16.0) / 116.0;
    if tone > 8.0 { f * f * f } else { tone / 903.2962962962963 }
}

fn in_gamut(c: [f64; 3]) -> bool {
    c.iter().all(|v| (-1e-6..=1.0 + 1e-6).contains(v))
}

fn to_color(c: [f64; 3]) -> Color {
    let ch = |v: f64| from_linear(v.clamp(0.0, 1.0)).clamp(0.0, 1.0) as f32;
    Color { r: ch(c[0]), g: ch(c[1]), b: ch(c[2]), a: 1.0 }
}

/// The color with the luminance of `tone`, the hue, and the largest chroma up to the requested one
/// that still fits sRGB.
pub fn tone_color(hue: f64, chroma: f64, tone: f64) -> Color {
    if tone <= 0.0 {
        return Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
    }
    if tone >= 100.0 {
        return Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
    }
    let target = tone_to_y(tone);
    let (cos, sin) = (hue.cos(), hue.sin());
    // Luminance grows with OKLab L at a fixed hue and chroma, so bisection finds the tone.
    let solve = |c: f64| {
        let (mut low, mut high) = (0.0, 1.0);
        for _ in 0..40 {
            let mid = (low + high) / 2.0;
            if luminance(from_oklab([mid, c * cos, c * sin])) < target {
                low = mid;
            } else {
                high = mid;
            }
        }
        from_oklab([(low + high) / 2.0, c * cos, c * sin])
    };
    let mut result = solve(chroma);
    if !in_gamut(result) {
        let (mut low, mut high) = (0.0, chroma);
        result = solve(0.0);
        for _ in 0..24 {
            let mid = (low + high) / 2.0;
            let candidate = solve(mid);
            if in_gamut(candidate) {
                low = mid;
                result = candidate;
            } else {
                high = mid;
            }
        }
    }
    to_color(result)
}

/// OKLCH hue (radians) and chroma of an sRGB color.
pub fn hue_chroma(rgb: [f32; 3]) -> (f64, f64) {
    let lab = to_oklab(rgb.map(|v| to_linear((v as f64 / 255.0).clamp(0.0, 1.0))));
    (lab[2].atan2(lab[1]), lab[1].hypot(lab[2]))
}

pub fn make_palette(seed: [f32; 3], dark: bool) -> Palette {
    let (hue, chroma) = hue_chroma(seed);
    let chroma = chroma.min(MAX_SEED_CHROMA);
    let primary = |t| tone_color(hue, chroma, t);
    let secondary = |t| tone_color(hue, (chroma / 3.0).min(0.045), t);
    // The tertiary family sits across the hue circle, softened like the seed.
    let tertiary = |t| tone_color(hue + 200.0 * PI / 180.0, (chroma * 0.75).clamp(0.045, 0.09), t);
    let neutral = |t| tone_color(hue, (chroma / 10.0).min(0.012), t);
    let neutral_variant = |t| tone_color(hue, (chroma / 5.0).min(0.026), t);
    let error = |t| tone_color(29.0 * PI / 180.0, 0.19, t);

    // Material 3 standard-contrast tones.
    let (accent, on_accent, sec_container, on_sec_container) =
        if dark { (80.0, 20.0, 30.0, 90.0) } else { (40.0, 100.0, 86.0, 10.0) };
    let (on_surface, on_surface_variant, outline, outline_variant) =
        if dark { (90.0, 80.0, 60.0, 30.0) } else { (10.0, 30.0, 50.0, 80.0) };

    Palette {
        dark,
        primary: primary(accent),
        on_primary: primary(on_accent),
        secondary: secondary(accent),
        secondary_container: secondary(sec_container),
        on_secondary_container: secondary(on_sec_container),
        tertiary: tertiary(accent),
        error: error(accent),
        surface: neutral(if dark { 6.0 } else { 98.0 }),
        surface_container: neutral(if dark { 12.0 } else { 94.0 }),
        surface_container_high: neutral(if dark { 17.0 } else { 92.0 }),
        surface_container_highest: neutral(if dark { 22.0 } else { 90.0 }),
        on_surface: neutral(on_surface),
        on_surface_variant: neutral_variant(on_surface_variant),
        outline: neutral_variant(outline),
        outline_variant: neutral_variant(outline_variant),
    }
}

/// A seed color at a tone that reads well on the given background, for swatches and the tray icon.
pub fn seed_swatch(seed: [f32; 3], dark_background: bool) -> Color {
    let (hue, chroma) = hue_chroma(seed);
    tone_color(hue, chroma.min(0.16), if dark_background { 74.0 } else { 46.0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contrast(a: Color, b: Color) -> f64 {
        let y = |c: Color| luminance([c.r, c.g, c.b].map(|v| to_linear(v as f64)));
        let (l1, l2) = (y(a).max(y(b)), y(a).min(y(b)));
        (l1 + 0.05) / (l2 + 0.05)
    }

    #[test]
    fn text_keeps_contrast_for_any_seed() {
        for seed in [[255.0, 42.0, 138.0], [0.0, 255.0, 192.0], [255.0, 255.0, 255.0], [16.0, 96.0, 255.0]] {
            for dark in [false, true] {
                let p = make_palette(seed, dark);
                assert!(contrast(p.on_surface, p.surface) > 7.0, "{seed:?} dark={dark}");
                assert!(contrast(p.on_primary, p.primary) > 4.5, "{seed:?} dark={dark}");
                assert!(contrast(p.on_surface_variant, p.surface_container) > 4.5, "{seed:?} dark={dark}");
            }
        }
    }
}
