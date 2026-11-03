//! STUB (audio agent): captures what macOS plays, of a whole output device or of one app alone.
//! Must keep the public API of src/audio.rs, which the engine uses unchanged.

use std::time::Duration;

use cpal::{DeviceId, Host, traits::{DeviceTrait, HostTrait}};

use crate::player::App;

pub struct Capture {
    pub id: Option<DeviceId>,
    pub name: String,
    pub sample_rate: u32,
    /// Topmost process of the app captured alone; None for a whole output device.
    pub app_root: Option<u32>,
}

/// Names of all output devices, for the device menu.
pub fn output_device_names() -> Vec<String> {
    cpal::default_host().output_devices().map(|devices| devices.map(|d| d.to_string()).collect()).unwrap_or_default()
}

/// Id of the current default output device.
pub fn default_output_id(host: &Host) -> Option<DeviceId> {
    host.default_output_device().and_then(|d| d.id().ok())
}

impl Capture {
    pub fn open(_host: &Host, _query: &str) -> Result<Self, String> {
        Err("audio capture is not implemented on macOS yet".into())
    }

    pub fn open_app(_app: &App) -> Result<Self, String> {
        Err("app capture is not implemented on macOS yet".into())
    }

    pub fn failed(&self) -> bool {
        false
    }

    pub fn data_age(&self) -> Option<Duration> {
        None
    }

    pub fn take_block_count(&self) -> u32 {
        0
    }

    pub fn latest(&self, out: &mut [f32]) {
        out.fill(0.0);
    }
}
