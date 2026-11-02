//! Turns bar heights into an RGB frame laid out for `wooting_rgb_array_set_full`.
//!
//! The keyboard is split into vertical strips of equal physical width, one band each.
//! Heights are physical: every keyboard row is one level, so bar tops line up across
//! the board and only a full bar reaches the top row. A key belongs to every strip
//! whose center it covers; a wide key (Shift, Enter) shows the average of its strips.

use crate::{
    layout::{Key, Layout},
    settings::{ColorMode, Settings},
};

/// Buffer stride expected by the SDK, independent of the keyboard model.
pub const SDK_ROWS: usize = 6;
pub const SDK_COLS: usize = 21;
pub const FRAME_LEN: usize = SDK_ROWS * SDK_COLS * 3;

pub type Frame = [u8; FRAME_LEN];

const CLASSIC_STOPS: [[f32; 3]; 3] = [[0.0, 255.0, 40.0], [255.0, 210.0, 0.0], [255.0, 0.0, 0.0]];
const FILL_GAMMA: f32 = 1.8;
/// Minimum overlap (in key units) for a neighbor key to stand in for a strip's missing key.
const HOLE_FILL_MIN_OVERLAP: f32 = 0.25;

/// Color of a bar at height fraction `t` (0 bottom, 1 top) in band `band` of `n_bands`.
pub fn color_at(mode: ColorMode, t: f32, band: usize, n_bands: usize, hue_offset: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    match mode {
        ColorMode::Classic => {
            let pos = t * (CLASSIC_STOPS.len() - 1) as f32;
            let i = (pos as usize).min(CLASSIC_STOPS.len() - 2);
            mix(CLASSIC_STOPS[i], CLASSIC_STOPS[i + 1], pos - i as f32)
        }
        // Squared so the lower rows stay close to the base color.
        ColorMode::Gradient(bottom, top) => mix(bottom, top, t * t),
        ColorMode::Rainbow | ColorMode::Flow => {
            let x = band as f32 / (n_bands.max(2) - 1) as f32;
            hsv_to_rgb((x * 0.8 + hue_offset).rem_euclid(1.0), 1.0)
        }
        ColorMode::Solid(rgb) => rgb,
    }
}

struct PlacedKey {
    offset: usize,
    /// Height level, 0 for the lowest row.
    level: usize,
    strips: Vec<usize>,
}

pub struct Renderer {
    keys: Vec<PlacedKey>,
    levels: usize,
    pub n_bands: usize,
    mode: ColorMode,
    brightness: f32,
    hue_speed: f32,
    hue_offset: f32,
    heights: Vec<f32>,
}

/// Strips each key belongs to: those whose center it covers, plus gap filling for
/// strips that would otherwise miss a key in a row the strip passes through.
fn assign_strips(keys: &[&Key], n_strips: usize, strip_w: f32) -> Vec<Vec<usize>> {
    let center = |s: usize| (s as f32 + 0.5) * strip_w;
    let mut strips: Vec<Vec<usize>> = keys
        .iter()
        .map(|key| {
            let covered: Vec<usize> = (0..n_strips).filter(|s| key.x0 <= center(*s) && center(*s) < key.x1).collect();
            if covered.is_empty() {
                let mid = (key.x0 + key.x1) / 2.0;
                vec![((mid / strip_w) as usize).min(n_strips - 1)]
            } else {
                covered
            }
        })
        .collect();

    let mut rows: Vec<usize> = keys.iter().map(|k| k.row).collect();
    rows.sort_unstable();
    rows.dedup();
    for s in 0..n_strips {
        let (a, b) = (s as f32 * strip_w, (s + 1) as f32 * strip_w);
        for row in &rows {
            let in_row = || keys.iter().enumerate().filter(|(_, k)| k.row == *row);
            if in_row().any(|(i, _)| strips[i].contains(&s)) {
                continue;
            }
            let best = in_row()
                .map(|(i, k)| (i, b.min(k.x1) - a.max(k.x0)))
                .fold(None, |acc: Option<(usize, f32)>, x| if acc.is_none_or(|y| x.1 > y.1) { Some(x) } else { acc });
            if let Some((i, overlap)) = best {
                if overlap >= HOLE_FILL_MIN_OVERLAP {
                    strips[i].push(s);
                }
            }
        }
    }
    strips
}

