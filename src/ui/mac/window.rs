//! STUB (window agent): the main window hosted in an NSWindow.

use std::sync::Arc;

use crate::engine::Shared;

pub fn show(_shared: &Arc<Shared>, _devices: Vec<String>) {}

pub fn is_visible() -> bool {
    false
}

pub fn hide() {}

pub fn refresh(_config_error: Option<String>) {}

pub fn destroy() {}
