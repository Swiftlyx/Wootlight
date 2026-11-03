//! Small Win32 helpers.

use std::{
    env,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process, ptr, thread,
};

use windows_sys::Win32::{
    Foundation::{ERROR_ALREADY_EXISTS, ERROR_SUCCESS, GetLastError, HWND, LPARAM, LRESULT, WPARAM},
    System::{
        Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize},
        LibraryLoader::GetModuleHandleW,
        Registry::{HKEY, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW},
        Threading::CreateMutexW,
    },
    UI::{
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
