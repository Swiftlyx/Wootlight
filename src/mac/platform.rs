//! Small macOS helpers standing in for src/win.rs: single instance, the second-launch signal,
//! message boxes, opening files and the system version.
//!
//! The first copy holds an flock on `wootlight.lock` and listens on `wootlight.sock`, both in
//! ~/Library/Application Support/Wootlight. A second launch connects to the socket and sends
//! `open`; the listener sets the flag behind `open_requested` and also queues a call on the
//! main dispatch queue, which the AppKit run loop drains, so the window opens without waiting
//! for the next timer tick. Whichever comes first takes the flag.

use std::{
    ffi::{CStr, c_void},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::{
        fd::AsRawFd,
        unix::net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{NSAlert, NSAlertStyle, NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::NSString;

use crate::{log, paths};

const LOCK_NAME: &str = "wootlight.lock";
const SOCKET_NAME: &str = "wootlight.sock";
const OPEN_MESSAGE: &str = "open";
/// How long a second launch waits for the running copy to answer.
const SIGNAL_TIMEOUT: Duration = Duration::from_secs(2);

static OPEN_REQUESTED: AtomicBool = AtomicBool::new(false);
/// The lock file, held open (and so locked) for the process lifetime.
static LOCK: Mutex<Option<File>> = Mutex::new(None);

unsafe extern "C" {
    static _dispatch_main_q: c_void;
    fn dispatch_async_f(queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
}

/// Takes the lock in `dir`; false if another process holds it.
fn lock(dir: &Path) -> Result<File, String> {
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = dir.join(LOCK_NAME);
    let file = OpenOptions::new().create(true).truncate(false).write(true).open(&path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    // SAFETY: flock on a descriptor this function owns.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("locked".into());
    }
    Ok(file)
}

/// Runs on the main queue: opens the window unless a timer tick already took the request.
extern "C" fn open_on_main(_context: *mut c_void) {
    if OPEN_REQUESTED.swap(false, Ordering::SeqCst) {
        crate::ui::post(crate::ui::Command::OpenWindow);
    }
}

fn request_open() {
    OPEN_REQUESTED.store(true, Ordering::SeqCst);
    // SAFETY: the main queue is a static object; the function takes no context.
    unsafe { dispatch_async_f(&raw const _dispatch_main_q, std::ptr::null_mut(), open_on_main) };
}

/// Accepts second launches on a socket in `dir`; `on_open` runs for each request.
fn listen(dir: &Path, on_open: fn()) -> Result<(), String> {
    let path = dir.join(SOCKET_NAME);
    // Holding the lock means any socket file left is stale.
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path).map_err(|e| format!("cannot listen on {}: {e}", path.display()))?;
    thread::Builder::new()
        .name("instance".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(SIGNAL_TIMEOUT));
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).is_ok() && line.trim() == OPEN_MESSAGE {
                    log!("second launch: opening the window");
                    on_open();
                    let _ = stream.write_all(b"ok\n");
                }
            }
        })
        .map(|_| ())
        .map_err(|e| format!("cannot start the listener: {e}"))
}

/// Sends `open` to the copy listening in `dir`; true once it answered.
fn signal(dir: &Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(dir.join(SOCKET_NAME)) else { return false };
    let _ = stream.set_read_timeout(Some(SIGNAL_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SIGNAL_TIMEOUT));
    if stream.write_all(format!("{OPEN_MESSAGE}\n").as_bytes()).is_err() {
        return false;
    }
    let mut answer = String::new();
    BufReader::new(&stream).read_line(&mut answer).is_ok() && answer.trim() == "ok"
}

fn instance_dir() -> PathBuf {
    paths::local_dir()
}

/// False if another instance already runs for this user. The first one also starts listening
/// for later launches.
pub fn acquire_single_instance() -> bool {
    let dir = instance_dir();
    match lock(&dir) {
        Ok(file) => {
            *LOCK.lock().unwrap_or_else(|e| e.into_inner()) = Some(file);
            if let Err(e) = listen(&dir, request_open) {
                eprintln!("Wootlight: {e}");
            }
            true
        }
        Err(e) if e == "locked" => false,
        // Without a lock file there is no way to tell; run rather than refuse to start.
        Err(e) => {
            eprintln!("Wootlight: {e}");
            true
        }
    }
}

/// Asks the copy already running to open its window; false if none answered.
pub fn signal_running_instance() -> bool {
    signal(&instance_dir())
}

