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

    /// True while any session reports playback.
    pub fn playing(&self) -> Result<bool> {
        let sessions = self.manager.GetSessions()?;
        for i in 0..sessions.Size()? {
            if sessions.GetAt(i)?.GetPlaybackInfo()?.PlaybackStatus()? == PlaybackStatus::Playing {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
