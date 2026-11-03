//! STUB (system agent): whether a player reports playback in macOS Now Playing.

pub struct MediaSessions;

impl MediaSessions {
    pub fn connect() -> Result<Self, String> {
        Err("Now Playing is not implemented on macOS yet".into())
    }

    /// The app of every session (its bundle id) and whether it reports playback.
    pub fn sessions(&self) -> Result<Vec<(String, bool)>, String> {
        Ok(Vec::new())
    }
}
