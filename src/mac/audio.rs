//! Core Audio process taps (macOS 14.4+) into the shared mono ring: everything one output device
//! plays, or the sound of one app's process tree alone, whatever device it plays on. Each tap feeds
//! a private aggregate device whose IOProc reads it; both are destroyed with the capture, and the
//! system removes private ones with the process if it dies first.
//!
//! Taps need the "System Audio Recording Only" permission (TCC kTCCServiceAudioCapture), which
//! macOS asks for the first time a tap starts, naming the app (Info.plist
//! NSAudioCaptureUsageDescription). Opening returns at once; the tap thread waits in the start
//! while the prompt is up, and the capture delivers nothing meanwhile (the engine sees no sound
//! data). Once allowed, the capture reports itself failed so the engine reopens it with sound.
//! While denied, opening fails with a message that says where to allow it.

use std::{
    ffi::{CStr, c_void},
    mem::MaybeUninit,
    ptr::{self, NonNull},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::Duration,
};

use cpal::{Host, traits::DeviceTrait};
use objc2::{AnyThread, rc::Retained, runtime::AnyObject};
use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
    AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, AudioObjectSetPropertyData, CATapDescription, CATapMuteBehavior,
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceNameKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioDevicePropertyDeviceIsAlive,
    kAudioDevicePropertyNominalSampleRate, kAudioHardwarePropertyProcessObjectList,
    kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioHardwarePropertyTranslateUIDToDevice,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    kAudioProcessPropertyPID, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, kAudioTapPropertyDescription,
    kAudioTapPropertyFormat,
};
use objc2_core_audio_types::{AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp, kAudioFormatFlagIsFloat};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

pub use crate::audio_common::{Capture, default_output_id, output_device_names};
use crate::{
    audio_common::{Ring, find_device},
    player::App,
};

/// How often a running tap checks its device, sample rate, permission and the app's processes.
const WATCH_EVERY: Duration = Duration::from_millis(500);
/// Ancestors followed when deciding whether a process belongs to an app.
const MAX_TREE_DEPTH: usize = 32;
/// How long dropping a capture waits for its tap to be torn down.
const TEARDOWN_WAIT: Duration = Duration::from_secs(2);
const DENIED: &str = "System Audio Recording is not allowed for Wootlight: allow it in System Settings > \
                      Privacy & Security > Screen & System Audio Recording";
const UNDECIDED: &str = "System Audio Recording is not allowed yet: allow Wootlight in System Settings > \
                         Privacy & Security > Screen & System Audio Recording";

/// What a capture keeps alive: the tap and the aggregate device that reads it.
pub(crate) type Source = TapStream;

impl Capture {
    /// Opens capture of what the named output device plays, or the default one for "".
    pub fn open(host: &Host, query: &str) -> Result<Self, String> {
        let device = find_device(host, query)?;
        let id = device.id().map_err(|e| e.to_string())?;
        let ring = Ring::new();
        let failed = Arc::new(AtomicBool::new(false));
        let (stream, sample_rate) =
            TapStream::start(Target::Device { uid: id.id().to_string() }, Arc::clone(&ring), Arc::clone(&failed))?;
        Ok(Self {
            _source: stream,
            ring,
            failed,
            id: Some(id),
            name: device.to_string(),
            sample_rate,
            app_root: None,
        })
    }

    /// Opens capture of the sound of `app`'s process tree alone, whatever device it plays on.
    pub fn open_app(app: &App) -> Result<Self, String> {
        let root = i32::try_from(app.root).map_err(|_| format!("bad pid {}", app.root))?;
        let ring = Ring::new();
        let failed = Arc::new(AtomicBool::new(false));
        let (stream, sample_rate) = TapStream::start(Target::App { root }, Arc::clone(&ring), Arc::clone(&failed))?;
        Ok(Self {
            _source: stream,
            ring,
            failed,
            id: None,
            name: app.exe.strip_suffix(".app").unwrap_or(&app.exe).to_string(),
            sample_rate,
            app_root: Some(app.root),
        })
    }
}

/// What a tap listens to.
#[derive(Clone)]
enum Target {
    /// Every process but ours, on one output device's first stream.
    Device { uid: String },
    /// The processes of one app tree, mixed to stereo.
    App { root: i32 },
}

