//! STUB (UI shell agent): the app mark for the menu bar and the window.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Live,
    Idle,
    Paused,
}

pub fn release_canvas() {}

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
