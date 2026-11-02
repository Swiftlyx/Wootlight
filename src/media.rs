//! Whether a player reports playback through the Windows media controls (the media flyout):
//! YouTube Music in a browser, Spotify, Media Player and so on.

use windows::{
    Media::Control::{
        GlobalSystemMediaTransportControlsSessionManager as SessionManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
    },
    core::Result,
};

pub struct MediaSessions {
    manager: SessionManager,
}

impl MediaSessions {
    /// Connects to the media controls; blocks briefly. The calling thread must have COM initialized.
    pub fn connect() -> Result<Self> {
        Ok(Self { manager: SessionManager::RequestAsync()?.join()? })
    }

    /// The app of every session (its AppUserModelID) and whether it reports playback.
    pub fn sessions(&self) -> Result<Vec<(String, bool)>> {
        let sessions = self.manager.GetSessions()?;
        let mut out = Vec::new();
        for i in 0..sessions.Size()? {
            let session = sessions.GetAt(i)?;
            let playing = session.GetPlaybackInfo()?.PlaybackStatus()? == PlaybackStatus::Playing;
            out.push((session.SourceAppUserModelId()?.to_string(), playing));
        }
        Ok(out)
    }
}
