//! Finds the process behind the app in Now Playing, so that its sound alone can be captured.
//! Now Playing names an app by bundle id ("com.google.Chrome", "com.spotify.client"); the app's
//! main process is the root of the tree that plays, since browser audio helpers are its children.
//! Safari plays from the WebKit GPU service, an XPC process that is not a child of Safari; it
//! is only "responsible" to Safari, which process taps that follow responsibility can use.

use objc2_app_kit::NSRunningApplication;
use objc2_foundation::NSString;

/// The process tree an app plays its sound from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    /// Topmost process of the app: its tree includes the helper process that plays the sound.
    pub root: u32,
    pub exe: String,
}

/// File name of a running app's executable, else its display name.
fn exe_name(app: &NSRunningApplication) -> String {
    app.executableURL()
        .and_then(|url| url.lastPathComponent())
        .or_else(|| app.localizedName())
        .map(|name| name.to_string())
        .unwrap_or_default()
}

/// Name of the executable of any process, from the kernel.
fn proc_name(pid: u32) -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer length is passed along; proc_name writes at most that many bytes.
    let len = unsafe { libc::proc_name(pid as i32, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (len > 0).then(|| String::from_utf8_lossy(&buf[..len as usize]).into_owned())
}

/// The app whose Now Playing session is `id` (a bundle id), found among running apps.
pub fn find(id: &str) -> Option<App> {
    let reported = crate::media::reported_pid(id);
    let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(id));
    let apps: Vec<_> = apps.iter().filter(|app| !app.isTerminated()).collect();
    // Several copies of one app: prefer the one Now Playing named.
    let app = apps.iter().find(|app| reported.is_some_and(|pid| app.processIdentifier() as u32 == pid)).or(apps.first());
    if let Some(app) = app {
        let pid = app.processIdentifier();
        if pid > 0 {
            return Some(App { root: pid as u32, exe: exe_name(app) });
        }
    }
    // A player without an app bundle: trust the pid Now Playing reported while it is alive.
    let pid = reported?;
    if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid as i32) {
        return Some(App { root: pid, exe: exe_name(&app) });
    }
    proc_name(pid).map(|exe| App { root: pid, exe })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Finder always runs in a login session.
    #[test]
    #[ignore]
    fn finds_finder() {
        let app = find("com.apple.finder").expect("Finder runs");
        assert_eq!(app.exe, "Finder");
        assert_eq!(proc_name(app.root).as_deref(), Some("Finder"));
    }

    #[test]
    fn unknown_app_is_none() {
        assert_eq!(find("io.github.swiftlyx.wootlight.no-such-app"), None);
    }
}