/// A capture's tap thread. It owns the Core Audio objects: it creates them, starts them (which
/// blocks while macOS asks for permission), watches them, and tears them down when told to stop.
pub(crate) struct TapStream {
    stop: Option<mpsc::Sender<()>>,
    done: mpsc::Receiver<()>,
}

impl TapStream {
    /// Creates the tap and its aggregate device and has them start; returns the sample rate.
    fn start(target: Target, ring: Arc<Mutex<Ring>>, failed: Arc<AtomicBool>) -> Result<(Self, u32), String> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel();
        thread::Builder::new()
            .name("audio tap".into())
            .spawn(move || {
                run_tap(target, ring, failed, &ready_tx, &stop_rx);
                let _ = done_tx.send(());
            })
            .map_err(|e| e.to_string())?;
        let sample_rate = ready_rx.recv().map_err(|_| "audio tap thread ended".to_string())??;
        Ok((Self { stop: Some(stop_tx), done: done_rx }, sample_rate))
    }
}

impl Drop for TapStream {
    fn drop(&mut self) {
        drop(self.stop.take());
        // Normally quick; while a permission prompt holds the start, the thread finishes alone.
        let _ = self.done.recv_timeout(TEARDOWN_WAIT);
    }
}

/// The tap thread: sets up, reports the sample rate or the error, starts, then watches.
fn run_tap(
    target: Target,
    ring: Arc<Mutex<Ring>>,
    failed: Arc<AtomicBool>,
    ready: &mpsc::SyncSender<Result<u32, String>>,
    stop: &mpsc::Receiver<()>,
) {
    // A tap started before the permission is granted stalls inside coreaudiod, and with it the
    // playback of every app, so nothing is created until macOS reports the permission granted.
    let permission = audio_capture_permission();
    match permission {
        Permission::Granted => {}
        Permission::Denied => {
            let _ = ready.send(Err(DENIED.into()));
            return;
        }
        Permission::Unknown => {
            request_audio_capture_permission();
            let _ = ready.send(Err(UNDECIDED.into()));
            return;
        }
    }
    let processes = match &target {
        Target::Device { .. } => own_process_object().into_iter().collect(),
        Target::App { root } => app_process_objects(*root),
    };
    let tap = match Tap::create(&target, &processes) {
        Ok(tap) => tap,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(tap.sample_rate));
    let tap = match tap.start(ring) {
        Ok(tap) => tap,
        Err(e) => {
            crate::log!("audio tap: {e}");
            failed.store(true, Ordering::Relaxed);
            return;
        }
    };
    let watch = Watch {
        tap: tap.tap,
        aggregate: tap.aggregate,
        target,
        processes,
        sample_rate: tap.sample_rate,
        permission_pending: false,
        failed,
    };
    watch.run(stop);
    drop(tap);
}

/// A process tap, its private aggregate device and the IOProc that fills the ring. Drop tears
/// down whatever was set up, in reverse order.
struct Tap {
    tap: AudioObjectID,
    aggregate: AudioObjectID,
    io_proc: AudioDeviceIOProcID,
    started: bool,
    /// The ring the IOProc writes into, from `Arc::into_raw`; released after the IOProc.
    ring: *const Mutex<Ring>,
    sample_rate: u32,
}

impl Tap {
    fn create(target: &Target, processes: &[AudioObjectID]) -> Result<Self, String> {
        let mut tap = Tap { tap: 0, aggregate: 0, io_proc: None, started: false, ring: ptr::null(), sample_rate: 0 };
        let description = describe(target, processes);
        // SAFETY: the description is a valid tap description; the id is written on success.
        let status = unsafe { AudioHardwareCreateProcessTap(Some(&description), &mut tap.tap) };
        check(status, "create the process tap")?;

        let format: AudioStreamBasicDescription =
            get(tap.tap, kAudioTapPropertyFormat).map_err(|s| status_text(s, "read the tap format"))?;
        if format.mFormatFlags & kAudioFormatFlagIsFloat == 0 || format.mBitsPerChannel != 32 {
            return Err(format!("unsupported tap format: flags {:#x}, {} bits", format.mFormatFlags, format.mBitsPerChannel));
        }
        // SAFETY: the description is alive; UUID returns its tap UID.
        let tap_uid = unsafe { description.UUID().UUIDString() };
        tap.aggregate = create_aggregate(&tap_uid)?;
        let rate: f64 = get(tap.aggregate, kAudioDevicePropertyNominalSampleRate).unwrap_or(format.mSampleRate);
        tap.sample_rate = rate.round() as u32;
        if tap.sample_rate == 0 {
            return Err("the tap reports no sample rate".into());
        }
        Ok(tap)
    }

