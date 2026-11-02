//! WASAPI loopback capture of an output device into a mono ring buffer.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use cpal::{
    Device, DeviceId, ErrorKind, FromSample, Host, SampleFormat, SizedSample, Stream,
    StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};

const RING_SIZE: usize = 1 << 15;
/// Loopback delivers nothing while the device is idle; treat that as silence.
const STALE_AFTER: Duration = Duration::from_millis(150);

struct Ring {
    buf: Vec<f32>,
    pos: usize,
    last_data: Option<Instant>,
    blocks: u32,
}

pub struct Capture {
    _stream: Stream,
    ring: Arc<Mutex<Ring>>,
    failed: Arc<AtomicBool>,
    pub id: Option<DeviceId>,
    pub name: String,
    pub sample_rate: u32,
}

/// Names of all output devices, for the device menu.
pub fn output_device_names() -> Vec<String> {
    cpal::default_host()
        .output_devices()
        .map(|devices| devices.map(|d| d.to_string()).collect())
        .unwrap_or_default()
}

/// Id of the current Windows default output device.
pub fn default_output_id(host: &Host) -> Option<DeviceId> {
    host.default_output_device().and_then(|d| d.id().ok())
}

fn find_device(host: &Host, query: &str) -> Result<Device, String> {
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
    /// Opens loopback capture of the named output device, or the default one for "".
    pub fn open(host: &Host, query: &str) -> Result<Self, String> {
        let device = find_device(host, query)?;
        let name = device.to_string();
        let supported = device.default_output_config().map_err(|e| e.to_string())?;
        let config = supported.config();
        let ring = Arc::new(Mutex::new(Ring { buf: vec![0.0; RING_SIZE], pos: 0, last_data: None, blocks: 0 }));
        let failed = Arc::new(AtomicBool::new(false));

        // On WASAPI an input stream on an output device is a loopback stream.
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, config, &ring, &failed),
            SampleFormat::I16 => build::<i16>(&device, config, &ring, &failed),
            SampleFormat::I32 => build::<i32>(&device, config, &ring, &failed),
            other => Err(format!("unsupported sample format {other:?}")),
        }?;
        stream.play().map_err(|e| e.to_string())?;

        Ok(Self {
            _stream: stream,
            ring,
            failed,
            id: device.id().ok(),
            name,
            sample_rate: supported.sample_rate(),
        })
    }

    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
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

fn build<T>(
    device: &Device,
    config: StreamConfig,
    ring: &Arc<Mutex<Ring>>,
    failed: &Arc<AtomicBool>,
) -> Result<Stream, String>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels.max(1));
    let scale = 1.0 / channels as f32;
    let ring = Arc::clone(ring);
    let failed = Arc::clone(failed);
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data: &[T], _| {
                let mut ring = ring.lock().unwrap_or_else(|e| e.into_inner());
                let len = ring.buf.len();
                for frame in data.chunks_exact(channels) {
                    let mono: f32 = frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() * scale;
                    let pos = ring.pos;
                    ring.buf[pos] = mono;
                    ring.pos = (pos + 1) % len;
                }
                ring.last_data = Some(Instant::now());
                ring.blocks = ring.blocks.wrapping_add(1);
            },
            move |err| {
                // Buffer overruns are recoverable; anything else needs a reopen.
                if err.kind() != ErrorKind::Xrun {
                    failed.store(true, Ordering::Relaxed);
                }
            },
            None,
        )
        .map_err(|e| e.to_string())
}
