//! STUB (system agent): start at login.

pub fn is_enabled() -> bool {
    false
}

pub fn set_enabled(_enabled: bool) -> bool {
    false
}

/// Points the login item at the current app, e.g. after it was moved.
pub fn refresh() {}