    /// Starts reading into `ring`. Until the permission is answered, Core Audio holds both calls
    /// here while macOS asks.
    fn start(mut self, ring: Arc<Mutex<Ring>>) -> Result<Self, String> {
        self.ring = Arc::into_raw(ring);
        // SAFETY: the client data is the ring, alive until the IOProc is destroyed in Drop.
        let status = unsafe {
            AudioDeviceCreateIOProcID(self.aggregate, Some(read_tap), self.ring as *mut c_void, NonNull::from(&mut self.io_proc))
        };
        check(status, "create the IOProc")?;
        // SAFETY: the IOProc was created on this aggregate device.
        check(unsafe { AudioDeviceStart(self.aggregate, self.io_proc) }, "start the tap")?;
        self.started = true;
        crate::log!("audio tap: started at {} Hz", self.sample_rate);
        Ok(self)
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        // SAFETY: each object was created by `create` and is destroyed once; AudioDeviceStop
        // called off the IO thread returns after the IOProc has run for the last time.
        unsafe {
            if self.started {
                AudioDeviceStop(self.aggregate, self.io_proc);
            }
            if self.io_proc.is_some() {
                AudioDeviceDestroyIOProcID(self.aggregate, self.io_proc);
            }
            if self.aggregate != 0 {
                AudioHardwareDestroyAggregateDevice(self.aggregate);
            }
            if self.tap != 0 {
                AudioHardwareDestroyProcessTap(self.tap);
            }
            if !self.ring.is_null() {
                drop(Arc::from_raw(self.ring));
            }
        }
    }
}

/// Mixes each frame of the tap's buffers to mono and pushes it into the ring.
unsafe extern "C-unwind" fn read_tap(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    // SAFETY: the client data is the ring TapStream keeps alive while the IOProc exists; the
    // buffer list holds mNumberBuffers buffers of 32-bit float samples, valid for this call.
    unsafe {
        let ring = &*(client as *const Mutex<Ring>);
        let list = input.as_ref();
        let buffers = std::slice::from_raw_parts(list.mBuffers.as_ptr(), list.mNumberBuffers as usize);
        let channels: u32 = buffers.iter().filter(|b| !b.mData.is_null()).map(|b| b.mNumberChannels).sum();
        let Some(first) = buffers.first().filter(|_| channels > 0) else { return 0 };
        let frames = first.mDataByteSize as usize / 4 / first.mNumberChannels.max(1) as usize;
        let scale = 1.0 / channels as f32;
        let mut ring = ring.lock().unwrap_or_else(|e| e.into_inner());
        for frame in 0..frames {
            let mut sum = 0.0f32;
            for buffer in buffers.iter().filter(|b| !b.mData.is_null()) {
                let n = buffer.mNumberChannels as usize;
                let samples = std::slice::from_raw_parts(buffer.mData as *const f32, buffer.mDataByteSize as usize / 4);
                if let Some(frame) = samples.get(frame * n..frame * n + n) {
                    sum += frame.iter().sum::<f32>();
                }
            }
            ring.push(sum * scale);
        }
        ring.delivered();
    }
    0
}

/// The tap description for `target`: `processes` are excluded from a device tap and are the
/// whole of an app tap.
fn describe(target: &Target, processes: &[AudioObjectID]) -> Retained<CATapDescription> {
    let list = process_list(processes);
    // SAFETY: plain Objective-C initialisers and setters on a fresh object.
    unsafe {
        let description = match target {
            Target::Device { uid } => CATapDescription::initExcludingProcesses_andDeviceUID_withStream(
                CATapDescription::alloc(),
                &list,
                &NSString::from_str(uid),
                0,
            ),
            Target::App { .. } => CATapDescription::initStereoMixdownOfProcesses(CATapDescription::alloc(), &list),
        };
        description.setName(&NSString::from_str("Wootlight"));
        description.setPrivate(true);
        description.setMuteBehavior(CATapMuteBehavior::Unmuted);
        description
    }
}

