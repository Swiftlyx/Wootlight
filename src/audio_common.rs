//! What both platforms' audio capture share: the mono ring buffer the capture fills, the
//! `Capture` the engine reads, and the output-device list. `audio` opens the platform stream.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use cpal::{
    Device, DeviceId, Host,
    traits::{DeviceTrait, HostTrait},
};

use crate::audio::Source;

const RING_SIZE: usize = 1 << 15;
/// Capture delivers nothing while the device is idle; treat that as silence.
const STALE_AFTER: Duration = Duration::from_millis(150);

pub(crate) struct Ring {
    buf: Vec<f32>,
    pos: usize,
    last_data: Option<Instant>,
    blocks: u32,
}

impl Ring {
    pub(crate) fn new() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Ring { buf: vec![0.0; RING_SIZE], pos: 0, last_data: None, blocks: 0 }))
    }

    pub(crate) fn push(&mut self, mono: f32) {
        let pos = self.pos;
        self.buf[pos] = mono;
        self.pos = (pos + 1) % self.buf.len();
    }

    /// Marks the end of one block of samples from the stream.
    pub(crate) fn delivered(&mut self) {
        self.last_data = Some(Instant::now());
        self.blocks = self.blocks.wrapping_add(1);
    }
}

pub struct Capture {
    /// Keeps the platform stream alive.
    pub(crate) _source: Source,
    pub(crate) ring: Arc<Mutex<Ring>>,
    pub(crate) failed: Arc<AtomicBool>,
    pub id: Option<DeviceId>,
    pub name: String,
    pub sample_rate: u32,
    /// Topmost process of the app captured alone; None for a whole output device.
    pub app_root: Option<u32>,
}

/// Names of all output devices, for the device menu.
pub fn output_device_names() -> Vec<String> {
    cpal::default_host()
        .output_devices()
        .map(|devices| devices.map(|d| d.to_string()).collect())
        .unwrap_or_default()
}

/// Id of the current system default output device.
pub fn default_output_id(host: &Host) -> Option<DeviceId> {
    host.default_output_device().and_then(|d| d.id().ok())
}

/// The output device named `query` exactly, else the first whose name contains it; the default
/// output for "".
pub(crate) fn find_device(host: &Host, query: &str) -> Result<Device, String> {
    if query.is_empty() {
        return host.default_output_device().ok_or_else(|| "no output device".to_string());
    }
    let devices: Vec<Device> = host.output_devices().map_err(|e| e.to_string())?.collect();
    let needle = query.to_lowercase();
    devices
        .iter()
        .position(|d| d.to_string() == query)
        .or_else(|| devices.iter().position(|d| d.to_string().to_lowercase().contains(&needle)))
        .map(|i| devices[i].clone())
        .ok_or_else(|| format!("device \"{query}\" not found"))
}

impl Capture {
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    /// Time since the stream last delivered samples; None before the first.
    pub fn data_age(&self) -> Option<Duration> {
        self.ring.lock().unwrap_or_else(|e| e.into_inner()).last_data.map(|t| t.elapsed())
    }

    /// Callbacks received since the previous call; for diagnostics.
    pub fn take_block_count(&self) -> u32 {
        std::mem::take(&mut self.ring.lock().unwrap_or_else(|e| e.into_inner()).blocks)
    }

    /// Fills `out` with the newest samples in chronological order, or zeros while idle.
    pub fn latest(&self, out: &mut [f32]) {
        let ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        if ring.last_data.is_none_or(|t| t.elapsed() > STALE_AFTER) {
            out.fill(0.0);
            return;
        }
        let len = ring.buf.len();
        let start = (ring.pos + len - out.len() % len) % len;
        for (i, dst) in out.iter_mut().enumerate() {
            *dst = ring.buf[(start + i) % len];
        }
    }
}

