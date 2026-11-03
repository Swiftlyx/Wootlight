//! Small Win32 helpers.

use std::{
    env,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process, ptr, thread,
    time::Duration,
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, GetLastError, HWND, LPARAM, LRESULT, SYSTEMTIME, WPARAM},
    Networking::WinHttp::{
        INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER,
        WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
        WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
    },
    Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash},
    System::{
        Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize},
        LibraryLoader::GetModuleHandleW,
        Registry::{HKEY, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW},
        SystemInformation::{GetLocalTime, GetTickCount},
        Threading::{CreateMutexW, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    },
    UI::{
        Controls::Dialogs::{
            GetOpenFileNameW, GetSaveFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT,
            OFN_PATHMUSTEXIST, OPENFILENAMEW,
        },
        Input::KeyboardAndMouse::{GetAsyncKeyState, GetLastInputInfo, LASTINPUTINFO, VK_DOWN, VK_LEFT, VK_RIGHT, VK_UP},
        Shell::{SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW},
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, FindWindowW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
            MessageBoxW, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND, PostMessageW, PostQuitMessage, RegisterClassW,
            SW_SHOWNORMAL, WM_APP, WM_CLOSE, WM_ENDSESSION, WM_POWERBROADCAST, WM_QUERYENDSESSION, WNDCLASSW,
            WS_OVERLAPPED,
        },
    },
};

/// Sent by a second launch to the running instance, which then opens its window.
const WM_APP_OPEN: u32 = WM_APP + 30;
const SESSION_CLASS: &str = "Wootlight.Session";

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// False if another instance already runs in this session.
pub fn acquire_single_instance() -> bool {
    let name = wide("Local\\Wootlight.SingleInstance");
    // SAFETY: valid null-terminated name; the handle is kept open for the process lifetime.
    unsafe {
        let handle = CreateMutexW(ptr::null(), 0, name.as_ptr());
        !handle.is_null() && GetLastError() != ERROR_ALREADY_EXISTS
    }
}