fn process_list(processes: &[AudioObjectID]) -> Retained<NSArray<NSNumber>> {
    let numbers: Vec<Retained<NSNumber>> = processes.iter().map(|&p| NSNumber::new_u32(p)).collect();
    NSArray::from_retained_slice(&numbers)
}

/// Creates a private aggregate device holding the tap `tap_uid`, started with it.
fn create_aggregate(tap_uid: &NSString) -> Result<AudioObjectID, String> {
    static INSTANCE: AtomicU32 = AtomicU32::new(0);
    let key = |k: &CStr| NSString::from_str(k.to_str().unwrap_or_default());
    let uid = NSString::from_str(&format!(
        "app.wootlight.capture.{}.{}",
        std::process::id(),
        INSTANCE.fetch_add(1, Ordering::Relaxed)
    ));
    let name = NSString::from_str("Wootlight capture");
    let yes = NSNumber::new_bool(true);

    let sub_tap = NSDictionary::<NSString, AnyObject>::from_slices(
        &[&*key(kAudioSubTapUIDKey), &*key(kAudioSubTapDriftCompensationKey)],
        &[tap_uid.as_ref(), yes.as_ref()],
    );
    let taps = NSArray::from_retained_slice(&[sub_tap]);
    let description = NSDictionary::<NSString, AnyObject>::from_slices(
        &[
            &*key(kAudioAggregateDeviceNameKey),
            &*key(kAudioAggregateDeviceUIDKey),
            &*key(kAudioAggregateDeviceIsPrivateKey),
            &*key(kAudioAggregateDeviceTapListKey),
            &*key(kAudioAggregateDeviceTapAutoStartKey),
        ],
        &[name.as_ref(), uid.as_ref(), yes.as_ref(), taps.as_ref(), yes.as_ref()],
    );
    let mut aggregate: AudioObjectID = 0;
    // SAFETY: NSDictionary is toll-free bridged to CFDictionary; the id is written on success.
    let status = unsafe {
        let description = &*(Retained::as_ptr(&description) as *const CFDictionary);
        AudioHardwareCreateAggregateDevice(description, NonNull::from(&mut aggregate))
    };
    check(status, "create the aggregate device")?;
    Ok(aggregate)
}

/// Keeps a running tap honest: follows the app's processes as they come and go, and reports the
/// capture failed when its device goes away, the sample rate changes or permission is answered.
struct Watch {
    tap: AudioObjectID,
    aggregate: AudioObjectID,
    target: Target,
    processes: Vec<AudioObjectID>,
    sample_rate: u32,
    permission_pending: bool,
    failed: Arc<AtomicBool>,
}

impl Watch {
    fn run(mut self, stop: &mpsc::Receiver<()>) {
        while let Err(RecvTimeoutError::Timeout) = stop.recv_timeout(WATCH_EVERY) {
            if let Some(why) = self.check() {
                crate::log!("audio tap: {why}");
                self.failed.store(true, Ordering::Relaxed);
                return;
            }
        }
    }

    /// Why the capture must be reopened, if it must.
    fn check(&mut self) -> Option<String> {
        if self.permission_pending {
            match audio_capture_permission() {
                Permission::Granted => return Some("system audio recording allowed, reopening".into()),
                Permission::Denied => return Some("system audio recording denied".into()),
                Permission::Unknown => {}
            }
        }
        match &self.target {
            Target::Device { uid } => {
                let device = device_by_uid(uid);
                if device == 0 || get::<u32>(device, kAudioDevicePropertyDeviceIsAlive).is_ok_and(|alive| alive == 0) {
                    return Some("the output device is gone".into());
                }
            }
            Target::App { root } => {
                let processes = app_process_objects(*root);
                if processes != self.processes {
                    match retarget(self.tap, &processes) {
                        Ok(()) => crate::log!("audio tap: app now plays from {} process(es)", processes.len()),
                        Err(e) => return Some(format!("could not follow the app's processes: {e}")),
                    }
                    self.processes = processes;
                }
            }
        }
        match get::<f64>(self.aggregate, kAudioDevicePropertyNominalSampleRate) {
            Ok(rate) if rate.round() as u32 != self.sample_rate => {
                Some(format!("sample rate changed from {} to {rate} Hz", self.sample_rate))
            }
            _ => None,
        }
    }
}

