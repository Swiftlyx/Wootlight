//! Posts a message to a window once per display refresh while running, so the window animates at
//! the monitor's refresh rate (e.g. 165 Hz) instead of a fixed timer. A helper thread waits for the
//! vertical blank of the monitor that shows the window; without one (display asleep, remote
//! session) it keeps the refresh rate with a high-resolution sleep.

use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicIsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        Graphics::{
            Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput},
            Gdi::{
                DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW, HMONITOR,
                MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW, MonitorFromWindow,
            },
        },
        UI::WindowsAndMessaging::PostMessageW,
    },
    core::PCWSTR,
};

const FALLBACK_HZ: u32 = 60;

/// Refresh rate of the monitor showing `hwnd`.
pub fn refresh_rate(hwnd: HWND) -> u32 {
    // SAFETY: plain monitor queries with valid out structures.
    unsafe { monitor_rate(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)) }
}

unsafe fn monitor_rate(monitor: HMONITOR) -> u32 {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    let mut mode = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    // SAFETY: MONITORINFOEXW starts with MONITORINFO; the device name is null-terminated.
    unsafe {
        if GetMonitorInfoW(monitor, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool()
            && EnumDisplaySettingsW(PCWSTR(info.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode).as_bool()
            && mode.dmDisplayFrequency > 1
        {
            return mode.dmDisplayFrequency;
        }
    }
    FALLBACK_HZ
}

struct Inner {
    hwnd: AtomicIsize,
    message: u32,
    running: Mutex<bool>,
    wake: Condvar,
    quit: AtomicBool,
    /// At most one frame message is queued, and none while a frame draws.
    posted: AtomicBool,
}

pub struct FrameClock {
    inner: Arc<Inner>,
    thread: Option<JoinHandle<()>>,
}

impl FrameClock {
    pub fn new(hwnd: HWND, message: u32) -> Self {
        let inner = Arc::new(Inner {
            hwnd: AtomicIsize::new(hwnd.0 as isize),
            message,
            running: Mutex::new(false),
            wake: Condvar::new(),
            quit: AtomicBool::new(false),
            posted: AtomicBool::new(false),
        });
        Self { inner, thread: None }
    }

    pub fn start(&mut self) {
        if self.thread.is_none() {
            let inner = Arc::clone(&self.inner);
            self.thread = thread::Builder::new().name("frame clock".into()).spawn(move || run(&inner)).ok();
        }
        let mut running = self.inner.running.lock().unwrap_or_else(|e| e.into_inner());
        if !*running {
            *running = true;
            // A message posted while the window was hidden may never have been handled.
            self.inner.posted.store(false, Ordering::SeqCst);
            self.inner.wake.notify_all();
        }
    }

    pub fn stop(&self) {
        *self.inner.running.lock().unwrap_or_else(|e| e.into_inner()) = false;
    }

    /// Call once the frame for the posted message is drawn.
    pub fn frame_handled(&self) {
        self.inner.posted.store(false, Ordering::SeqCst);
    }
}

impl Drop for FrameClock {
    fn drop(&mut self) {
        self.inner.quit.store(true, Ordering::SeqCst);
        {
            let _running = self.inner.running.lock().unwrap_or_else(|e| e.into_inner());
            self.inner.wake.notify_all();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The DXGI output of one monitor and its refresh period; lives on the clock thread only.
struct Display {
    factory: Option<IDXGIFactory1>,
    monitor: HMONITOR,
    output: Option<IDXGIOutput>,
    period: Duration,
    last: Instant,
}

impl Display {
    fn output_for(&mut self, monitor: HMONITOR) -> Option<IDXGIOutput> {
        // SAFETY: DXGI enumeration on this thread's own factory.
        unsafe {
            let current = self.factory.as_ref().is_some_and(|f| f.IsCurrent().as_bool());
            if monitor == self.monitor && current && self.output.is_some() {
                return self.output.clone();
            }
            self.monitor = monitor;
            self.period = Duration::from_secs_f64(1.0 / monitor_rate(monitor) as f64);
            self.output = None;
            if !current {
                self.factory = CreateDXGIFactory1::<IDXGIFactory1>().ok();
            }
            let factory = self.factory.as_ref()?;
            let mut a = 0;
            while let Ok(adapter) = factory.EnumAdapters1(a) {
                let mut o = 0;
                while let Ok(output) = adapter.EnumOutputs(o) {
                    if output.GetDesc().is_ok_and(|d| d.Monitor == monitor) {
                        self.output = Some(output);
                        return self.output.clone();
                    }
                    o += 1;
                }
                a += 1;
            }
            None
        }
    }

    fn wait_for_refresh(&mut self, hwnd: HWND) {
        // SAFETY: plain monitor query.
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        if let Some(output) = self.output_for(monitor) {
            // A vertical blank right after the previous frame is skipped, so frames never come
            // faster than the refresh rate even if the wait returns early.
            for _ in 0..2 {
                // SAFETY: blocks until the next vertical blank of this output.
                if unsafe { output.WaitForVBlank() }.is_err() {
                    break;
                }
                let now = Instant::now();
                if now.duration_since(self.last) >= self.period / 2 {
                    self.last = now;
                    return;
                }
            }
        }
        // Frames stay on a fixed grid so sleep latency does not add up; a stall restarts the grid.
        let now = Instant::now();
        let mut next = self.last + self.period;
        if next + self.period < now {
            next = now;
        }
        if let Some(rest) = next.checked_duration_since(now) {
            thread::sleep(rest);
        }
        self.last = next;
    }
}

fn run(inner: &Inner) {
    let mut display = Display {
        factory: None,
        monitor: HMONITOR::default(),
        output: None,
        period: Duration::from_secs_f64(1.0 / FALLBACK_HZ as f64),
        last: Instant::now(),
    };
    loop {
        {
            let running = inner.running.lock().unwrap_or_else(|e| e.into_inner());
            let _running = inner
                .wake
                .wait_while(running, |r| !*r && !inner.quit.load(Ordering::SeqCst))
                .unwrap_or_else(|e| e.into_inner());
        }
        if inner.quit.load(Ordering::SeqCst) {
            break;
        }
        let hwnd = HWND(inner.hwnd.load(Ordering::SeqCst) as *mut _);
        display.wait_for_refresh(hwnd);
        if inner.quit.load(Ordering::SeqCst) {
            break;
        }
        let running = *inner.running.lock().unwrap_or_else(|e| e.into_inner());
        if running && !inner.posted.swap(true, Ordering::SeqCst) {
            // SAFETY: posting to a window owned by the UI thread is allowed from any thread.
            if unsafe { PostMessageW(Some(hwnd), inner.message, WPARAM(0), LPARAM(0)) }.is_err() {
                inner.posted.store(false, Ordering::SeqCst);
            }
        }
    }
}
