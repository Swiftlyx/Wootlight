//! STUB (UI shell agent): the app mark for the menu bar and the window.

use super::canvas::Canvas;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Live,
    Idle,
    Paused,
}

pub fn release_canvas() {}

/// Draws the mark into a `size` x `size` square of points on the canvas.
pub fn draw_mark(_c: &Canvas, _x: f32, _y: f32, _size: f32, _mark: Mark, _seed: [f32; 3], _dark_background: bool) {}

/// Whether the menu bar is dark.
pub fn taskbar_is_dark() -> bool {
    true
}

/// Whether apps use the dark appearance.
pub fn apps_prefer_dark() -> bool {
    false
}

/// Renders the mark to straight-alpha RGBA pixels.
pub fn render_rgba(size: u32, _mark: Mark, _seed: [f32; 3], _dark_background: bool) -> Vec<u8> {
    vec![0; (size * size * 4) as usize]
}