/// Points a running app tap at a new set of process objects.
fn retarget(tap: AudioObjectID, processes: &[AudioObjectID]) -> Result<(), String> {
    let current: *mut CATapDescription =
        get(tap, kAudioTapPropertyDescription).map_err(|s| status_text(s, "read the tap description"))?;
    // SAFETY: Core Audio returned a tap description object. It is retained rather than adopted:
    // the property's ownership is not documented, and a leak on each change beats a double free.
    let description = unsafe { Retained::retain(current) }.ok_or("no tap description")?;
    // SAFETY: a setter on a valid description; the property takes a pointer to the object.
    unsafe {
        description.setProcesses(&process_list(processes));
        let value: *const CATapDescription = Retained::as_ptr(&description);
        let status = AudioObjectSetPropertyData(
            tap,
            NonNull::from(&address(kAudioTapPropertyDescription)),
            0,
            ptr::null(),
            size_of::<*const CATapDescription>() as u32,
            NonNull::from(&value).cast(),
        );
        check(status, "update the tap")
    }
}

/// Core Audio process objects of every process in `root`'s app: `root` itself and every process
/// it started, or whose responsible process it is (browser helpers, WebKit's GPU process).
fn app_process_objects(root: i32) -> Vec<AudioObjectID> {
    let objects = get_list(kAudioObjectSystemObject as AudioObjectID, kAudioHardwarePropertyProcessObjectList)
        .unwrap_or_default();
    let mut tree: Vec<AudioObjectID> = objects
        .into_iter()
        .filter(|&object| get::<i32>(object, kAudioProcessPropertyPID).is_ok_and(|pid| belongs_to(pid, root)))
        .collect();
    tree.sort_unstable();
    tree
}

fn belongs_to(pid: i32, root: i32) -> bool {
    descends_from(pid, root) || responsible_pid(pid).is_some_and(|r| r != pid && descends_from(r, root))
}

fn descends_from(mut pid: i32, root: i32) -> bool {
    for _ in 0..MAX_TREE_DEPTH {
        if pid == root {
            return true;
        }
        match parent_pid(pid) {
            Some(parent) if parent > 1 && parent != pid => pid = parent,
            _ => return false,
        }
    }
    false
}

fn parent_pid(pid: i32) -> Option<i32> {
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: the buffer holds a proc_bsdinfo of the size passed.
    let written = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), size) };
    // SAFETY: fully written when proc_pidinfo returns its size.
    (written == size).then(|| unsafe { info.assume_init() }.pbi_ppid as i32)
}

/// The process macOS holds responsible for `pid` (the app for its XPC helpers); private libSystem
/// API, looked up at run time so its absence only narrows what an app capture hears.
fn responsible_pid(pid: i32) -> Option<i32> {
    type Responsible = unsafe extern "C" fn(i32) -> i32;
    static FUNCTION: OnceLock<Option<Responsible>> = OnceLock::new();
    let function = FUNCTION.get_or_init(|| {
        // SAFETY: dlsym with RTLD_DEFAULT and a NUL-terminated name; the symbol has this signature.
        unsafe {
            let symbol = libc::dlsym(libc::RTLD_DEFAULT, c"responsibility_get_pid_responsible_for_pid".as_ptr());
            (!symbol.is_null()).then(|| std::mem::transmute::<*mut c_void, Responsible>(symbol))
        }
    });
    // SAFETY: the function takes any pid and returns -1 when it has none.
    function.map(|f| unsafe { f(pid) }).filter(|&r| r > 0)
}

