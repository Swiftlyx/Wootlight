//! The app mark, a keycap with equalizer bars, drawn on either platform's canvas for the tray, the
//! menu bar and the window.

use super::{
    canvas::{Canvas, Rect},
    palette::{Color, hue_chroma, seed_swatch, tone_color},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Live,
    Idle,
    Paused,
}

/// Draws the mark into a `size` x `size` square of DIPs (points on macOS) on the canvas.
pub fn draw_mark(c: &Canvas, x: f32, y: f32, size: f32, mark: Mark, seed: [f32; 3], dark_background: bool) {
    let (hue, chroma) = hue_chroma(seed);
    let cap = seed_swatch(seed, dark_background);
    let ink = tone_color(hue, chroma.min(0.05), if dark_background { 18.0 } else { 98.0 });
    draw_mark_in(c, x, y, size, mark, cap, ink);
}

/// Draws the mark with the keycap in `cap` and the bars in `ink`.
pub fn draw_mark_in(c: &Canvas, x: f32, y: f32, size: f32, mark: Mark, cap: Color, ink: Color) {
    let body = Rect::new(x + size * 0.06, y + size * 0.06, x + size * 0.94, y + size * 0.94);
    let radius = size * 0.26;
    if mark == Mark::Paused {
        let stroke = (size * 0.1).max(1.0);
        c.stroke_round(body.inset(stroke / 2.0, stroke / 2.0), radius - stroke / 2.0, cap, stroke);
        return;
    }
    c.fill_round(body, radius, cap);
    let heights: [f32; 3] = if mark == Mark::Live { [0.34, 0.56, 0.44] } else { [0.12, 0.12, 0.12] };
    let bar_w = size * 0.15;
    let gap = size * 0.07;
    let left = x + (size - (3.0 * bar_w + 2.0 * gap)) / 2.0;
    let bottom = y + size * 0.76;
    for (i, h) in heights.iter().enumerate() {
        let bx = left + i as f32 * (bar_w + gap);
        c.fill_round(Rect::new(bx, bottom - size * h, bx + bar_w, bottom), bar_w / 2.0, ink);
    }
}
