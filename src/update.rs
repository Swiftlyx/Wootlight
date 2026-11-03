//! Updates from the GitHub releases of Swiftlyx/Wootlight. A background thread asks the GitHub API
//! for the latest release half a minute after start and every 6 hours.
//!
//! Windows: a newer Wootlight.exe is downloaded next to the running one and checked against the
//! release's SHA256SUMS.txt. It goes in place while the user is away, at the next start, or on
//! request: the running file is renamed (Windows allows that for a running executable), the new one
//! takes its name and starts with `--updated-from <pid>`, waits for the old process to end, and
//! removes the old file. macOS: the app tells that a version is out and opens its page.

use std::{
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};
#[cfg(windows)]
use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::{engine::Shared, log, win};

const LATEST: &str = "https://api.github.com/repos/Swiftlyx/Wootlight/releases/latest";
const FIRST_CHECK: Duration = Duration::from_secs(30);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
const RETRY_AFTER: Duration = Duration::from_secs(3600);
const MAX_JSON: usize = 2 << 20;
/// The argument a freshly installed version starts with, followed by the id of the process it replaces.
pub const UPDATED_FROM: &str = "--updated-from";

#[cfg(windows)]
const EXE_ASSET: &str = "Wootlight.exe";
#[cfg(windows)]
const SUMS_ASSET: &str = "SHA256SUMS.txt";
#[cfg(windows)]
const MAX_SUMS: usize = 64 << 10;
#[cfg(windows)]
const MAX_EXE: usize = 64 << 20;
#[cfg(windows)]
const OLD_EXE: &str = "Wootlight.old.exe";
/// A downloaded version waits next to the executable as Wootlight.update-<version>.exe.
#[cfg(windows)]
const STAGED_PREFIX: &str = "Wootlight.update-";
/// How long nobody must use the keyboard or mouse before an update installs by itself.
#[cfg(windows)]
const AWAY_FOR: Duration = Duration::from_secs(180);

/// macOS never downloads, so it never reaches `Downloading` or `Ready`.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum State {
    #[default]
    Idle,
    Checking,
    /// Local hour and minute of the check.
    UpToDate { at: (u32, u32) },
    /// A newer release and its web page.
    Available { version: String, page: String },
    Downloading { version: String },
    /// Downloaded and verified, waiting to be installed.
    Ready { version: String, page: String },
    Failed(String),
}

struct Inner {
    state: State,
    /// Check now instead of at the next scheduled time.
    check: bool,
    /// The user asked to install; `Some(true)` reopens the window afterwards.
    #[cfg_attr(not(windows), allow(dead_code))]
    install: Option<bool>,
}

static INNER: Mutex<Inner> = Mutex::new(Inner { state: State::Idle, check: false, install: None });
static WAKE: Condvar = Condvar::new();

fn inner() -> MutexGuard<'static, Inner> {
    INNER.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn state() -> State {
    inner().state.clone()
}

fn set_state(state: State) {
    inner().state = state;
}

/// Checks right away, on the update thread.
pub fn check_now() {
    inner().check = true;
    WAKE.notify_all();
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    #[cfg_attr(not(windows), allow(dead_code))]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
#[cfg_attr(not(windows), allow(dead_code))]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
}

/// "v1.4.0" or "1.4.0" as numbers to compare; None for anything else, such as a pre-release.
fn version_key(version: &str) -> Option<Vec<u64>> {
    version.trim().trim_start_matches('v').split('.').map(|part| part.parse().ok()).collect()
}

/// The version of `tag` when it is newer than `current`.
fn newer(tag: &str, current: &str) -> Option<String> {
    (version_key(tag)? > version_key(current)?).then(|| tag.trim().trim_start_matches('v').to_string())
}

/// The hash SHA256SUMS.txt lists for `name`, in lower case.
#[cfg_attr(not(windows), allow(dead_code))]
fn listed_hash(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let (hash, file) = (parts.next()?, parts.next()?);
        (file.trim_start_matches('*') == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hash.to_ascii_lowercase())
    })
}

