//! STUB (system agent): small macOS helpers standing in for src/win.rs.

use std::path::Path;

pub fn acquire_single_instance() -> bool {
    true
}

/// Asks the copy already running to open its window; false if none answered.
pub fn signal_running_instance() -> bool {
    false
}

/// True once after a second launch asked this copy to open its window; the tray checks it on
/// every timer tick (the listener runs off the UI thread, so it cannot call `ui::post`).
pub fn open_requested() -> bool {
    false
}

pub fn windows_version() -> String {
    "macOS".into()
}

pub fn message_box(text: &str, _error: bool) {
    eprintln!("{text}");
}

pub fn open_file(_path: &Path) {}

pub fn reveal_file(_path: &Path) {}
