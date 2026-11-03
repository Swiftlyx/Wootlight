//! Runtime binding to wooting-rgb-sdk64.dll (libwooting-rgb-sdk.dylib on macOS).
//!
//! The library is embedded in the executable and extracted to %LOCALAPPDATA%\Wootlight
//! (~/Library/Application Support/Wootlight on macOS) unless a copy sits next to the executable.

use std::{
    ffi::{CStr, c_char},
    fs,
    path::PathBuf,
};

use libloading::Library;

use crate::{paths, render::Frame};

#[cfg(windows)]
const DLL_NAME: &str = "wooting-rgb-sdk64.dll";
#[cfg(windows)]
static EMBEDDED_DLL: &[u8] = include_bytes!("../vendor/wooting-rgb-sdk64.dll");
#[cfg(target_os = "macos")]
const DLL_NAME: &str = "libwooting-rgb-sdk.dylib";
#[cfg(target_os = "macos")]
static EMBEDDED_DLL: &[u8] = include_bytes!("../vendor/macos/libwooting-rgb-sdk.dylib");

/// Mirrors WOOTING_USB_META from wooting-usb.h (SDK v1.8.0).
#[repr(C)]
struct UsbMeta {
    connected: bool,
    model: *const c_char,
    max_rows: u8,
    max_columns: u8,
    led_index_max: u8,
    device_type: i32,
    v2_interface: bool,
    layout: i32,
    uses_small_packets: bool,
    uses_multi_report: bool,
}

type FnBool = unsafe extern "C" fn() -> bool;
type FnDeviceInfo = unsafe extern "C" fn() -> *const UsbMeta;
type FnSetFull = unsafe extern "C" fn(*const u8) -> bool;
type FnAutoUpdate = unsafe extern "C" fn(bool);

#[derive(Clone, Debug, PartialEq)]
pub struct KeyboardInfo {
    pub model: String,
    pub rows: usize,
    pub cols: usize,
    /// WOOTING_DEVICE_TYPE.
    pub device_type: i32,
    /// WOOTING_DEVICE_LAYOUT: 0 ANSI, 1 ISO, -1 unknown.
    pub layout: i32,
}

/// Not thread-safe: the SDK keeps global state, so use it from one thread only.
pub struct Sdk {
    kbd_connected: FnBool,
    device_info: FnDeviceInfo,
    array_set_full: FnSetFull,
    array_update_keyboard: FnBool,
    array_auto_update: FnAutoUpdate,
    close: FnBool,
    _lib: Library,
}

impl Sdk {
    pub fn load() -> Result<Self, String> {
        let path = locate_dll()?;
        // SAFETY: the symbol signatures match wooting-rgb-sdk.h of the bundled SDK version.
        unsafe {
            let lib = Library::new(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let sym = |name: &str| format!("{DLL_NAME} has no symbol {name}");
            Ok(Self {
                kbd_connected: *lib.get::<FnBool>("wooting_rgb_kbd_connected").map_err(|_| sym("kbd_connected"))?,
                device_info: *lib.get::<FnDeviceInfo>("wooting_rgb_device_info").map_err(|_| sym("device_info"))?,
                array_set_full: *lib.get::<FnSetFull>("wooting_rgb_array_set_full").map_err(|_| sym("array_set_full"))?,
                array_update_keyboard: *lib
                    .get::<FnBool>("wooting_rgb_array_update_keyboard")
                    .map_err(|_| sym("array_update_keyboard"))?,
                array_auto_update: *lib
                    .get::<FnAutoUpdate>("wooting_rgb_array_auto_update")
                    .map_err(|_| sym("array_auto_update"))?,
                close: *lib.get::<FnBool>("wooting_rgb_close").map_err(|_| sym("close"))?,
                _lib: lib,
            })
        }
    }

    pub fn connect(&self) -> bool {
        // SAFETY: plain SDK calls without arguments.
        unsafe {
            if !(self.kbd_connected)() {
                return false;
            }
            (self.array_auto_update)(false);
        }
        true
    }

    pub fn info(&self) -> KeyboardInfo {
        // SAFETY: the SDK returns a pointer to its static meta struct.
        unsafe {
            let meta = (self.device_info)();
            if meta.is_null() {
                return KeyboardInfo { model: "Wooting".into(), rows: 6, cols: 21, device_type: -1, layout: -1 };
            }
            let meta = &*meta;
            let model = if meta.model.is_null() {
                "Wooting".into()
            } else {
                CStr::from_ptr(meta.model).to_string_lossy().into_owned()
            };
            KeyboardInfo {
                model,
                rows: meta.max_rows as usize,
                cols: meta.max_columns as usize,
                device_type: meta.device_type,
                layout: meta.layout,
            }
        }
    }

    /// Sends a full frame; false means the keyboard is gone.
    pub fn show(&self, frame: &Frame) -> bool {
        // SAFETY: the frame has the 6 x 21 x 3 layout the SDK reads.
        unsafe { (self.array_set_full)(frame.as_ptr()) && (self.array_update_keyboard)() }
    }

    /// Restores the profile lighting and releases the HID handle.
    pub fn close(&self) {
        // SAFETY: plain SDK call; safe to repeat when nothing is connected.
        unsafe {
            (self.close)();
        }
    }
}

fn locate_dll() -> Result<PathBuf, String> {
    if let Some(local) = std::env::current_exe().ok().map(|exe| exe.with_file_name(DLL_NAME)) {
        if local.is_file() {
            return Ok(local);
        }
    }
    let dir = paths::local_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(DLL_NAME);
    let up_to_date = fs::read(&path).is_ok_and(|bytes| bytes == EMBEDDED_DLL);
    if !up_to_date {
        fs::write(&path, EMBEDDED_DLL).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(path)
}
