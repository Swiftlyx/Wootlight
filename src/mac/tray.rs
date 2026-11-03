//! STUB (UI shell agent): the menu bar item, the command queue, settings and the run loop.

use std::{path::PathBuf, sync::Arc};

use crate::engine::Shared;

/// Wakes the run loop to execute queued `ui::Command`s; callable from the UI thread.
pub fn wake() {}

/// Runs the menu bar app until the user quits or the session ends.
pub fn run(
    _shared: Arc<Shared>,
    _config_path: PathBuf,
    _config_error: Option<String>,
    _devices: Vec<String>,
    _open_window: bool,
) -> Result<(), String> {
    Err("the menu bar is not implemented on macOS yet".into())
}
