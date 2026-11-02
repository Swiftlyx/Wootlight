//! WASAPI loopback capture into a mono ring buffer: of a whole output device, or of one app's
//! process tree alone (process loopback, Windows 10 2004 and later).

use std::{
    mem::ManuallyDrop,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use cpal::{
    Device, DeviceId, ErrorKind, FromSample, Host, SampleFormat, SizedSample, Stream,
    StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
        Media::Audio::{
            AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK, AUDIOCLIENT_ACTIVATION_PARAMS,
            AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
            IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient,
            IAudioClient, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            WAVE_FORMAT_PCM, WAVEFORMATEX,
        },
        System::{
            Com::{BLOB, COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize, StructuredStorage::PROPVARIANT},
            Threading::{CreateEventW, SetEvent, WaitForSingleObject},
            Variant::VT_BLOB,
        },
    },
    core::{HRESULT, IUnknown, Interface, Ref, implement},
};

use crate::player::App;

const RING_SIZE: usize = 1 << 15;
/// Loopback delivers nothing while the device is idle; treat that as silence.
const STALE_AFTER: Duration = Duration::from_millis(150);
/// Sample rate requested from process loopback; the audio engine converts to it.
const APP_SAMPLE_RATE: u32 = 48000;
/// 20 ms, in 100 ns units.
const APP_BUFFER: i64 = 200_000;
const ACTIVATE_TIMEOUT_MS: u32 = 3000;

struct Ring {
    buf: Vec<f32>,
    pos: usize,
    last_data: Option<Instant>,
    blocks: u32,
}

impl Ring {
    fn new() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Ring { buf: vec![0.0; RING_SIZE], pos: 0, last_data: None, blocks: 0 }))
    }

    fn push(&mut self, mono: f32) {
        let pos = self.pos;
        self.buf[pos] = mono;
        self.pos = (pos + 1) % self.buf.len();
    }
}

