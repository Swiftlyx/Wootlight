//! STUB (system agent): finds the process that plays a media session's sound.

/// The process tree an app plays its sound from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    /// Topmost process of the app: its tree includes the helper process that plays the sound.
    pub root: u32,
    pub exe: String,
}

/// The app whose media session is `id`, found among running processes.
pub fn find(_id: &str) -> Option<App> {
    None
}