/// True once after a second launch asked this copy to open its window; the tray checks it on
/// every timer tick (the listener runs off the UI thread, so it cannot call `ui::post`).
pub fn open_requested() -> bool {
    OPEN_REQUESTED.swap(false, Ordering::SeqCst)
}

/// A sysctl string such as kern.osproductversion.
fn sysctl_string(name: &CStr) -> Option<String> {
    let mut buf = [0u8; 256];
    let mut len = buf.len();
    // SAFETY: the buffer and its length are passed along; no new value is set.
    let result = unsafe { libc::sysctlbyname(name.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) };
    if result != 0 {
        return None;
    }
    let text = CStr::from_bytes_until_nul(&buf[..len.min(buf.len())]).ok()?;
    Some(text.to_string_lossy().into_owned())
}

/// For example "macOS 26.6.2 (arm64)". Keeps the Windows name: shared code calls it.
pub fn windows_version() -> String {
    let version = sysctl_string(c"kern.osproductversion").unwrap_or_else(|| "?".into());
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        arch => arch,
    };
    // An x86_64 build on Apple silicon runs under Rosetta.
    let translated = sysctl_string(c"sysctl.proc_translated").is_some_and(|v| v == "\u{1}" || v == "1");
    format!("macOS {version} ({arch}{})", if translated { ", Rosetta" } else { "" })
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
    fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
}

/// Which arrow keys are held now: left, right, up, down.
pub fn arrow_keys() -> [bool; 4] {
    // kCGEventSourceStateCombinedSessionState and the virtual key codes of the arrows.
    // SAFETY: plain queries without pointers.
    [123u16, 124, 126, 125].map(|key| unsafe { CGEventSourceKeyState(0, key) })
}

/// Time since the last keyboard, mouse or trackpad input in this login session.
pub fn idle_time() -> Option<Duration> {
    // kCGEventSourceStateCombinedSessionState and kCGAnyInputEventType.
    // SAFETY: a plain query without pointers.
    let secs = unsafe { CGEventSourceSecondsSinceLastEventType(0, u32::MAX) };
    (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs))
}

/// Local year, month and day.
pub fn local_date() -> (u32, u32, u32) {
    // SAFETY: localtime_r fills a local struct from the current time.
    let t = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut t: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut t);
        t
    };
    ((t.tm_year + 1900) as u32, (t.tm_mon + 1) as u32, t.tm_mday as u32)
}

/// Runs an AppleScript that yields a POSIX path; None when the user cancels.
fn osascript_path(script: &str) -> Option<PathBuf> {
    let output = Command::new("/usr/bin/osascript").args(["-e", script]).output().ok()?;
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !path.is_empty()).then(|| PathBuf::from(path))
}

/// Asks where to save a settings file, suggesting `name`. Blocks until the panel closes.
pub fn choose_save_path(title: &str, _filter: &str, name: &str) -> Option<PathBuf> {
    osascript_path(&format!("POSIX path of (choose file name with prompt {} default name {})", applescript_quote(title), applescript_quote(name)))
}

/// Asks for a settings file to open. Blocks until the panel closes.
pub fn choose_open_path(title: &str, _filter: &str) -> Option<PathBuf> {
    osascript_path(&format!("POSIX path of (choose file with prompt {})", applescript_quote(title)))
}

/// Local hour, minute and second.
pub fn local_clock() -> (u32, u32, u32) {
    // SAFETY: localtime_r fills a local struct from the current time.
    let t = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut t: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut t);
        t
    };
    (t.tm_hour as u32, t.tm_min as u32, t.tm_sec as u32)
}

