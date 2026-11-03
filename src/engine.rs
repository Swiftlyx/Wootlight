//! Two threads. Analysis: audio capture -> spectrum -> bars -> frame, at the display refresh rate
//! while the window is open so it animates smoothly. Keyboard: owns the SDK and sends the freshest
//! frame as fast as the keyboard accepts it (a full frame takes about 16 ms over USB).

use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use cpal::Host;
#[cfg(windows)]
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

use crate::{
    audio::{self, Capture},
    dsp::{BarDynamics, DynamicsConfig, Spectrum, WINDOW},
    effects::{Effect, Effects, Palette, Steer},
    layout::Layout,
    log,
    media::MediaSessions,
    player::{self, App},
    render::{FRAME_LEN, Frame, Renderer},
    sdk::{KeyboardInfo, Sdk},
    settings::Settings,
    win,
};

const KEEPALIVE: Duration = Duration::from_secs(1);
/// Shortest gap between two frames sent to the keyboard. On macOS a back-to-back stream keeps the
/// keyboard so busy that it lags on keystrokes and can stop showing the lighting at all.
#[cfg(target_os = "macos")]
const SEND_INTERVAL: Duration = Duration::from_millis(33);
#[cfg(not(target_os = "macos"))]
const SEND_INTERVAL: Duration = Duration::ZERO;
const RETRY: Duration = Duration::from_secs(1);
const IDLE_POLL: Duration = Duration::from_millis(100);
const DEVICE_CHECK: Duration = Duration::from_secs(2);
const MEDIA_POLL: Duration = Duration::from_millis(250);
const MEDIA_RETRY: Duration = Duration::from_secs(10);
/// How often the playing app is matched to its process again, to follow restarts.
const MEDIA_REMATCH: Duration = Duration::from_secs(5);
const STATS_EVERY: Duration = Duration::from_secs(5);
/// An engine thread that makes no progress this long is logged as stuck.
const STUCK_AFTER: Duration = Duration::from_secs(5);
/// Engine threads, indexing `Shared::beats`.
const KEYBOARD: usize = 0;
const ANALYSIS: usize = 1;
const MEDIA: usize = 2;
const THREAD_NAMES: [&str; 3] = ["keyboard", "analysis", "media"];
/// Beat of a thread that has ended; the watchdog ignores it.
const ENDED: u64 = u64::MAX;
/// Bar height at which a 6-row keyboard shows its top-row key at half fill.
const TOP_ROW_HALF_LIT: f32 = 5.5 / 6.0;
/// Peak sample level (-70 dBFS) above which the output counts as playing.
const AUDIBLE_PEAK: f32 = 3.2e-4;
/// Sound must last this long before the visualizer takes the lighting back.
const RESUME_AFTER: Duration = Duration::from_millis(200);
/// Analysis rate limits, frames per second.
const MIN_RATE: f32 = 30.0;
const MAX_RATE: f32 = 360.0;
/// How often the time since the last keyboard or mouse input is read.
const INPUT_POLL: Duration = Duration::from_millis(100);
/// Input this close to an arrow press counts as that press.
const ARROW_SLACK: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub keyboard: Option<String>,
    pub audio: Option<String>,
    pub problem: Option<String>,
    /// The keyboard shows its own lighting because nothing is playing.
    pub idle: bool,
    /// Wootility's Dim After of the keyboard, `ZERO` when off; None until read or when unreadable.
    pub dim_after: Option<Duration>,
}

/// Decides who owns the lighting: the keyboard after a stretch of silence, the
/// visualizer once sound has been back for a moment. Starts with the keyboard.
#[derive(Default)]
struct SilenceGate {
    last_sound: Option<Instant>,
    sound_since: Option<Instant>,
    visualizer: bool,
}

impl SilenceGate {
    /// True while the visualizer should drive the lighting.
    fn update(&mut self, samples: &[f32], now: Instant, release_after: Duration) -> bool {
        let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        if peak > AUDIBLE_PEAK {
            self.last_sound = Some(now);
            let since = *self.sound_since.get_or_insert(now);
            if now.duration_since(since) >= RESUME_AFTER {
                self.visualizer = true;
            }
        } else {
            self.sound_since = None;
            if self.last_sound.is_none_or(|t| now.duration_since(t) >= release_after) {
                self.visualizer = false;
            }
        }
        self.visualizer
    }
}

/// Who drives the keyboard lighting right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Owner {
    #[default]
    Starting,
    Live,
    /// Nothing plays; the keyboard shows its own profile.
    Idle,
    /// Nothing plays; an effect animates the keyboard.
    Effect,
    Paused,
    NoKeyboard,
}

/// The latest analysis result, for the window's live views.
#[derive(Clone)]
pub struct Preview {
    pub seq: u64,
    pub frame: Frame,
    pub bars: Vec<f32>,
    /// Hue rotation of the flowing rainbow, 0..1.
    pub hue_offset: f32,
    pub owner: Owner,
    pub keyboard: Option<KeyboardInfo>,
}

impl Default for Preview {
    fn default() -> Self {
        Self { seq: 0, frame: [0; FRAME_LEN], bars: Vec::new(), hue_offset: 0.0, owner: Owner::Starting, keyboard: None }
    }
}

/// The newest frame waiting for the keyboard thread. `generation` also moves on state changes
/// (settings, silence, quit), so one wait covers everything the keyboard thread reacts to.
struct Outbox {
    seq: u64,
    generation: u64,
    frame: Frame,
}