impl Renderer {
    pub fn new(layout: &Layout, settings: &Settings) -> Self {
        let n_strips = layout.default_strips.max(1);
        let valid: Vec<_> = layout.keys.iter().filter(|k| k.row < SDK_ROWS && k.col < SDK_COLS).collect();
        // Boards without a function row (60HE) start lower in the SDK grid.
        let top_row = valid.iter().map(|k| k.row).min().unwrap_or(0);
        let bottom_row = valid.iter().map(|k| k.row).max().unwrap_or(0);
        let levels = bottom_row - top_row + 1;
        let strips = assign_strips(&valid, n_strips, layout.width / n_strips as f32);
        let keys = valid
            .iter()
            .zip(strips)
            .map(|(key, strips)| PlacedKey { offset: (key.row * SDK_COLS + key.col) * 3, level: bottom_row - key.row, strips })
            .collect();
        Self {
            keys,
            levels,
            n_bands: n_strips,
            mode: settings.color_mode(),
            brightness: settings.brightness,
            hue_speed: settings.hue_speed,
            hue_offset: 0.0,
            heights: vec![0.0; n_strips],
        }
    }

    pub fn hue_offset(&self) -> f32 {
        self.hue_offset
    }

    /// Continues the flowing rainbow where `previous` left it.
    pub fn keep_hue_of(mut self, previous: &Renderer) -> Self {
        self.hue_offset = previous.hue_offset;
        self
    }

    pub fn render(&mut self, bars: &[f32], dt: f32, frame: &mut Frame) {
        if self.mode == ColorMode::Flow {
            self.hue_offset = (self.hue_offset + self.hue_speed * dt).rem_euclid(1.0);
        }
        let levels = self.levels as f32;
        for (h, bar) in self.heights.iter_mut().zip(bars) {
            *h = bar * levels;
        }

        frame.fill(0);
        for key in &self.keys {
            let t = key.level as f32 / (self.levels.max(2) - 1) as f32;
            let mut acc = [0.0f32; 3];
            for &strip in &key.strips {
                let fill = (self.heights[strip] - key.level as f32).clamp(0.0, 1.0);
                if fill <= 0.0 {
                    continue;
                }
                // Perceptual curve for the partially lit top key of each bar.
                let k = fill.powf(FILL_GAMMA);
                let rgb = color_at(self.mode, t, strip, self.n_bands, self.hue_offset);
                for i in 0..3 {
                    acc[i] += rgb[i] * k;
                }
            }
            let scale = self.brightness / key.strips.len() as f32;
            for i in 0..3 {
                frame[key.offset + i] = (acc[i] * scale).clamp(0.0, 255.0) as u8;
            }
        }
    }
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [0, 1, 2].map(|i| a[i] * (1.0 - t) + b[i] * t)
}

/// Hue and saturation (0..1) at full value to 0..255 RGB.
pub fn hsv_to_rgb(h: f32, s: f32) -> [f32; 3] {
    let h6 = h.rem_euclid(1.0) * 6.0;
    let x = 1.0 - (h6.rem_euclid(2.0) - 1.0).abs();
    let (r, g, b) = match h6 as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    let s = s.clamp(0.0, 1.0);
    [r, g, b].map(|c| (1.0 - s + s * c) * 255.0)
}

