//! Runtime binding to wooting-rgb-sdk64.dll (libwooting-rgb-sdk.dylib on macOS).
//!
//! The library is embedded in the executable and extracted to %LOCALAPPDATA%\Wootlight
//! (~/Library/Application Support/Wootlight on macOS) unless a copy sits next to the executable.
//!
//! macOS: the dylib is built for arm64 by scripts/build-wooting-sdk-macos.sh. The SDK talks to
//! the keyboard's vendor-defined HID interface, which macOS opens without any TCC prompt (no
//! Input Monitoring), bundled or not; hidapi is built to open it shared, not seized.

use std::{
    ffi::{CStr, c_char},
    fs,
    path::PathBuf,
    time::Duration,
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
type FnFeatureResponse = unsafe extern "C" fn(*mut u8, usize, u8, u8, u8, u8, u8) -> i32;
type FnResponseSize = unsafe extern "C" fn() -> usize;

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
    /// Optional: only used to read settings such as Dim After.
    feature_response: Option<FnFeatureResponse>,
    response_size: Option<FnResponseSize>,
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
                feature_response: lib.get::<FnFeatureResponse>("wooting_usb_send_feature_with_response").ok().map(|f| *f),
                response_size: lib.get::<FnResponseSize>("wooting_usb_get_response_size").ok().map(|f| *f),
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

    /// Wootility's Dim After of the connected keyboard: `Some(ZERO)` when it is off, None when the
    /// keyboard does not say. Read with an undocumented command whose reply is checked, so a
    /// changed format reads as None.
    pub fn dim_after(&self) -> Option<Duration> {
        let send = self.feature_response?;
        // SAFETY: a plain query.
        let size = self.response_size.map_or(256, |f| unsafe { f() }).clamp(16, 4096);
        let mut reply = vec![0u8; size];
        // SAFETY: the buffer is `size` bytes long, as the call is told.
        let read = unsafe { send(reply.as_mut_ptr(), size, GET_GLOBAL_SETTINGS, 0, 0, 0, 0) };
        parse_dim_after(&reply[..usize::try_from(read).ok()?.min(size)])
    }
}

/// Feature command whose reply holds the keyboard's global settings, Dim After among them.
const GET_GLOBAL_SETTINGS: u8 = 51;
const REPLY_MAGIC: [u8; 2] = [0xD1, 0xDA];
const REPLY_SUCCESS: u8 = 0x88;

/// A reply: maybe a report id, the magic bytes, the command, the status, the payload length (u16 LE)
/// and a protobuf message whose field 1.1 is Dim After in minutes (absent while it is off).
fn parse_dim_after(reply: &[u8]) -> Option<Duration> {
    let start = (0..reply.len().min(4)).find(|&i| reply[i..].starts_with(&REPLY_MAGIC))?;
    let header = reply.get(start..start + 6)?;
    if header[2] != GET_GLOBAL_SETTINGS || header[3] != REPLY_SUCCESS {
        return None;
    }
    let len = usize::from(u16::from_le_bytes([header[4], header[5]]));
    let message = reply.get(start + 6..start + 6 + len)?;
    let minutes = match proto_fields(message)?.into_iter().find(|(number, _)| *number == 1) {
        None => 0,
        Some((_, Proto::Bytes(dim))) => match proto_fields(dim)?.into_iter().find(|(number, _)| *number == 1) {
            None => 0,
            Some((_, Proto::Varint(minutes))) => minutes,
            Some(_) => return None,
        },
        Some(_) => return None,
    };
    Some(Duration::from_secs(minutes.min(24 * 60) * 60))
}

enum Proto<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

/// The fields of a protobuf message with their numbers; None if it is malformed.
fn proto_fields(message: &[u8]) -> Option<Vec<(u64, Proto<'_>)>> {
    let mut fields = Vec::new();
    let mut i = 0;
    while i < message.len() {
        let key = varint(message, &mut i)?;
        let value = match key & 7 {
            0 => Proto::Varint(varint(message, &mut i)?),
            2 => {
                let len = usize::try_from(varint(message, &mut i)?).ok()?;
                let bytes = message.get(i..i.checked_add(len)?)?;
                i += len;
                Proto::Bytes(bytes)
            }
            wire @ (1 | 5) => {
                i += if wire == 1 { 8 } else { 4 };
                if i > message.len() {
                    return None;
                }
                Proto::Fixed
            }
            _ => return None,
        };
        fields.push((key >> 3, value));
    }
    Some(fields)
}

fn varint(message: &[u8], i: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *message.get(*i)?;
        *i += 1;
        value |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
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

#[cfg(test)]
mod reply_tests {
    use super::*;

    /// The reply of a Wooting 80HE with Dim After at 5 minutes.
    const REPLY_80HE: [u8; 19] = [0x01, 0xD1, 0xDA, 0x33, 0x88, 0x0C, 0x00, 0x0A, 0x06, 0x08, 0x05, 0x10, 0x0A, 0x18, 0x00, 0x10, 0x00, 0x18, 0x01];

    #[test]
    fn reads_dim_after_from_the_reply() {
        let mut reply = REPLY_80HE.to_vec();
        reply.resize(64, 0);
        assert_eq!(parse_dim_after(&reply), Some(Duration::from_secs(300)));
        // Without the report id, as hidapi may deliver it.
        assert_eq!(parse_dim_after(&reply[1..]), Some(Duration::from_secs(300)));
    }

    #[test]
    fn a_missing_field_means_off() {
        let reply = [0x01, 0xD1, 0xDA, 0x33, 0x88, 0x04, 0x00, 0x0A, 0x02, 0x10, 0x0A];
        assert_eq!(parse_dim_after(&reply), Some(Duration::ZERO));
    }

    #[test]
    fn anything_else_reads_as_unknown() {
        let mut wrong_command = REPLY_80HE;
        wrong_command[3] = 0x34;
        let mut failed = REPLY_80HE;
        failed[4] = 0x00;
        let mut wrong_type = REPLY_80HE;
        wrong_type[9] = 0x0D;
        for reply in [&wrong_command[..], &failed[..], &wrong_type[..], &REPLY_80HE[..12], &[0u8; 32][..]] {
            assert_eq!(parse_dim_after(reply), None, "{reply:02x?}");
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::{thread, time::Duration};

    use super::*;
    use crate::render::{FRAME_LEN, SDK_COLS};

    /// Needs a Wooting keyboard: `cargo test hardware_sweep -- --ignored --nocapture`.
    /// Sweeps a color bar across the board for about 3 s, then restores the profile lighting.
    #[test]
    #[ignore = "needs a connected Wooting keyboard"]
    fn hardware_sweep() {
        let sdk = Sdk::load().expect("load the SDK");
        assert!(sdk.connect(), "no Wooting keyboard found");
        let info = sdk.info();
        println!("{info:?}");
        assert!(info.rows > 0 && info.cols > 0 && info.cols <= SDK_COLS);
        for step in 0..90 {
            let lit = step % info.cols;
            let mut frame = [0u8; FRAME_LEN];
            for row in 0..info.rows {
                for col in 0..info.cols {
                    let i = (row * SDK_COLS + col) * 3;
                    let rgb = if col == lit { [255, 40, 200] } else { [0, 0, 24] };
                    frame[i..i + 3].copy_from_slice(&rgb);
                }
            }
            assert!(sdk.show(&frame), "frame {step} was not sent");
            thread::sleep(Duration::from_millis(33));
        }
        sdk.close();
    }
}