/// State shared between the UI thread (writer of settings) and the engine threads.
pub struct Shared {
    settings: Mutex<Settings>,
    version: AtomicU64,
    status: Mutex<Status>,
    preview: Mutex<Preview>,
    /// The identified keyboard, set by the keyboard thread; `keyboard_version` moves with it.
    keyboard: Mutex<Option<KeyboardInfo>>,
    keyboard_version: AtomicU64,
    keyboard_missing: AtomicBool,
    /// Set by analysis: sound plays (or silence release is off), so the visualizer drives the lighting.
    wants_lighting: AtomicBool,
    /// Set by the media thread: a player reports playback, or the media check is off or unavailable.
    media_playing: AtomicBool,
    /// The app whose sound media-only mode captures alone; `media_app_version` moves with it.
    media_app: Mutex<Option<App>>,
    media_app_version: AtomicU64,
    outbox: Mutex<Outbox>,
    outbox_changed: Condvar,
    /// Refresh rate of the monitor showing the window, 0 while it is hidden.
    ui_hz: AtomicU32,
    /// The window shows the effects page, so the effect plays as a preview.
    effect_preview: AtomicBool,
    /// Progress counter of each engine thread and what it does now, for the watchdog.
    beats: [AtomicU64; 3],
    doing: Mutex<[&'static str; 3]>,
    quit: AtomicBool,
    pub stats: bool,
}

impl Shared {
    pub fn new(settings: Settings, stats: bool) -> Self {
        Self {
            settings: Mutex::new(settings),
            version: AtomicU64::new(1),
            status: Mutex::new(Status::default()),
            preview: Mutex::new(Preview::default()),
            keyboard: Mutex::new(None),
            keyboard_version: AtomicU64::new(0),
            keyboard_missing: AtomicBool::new(false),
            wants_lighting: AtomicBool::new(false),
            media_playing: AtomicBool::new(true),
            media_app: Mutex::new(None),
            media_app_version: AtomicU64::new(0),
            outbox: Mutex::new(Outbox { seq: 0, generation: 0, frame: [0; FRAME_LEN] }),
            outbox_changed: Condvar::new(),
            ui_hz: AtomicU32::new(0),
            effect_preview: AtomicBool::new(false),
            beats: Default::default(),
            doing: Mutex::new([""; 3]),
            quit: AtomicBool::new(false),
            stats,
        }
    }

    /// Marks progress of engine thread `thread` as it starts `doing` something.
    fn beat(&self, thread: usize, doing: &'static str) {
        self.doing.lock().unwrap_or_else(|e| e.into_inner())[thread] = doing;
        self.beats[thread].fetch_add(1, Ordering::Relaxed);
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Moves whenever the settings change.
    pub fn settings_version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }

    fn enabled(&self) -> bool {
        self.settings.lock().unwrap_or_else(|e| e.into_inner()).enabled
    }

    fn media_only(&self) -> bool {
        let s = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        s.enabled && s.media_only
    }