/// Hue and saturation (0..1) of an RGB color, ignoring its value.
pub fn rgb_to_hs(rgb: [f32; 3]) -> (f32, f32) {
    let max = rgb[0].max(rgb[1]).max(rgb[2]);
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    if max <= 0.0 {
        return (0.0, 0.0);
    }
    let d = max - min;
    let s = d / max;
    if d <= 0.0 {
        return (0.0, s);
    }
    let h = if max == rgb[0] {
        ((rgb[1] - rgb[2]) / d).rem_euclid(6.0)
    } else if max == rgb[1] {
        (rgb[2] - rgb[0]) / d + 2.0
    } else {
        (rgb[0] - rgb[1]) / d + 4.0
    };
    (h / 6.0, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(frame: &Frame, row: usize, col: usize) -> [u8; 3] {
        let idx = (row * SDK_COLS + col) * 3;
        [frame[idx], frame[idx + 1], frame[idx + 2]]
    }

    fn lit(frame: &Frame, row: usize, col: usize) -> bool {
        px(frame, row, col).iter().any(|v| *v > 0)
    }

    fn render(layout: &Layout, settings: &Settings, bars: &[f32]) -> Frame {
        let mut r = Renderer::new(layout, settings);
        let mut frame = [0u8; FRAME_LEN];
        r.render(bars, 0.016, &mut frame);
        frame
    }

    #[test]
    fn grid_bars_grow_from_bottom_row() {
        let mut bars = vec![0.0; 17];
        bars[3] = 0.5;
        let frame = render(&Layout::grid(6, 17), &Settings::default(), &bars);
        let column: Vec<bool> = (0..6).map(|row| lit(&frame, row, 3)).collect();
        assert_eq!(column, [false, false, false, true, true, true]);
        assert!(!lit(&frame, 5, 2));
    }

    #[test]
    fn heights_stay_physical_in_short_strips() {
        // Strip 16 of the 80HE has Pause, Home, End and the right arrow: half height lights only the arrow.
        let mut bars = vec![0.0; 17];
        bars[16] = 0.5;
        let frame = render(&Layout::wooting_80he(false), &Settings::default(), &bars);
        assert!(lit(&frame, 5, 16));
        assert!((0..=2).all(|row| !lit(&frame, row, 16)));
    }

    #[test]
    fn full_bars_reach_f_row_including_the_f8_f9_gap() {
        let mut bars = vec![0.0; 17];
        bars[9] = 1.0;
        let frame = render(&Layout::wooting_80he(false), &Settings::default(), &bars);
        // F9 stands in for the gap above strip 9, shared with strip 10 at half brightness.
        let f9 = px(&frame, 0, 10);
        assert!(f9.iter().any(|v| *v > 0) && f9.iter().all(|v| *v < 200), "{f9:?}");
        let full = render(&Layout::wooting_80he(false), &Settings::default(), &[1.0; 17]);
        assert!((0..=16).filter(|c| *c != 9).all(|col| lit(&full, 0, col)));
        assert!((4..=8).all(|col| lit(&full, 5, col)));
    }

    #[test]
    fn wide_key_averages_its_strips() {
        // Right Shift spans strips 12-14; only strip 13 is full.
        let mut bars = vec![0.0; 17];
        bars[13] = 1.0;
        let shift = px(&render(&Layout::wooting_80he(false), &Settings::default(), &bars), 4, 14);
        let full_shift = px(&render(&Layout::wooting_80he(false), &Settings::default(), &[1.0; 17]), 4, 14);
        assert!(shift[0] > 0 && (shift[0] as f32) < full_shift[0] as f32 * 0.5, "{shift:?} vs {full_shift:?}");
    }

    #[test]
    fn gradient_goes_from_base_to_accent() {
        // Strip 4 of the 80HE is a clean column: Space LED 1, C, D, R, 4, F4.
        let settings = Settings { color: "#ffffff>#ff0080".into(), ..Settings::default() };
        let frame = render(&Layout::wooting_80he(false), &settings, &[1.0; 17]);
        assert_eq!(px(&frame, 5, 4), [255, 255, 255]);
        assert_eq!(px(&frame, 0, 4), [255, 0, 128]);
    }

    #[test]
    fn every_board_reaches_its_top_row() {
        for (layout, top_row, cols) in [
            (Layout::wooting_60(false), 1, 14),
            (Layout::wooting_one(false), 0, 17),
            (Layout::wooting_two(false), 0, 21),
            (Layout::wooting_80he(true), 0, 17),
        ] {
            let n = Renderer::new(&layout, &Settings::default()).n_bands;
            let frame = render(&layout, &Settings::default(), &vec![1.0; n]);
            assert!((0..cols).any(|c| lit(&frame, top_row, c)), "top row {top_row} dark");
            assert!(lit(&frame, 5, 0), "bottom-left key dark");
        }
    }

    #[test]
    fn hue_saturation_round_trip() {
        for (h, s) in [(0.0, 1.0), (0.33, 0.5), (0.9, 0.2)] {
            let (h2, s2) = rgb_to_hs(hsv_to_rgb(h, s));
            assert!((h - h2).abs() < 0.01 && (s - s2).abs() < 0.01, "{h},{s} -> {h2},{s2}");
        }
    }
}