/// GETs an https `url` through the system's curl, following redirects; returns the body when the
/// request succeeds and the body is at most `limit` bytes.
pub fn http_get(url: &str, accept: &str, limit: usize) -> Result<Vec<u8>, String> {
    let agent = format!("Wootlight/{}", env!("CARGO_PKG_VERSION"));
    let output = Command::new("/usr/bin/curl")
        .args(["--fail", "--silent", "--show-error", "--location", "--proto", "=https", "--max-time", "120"])
        .args(["--user-agent", &agent, "--header", &format!("Accept: {accept}"), url])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !output.status.success() {
        return Err(format!("curl: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    if output.stdout.len() > limit {
        return Err(format!("{url} is larger than {limit} bytes"));
    }
    Ok(output.stdout)
}

/// Opens a web page in the default browser.
pub fn open_url(url: &str) {
    if let Err(e) = Command::new("/usr/bin/open").arg(url).spawn() {
        log!("opening {url} failed: {e}");
    }
}

/// An alert with an OK button; on the main thread through AppKit, elsewhere through osascript.
pub fn message_box(text: &str, error: bool) {
    eprintln!("{text}");
    let Some(mtm) = MainThreadMarker::new() else {
        osascript_alert(text, error);
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    // A bare binary starts with no activation policy, which keeps alerts behind other windows.
    if app.activationPolicy() == NSApplicationActivationPolicy::Prohibited {
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    }
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    let alert: Retained<NSAlert> = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str("Wootlight"));
    alert.setInformativeText(&NSString::from_str(text));
    alert.setAlertStyle(if error { NSAlertStyle::Critical } else { NSAlertStyle::Informational });
    alert.runModal();
}

/// AppleScript string literal.
fn applescript_quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn osascript_alert(text: &str, error: bool) {
    let script = format!(
        "display alert \"Wootlight\" message {} as {} buttons {{\"OK\"}} default button 1",
        applescript_quote(text),
        if error { "critical" } else { "informational" }
    );
    if let Err(e) = Command::new("/usr/bin/osascript").args(["-e", &script]).status() {
        log!("osascript failed: {e}");
    }
}

/// Runs /usr/bin/open; true if it succeeded.
fn open(args: &[&str], path: &Path) -> bool {
    Command::new("/usr/bin/open")
        .args(args)
        .arg(path)
        .output()
        .map_err(|e| log!("open failed to start: {e}"))
        .is_ok_and(|out| out.status.success())
}

/// Opens a file in its default app; files with none (often .toml) open in the default text
/// editor, and failing that the file is shown in the Finder.
pub fn open_file(path: &Path) {
    let path = path.to_path_buf();
    let spawned = thread::Builder::new().name("open".into()).spawn(move || {
        if !open(&[], &path) && !open(&["-t"], &path) {
            log!("no app opened {}", path.display());
            reveal_file(&path);
        }
    });
    if let Err(e) = spawned {
        log!("opening a file failed: {e}");
    }
}

/// Opens a Finder window with the file selected.
pub fn reveal_file(path: &Path) {
    if let Err(e) = Command::new("/usr/bin/open").arg("-R").arg(path).spawn() {
        log!("opening the Finder failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use std::{env, sync::atomic::AtomicU32};

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        // Socket paths are limited to 104 bytes; the temporary folder leaves room.
        let dir = env::temp_dir().join(format!("wl-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn version_names_macos() {
        let v = windows_version();
        assert!(v.starts_with("macOS ") && v.ends_with(')'), "{v}");
        assert!(!v.contains('?'), "{v}");
    }

    #[test]
    fn quotes_applescript() {
        assert_eq!(applescript_quote(r#"a "b" \c"#), r#""a \"b\" \\c""#);
    }

    static OPENS: AtomicU32 = AtomicU32::new(0);

    fn count_open() {
        OPENS.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn single_instance_and_signal() {
        let dir = temp_dir("instance");
        assert!(!signal(&dir), "nothing listens yet");
        let first = lock(&dir).expect("first lock");
        // flock locks belong to the open file, so a second open in this process conflicts too.
        assert_eq!(lock(&dir).err().as_deref(), Some("locked"));
        listen(&dir, count_open).expect("listen");
        assert!(signal(&dir));
        assert!(signal(&dir));
        assert_eq!(OPENS.load(Ordering::SeqCst), 2);
        drop(first);
        lock(&dir).expect("lock is free once the holder is gone");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A second process is refused while the first holds the lock.
    #[test]
    fn lock_excludes_other_processes() {
        let dir = temp_dir("process");
        let _held = lock(&dir).expect("lock");
        let status = Command::new("/usr/bin/perl")
            .args(["-e", "use Fcntl ':flock'; open(my $f, '>>', $ARGV[0]) or exit 2; exit(flock($f, LOCK_EX | LOCK_NB) ? 0 : 1)"])
            .arg(dir.join(LOCK_NAME))
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(1));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Shows an alert through osascript: `cargo test alert_shows -- --ignored`.
    #[test]
    #[ignore]
    fn alert_shows() {
        message_box("Test alert from cargo test (off the main thread).", false);
    }
}
