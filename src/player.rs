//! Finds the process behind a session of the Windows media controls, so that its sound alone can
//! be captured. The media controls name an app (an AppUserModelID such as "Spotify.exe",
//! "MSEdge" or "com.github.th-ch.youtube-music"), not a process, so the app is matched against
//! the processes that have an audio session.

use std::collections::{HashMap, HashSet};

use windows::{
    Win32::{
        Foundation::{CloseHandle, HWND, LPARAM},
        Media::Audio::{
            AudioSessionStateActive, DEVICE_STATE_ACTIVE, IAudioSessionControl2, IAudioSessionManager2,
            IMMDeviceEnumerator, MMDeviceEnumerator, eRender,
        },
        Storage::{EnhancedStorage::PKEY_AppUserModel_ID, Packaging::Appx::GetApplicationUserModelId},
        System::{
            Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, StructuredStorage::PropVariantToStringAlloc},
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
            },
            Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW},
        },
        UI::{
            Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow},
            WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId},
        },
    },
    core::{BOOL, Interface, PWSTR},
};

/// Media-control names of browsers that do not use their file name.
const BROWSERS: &[(&str, &str)] = &[
    ("chrome", "chrome.exe"),
    ("msedge", "msedge.exe"),
    ("opera", "opera.exe"),
    ("brave", "brave.exe"),
    ("vivaldi", "vivaldi.exe"),
];
/// Firefox and its forks name their sessions by a 16-digit hash of the install folder.
const FIREFOX_LIKE: &[&str] = &["firefox.exe", "librewolf.exe", "waterfox.exe", "floorp.exe", "zen.exe"];

/// The process tree an app plays its sound from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    /// Topmost process of the app: its tree includes the helper process that plays the sound.
    pub root: u32,
    pub exe: String,
}

struct Proc {
    parent: u32,
    exe: String,
}

fn processes() -> HashMap<u32, Proc> {
    let mut out = HashMap::new();
    // SAFETY: a process snapshot walked with correctly sized entries and closed afterwards.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snapshot, &mut entry).is_ok();
        while ok {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let exe = String::from_utf16_lossy(&entry.szExeFile[..len]);
            out.insert(entry.th32ProcessID, Proc { parent: entry.th32ParentProcessID, exe });
            ok = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    out
}

/// The topmost ancestor running the same executable: a browser's main process for its audio helper.
fn root_of(pid: u32, procs: &HashMap<u32, Proc>) -> u32 {
    let mut current = pid;
    for _ in 0..64 {
        let (Some(me), Some(parent)) = (procs.get(&current), procs.get(&current).and_then(|p| procs.get(&p.parent))) else { break };
        if !parent.exe.eq_ignore_ascii_case(&me.exe) || me.parent == current {
            break;
        }
        current = me.parent;
    }
    current
}

fn exe_path(pid: u32) -> Option<String> {
    // SAFETY: a query-only process handle, closed after use; the buffer length is passed along.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(handle);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// AppUserModelID of a packaged (Microsoft Store) app's process.
fn package_aumid(pid: u32) -> Option<String> {
    // SAFETY: a query-only process handle, closed after use; the buffer length is passed along.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let result = GetApplicationUserModelId(handle, &mut len, Some(PWSTR(buf.as_mut_ptr())));
        let _ = CloseHandle(handle);
        let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
        (result.is_ok() && len > 0).then(|| String::from_utf16_lossy(&buf[..len]))
    }
}

/// AppUserModelIDs that windows carry as a property, by process.
fn window_aumids() -> HashMap<u32, Vec<String>> {
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam points to the map owned by the caller for the duration of EnumWindows.
        let map = unsafe { &mut *(lparam.0 as *mut HashMap<u32, Vec<String>>) };
        let mut pid = 0;
        // SAFETY: plain window queries; the property string is freed after copying.
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if let Ok(store) = SHGetPropertyStoreForWindow::<IPropertyStore>(hwnd) {
                if let Ok(value) = store.GetValue(&PKEY_AppUserModel_ID) {
                    if let Ok(text) = PropVariantToStringAlloc(&value) {
                        let id = text.to_string().unwrap_or_default();
                        CoTaskMemFree(Some(text.0 as _));
                        if !id.is_empty() {
                            map.entry(pid).or_default().push(id);
                        }
                    }
                }
            }
        }
        true.into()
    }
    let mut map: HashMap<u32, Vec<String>> = HashMap::new();
    // SAFETY: the callback only runs during this call, while the map is alive.
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut map as *mut _ as isize));
    }
    map
}