    fn set_media_app(&self, app: Option<App>) {
        let mut current = self.media_app.lock().unwrap_or_else(|e| e.into_inner());
        if *current != app {
            *current = app;
            self.media_app_version.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn media_app(&self) -> Option<App> {
        self.media_app.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_settings(&self, settings: Settings) {
        *self.settings.lock().unwrap_or_else(|e| e.into_inner()) = settings;
        self.version.fetch_add(1, Ordering::SeqCst);
        self.wake_keyboard();
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn update_status(&self, f: impl FnOnce(&mut Status)) {
        f(&mut self.status.lock().unwrap_or_else(|e| e.into_inner()));
    }

    pub fn preview(&self) -> Preview {
        self.preview.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn preview_seq(&self) -> u64 {
        self.preview.lock().unwrap_or_else(|e| e.into_inner()).seq
    }

    /// The window asks for analysis at its monitor's refresh rate; 0 when it is hidden.
    pub fn set_ui_hz(&self, hz: u32) {
        self.ui_hz.store(hz, Ordering::Relaxed);
    }

    /// The window opened or left the effects page.
    pub fn set_effect_preview(&self, on: bool) {
        self.effect_preview.store(on, Ordering::Relaxed);
    }

    fn keyboard(&self) -> Option<KeyboardInfo> {
        self.keyboard.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_keyboard(&self, info: KeyboardInfo) {
        *self.keyboard.lock().unwrap_or_else(|e| e.into_inner()) = Some(info);
        self.keyboard_version.fetch_add(1, Ordering::SeqCst);
    }

    /// Publishes a new analysis result; the sequence number moves only when something changed,
    /// so the window redraws nothing while the picture stands still (silence, pause).
    fn publish(&self, frame: &Frame, bars: &[f32], hue_offset: f32, owner: Owner, keyboard: Option<&KeyboardInfo>) {
        let mut p = self.preview.lock().unwrap_or_else(|e| e.into_inner());
        let unchanged = p.owner == owner
            && p.frame == *frame
            && p.bars.as_slice() == bars
            && p.hue_offset == hue_offset
            && (keyboard.is_none() || p.keyboard.as_ref() == keyboard);
        if unchanged {
            return;
        }
        p.seq += 1;
        p.frame = *frame;
        p.bars.clear();
        p.bars.extend_from_slice(bars);
        p.hue_offset = hue_offset;
        p.owner = owner;
        if keyboard.is_some() {
            p.keyboard = keyboard.cloned();
        }
    }

    fn publish_paused(&self) {
        let mut p = self.preview.lock().unwrap_or_else(|e| e.into_inner());
        if p.owner != Owner::Paused {
            p.seq += 1;
            p.owner = Owner::Paused;
            p.frame = [0; FRAME_LEN];
            p.bars.iter_mut().for_each(|b| *b = 0.0);
        }
    }

    fn push_frame(&self, frame: &Frame) {
        let mut o = self.outbox.lock().unwrap_or_else(|e| e.into_inner());
        o.seq += 1;
        o.generation += 1;
        o.frame = *frame;
        self.outbox_changed.notify_all();
    }

    fn set_wants_lighting(&self, wants: bool) {
        if self.wants_lighting.swap(wants, Ordering::SeqCst) != wants {
            self.wake_keyboard();
        }
    }

    fn wake_keyboard(&self) {
        let mut o = self.outbox.lock().unwrap_or_else(|e| e.into_inner());
        o.generation += 1;
        self.outbox_changed.notify_all();
    }

    /// Waits until anything changes after generation `seen` or `timeout` ends;
    /// returns the generation, the frame number and the frame.
    fn wait_change(&self, seen: u64, timeout: Duration) -> (u64, u64, Frame) {
        let guard = self.outbox.lock().unwrap_or_else(|e| e.into_inner());
        let (o, _) = self
            .outbox_changed
            .wait_timeout_while(guard, timeout, |o| o.generation == seen && !self.quitting())
            .unwrap_or_else(|e| e.into_inner());
        (o.generation, o.seq, o.frame)
    }

    fn quitting(&self) -> bool {
        self.quit.load(Ordering::SeqCst)
    }

    pub fn request_quit(&self) {
        self.quit.store(true, Ordering::SeqCst);
        self.wake_keyboard();
    }

    /// Sleeps in short steps so quitting stays responsive.
    fn sleep(&self, total: Duration) {
        let end = Instant::now() + total;
        while !self.quitting() {
            let now = Instant::now();
            if now >= end {
                break;
            }
            thread::sleep((end - now).min(Duration::from_millis(50)));
        }
    }
}

/// Starts the engine threads; each restarts after a panic until quit.
pub fn start(shared: &Arc<Shared>) -> std::io::Result<Vec<JoinHandle<()>>> {
    let spawn = |index: usize, body: fn(&Shared)| {
        let shared = Arc::clone(shared);
        let name = THREAD_NAMES[index];
        thread::Builder::new().name(name.into()).spawn(move || {
            while !shared.quitting() {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&shared)));
                if result.is_ok() {
                    break;
                }
                log!("{name} thread panicked, restarting");
                shared.sleep(RETRY);
            }
            shared.beats[index].store(ENDED, Ordering::Relaxed);
        })
    };
    let threads: [(usize, fn(&Shared)); 3] = [(KEYBOARD, run_keyboard), (ANALYSIS, run_analysis), (MEDIA, run_media)];
    let mut handles = Vec::new();
    for (index, body) in threads {
        match spawn(index, body) {
            Ok(handle) => handles.push(handle),
            Err(e) => {
                // The keyboard thread restores the lighting on its way out.
                shared.request_quit();
                for handle in handles {
                    let _ = handle.join();
                }
                return Err(e);
            }
        }
    }
    Ok(handles)
}

/// Notices engine threads stuck in a call that does not return and logs each episode once.
pub struct Watchdog {
    last_check: Instant,
    seen: [u64; 3],
    since: [Instant; 3],
    stuck: [bool; 3],
}

impl Watchdog {
    pub fn new() -> Self {
        let now = Instant::now();
        Self { last_check: now, seen: [0; 3], since: [now; 3], stuck: [false; 3] }
    }

    /// Call every second or so from a thread outside the engine.
    pub fn check(&mut self, shared: &Shared) {
        let now = Instant::now();
        // A long gap between checks (sleep, a busy UI thread) does not count against the engine.
        let gap = now.duration_since(self.last_check) >= STUCK_AFTER;
        self.last_check = now;
        let doing = *shared.doing.lock().unwrap_or_else(|e| e.into_inner());
        for (i, name) in THREAD_NAMES.iter().enumerate() {
            let beat = shared.beats[i].load(Ordering::Relaxed);
            if beat != self.seen[i] || beat == ENDED {
                if self.stuck[i] {
                    log!("{name} thread running again after {:.0} s", now.duration_since(self.since[i]).as_secs_f32());
                    self.stuck[i] = false;
                }
                self.seen[i] = beat;
                self.since[i] = now;
            } else if gap {
                self.since[i] = now;
            } else if !self.stuck[i] && now.duration_since(self.since[i]) >= STUCK_AFTER {
                log!("{name} thread stuck for {} s while {}", STUCK_AFTER.as_secs(), doing[i]);
                self.stuck[i] = true;
            }
        }
    }
}

/// "Chrome playing, Spotify not playing", or "none".
fn describe_sessions(list: &[(String, bool)]) -> String {
    if list.is_empty() {
        return "none".into();
    }
    list.iter().map(|(id, playing)| format!("{id} {}", if *playing { "playing" } else { "not playing" })).collect::<Vec<_>>().join(", ")
}

/// Tracks whether a player reports playback, and which process plays it, for the media-only mode.
fn run_media(shared: &Shared) {
    // SAFETY: balanced by CoUninitialize at the end of this function, on the same thread.
    #[cfg(windows)]
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let mut sessions: Option<MediaSessions> = None;
    let mut next_connect = Instant::now();
    // The app last seen playing and its process; kept while paused so its capture stays open.
    let mut matched: Option<(String, Option<App>)> = None;
    let mut next_match = Instant::now();
    // Players and their states as last logged.
    let mut logged: Option<Vec<(String, bool)>> = None;
    while !shared.quitting() {
        shared.beat(MEDIA, "waiting");
        let playing = if shared.media_only() {
            if sessions.is_none() && Instant::now() >= next_connect {
                shared.beat(MEDIA, if cfg!(windows) { "connecting to the Windows media controls" } else { "connecting to Now Playing" });
                match MediaSessions::connect() {
                    Ok(s) => sessions = Some(s),
                    Err(e) => {
                        log!("media controls unavailable: {e}");
                        next_connect = Instant::now() + MEDIA_RETRY;
                    }
                }
            }
            shared.beat(MEDIA, if cfg!(windows) { "reading the Windows media controls" } else { "reading Now Playing" });
            match sessions.as_ref().map(MediaSessions::sessions) {
                Some(Ok(list)) => {
                    if logged.as_ref() != Some(&list) {
                        log!("media players: {}", describe_sessions(&list));
                        logged = Some(list.clone());
                    }
                    let now = Instant::now();
                    let playing_id = list.into_iter().find(|(_, playing)| *playing).map(|(id, _)| id);
                    if let Some(id) = &playing_id {
                        let changed = matched.as_ref().is_none_or(|(seen, _)| seen != id);
                        if changed || now >= next_match {
                            shared.beat(MEDIA, "matching the player to its process");
                            let app = player::find(id);
                            if changed || matched.as_ref().is_some_and(|(_, seen)| *seen != app) {
                                match &app {
                                    Some(a) => log!("media: {id} plays from {} (pid {})", a.exe, a.root),
                                    None => log!("media: {id} plays from no process found, listening to the whole device"),
                                }
                            }
                            shared.set_media_app(app.clone());
                            matched = Some((id.clone(), app));
                            next_match = now + MEDIA_REMATCH;
                        }
                    }
                    playing_id.is_some()
                }
                Some(Err(e)) => {
                    log!("media controls failed: {e}");
                    sessions = None;
                    next_connect = Instant::now() + RETRY;
                    true
                }
                // Without the media controls every sound counts.
                None => true,
            }
        } else {
            sessions = None;
            logged = None;
            if matched.take().is_some() {
                shared.set_media_app(None);
            }
            true
        };
        shared.media_playing.store(playing, Ordering::Relaxed);
        shared.sleep(MEDIA_POLL);
    }
    drop(sessions);
    #[cfg(windows)]
    if com {
        // SAFETY: matches the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
}

/// Layout used before a keyboard is identified.
fn fallback_keyboard() -> KeyboardInfo {
    KeyboardInfo { model: "Wooting".into(), rows: 6, cols: 17, device_type: 5, layout: 0 }
}

/// Everything that forces the spectrum and bar state to be rebuilt.
type PipelineKey = (i32, i32, usize, usize, u32, u32, u32, u32);

struct Pipeline {
    key: PipelineKey,
    layout: Layout,
    spectrum: Spectrum,
    dynamics: BarDynamics,
    renderer: Renderer,
    effects: Effects,
    levels: Vec<f32>,
}

fn dynamics_config(s: &Settings) -> DynamicsConfig {
    DynamicsConfig { range_db: s.range_db, floor_db: s.floor_db, headroom_db: s.headroom_db, equalize: s.equalize }
}

impl Pipeline {
    fn key(kb: &KeyboardInfo, sample_rate: u32, s: &Settings) -> PipelineKey {
        (kb.device_type, kb.layout, kb.rows, kb.cols, sample_rate, s.fmin.to_bits(), s.fmax.to_bits(), s.tilt_db.to_bits())
    }

    fn new(kb: &KeyboardInfo, sample_rate: u32, s: &Settings) -> Self {
        let layout = Layout::detect(kb);
        let renderer = Renderer::new(&layout, s);
        let n = renderer.n_bands;
        Self {
            key: Self::key(kb, sample_rate, s),
            spectrum: Spectrum::new(sample_rate, n, s.fmin, s.fmax, s.tilt_db),
            dynamics: BarDynamics::new(n, dynamics_config(s)),
            renderer,
            effects: Effects::new(&layout),
            levels: vec![0.0; n],
            layout,
        }
    }

    /// Keeps bar state and the rainbow's flow when only colors or gain change.
    fn reconfigure(self, kb: &KeyboardInfo, sample_rate: u32, s: &Settings) -> Self {
        if self.key != Self::key(kb, sample_rate, s) {
            return Self::new(kb, sample_rate, s);
        }
        let mut dynamics = self.dynamics;
        dynamics.config = dynamics_config(s);
        Self { renderer: Renderer::new(&self.layout, s).keep_hue_of(&self.renderer), dynamics, ..self }
    }
}

#[derive(Default)]
struct Stats {
    frames: u32,
    bar_sum: f32,
    topped: u32,
    since: Option<Instant>,
    // Per band: input level (dB, before equalization), bar height and bar movement.
    band_level: Vec<f32>,
    band_bar: Vec<f32>,
    band_motion: Vec<f32>,
    prev_bars: Vec<f32>,
}

impl Stats {
    fn add(&mut self, levels: &[f32], bars: &[f32]) {
        self.frames += 1;
        self.bar_sum += bars.iter().sum::<f32>() / bars.len().max(1) as f32;
        self.topped += u32::from(bars.iter().any(|b| *b >= TOP_ROW_HALF_LIT));
        if self.band_bar.len() != bars.len() {
            self.band_level = vec![0.0; bars.len()];
            self.band_bar = vec![0.0; bars.len()];
            self.band_motion = vec![0.0; bars.len()];
            self.prev_bars = bars.to_vec();
        }
        for i in 0..bars.len() {
            self.band_level[i] += levels[i];
            self.band_bar[i] += bars[i];
            self.band_motion[i] += (bars[i] - self.prev_bars[i]).abs();
        }
        self.prev_bars.copy_from_slice(bars);
    }

    fn due(&mut self, now: Instant) -> bool {
        now.duration_since(*self.since.get_or_insert(now)) >= STATS_EVERY
    }

    /// Logs and resets; call when `due`.
    fn report(&mut self, now: Instant, peak: f32, blocks: u32) {
        let since = self.since.unwrap_or(now);
        let secs = now.duration_since(since).as_secs_f32();
        let frames = self.frames.max(1) as f32;
        log!(
            "analysis: {:.1} fps, audio {:.0} blocks/s, peak {:.4}, mean bar {:.2}, top row lit in {:.0}% of frames",
            self.frames as f32 / secs,
            blocks as f32 / secs,
            peak,
            self.bar_sum / frames,
            self.topped as f32 / frames * 100.0
        );
        log!("  band level dB: {}", format_row(&self.band_level, 1.0 / frames, 0));
        log!("  band mean bar: {}", format_row(&self.band_bar, 1.0 / frames, 2));
        log!("  band motion/s: {}", format_row(&self.band_motion, 1.0 / secs, 1));
        *self = Stats { since: Some(now), ..Stats::default() };
    }
}

fn format_row(values: &[f32], scale: f32, digits: usize) -> String {
    values.iter().map(|v| format!("{:.*}", digits, v * scale)).collect::<Vec<_>>().join(" ")
}

/// The settings that decide when the visualizer drives the lighting, for the log.
fn describe_behavior(s: &Settings) -> String {
    let release = if s.release_when_silent { format!("silence after {} s", s.release_after_sec) } else { "never".into() };
    format!(
        "visualizer {}, react to {}, hand back on {release}, audio source {}",
        if s.enabled { "on" } else { "off" },
        if s.media_only { "media players" } else { "any sound" },
        if s.audio_device.is_empty() { if cfg!(windows) { "Windows default" } else { "system default" } } else { &s.audio_device },
    )
}

/// Whether `s.effect` shows now: never over music; always as a preview or when set to always;
/// otherwise once the keyboard and mouse have `rested` for `effect_after_sec`, until the keyboard's
/// Dim After passes after the last input of any kind (`idle`). `rested` leaves out the arrow keys
/// that steer the snake; otherwise the two are the same.
fn effect_shows(s: &Settings, music: bool, preview: bool, rested: Option<Duration>, idle: Option<Duration>, dim_after: Option<Duration>) -> bool {
    if s.effect().is_none() || music {
        return false;
    }
    if preview || s.effect_always() {
        return true;
    }
    let (Some(rested), Some(idle)) = (rested, idle) else { return false };
    rested >= Duration::from_secs_f32(s.effect_after_sec) && dim_after.is_none_or(|dim| dim.is_zero() || idle < dim)
}

/// Why an effect started or stopped, for the log; `was_preview` tells whether the preview showed it.
fn effect_reason(s: &Settings, starts: bool, music: bool, preview: bool, was_preview: bool, idle: Option<Duration>, dim_after: Option<Duration>) -> String {
    let idle_secs = idle.map_or(0.0, |d| d.as_secs_f32());
    if starts {
        return if preview {
            "effects page open".into()
        } else if s.effect_always() {
            "no music, set to always".into()
        } else {
            format!("no keyboard or mouse input for {idle_secs:.0} s")
        };
    }
    if music {
        "music plays".into()
    } else if s.effect().is_none() {
        "turned off".into()
    } else if was_preview && !preview {
        "effects page closed".into()
    } else if idle_secs < s.effect_after_sec {
        "keyboard or mouse used".into()
    } else if let Some(dim) = dim_after.filter(|d| !d.is_zero()) {
        format!("the keyboard's Dim After of {:.0} min passed", dim.as_secs_f32() / 60.0)
    } else {
        "set to show when idle".into()
    }
}

/// "5 min", "off" or "unknown", for the log.
fn describe_dim_after(dim_after: Option<Duration>) -> String {
    match dim_after {
        None => "unknown".into(),
        Some(d) if d.is_zero() => "off".into(),
        Some(d) => format!("{:.0} min", d.as_secs_f32() / 60.0),
    }
}

/// Why the analysis heard nothing for a while, for the log.
fn silence_reason(settings: &Settings, media_playing: bool, capture: Option<&Capture>) -> String {
    if settings.media_only && !media_playing {
        return "no player is playing".into();
    }
    let Some(c) = capture else { return "no audio source is open".into() };
    let while_playing = if settings.media_only { " while a player is playing" } else { "" };
    match c.data_age() {
        Some(age) if age < Duration::from_secs(1) => format!("{} is silent{while_playing}", c.name),
        Some(age) => format!("no sound data from {} for {:.0} s{while_playing}", c.name, age.as_secs_f32()),
        None => format!("no sound data from {} since it opened{while_playing}", c.name),
    }
}

fn run_analysis(shared: &Shared) {
    let host: Host = cpal::default_host();
    let mut settings = shared.settings();
    let mut seen_version = 0;
    let mut capture: Option<Capture> = None;
    let mut next_capture_try = Instant::now();
    // The app media-only mode captures alone, and one whose capture failed, not to be retried.
    let mut media_app: Option<App> = None;
    let mut seen_media_app = 0;
    let mut failed_app: Option<u32> = None;
    let mut next_device_check = Instant::now();
    let mut pipeline: Option<Pipeline> = None;
    // Settings version, keyboard version and sample rate the pipeline was built for.
    let mut pipeline_for = None;
    let mut known: Option<KeyboardInfo> = None;
    let mut seen_keyboard = 0;
    let mut silence = SilenceGate::default();
    let mut owned = false;
    let mut idle: Option<Duration> = None;
    // The last input that was not an arrow steering the snake, and the last arrow press.
    let mut rested_since: Option<Instant> = None;
    let mut arrow_at: Option<Instant> = None;
    let mut arrows = [false; 4];
    let mut dim_after: Option<Duration> = None;
    let mut next_input_poll = Instant::now();
    let mut showing: Option<Effect> = None;
    let mut previewed = false;
    let mut palette: Option<(u64, Effect, Palette)> = None;
    let mut behavior = String::new();
    let mut samples = vec![0.0f32; WINDOW];
    let mut frame: Frame = [0; FRAME_LEN];
    let mut last_tick = Instant::now();
    let mut deadline = Instant::now();
    let mut stats = Stats::default();

    while !shared.quitting() {
        shared.beat(ANALYSIS, "analysing the sound");
        let version = shared.version.load(Ordering::SeqCst);
        if version != seen_version {
            let fresh = shared.settings();
            if fresh.audio_device != settings.audio_device {
                capture = None;
                next_capture_try = Instant::now();
            }
            settings = fresh;
            seen_version = version;
            let described = describe_behavior(&settings);
            if described != behavior {
                log!("settings: {described}");
                behavior = described;
            }
        }

        if !settings.enabled {
            capture = None;
            pipeline = None;
            silence = SilenceGate::default();
            owned = false;
            showing = None;
            shared.set_wants_lighting(false);
            shared.publish_paused();
            shared.update_status(|s| {
                s.audio = None;
                s.idle = false;
            });
            shared.sleep(IDLE_POLL);
            last_tick = Instant::now();
            deadline = last_tick;
            continue;
        }

        let now = Instant::now();
        if settings.audio_device.is_empty() && now >= next_device_check {
            next_device_check = now + DEVICE_CHECK;
            if let Some(c) = &capture {
                if c.id.is_some() && audio::default_output_id(&host) != c.id {
                    log!("default output changed");
                    capture = None;
                    next_capture_try = now;
                }
            }
        }
        if capture.as_ref().is_some_and(Capture::failed) {
            log!("audio stream failed, reopening");
            capture = None;
            next_capture_try = now + Duration::from_millis(500);
        }
        let media_app_version = shared.media_app_version.load(Ordering::SeqCst);
        if media_app_version != seen_media_app {
            media_app = shared.media_app();
            seen_media_app = media_app_version;
        }
        // Media-only mode listens to the playing app alone, so voice chat and games stay out.
        let wanted_app = media_app.as_ref().filter(|app| settings.media_only && failed_app != Some(app.root));
        if capture.as_ref().is_some_and(|c| c.app_root != wanted_app.map(|app| app.root)) {
            capture = None;
            next_capture_try = now;
        }
        if capture.is_none() && now >= next_capture_try {
            shared.beat(ANALYSIS, "opening the audio capture");
            let wanted_app_root = wanted_app.map(|app| app.root);
            let opened = match wanted_app {
                Some(app) => Capture::open_app(app).inspect_err(|e| {
                    log!("capturing {} alone failed: {e}; listening to the whole device", app.exe);
                    failed_app = Some(app.root);
                }),
                None => Capture::open(&host, &settings.audio_device),
            };
            match opened {
                Ok(c) => {
                    log!("audio: {} @ {} Hz", c.name, c.sample_rate);
                    let name = c.name.clone();
                    shared.update_status(|s| {
                        s.audio = Some(name);
                        s.problem = None;
                    });
                    capture = Some(c);
                }
                // The whole device takes over at once from an app that cannot be captured alone.
                Err(_) if wanted_app_root.is_some() && wanted_app_root == failed_app => {}
                Err(e) => {
                    log!("audio open failed: {e}");
                    shared.update_status(|s| {
                        s.audio = None;
                        s.problem = Some(format!("Audio: {e}"));
                    });
                    next_capture_try = now + RETRY * 2;
                }
            }
        }
        match &capture {
            Some(c) => c.latest(&mut samples),
            None => samples.fill(0.0),
        }
        // Media-only mode treats everything as silence while no player reports playback.
        let media_playing = shared.media_playing.load(Ordering::Relaxed);
        if settings.media_only && !media_playing {
            samples.fill(0.0);
        }

        let release_after = Duration::from_secs_f32(settings.release_after_sec);
        let music = silence.update(&samples, now, release_after);
        let owns = music || !settings.release_when_silent;
        if owns != owned {
            let source = capture.as_ref().map_or("no audio source", |c| c.name.as_str());
            if !owns {
                log!("{}: handing the lighting back", silence_reason(&settings, media_playing, capture.as_ref()));
            } else if settings.release_when_silent {
                log!("sound from {source}: taking the lighting");
            } else {
                log!("taking the lighting (no hand-back on silence), listening to {source}");
            }
            owned = owns;
        }

        let playing = settings.snake_play && showing == Some(Effect::Snake);
        if now >= next_input_poll {
            next_input_poll = now + INPUT_POLL;
            idle = win::idle_time();
            dim_after = shared.status().dim_after;
            let input_at = idle.and_then(|d| now.checked_sub(d));
            let steering = playing && input_at.zip(arrow_at).is_some_and(|(input, arrow)| input <= arrow + ARROW_SLACK);
            if !steering {
                rested_since = input_at;
            }
        }
        let rested = rested_since.map(|t| now.saturating_duration_since(t));
        let preview = shared.effect_preview.load(Ordering::Relaxed) && shared.ui_hz.load(Ordering::Relaxed) > 0;
        let effect = settings.effect().filter(|_| effect_shows(&settings, music, preview, rested, idle, dim_after));
        if effect != showing {
            match (showing, effect) {
                (_, Some(e)) => log!("{}: showing {}", effect_reason(&settings, true, music, preview, previewed, rested, dim_after), e.code()),
                (Some(e), None) => log!("{}: {} stopped", effect_reason(&settings, false, music, preview, previewed, rested, dim_after), e.code()),
                (None, None) => {}
            }
            showing = effect;
        }
        previewed = preview;
        let lights = owns || effect.is_some();
        shared.set_wants_lighting(lights);
        shared.update_status(|s| s.idle = !lights);

        let keyboard_version = shared.keyboard_version.load(Ordering::SeqCst);
        if keyboard_version != seen_keyboard {
            known = shared.keyboard();
            seen_keyboard = keyboard_version;
        }
        let sample_rate = capture.as_ref().map_or(48000, |c| c.sample_rate);
        let inputs = Some((seen_version, seen_keyboard, sample_rate));
        let kb = || known.clone().unwrap_or_else(fallback_keyboard);
        let p = match pipeline.take() {
            Some(p) if pipeline_for == inputs => p,
            Some(p) => p.reconfigure(&kb(), sample_rate, &settings),
            None => Pipeline::new(&kb(), sample_rate, &settings),
        };
        pipeline_for = inputs;
        let p = pipeline.insert(p);

        let dt = now.duration_since(last_tick).as_secs_f32().min(0.1);
        last_tick = now;
        p.spectrum.bands_db(&samples, &mut p.levels);
        p.dynamics.update(&p.levels, dt);
        p.renderer.render(&p.dynamics.bars, dt, &mut frame);
        match effect {
            Some(e) => {
                if palette.as_ref().is_none_or(|(version, of, _)| *version != seen_version || *of != e) {
                    palette = Some((seen_version, e, Palette::for_settings(&settings, e)));
                }
                let colors = palette.as_ref().map(|(_, _, colors)| colors).expect("set above");
                // The arrow keys are read only while they steer the snake.
                if settings.snake_play && e == Effect::Snake {
                    let held = win::arrow_keys();
                    for (direction, (down, was)) in Steer::ALL.into_iter().zip(held.into_iter().zip(arrows)) {
                        if down && !was {
                            p.effects.steer(direction);
                        }
                    }
                    if held.contains(&true) {
                        arrow_at = Some(now);
                    }
                    arrows = held;
                }
                p.effects.render(e, colors, settings.brightness, dt, &mut frame);
            }
            // The next time an effect shows, it starts afresh.
            None => p.effects.restart(),
        }

        let owner = if shared.keyboard_missing.load(Ordering::SeqCst) {
            Owner::NoKeyboard
        } else if effect.is_some() {
            Owner::Effect
        } else if !owns {
            Owner::Idle
        } else if known.is_some() {
            Owner::Live
        } else {
            Owner::Starting
        };
        shared.publish(&frame, &p.dynamics.bars, p.renderer.hue_offset(), owner, known.as_ref());
        if lights {
            shared.push_frame(&frame);
        }

        if shared.stats {
            stats.add(&p.levels, &p.dynamics.bars);
            if stats.due(now) {
                let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
                let blocks = capture.as_ref().map_or(0, Capture::take_block_count);
                stats.report(now, peak, blocks);
            }
        }

        // Fixed-rate ticks: the window's refresh rate while it is open, else the configured rate.
        let ui_hz = shared.ui_hz.load(Ordering::Relaxed) as f32;
        let rate = settings.fps.max(ui_hz).clamp(MIN_RATE, MAX_RATE);
        let period = Duration::from_secs_f32(1.0 / rate);
        deadline += period;
        let now = Instant::now();
        if deadline + period < now {
            deadline = now;
        }
        if let Some(rest) = deadline.checked_duration_since(now) {
            thread::sleep(rest);
        }
    }
    drop(capture);
}

#[derive(Default)]
struct SendStats {
    sent: u32,
    time: Duration,
    since: Option<Instant>,
}

fn run_keyboard(shared: &Shared) {
    shared.beat(KEYBOARD, "loading the Wooting SDK");
    let sdk = match Sdk::load() {
        Ok(sdk) => sdk,
        Err(e) => {
            log!("SDK load failed: {e}");
            shared.update_status(|s| s.problem = Some(format!("Wooting SDK: {e}")));
            return;
        }
    };
    let mut connected = false;
    let mut identified = false;
    let mut generation = 0;
    let mut last_seq = 0;
    let mut last_frame: Option<Frame> = None;
    let mut last_sent = Instant::now();
    let mut stats = SendStats::default();

    while !shared.quitting() {
        shared.beat(KEYBOARD, "waiting");
        let enabled = shared.enabled();
        let wanted = enabled && shared.wants_lighting.load(Ordering::SeqCst);

        // Identify the board once at start, and connect whenever the visualizer takes over.
        if !identified || (wanted && !connected) {
            shared.beat(KEYBOARD, "connecting to the keyboard");
            if !sdk.connect() {
                if !shared.keyboard_missing.swap(true, Ordering::SeqCst) {
                    log!("keyboard not found");
                }
                shared.update_status(|s| s.keyboard = None);
                shared.sleep(RETRY);
                continue;
            }
            let info = sdk.info();
            if !identified {
                log!("keyboard: {} ({}x{}, type {}, layout {})", info.model, info.rows, info.cols, info.device_type, info.layout);
            }
            // Read on every connect, so a change in Wootility shows up at the next one.
            let dim_after = sdk.dim_after();
            if !identified || shared.status().dim_after != dim_after {
                log!("keyboard Dim After: {}", describe_dim_after(dim_after));
            }
            shared.update_status(|s| s.dim_after = dim_after);
            shared.keyboard_missing.store(false, Ordering::SeqCst);
            shared.update_status(|s| s.keyboard = Some(info.model.clone()));
            shared.set_keyboard(info);
            identified = true;
            last_seq = 0;
            last_frame = None;
            if wanted {
                connected = true;
                log!("visualizer took over the lighting");
            } else {
                // Connected only to identify the board; the lighting stays with the keyboard.
                sdk.close();
            }
        }

        if connected && !wanted {
            shared.beat(KEYBOARD, "handing the lighting back");
            sdk.close();
            connected = false;
            log!("{}", if enabled { "lighting handed back to the keyboard" } else { "paused, lighting restored" });
        }
        if !connected {
            generation = shared.wait_change(generation, IDLE_POLL).0;
            continue;
        }

        // Waiting out the gap first means the frame picked up below is the freshest one.
        let gap = SEND_INTERVAL.saturating_sub(last_sent.elapsed());
        if !gap.is_zero() {
            shared.sleep(gap);
        }
        let (next_generation, seq, frame) = shared.wait_change(generation, KEEPALIVE.saturating_sub(last_sent.elapsed()));
        generation = next_generation;
        if shared.quitting() {
            break;
        }
        // A state change without a new frame: re-check who owns the lighting first.
        if seq == last_seq && last_sent.elapsed() < KEEPALIVE {
            continue;
        }
        // Quiet passages render the same frame over and over; the keyboard already shows it.
        if last_frame == Some(frame) && last_sent.elapsed() < KEEPALIVE {
            last_seq = seq;
            continue;
        }
        let started = Instant::now();
        shared.beat(KEYBOARD, "sending a frame to the keyboard");
        if !sdk.show(&frame) {
            log!("keyboard disconnected");
            connected = false;
            identified = false;
            continue;
        }
        last_seq = seq;
        last_frame = Some(frame);
        last_sent = Instant::now();

        if shared.stats {
            stats.time += started.elapsed();
            stats.sent += 1;
            let since = *stats.since.get_or_insert(started);
            let secs = started.duration_since(since).as_secs_f32();
            if secs >= STATS_EVERY.as_secs_f32() {
                log!("keyboard: {:.1} frames/s, {:.2} ms per frame", stats.sent as f32 / secs, stats.time.as_secs_f32() * 1000.0 / stats.sent as f32);
                stats = SendStats { since: Some(started), ..SendStats::default() };
            }
        }
    }
    if connected {
        sdk.close();
        log!("lighting restored");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE: Duration = Duration::from_secs(3);

    #[test]
    fn effects_follow_input_and_dim_after() {
        let s = Settings { effect: "rain".into(), effect_after_sec: 60.0, ..Settings::default() };
        let secs = |v: u64| Some(Duration::from_secs(v));
        let five_min = secs(300);
        let shows = |s: &Settings, music, preview, idle, dim| effect_shows(s, music, preview, idle, idle, dim);
        assert!(!shows(&s, false, false, secs(30), five_min), "before the wait");
        assert!(shows(&s, false, false, secs(90), five_min));
        assert!(!shows(&s, false, false, secs(301), five_min), "the keyboard dims now");
        assert!(shows(&s, false, false, secs(3600), Some(Duration::ZERO)), "Dim After off");
        assert!(shows(&s, false, false, secs(3600), None), "Dim After unknown");
        assert!(!shows(&s, false, false, None, None), "no input time");
        assert!(!shows(&s, true, true, secs(90), None), "never over music");
        assert!(shows(&s, false, true, secs(0), five_min), "the effects page previews it");
        let short_dim = Settings { effect_after_sec: 300.0, ..s.clone() };
        assert!(!shows(&short_dim, false, false, secs(400), five_min), "dims before it would start");
        let always = Settings { effect_when: "always".into(), ..s.clone() };
        assert!(shows(&always, false, false, secs(0), five_min));
        // Steering the snake: arrows just pressed, the last other input long ago.
        assert!(effect_shows(&s, false, false, secs(200), secs(0), five_min), "arrows keep the game going");
        assert!(effect_shows(&s, false, false, secs(900), secs(1), five_min), "and keep the keyboard from dimming");
        let off = Settings { effect: "off".into(), ..s };
        assert!(!shows(&off, false, true, secs(3600), None));
    }

    fn feed(gate: &mut SilenceGate, start: Instant, from_ms: u64, to_ms: u64, level: f32) -> bool {
        let samples = vec![level; 64];
        let mut owns = false;
        for ms in (from_ms..to_ms).step_by(20) {
            owns = gate.update(&samples, start + Duration::from_millis(ms), RELEASE);
        }
        owns
    }

    #[test]
    fn keyboard_keeps_lighting_until_sound_lasts() {
        let mut gate = SilenceGate::default();
        let t = Instant::now();
        assert!(!feed(&mut gate, t, 0, 1000, 0.0));
        // A click shorter than the resume delay does not take over.
        assert!(!feed(&mut gate, t, 1000, 1100, 0.2));
        assert!(!feed(&mut gate, t, 1100, 1200, 0.0));
        assert!(feed(&mut gate, t, 1200, 1500, 0.2));
    }

    #[test]
    fn short_gaps_keep_visualizer_long_silence_releases() {
        let mut gate = SilenceGate::default();
        let t = Instant::now();
        assert!(feed(&mut gate, t, 0, 1000, 0.2));
        // A two-second gap between tracks keeps the visualizer.
        assert!(feed(&mut gate, t, 1000, 3000, 0.0));
        assert!(feed(&mut gate, t, 3000, 4000, 0.2));
        assert!(!feed(&mut gate, t, 4000, 7100, 0.0));
    }

    #[test]
    fn faint_noise_counts_as_silence() {
        let mut gate = SilenceGate::default();
        assert!(!feed(&mut gate, Instant::now(), 0, 1000, 1e-4));
    }

    #[test]
    fn reconfigure_keeps_the_flowing_hue() {
        let kb = fallback_keyboard();
        let s = Settings { color: "flow".into(), ..Settings::default() };
        let mut p = Pipeline::new(&kb, 48000, &s);
        let mut frame = [0; FRAME_LEN];
        p.renderer.render(&p.dynamics.bars, 1.0, &mut frame);
        let hue = p.renderer.hue_offset();
        let p = p.reconfigure(&kb, 48000, &Settings { brightness: 0.5, ..s });
        assert!(hue > 0.0);
        assert_eq!(p.renderer.hue_offset(), hue);
    }

    #[test]
    fn keyboard_thread_wakes_on_new_frame() {
        let shared = Arc::new(Shared::new(Settings::default(), false));
        let waiter = {
            let shared = Arc::clone(&shared);
            thread::spawn(move || {
                let started = Instant::now();
                let (_, seq, _) = shared.wait_change(0, Duration::from_secs(5));
                (seq, started.elapsed())
            })
        };
        thread::sleep(Duration::from_millis(50));
        shared.push_frame(&[7; FRAME_LEN]);
        let (seq, waited) = waiter.join().unwrap();
        assert_eq!(seq, 1);
        assert!(waited < Duration::from_secs(1));
    }
}