/// What a capture keeps alive: a cpal stream, or the thread that reads one app's sound.
enum Source {
    Device(#[allow(dead_code)] Stream),
    App(#[allow(dead_code)] AppStream),
}

pub struct Capture {
    _source: Source,
    ring: Arc<Mutex<Ring>>,
    failed: Arc<AtomicBool>,
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
        let ring = Ring::new();
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
            _source: Source::Device(stream),
            ring,
            failed,
            id: device.id().ok(),
            name,
            sample_rate: supported.sample_rate(),
            app_root: None,
        })
    }

    /// Opens capture of the sound of `app`'s process tree alone, whatever device it plays on.
    pub fn open_app(app: &App) -> Result<Self, String> {
        let ring = Ring::new();
        let failed = Arc::new(AtomicBool::new(false));
        let stream = AppStream::start(app.root, Arc::clone(&ring), Arc::clone(&failed))?;
        Ok(Self {
            _source: Source::App(stream),
            ring,
            failed,
            id: None,
            name: app.exe.strip_suffix(".exe").unwrap_or(&app.exe).to_string(),
            sample_rate: APP_SAMPLE_RATE,
            app_root: Some(app.root),
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
                for frame in data.chunks_exact(channels) {
                    ring.push(frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() * scale);
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

/// Signals an event when process-loopback activation completes.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Activated(isize);

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(&self, _operation: Ref<IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        // SAFETY: the event outlives the activation; the caller waits for it before closing it.
        unsafe { SetEvent(HANDLE(self.0 as _)) }
    }
}

/// Process loopback of one process tree, read on a thread of its own.
struct AppStream {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl AppStream {
    /// Starts capturing; returns once the stream runs or failed to start.
    fn start(pid: u32, ring: Arc<Mutex<Ring>>, failed: Arc<AtomicBool>) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("app capture".into())
            .spawn(move || {
                // SAFETY: balanced by CoUninitialize below, on the same thread.
                let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
                // SAFETY: COM calls on this MTA thread; the stream objects live in this scope.
                match unsafe { open_process_loopback(pid) } {
                    Ok((client, capture, event)) => {
                        let _ = ready_tx.send(Ok(()));
                        // SAFETY: as above; the event is closed after the loop.
                        unsafe {
                            if let Err(e) = read_packets(&capture, event, &ring, &thread_stop) {
                                crate::log!("app capture failed: {e}");
                                failed.store(true, Ordering::Relaxed);
                            }
                            let _ = client.Stop();
                            let _ = CloseHandle(event);
                        }
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
                if com {
                    // SAFETY: matches the successful CoInitializeEx above.
                    unsafe { CoUninitialize() };
                }
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { stop, thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err("app capture thread ended".into())
            }
        }
    }
}

impl Drop for AppStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Activates process loopback for `pid`'s tree and starts the stream: 16-bit stereo PCM, as in
/// Microsoft's ApplicationLoopback sample, converted by the audio engine.
unsafe fn open_process_loopback(pid: u32) -> Result<(IAudioClient, IAudioCaptureClient, HANDLE), String> {
    let err = |what: &str, e: windows::core::Error| format!("{what}: {e}");
    // SAFETY: the activation parameters and the event outlive the wait below.
    unsafe {
        let done = CreateEventW(None, true, false, None).map_err(|e| err("event", e))?;
        let handler: IActivateAudioInterfaceCompletionHandler = Activated(done.0 as isize).into();
        let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        };
        // The blob points to `params` on this stack: PropVariantClear on drop would free it.
        let mut prop = ManuallyDrop::new(PROPVARIANT::default());
        (*prop.Anonymous.Anonymous).vt = VT_BLOB;
        (*prop.Anonymous.Anonymous).Anonymous.blob =
            BLOB { cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32, pBlobData: &mut params as *mut _ as *mut u8 };
        let activated = ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(&*prop), &handler)
            .map_err(|e| err("activate", e));
        let waited = activated.is_ok() && WaitForSingleObject(done, ACTIVATE_TIMEOUT_MS) == WAIT_OBJECT_0;
        let _ = CloseHandle(done);
        let operation = activated?;
        if !waited {
            return Err("activation timed out".into());
        }
        let mut result = HRESULT(0);
        let mut unknown: Option<IUnknown> = None;
        operation.GetActivateResult(&mut result, &mut unknown).map_err(|e| err("activation result", e))?;
        result.ok().map_err(|e| err("activation", e))?;
        let client: IAudioClient = unknown.ok_or("no audio client")?.cast().map_err(|e| err("audio client", e))?;

        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 2,
            nSamplesPerSec: APP_SAMPLE_RATE,
            nAvgBytesPerSec: APP_SAMPLE_RATE * 4,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let flags = AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM;
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, APP_BUFFER, 0, &format, None).map_err(|e| err("initialize", e))?;
        let event = CreateEventW(None, false, false, None).map_err(|e| err("event", e))?;
        let started = client
            .SetEventHandle(event)
            .and_then(|()| client.GetService::<IAudioCaptureClient>())
            .and_then(|capture| client.Start().map(|()| capture));
        match started {
            Ok(capture) => Ok((client, capture, event)),
            Err(e) => {
                let _ = CloseHandle(event);
                Err(err("start", e))
            }
        }
    }
}

/// Moves packets into the ring until `stop` is set.
unsafe fn read_packets(capture: &IAudioCaptureClient, event: HANDLE, ring: &Mutex<Ring>, stop: &AtomicBool) -> windows::core::Result<()> {
    while !stop.load(Ordering::Relaxed) {
        // SAFETY: the event belongs to the running stream; buffers are released after copying.
        unsafe {
            WaitForSingleObject(event, 100);
            while capture.GetNextPacketSize()? > 0 {
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                {
                    let mut ring = ring.lock().unwrap_or_else(|e| e.into_inner());
                    if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                        (0..frames).for_each(|_| ring.push(0.0));
                    } else {
                        let samples = std::slice::from_raw_parts(data as *const i16, frames as usize * 2);
                        for frame in samples.chunks_exact(2) {
                            ring.push((frame[0] as f32 + frame[1] as f32) * (0.5 / 32768.0));
                        }
                    }
                    ring.last_data = Some(Instant::now());
                    ring.blocks = ring.blocks.wrapping_add(1);
                }
                capture.ReleaseBuffer(frames)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captures the process tree given in WOOTLIGHT_TEST_PID for two seconds and prints its peak:
    /// `cargo test app_capture_smoke -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn app_capture_smoke() {
        let pid: u32 = std::env::var("WOOTLIGHT_TEST_PID").expect("WOOTLIGHT_TEST_PID").parse().unwrap();
        let capture = Capture::open_app(&App { root: pid, exe: "test.exe".into() }).expect("open");
        let mut samples = vec![0.0f32; 4096];
        for _ in 0..8 {
            std::thread::sleep(Duration::from_millis(250));
            capture.latest(&mut samples);
            let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            println!("peak {peak:.4} blocks {} failed {}", capture.take_block_count(), capture.failed());
        }
        drop(capture);
        println!("closed");
    }
}