/// Processes with an audio session on any active output device; active ones first.
fn audio_session_pids() -> Vec<u32> {
    let mut active = Vec::new();
    let mut idle = Vec::new();
    // SAFETY: COM calls on a thread with COM initialized; interfaces are released on drop.
    unsafe {
        let Ok(enumerator) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else {
            return active;
        };
        let Ok(devices) = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else { return active };
        for i in 0..devices.GetCount().unwrap_or(0) {
            let Ok(device) = devices.Item(i) else { continue };
            let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
            let Ok(sessions) = manager.GetSessionEnumerator() else { continue };
            for j in 0..sessions.GetCount().unwrap_or(0) {
                let Ok(session) = sessions.GetSession(j) else { continue };
                let Ok(control) = session.cast::<IAudioSessionControl2>() else { continue };
                let pid = control.GetProcessId().unwrap_or(0);
                if pid == 0 {
                    continue;
                }
                if session.GetState().is_ok_and(|s| s == AudioSessionStateActive) {
                    active.push(pid);
                } else {
                    idle.push(pid);
                }
            }
        }
    }
    active.extend(idle);
    active
}

/// Lowercase letters and digits only: "Pear Desktop" and "pear-desktop" become the same.
fn normalized(text: &str) -> String {
    text.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// Whether media-control name `aumid` belongs to the app whose topmost process is `root`.
fn matches(aumid: &str, root: u32, procs: &HashMap<u32, Proc>, windows: &HashMap<u32, Vec<String>>) -> bool {
    let Some(proc) = procs.get(&root) else { return false };
    let exe = proc.exe.to_ascii_lowercase();
    let stem = exe.strip_suffix(".exe").unwrap_or(&exe);
    let id = aumid.trim().to_ascii_lowercase();
    if id == exe || id == stem {
        return true;
    }
    let first = id.split(['.', '!']).next().unwrap_or("");
    if BROWSERS.iter().any(|(name, file)| first == *name && exe == *file) {
        return true;
    }
    if id.len() == 16 && id.chars().all(|c| c.is_ascii_hexdigit()) && FIREFOX_LIKE.contains(&exe.as_str()) {
        return true;
    }
    let tree: Vec<u32> = procs.keys().copied().filter(|&pid| root_of(pid, procs) == root).collect();
    if tree.iter().any(|&pid| package_aumid(pid).is_some_and(|p| p.eq_ignore_ascii_case(aumid))) {
        return true;
    }
    if tree.iter().filter_map(|pid| windows.get(pid)).flatten().any(|w| w.eq_ignore_ascii_case(aumid)) {
        return true;
    }
    // Explicit IDs often end with the product name: "com.github.th-ch.youtube-music".
    let last = normalized(id.rsplit(['.', '!', '\\']).next().unwrap_or(""));
    if last.len() < 4 {
        return false;
    }
    let path = exe_path(root).unwrap_or_default();
    let folder = path.rsplit('\\').nth(1).map(normalized).unwrap_or_default();
    normalized(stem) == last || folder == last
}

/// The app playing for media-control session `aumid`, among the processes with sound.
pub fn find(aumid: &str) -> Option<App> {
    let procs = processes();
    let windows = window_aumids();
    let mut seen = HashSet::new();
    audio_session_pids()
        .into_iter()
        .map(|pid| root_of(pid, &procs))
        .filter(|root| seen.insert(*root))
        .find(|&root| matches(aumid, root, &procs, &windows))
        .map(|root| App { root, exe: procs.get(&root).map(|p| p.exe.clone()).unwrap_or_default() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

    #[test]
    fn names_are_normalized() {
        assert_eq!(normalized("Pear Desktop"), normalized("pear-desktop"));
        assert_eq!(normalized("YouTube Music.exe"), "youtubemusicexe");
    }

    /// Prints the media sessions, the processes with sound and the matches:
    /// `cargo test probe_players -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn probe_players() {
        // SAFETY: COM for this test thread.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let procs = processes();
        let windows = window_aumids();
        let media = crate::media::MediaSessions::connect().expect("media controls");
        for (id, playing) in media.sessions().expect("sessions") {
            println!("media: {id} playing={playing} -> {:?}", find(&id));
        }
        for pid in audio_session_pids() {
            let root = root_of(pid, &procs);
            let exe = procs.get(&pid).map(|p| p.exe.as_str()).unwrap_or("?");
            let tree_ids: Vec<&String> =
                procs.keys().filter(|&&p| root_of(p, &procs) == root).filter_map(|p| windows.get(p)).flatten().collect();
            println!(
                "audio: pid {pid} {exe} root {root} path {:?} package {:?} window ids {tree_ids:?}",
                exe_path(root),
                package_aumid(pid)
            );
        }
    }
}