fn latest() -> Result<Release, String> {
    let body = win::http_get(LATEST, "application/vnd.github+json", MAX_JSON)?;
    serde_json::from_slice(&body).map_err(|e| format!("unexpected answer from GitHub: {e}"))
}

/// Starts the update thread.
pub fn start(shared: Arc<Shared>) {
    let spawned = thread::Builder::new().name("updates".into()).spawn(move || {
        let mut next = Instant::now() + FIRST_CHECK;
        loop {
            let asked = wait_until(next);
            let mode = shared.settings().updates;
            next = Instant::now()
                + if mode == "off" && !asked {
                    set_state(State::Idle);
                    CHECK_EVERY
                } else if check(&mode) {
                    CHECK_EVERY
                } else {
                    RETRY_AFTER
                };
        }
    });
    if let Err(e) = spawned {
        log!("updates: cannot start: {e}");
    }
}

/// Sleeps until `deadline` or a request to check; true for a request.
fn wait_until(deadline: Instant) -> bool {
    let mut guard = inner();
    loop {
        if guard.check {
            guard.check = false;
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        guard = WAKE.wait_timeout(guard, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
    }
}

/// Asks GitHub for the latest release and, on Windows, downloads it when it is to be installed.
/// False when something failed and the check should be retried sooner.
fn check(mode: &str) -> bool {
    let before = state();
    set_state(State::Checking);
    let release = match latest() {
        Ok(release) => release,
        Err(e) => {
            log!("updates: checking failed: {e}");
            set_state(State::Failed(e));
            return false;
        }
    };
    let current = env!("CARGO_PKG_VERSION");
    let Some(version) = newer(&release.tag_name, current) else {
        if !matches!(before, State::UpToDate { .. }) {
            log!("updates: {current} is the latest version");
        }
        let (hour, minute, _) = win::local_clock();
        set_state(State::UpToDate { at: (hour, minute) });
        return true;
    };
    if !matches!(&before, State::Available { version: v, .. } | State::Ready { version: v, .. } if *v == version) {
        log!("updates: {version} is out");
    }
    let page = release.html_url.clone();
    #[cfg(windows)]
    if mode == "auto" || inner().install.is_some() {
        set_state(State::Downloading { version: version.clone() });
        match stage(&release, &version) {
            Ok(_) => set_state(State::Ready { version, page }),
            Err(e) => {
                log!("updates: downloading {version} failed: {e}");
                // The page still offers the download by hand.
                if inner().install.take().is_some() {
                    win::open_url(&page);
                }
                set_state(State::Available { version, page });
                return false;
            }
        }
        return true;
    }
    let _ = mode;
    set_state(State::Available { version, page });
    true
}

/// The user asked to update. Installs a downloaded version, and returns true when the app should
/// now quit for it; otherwise downloads it first (Windows) or opens its page (macOS).
pub fn install(reopen: bool) -> bool {
    match state() {
        #[cfg(windows)]
        State::Ready { .. } => match install_staged(reopen) {
            Ok(()) => return true,
            Err(e) => {
                log!("updates: {e}");
                set_state(State::Failed(e));
            }
        },
        #[cfg(windows)]
        State::Available { .. } => {
            inner().install = Some(reopen);
            check_now();
        }
        #[cfg(not(windows))]
        State::Available { page, .. } | State::Ready { page, .. } => {
            let _ = reopen;
            win::open_url(&page);
        }
        _ => check_now(),
    }
    false
}

/// Called by the UI thread twice a second. Installs a downloaded version when the user asked for
/// it, or, set to automatic, while nobody uses the computer and no music plays (`quiet`). True
/// when the app should now quit for it.
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn on_tick(mode: &str, quiet: bool) -> bool {
    #[cfg(windows)]
    if matches!(state(), State::Ready { .. }) {
        let requested = inner().install;
        let away = mode == "auto" && quiet && win::idle_time().is_some_and(|idle| idle >= AWAY_FOR);
        if requested.is_some() || away {
            inner().install = None;
            match install_staged(requested.unwrap_or(false)) {
                Ok(()) => return true,
                Err(e) => {
                    log!("updates: {e}");
                    set_state(State::Failed(e));
                }
            }
        }
    }
    false
}

/// First thing in `main`: a version the updater started waits for the one it replaces to end,
/// which frees the single-instance lock. True when this process is such a version.
pub fn wait_for_previous() -> bool {
    let mut args = std::env::args();
    let Some(pid) = args.position(|a| a == UPDATED_FROM).and_then(|_| args.next()).and_then(|pid| pid.parse::<u32>().ok()) else {
        return false;
    };
    #[cfg(windows)]
    win::wait_for_exit(pid, Duration::from_secs(15));
    let _ = pid;
    true
}

#[cfg(windows)]
fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

/// Removes what an update leaves behind: the replaced executable and downloads that are not newer.
#[cfg(windows)]
pub fn clean_up(updated: bool) {
    if updated {
        log!("updates: now running {}", env!("CARGO_PKG_VERSION"));
    }
    let Some(dir) = exe_dir() else { return };
    let old = dir.join(OLD_EXE);
    // The old process has just ended; its file can stay locked a moment longer.
    for attempt in 0..10 {
        if !old.exists() || fs::remove_file(&old).is_ok() {
            break;
        }
        if attempt == 9 {
            log!("updates: cannot remove {}", old.display());
        }
        thread::sleep(Duration::from_millis(200));
    }
    let Ok(entries) = fs::read_dir(&dir) else { return };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else { continue };
        let Some(rest) = name.strip_prefix(STAGED_PREFIX) else { continue };
        let stale = rest.ends_with(".part") || rest.strip_suffix(".exe").is_some_and(|v| newer(v, env!("CARGO_PKG_VERSION")).is_none());
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// At start, set to automatic: installs a version downloaded earlier. True when this process
/// should end at once for the new one.
#[cfg(windows)]
pub fn install_at_start(mode: &str, reopen: bool) -> bool {
    if mode != "auto" {
        return false;
    }
    let Some((version, _)) = staged() else { return false };
    log!("updates: installing {version}, downloaded earlier");
    match install_staged(reopen) {
        Ok(()) => true,
        Err(e) => {
            log!("updates: {e}");
            false
        }
    }
}

/// The newest downloaded version that is newer than this one, and its file.
#[cfg(windows)]
fn staged() -> Option<(String, PathBuf)> {
    let entries = fs::read_dir(exe_dir()?).ok()?;
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let version = newer(name.strip_prefix(STAGED_PREFIX)?.strip_suffix(".exe")?, env!("CARGO_PKG_VERSION"))?;
            Some((version, entry.path()))
        })
        .max_by_key(|(version, _)| version_key(version))
}

/// Downloads the release's executable and checks it against SHA256SUMS.txt.
#[cfg(windows)]
fn download(release: &Release) -> Result<Vec<u8>, String> {
    let asset = |name: &str| release.assets.iter().find(|a| a.name == name).ok_or_else(|| format!("the release has no {name}"));
    let (exe, sums) = (asset(EXE_ASSET)?, asset(SUMS_ASSET)?);
    let sums = win::http_get(&sums.browser_download_url, "application/octet-stream", MAX_SUMS)?;
    let expected = listed_hash(&String::from_utf8_lossy(&sums), EXE_ASSET).ok_or("SHA256SUMS.txt does not list Wootlight.exe")?;
    let bytes = win::http_get(&exe.browser_download_url, "application/octet-stream", MAX_EXE)?;
    if bytes.len() as u64 != exe.size {
        return Err(format!("got {} of {} bytes", bytes.len(), exe.size));
    }
    let hash = win::sha256(&bytes).ok_or("hashing the download failed")?;
    if hash.iter().map(|b| format!("{b:02x}")).collect::<String>() != expected {
        return Err("the download does not match SHA256SUMS.txt".into());
    }
    Ok(bytes)
}

/// Downloads `version` next to the executable, unless it is there already.
#[cfg(windows)]
fn stage(release: &Release, version: &str) -> Result<PathBuf, String> {
    let path = exe_dir().ok_or("the executable's folder is unknown")?.join(format!("{STAGED_PREFIX}{version}.exe"));
    // Only a verified download gets this name.
    if path.is_file() {
        return Ok(path);
    }
    let bytes = download(release)?;
    let part = path.with_extension("part");
    fs::write(&part, &bytes).map_err(|e| format!("{}: {e}", part.display()))?;
    fs::rename(&part, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    log!("updates: downloaded {version} ({:.1} MB), its checksum matches", bytes.len() as f32 / 1_048_576.0);
    Ok(path)
}

/// Puts the newest downloaded version in place of the running executable and starts it.
#[cfg(windows)]
fn install_staged(reopen: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("the executable is unknown: {e}"))?;
    let (version, staged) = staged().ok_or("no update is downloaded")?;
    let old = exe.with_file_name(OLD_EXE);
    let _ = fs::remove_file(&old);
    fs::rename(&exe, &old).map_err(|e| format!("cannot move {}: {e}", exe.display()))?;
    if let Err(e) = fs::rename(&staged, &exe) {
        let _ = fs::rename(&old, &exe);
        return Err(format!("cannot put {version} in place: {e}"));
    }
    let mut command = std::process::Command::new(&exe);
    command.arg(UPDATED_FROM).arg(std::process::id().to_string());
    if reopen {
        command.arg("--open");
    }
    if let Err(e) = command.spawn() {
        let _ = fs::rename(&exe, &staged);
        let _ = fs::rename(&old, &exe);
        return Err(format!("cannot start {version}: {e}"));
    }
    log!("updates: installed {version}, restarting");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_newer_releases_count() {
        assert_eq!(newer("v1.4.0", "1.3.0").as_deref(), Some("1.4.0"));
        assert_eq!(newer("v1.10.0", "1.9.2").as_deref(), Some("1.10.0"));
        assert_eq!(newer("v2.0", "1.9.9").as_deref(), Some("2.0"));
        assert_eq!(newer("v1.3.0", "1.3.0"), None);
        assert_eq!(newer("v1.2.9", "1.3.0"), None);
        assert_eq!(newer("v1.4.0-beta", "1.3.0"), None, "pre-releases are skipped");
    }

    #[test]
    fn finds_the_listed_hash() {
        let hash = "4AA5156E4367453D3366E37BD63ECA638F31B9D45335445002ACEF736E07085B";
        let sums = format!("{}  Wootlight-1.3.0-win-x64.zip\r\n{hash} *Wootlight.exe\r\n", "1".repeat(64));
        assert_eq!(listed_hash(&sums, "Wootlight.exe"), Some(hash.to_ascii_lowercase()));
        assert_eq!(listed_hash(&sums, "Wootlight-1.3.0-macos-arm64.zip"), None);
        assert_eq!(listed_hash("abc  Wootlight.exe", "Wootlight.exe"), None, "not a SHA-256");
    }

    /// Needs the internet: `cargo test downloads_the_latest_release -- --ignored`.
    #[cfg(windows)]
    #[test]
    #[ignore = "downloads from GitHub"]
    fn downloads_the_latest_release() {
        let release = latest().expect("the latest release");
        let bytes = download(&release).expect("a verified download");
        let exe = release.assets.iter().find(|a| a.name == EXE_ASSET).unwrap();
        assert_eq!(bytes.len() as u64, exe.size);
        assert_eq!(&bytes[..2], b"MZ");
    }
}