/// Our own process object, kept out of a device tap.
fn own_process_object() -> Option<AudioObjectID> {
    let pid = std::process::id() as i32;
    get_with::<AudioObjectID, i32>(kAudioObjectSystemObject as AudioObjectID, kAudioHardwarePropertyTranslatePIDToProcessObject, Some(&pid))
        .ok()
        .filter(|&object| object != 0)
}

fn device_by_uid(uid: &str) -> AudioObjectID {
    let uid = NSString::from_str(uid);
    let uid_ref: *const NSString = Retained::as_ptr(&uid);
    get_with::<AudioObjectID, *const NSString>(
        kAudioObjectSystemObject as AudioObjectID,
        kAudioHardwarePropertyTranslateUIDToDevice,
        Some(&uid_ref),
    )
    .unwrap_or(0)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Permission {
    Granted,
    Denied,
    /// Not asked yet, or the state could not be read.
    Unknown,
}

/// Shows the System Audio Recording prompt once per run, without creating any audio object: a tap
/// would ask too, but it holds coreaudiod until the prompt is answered. The engine retries the
/// capture every few seconds and starts once the permission reads granted.
fn request_audio_capture_permission() {
    type Request = unsafe extern "C" fn(*const c_void, *const c_void, *const c_void);
    static ASKED: AtomicBool = AtomicBool::new(false);
    if ASKED.swap(true, Ordering::Relaxed) {
        return;
    }
    // SAFETY: dlopen and dlsym with NUL-terminated strings; the framework stays loaded and
    // TCCAccessRequest takes a service, an options dictionary and a completion block.
    let request = unsafe {
        let tcc = libc::dlopen(c"/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC".as_ptr(), libc::RTLD_LAZY);
        let symbol = if tcc.is_null() { ptr::null_mut() } else { libc::dlsym(tcc, c"TCCAccessRequest".as_ptr()) };
        (!symbol.is_null()).then(|| std::mem::transmute::<*mut c_void, Request>(symbol))
    };
    let Some(request) = request else {
        crate::log!("audio: cannot ask for System Audio Recording; allow Wootlight in System Settings");
        return;
    };
    crate::log!("audio: asking for System Audio Recording");
    let service = NSString::from_str("kTCCServiceAudioCapture");
    let done = block2::RcBlock::new(|granted: u8| {
        crate::log!("audio: System Audio Recording {}", if granted != 0 { "allowed" } else { "not allowed" });
    });
    // SAFETY: NSString is toll-free bridged to CFString; TCC copies the block before returning.
    unsafe { request(Retained::as_ptr(&service).cast(), ptr::null(), block2::RcBlock::as_ptr(&done).cast()) };
}

/// The System Audio Recording permission as macOS reports it to this process, for `--audio-permission`.
pub fn permission_text() -> &'static str {
    match audio_capture_permission() {
        Permission::Granted => "granted",
        Permission::Denied => "denied",
        Permission::Unknown => "not decided",
    }
}

/// The "System Audio Recording Only" permission, from TCC's private preflight call (the only way
/// to read it; it never prompts). Unknown where the call is missing.
fn audio_capture_permission() -> Permission {
    type Preflight = unsafe extern "C" fn(*const c_void, *const c_void) -> i32;
    static FUNCTION: OnceLock<Option<Preflight>> = OnceLock::new();
    let function = FUNCTION.get_or_init(|| {
        // SAFETY: dlopen and dlsym with NUL-terminated strings; the framework stays loaded and
        // TCCAccessPreflight has this signature.
        unsafe {
            let tcc = libc::dlopen(c"/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC".as_ptr(), libc::RTLD_LAZY);
            if tcc.is_null() {
                return None;
            }
            let symbol = libc::dlsym(tcc, c"TCCAccessPreflight".as_ptr());
            (!symbol.is_null()).then(|| std::mem::transmute::<*mut c_void, Preflight>(symbol))
        }
    });
    let Some(preflight) = function else { return Permission::Unknown };
    let service = NSString::from_str("kTCCServiceAudioCapture");
    // SAFETY: NSString is toll-free bridged to CFString; no options dictionary.
    match unsafe { preflight(Retained::as_ptr(&service).cast(), ptr::null()) } {
        0 => Permission::Granted,
        1 => Permission::Denied,
        _ => Permission::Unknown,
    }
}