/// A REG_SZ value, or None if it is missing.
pub fn reg_string(root: HKEY, key: &str, name: &str) -> Option<String> {
    let key = wide(key);
    let name = wide(name);
    let mut size = 0u32;
    // SAFETY: size query first, then a buffer of that size.
    unsafe {
        if RegGetValueW(root, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, ptr::null_mut(), ptr::null_mut(), &mut size)
            != ERROR_SUCCESS
        {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        if RegGetValueW(root, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, ptr::null_mut(), buf.as_mut_ptr().cast(), &mut size)
            != ERROR_SUCCESS
        {
            return None;
        }
        let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

/// For example "Windows 11 25H2 (build 26200)".
pub fn windows_version() -> String {
    const KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let build = reg_string(HKEY_LOCAL_MACHINE, KEY, "CurrentBuild").unwrap_or_default();
    let release = reg_string(HKEY_LOCAL_MACHINE, KEY, "DisplayVersion").unwrap_or_default();
    // Windows 11 still reports itself as Windows 10 in ProductName; the build tells them apart.
    let major = if build.parse::<u32>().is_ok_and(|b| b >= 22000) { 11 } else { 10 };
    format!("Windows {major} {release} (build {build})")
}

/// Time since the last keyboard or mouse input in this session.
pub fn idle_time() -> Option<Duration> {
    let mut info = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    // SAFETY: fills a local struct whose size is set.
    unsafe {
        if GetLastInputInfo(&mut info) == 0 {
            return None;
        }
        // Both tick counts wrap after 49 days.
        Some(Duration::from_millis(u64::from(GetTickCount().wrapping_sub(info.dwTime))))
    }
}

/// Which arrow keys are held now: left, right, up, down.
pub fn arrow_keys() -> [bool; 4] {
    // SAFETY: plain queries; the high bit is set while the key is down.
    [VK_LEFT, VK_RIGHT, VK_UP, VK_DOWN].map(|key| unsafe { GetAsyncKeyState(i32::from(key)) } as u16 & 0x8000 != 0)
}

/// Local year, month and day.
pub fn local_date() -> (u32, u32, u32) {
    // SAFETY: fills a local struct.
    let t = unsafe {
        let mut t: SYSTEMTIME = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    (u32::from(t.wYear), u32::from(t.wMonth), u32::from(t.wDay))
}

/// The Save As (`save`) or Open dialog for a settings file; None when cancelled. Blocks until the
/// dialog closes, so call it off the UI thread.
fn file_dialog(save: bool, title: &str, filter: &str, name: &str) -> Option<PathBuf> {
    // SAFETY: balanced by CoUninitialize below, on this thread; the dialog needs an apartment.
    let com = unsafe { CoInitializeEx(ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32) } >= 0;
    let title = wide(title);
    // Pairs of label and pattern, each null-terminated, then one more null.
    let filter: Vec<u16> = format!("{filter} (*.toml)\0*.toml\0*.*\0*.*\0").encode_utf16().chain(Some(0)).collect();
    let extension = wide("toml");
    let mut file = vec![0u16; 1024];
    for (slot, c) in file.iter_mut().zip(name.encode_utf16().take(1000)) {
        *slot = c;
    }
    // SAFETY: every string and the file buffer outlive the call; the struct is fully initialized.
    let chosen = unsafe {
        let mut ofn: OPENFILENAMEW = std::mem::zeroed();
        ofn.lStructSize = size_of::<OPENFILENAMEW>() as u32;
        ofn.lpstrFilter = filter.as_ptr();
        ofn.lpstrFile = file.as_mut_ptr();
        ofn.nMaxFile = file.len() as u32;
        ofn.lpstrTitle = title.as_ptr();
        ofn.lpstrDefExt = extension.as_ptr();
        ofn.Flags = OFN_EXPLORER | OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST | if save { OFN_OVERWRITEPROMPT } else { OFN_FILEMUSTEXIST };
        if save { GetSaveFileNameW(&mut ofn) != 0 } else { GetOpenFileNameW(&mut ofn) != 0 }
    };
    if com {
        // SAFETY: matches the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    let len = file.iter().position(|c| *c == 0).unwrap_or(file.len());
    (chosen && len > 0).then(|| PathBuf::from(String::from_utf16_lossy(&file[..len])))
}

/// Asks where to save a settings file, suggesting `name`.
pub fn choose_save_path(title: &str, filter: &str, name: &str) -> Option<PathBuf> {
    file_dialog(true, title, filter, name)
}

/// Asks for a settings file to open.
pub fn choose_open_path(title: &str, filter: &str) -> Option<PathBuf> {
    file_dialog(false, title, filter, "")
}

/// Local hour, minute and second.
pub fn local_clock() -> (u32, u32, u32) {
    // SAFETY: fills a local struct.
    let t = unsafe {
        let mut t: SYSTEMTIME = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    (u32::from(t.wHour), u32::from(t.wMinute), u32::from(t.wSecond))
}

/// A WinHTTP handle, closed when dropped.
struct Internet(*mut core::ffi::c_void);

impl Drop for Internet {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a handle WinHTTP returned, closed once.
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

/// Why a WinHTTP call failed, from its error code.
fn winhttp_error(call: &str) -> String {
    // SAFETY: plain query.
    let code = unsafe { GetLastError() };
    let what = match code {
        12002 => "timed out",
        12007 => "the server name could not be resolved (offline?)",
        12029 | 12030 => "the connection failed",
        12175 => "the secure connection failed",
        _ => "",
    };
    if what.is_empty() { format!("{call} failed (error {code})") } else { format!("{call}: {what}") }
}

/// GETs an https `url` through WinHTTP with the system proxy, following redirects; returns the
/// body when the status is 200 and the body is at most `limit` bytes.
pub fn http_get(url: &str, accept: &str, limit: usize) -> Result<Vec<u8>, String> {
    let rest = url.strip_prefix("https://").ok_or_else(|| format!("not an https address: {url}"))?;
    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host, format!("/{path}")),
        None => (rest, "/".into()),
    };
    let agent = wide(&format!("Wootlight/{}", env!("CARGO_PKG_VERSION")));
    let (host, path, verb) = (wide(host), wide(&path), wide("GET"));
    let headers = wide(&format!("Accept: {accept}\r\n"));
    // SAFETY: every handle is checked before use and closed by `Internet`; the strings and the
    // buffers outlive the calls that read or fill them.
    unsafe {
        let session = Internet(WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, ptr::null(), ptr::null(), 0));
        if session.0.is_null() {
            return Err(winhttp_error("WinHttpOpen"));
        }
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000);
        let connection = Internet(WinHttpConnect(session.0, host.as_ptr(), INTERNET_DEFAULT_HTTPS_PORT, 0));
        if connection.0.is_null() {
            return Err(winhttp_error("WinHttpConnect"));
        }
        let request = Internet(WinHttpOpenRequest(connection.0, verb.as_ptr(), path.as_ptr(), ptr::null(), ptr::null(), ptr::null(), WINHTTP_FLAG_SECURE));
        if request.0.is_null() {
            return Err(winhttp_error("WinHttpOpenRequest"));
        }
        // u32::MAX: the headers are null-terminated.
        if WinHttpSendRequest(request.0, headers.as_ptr(), u32::MAX, ptr::null(), 0, 0, 0) == 0 {
            return Err(winhttp_error("sending the request"));
        }
        if WinHttpReceiveResponse(request.0, ptr::null_mut()) == 0 {
            return Err(winhttp_error("receiving the response"));
        }
        let mut status = 0u32;
        let mut size = size_of::<u32>() as u32;
        let flags = WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER;
        if WinHttpQueryHeaders(request.0, flags, ptr::null(), (&mut status as *mut u32).cast(), &mut size, ptr::null_mut()) == 0 {
            return Err(winhttp_error("reading the status"));
        }
        if status != 200 {
            return Err(format!("{url} answered HTTP {status}"));
        }
        let mut body = Vec::new();
        let mut chunk = vec![0u8; 64 << 10];
        loop {
            let mut read = 0u32;
            if WinHttpReadData(request.0, chunk.as_mut_ptr().cast(), chunk.len() as u32, &mut read) == 0 {
                return Err(winhttp_error("reading the response"));
            }
            if read == 0 {
                return Ok(body);
            }
            body.extend_from_slice(&chunk[..read as usize]);
            if body.len() > limit {
                return Err(format!("{url} is larger than {limit} bytes"));
            }
        }
    }
}

/// SHA-256 of `data`, by the system's CNG.
pub fn sha256(data: &[u8]) -> Option<[u8; 32]> {
    let mut hash = [0u8; 32];
    let len = u32::try_from(data.len()).ok()?;
    // SAFETY: the algorithm pseudo-handle needs no opening; both buffers are as long as told.
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, ptr::null(), 0, data.as_ptr(), len, hash.as_mut_ptr(), 32) };
    (status >= 0).then_some(hash)
}

/// Waits up to `timeout` for process `pid` to end; returns at once if it is already gone.
pub fn wait_for_exit(pid: u32, timeout: Duration) {
    // SAFETY: the handle is checked and closed; waiting on a process handle has no other effect.
    unsafe {
        let process = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if !process.is_null() {
            WaitForSingleObject(process, timeout.as_millis().min(u32::MAX as u128) as u32);
            CloseHandle(process);
        }
    }
}

/// Opens a web page in the default browser.
pub fn open_url(url: &str) {
    let url = PathBuf::from(url);
    let spawned = thread::Builder::new().name("shell".into()).spawn(move || {
        // SAFETY: balanced by CoUninitialize on this thread.
        let com = unsafe { CoInitializeEx(ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32) } >= 0;
        if !shell_execute("open", &url, false) {
            crate::log!("no browser opened {}", url.display());
        }
        if com {
            // SAFETY: matches the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    });
    if let Err(e) = spawned {
        crate::log!("opening a page failed: {e}");
    }
}

pub fn message_box(text: &str, error: bool) {
    let text = wide(text);
    let title = wide("Wootlight");
    let icon = if error { MB_ICONERROR } else { MB_ICONINFORMATION };
    // SAFETY: valid null-terminated strings.
    unsafe {
        MessageBoxW(ptr::null_mut(), text.as_ptr(), title.as_ptr(), icon | MB_OK);
    }
}

/// Runs a shell verb on a file; the calling thread must be in a single-threaded apartment.
fn shell_execute(verb: &str, file: &Path, ui: bool) -> bool {
    let verb = wide(verb);
    let file = wide(&file.display().to_string());
    // SAFETY: the strings outlive the call; the struct is fully initialized.
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOASYNC | if ui { 0 } else { SEE_MASK_FLAG_NO_UI };
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.nShow = SW_SHOWNORMAL;
        ShellExecuteExW(&mut info) != 0
    }
}

/// Opens a file in the app Windows associates with it; without one, asks which app to use.
pub fn open_file(path: &Path) {
    let path = path.to_path_buf();
    // The shell starts packaged apps (like Notepad) and handlers through COM, which needs a
    // single-threaded apartment; the UI thread has none.
    let spawned = thread::Builder::new().name("shell".into()).spawn(move || {
        // SAFETY: balanced by CoUninitialize on this thread.
        let com = unsafe { CoInitializeEx(ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32) } >= 0;
        let opened = shell_execute("open", &path, false) || shell_execute("openas", &path, true);
        if com {
            // SAFETY: matches the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
        if !opened {
            crate::log!("no app opened {}", path.display());
            reveal_file(&path);
        }
    });
    if let Err(e) = spawned {
        crate::log!("opening a file failed: {e}");
    }
}

/// Opens Explorer with the file selected.
pub fn reveal_file(path: &Path) {
    let explorer = env::var_os("SystemRoot").map_or_else(|| PathBuf::from("explorer.exe"), |root| PathBuf::from(root).join("explorer.exe"));
    // Explorer wants the quotes around the path only, which the standard argument quoting cannot produce.
    let result = process::Command::new(explorer).raw_arg(format!("/select,\"{}\"", path.display())).spawn();
    if let Err(e) = result {
        crate::log!("opening Explorer failed: {e}");
    }
}

unsafe extern "system" fn session_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wparam != 0 {
                crate::stop_engine();
            }
            0
        }
        WM_APP_OPEN => {
            crate::ui::post(crate::ui::Command::OpenWindow);
            0
        }
        WM_POWERBROADCAST => {
            match wparam as u32 {
                PBT_APMSUSPEND => crate::log!("system going to sleep"),
                PBT_APMRESUMEAUTOMATIC => crate::log!("system woke up"),
                _ => {}
            }
            1
        }
        // Sent by `taskkill` without /F: shut down cleanly.
        WM_CLOSE => {
            // SAFETY: called on the thread that owns the message loop.
            unsafe { PostQuitMessage(0) };
            0
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Hidden top-level window: message-only windows do not get session-end broadcasts.
pub fn create_session_window() -> bool {
    let class = wide(SESSION_CLASS);
    // SAFETY: the class name outlives the calls; the window lives until process exit.
    unsafe {
        let instance = GetModuleHandleW(ptr::null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(session_wndproc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        );
        !hwnd.is_null()
    }
}

/// Asks the running instance to open its window; false if none answered.
pub fn signal_running_instance() -> bool {
    let class = wide(SESSION_CLASS);
    // SAFETY: valid null-terminated class name; posting to another process's window is allowed.
    unsafe {
        let hwnd = FindWindowW(class.as_ptr(), ptr::null());
        !hwnd.is_null() && PostMessageW(hwnd, WM_APP_OPEN, 0, 0) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_standard_vectors() {
        let hex = |h: [u8; 32]| h.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(hex(sha256(b"").unwrap()), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(sha256(b"abc").unwrap()), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