fn address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Reads a fixed-size property.
fn get<T: Copy>(object: AudioObjectID, selector: u32) -> Result<T, i32> {
    get_with::<T, ()>(object, selector, None)
}

/// Reads a fixed-size property, with a qualifier.
fn get_with<T: Copy, Q>(object: AudioObjectID, selector: u32, qualifier: Option<&Q>) -> Result<T, i32> {
    let mut value = MaybeUninit::<T>::zeroed();
    let mut size = size_of::<T>() as u32;
    let (qualifier_size, qualifier) =
        qualifier.map_or((0, ptr::null()), |q| (size_of::<Q>() as u32, q as *const Q as *const c_void));
    // SAFETY: the value buffer and qualifier are valid for the sizes passed.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address(selector)),
            qualifier_size,
            qualifier,
            NonNull::from(&mut size),
            NonNull::new_unchecked(value.as_mut_ptr().cast()),
        )
    };
    if status != 0 {
        return Err(status);
    }
    if size as usize != size_of::<T>() {
        return Err(-1);
    }
    // SAFETY: Core Audio wrote a whole T.
    Ok(unsafe { value.assume_init() })
}

/// Reads a property that is an array of object ids.
fn get_list(object: AudioObjectID, selector: u32) -> Result<Vec<AudioObjectID>, i32> {
    let addr = address(selector);
    let mut size = 0u32;
    // SAFETY: plain size query.
    let status = unsafe { AudioObjectGetPropertyDataSize(object, NonNull::from(&addr), 0, ptr::null(), NonNull::from(&mut size)) };
    if status != 0 {
        return Err(status);
    }
    let mut ids = vec![0 as AudioObjectID; size as usize / size_of::<AudioObjectID>()];
    if ids.is_empty() {
        return Ok(ids);
    }
    // SAFETY: the buffer holds `size` bytes; Core Audio reports how many it wrote.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&addr),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked(ids.as_mut_ptr().cast()),
        )
    };
    if status != 0 {
        return Err(status);
    }
    ids.truncate(size as usize / size_of::<AudioObjectID>());
    Ok(ids)
}

fn check(status: i32, what: &str) -> Result<(), String> {
    if status == 0 { Ok(()) } else { Err(status_text(status, what)) }
}

/// "could not <what>: 'code'", with the four-character code Core Audio errors usually are.
fn status_text(status: i32, what: &str) -> String {
    let bytes = status.to_be_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        format!("could not {what}: '{}'", String::from_utf8_lossy(&bytes))
    } else {
        format!("could not {what}: error {status}")
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        process::{Child, Command},
        time::Instant,
    };

    use realfft::RealFftPlanner;

    use super::*;

    #[test]
    fn process_tree_membership() {
        let me = std::process::id() as i32;
        let mut child = Command::new("/bin/sleep").arg("5").spawn().unwrap();
        let pid = child.id() as i32;
        assert!(belongs_to(me, me));
        assert!(belongs_to(pid, me), "a child belongs to its parent's app");
        assert!(!belongs_to(me, pid), "a parent does not belong to its child's app");
        assert!(!belongs_to(1, me));
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn status_codes_read_as_text() {
        assert_eq!(status_text(0x6e6f7065, "x"), "could not x: 'nope'");
        assert_eq!(status_text(-50, "x"), "could not x: error -50");
    }

    /// Prints the System Audio Recording state without prompting:
    /// `cargo test audio_permission -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn audio_permission() {
        println!("permission: {:?}", audio_capture_permission());
    }

    /// A quiet sine WAV at `freq` Hz, `secs` long, in the temp dir.
    fn tone(freq: f32, secs: f32) -> PathBuf {
        let rate = 44100u32;
        let samples: Vec<i16> = (0..(rate as f32 * secs) as usize)
            .map(|i| ((i as f32 * freq * std::f32::consts::TAU / rate as f32).sin() * 0.5 * 32767.0) as i16)
            .collect();
        let data = samples.len() as u32 * 2;
        let mut wav = Vec::with_capacity(44 + data as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data.to_le_bytes());
        samples.iter().for_each(|s| wav.extend_from_slice(&s.to_le_bytes()));
        let path = std::env::temp_dir().join(format!("wootlight-tone-{}-{freq}.wav", std::process::id()));
        std::fs::write(&path, wav).unwrap();
        path
    }

    fn play(path: &PathBuf) -> Child {
        Command::new("/usr/bin/afplay").args(["-v", "0.05"]).arg(path).spawn().expect("afplay")
    }

    /// Magnitude spectrum of `samples` (Hann window), one bin per `rate / len` Hz.
    fn spectrum(samples: &[f32]) -> Vec<f32> {
        let n = samples.len();
        let mut input: Vec<f32> = samples
            .iter()
            .enumerate()
            .map(|(i, s)| s * (0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()))
            .collect();
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
        let mut output = fft.make_output_vec();
        fft.process(&mut input, &mut output).unwrap();
        output.iter().map(|c| c.norm()).collect()
    }

    /// Waits up to `within` for the capture to carry sound; returns its peak frequency and the
    /// spectrum it came from.
    fn hear(capture: &Capture, within: Duration) -> Option<(f32, Vec<f32>)> {
        let mut samples = vec![0.0f32; 8192];
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            thread::sleep(Duration::from_millis(300));
            capture.latest(&mut samples);
            let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            println!("peak {peak:.4}, blocks {}, failed {}", capture.take_block_count(), capture.failed());
            if peak > 0.002 {
                let bins = spectrum(&samples);
                let top = (1..bins.len()).max_by(|&a, &b| bins[a].total_cmp(&bins[b])).unwrap();
                return Some((top as f32 * capture.sample_rate as f32 / samples.len() as f32, bins));
            }
        }
        None
    }

    fn bin(capture: &Capture, freq: f32) -> usize {
        (freq * 8192.0 / capture.sample_rate as f32).round() as usize
    }

    /// Captures the default output while a quiet 997 Hz tone plays, and finds the tone at the
    /// reported sample rate: `cargo test device_capture -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn device_capture_hears_a_tone() {
        println!("permission: {:?}", audio_capture_permission());
        let capture = Capture::open(&cpal::default_host(), "").expect("open");
        println!("{} @ {} Hz, id {:?}", capture.name, capture.sample_rate, capture.id);
        assert_eq!(capture.id, default_output_id(&cpal::default_host()));
        let wav = tone(997.0, 3.0);
        let mut player = play(&wav);
        let heard = hear(&capture, Duration::from_secs(3));
        let _ = player.kill();
        let _ = player.wait();
        let _ = std::fs::remove_file(&wav);
        let (freq, _) = heard.expect("no sound captured (is System Audio Recording allowed?)");
        println!("peak frequency {freq:.1} Hz");
        assert!((freq - 997.0).abs() < 15.0, "heard {freq} Hz, not 997 Hz: wrong sample rate?");
        assert!(!capture.failed());
    }

    /// Captures one afplay process alone while another plays a different tone, and finds only its
    /// own: `cargo test app_capture -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn app_capture_hears_only_its_process() {
        println!("permission: {:?}", audio_capture_permission());
        let (mine, other) = (tone(660.0, 4.0), tone(1500.0, 4.0));
        let mut players = [play(&mine), play(&other)];
        let pid = players[0].id();
        let capture = Capture::open_app(&App { root: pid, exe: "afplay".into() }).expect("open");
        println!("{} @ {} Hz, root {:?}", capture.name, capture.sample_rate, capture.app_root);
        let heard = hear(&capture, Duration::from_secs(3));
        for player in &mut players {
            let _ = player.kill();
            let _ = player.wait();
        }
        let _ = std::fs::remove_file(&mine);
        let _ = std::fs::remove_file(&other);
        let (freq, bins) = heard.expect("no sound captured (is System Audio Recording allowed?)");
        let (at_mine, at_other) = (bins[bin(&capture, 660.0)], bins[bin(&capture, 1500.0)]);
        println!("peak frequency {freq:.1} Hz; 660 Hz {at_mine:.2}, 1500 Hz {at_other:.2}");
        assert!((freq - 660.0).abs() < 15.0, "heard {freq} Hz, not 660 Hz");
        assert!(at_other < at_mine * 0.05, "the other process leaked into the app capture");
    }
}
