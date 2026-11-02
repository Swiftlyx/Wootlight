//! Wootlight: music spectrum visualizer on Wooting keyboard lighting, living in the tray.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod autostart;
mod dsp;
mod engine;
mod layout;
mod logging;
mod media;
mod paths;
mod render;
mod sdk;
mod settings;
mod tray;
mod ui;
mod win;

use std::{
    sync::{Arc, Mutex, OnceLock},
    thread::{self, JoinHandle},
};

use engine::Shared;
use settings::Settings;

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
static ENGINE: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

/// Stops the engine threads; the keyboard thread restores the lighting. Safe to call repeatedly.
pub fn stop_engine() {
    if let Some(shared) = SHARED.get() {
        shared.request_quit();
    }
    let handles = std::mem::take(&mut *ENGINE.lock().unwrap_or_else(|e| e.into_inner()));
    for handle in handles {
        let _ = handle.join();
    }
}

fn main() {
    let stats = std::env::args().any(|a| a == "--stats");
    let open_window = std::env::args().any(|a| a == "--open");
    // SAFETY: process-wide setting made before any window exists.
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    if !win::acquire_single_instance() {
        // A second launch opens the window of the one already running.
        if !win::signal_running_instance() {
            win::message_box("Wootlight is already running.", false);
        }
        return;
    }
    logging::init();
    log!("Wootlight {} starting", env!("CARGO_PKG_VERSION"));

    let config_path = paths::config_file();
    log!("settings: {}", config_path.display());
    let (settings, config_error) = match Settings::load(&config_path) {
        Ok(s) => {
            if let Err(e) = s.upgrade_file(&config_path) {
                log!("cannot update {}: {e}", config_path.display());
            }
            (s, None)
        }
        Err(_) if !config_path.exists() => {
            let s = Settings::default();
            if let Err(e) = s.save(&config_path) {
                log!("cannot create {}: {e}", config_path.display());
            }
            (s, None)
        }
        Err(e) => {
            log!("settings file error: {e}");
            (Settings::default(), Some(e))
        }
    };
    // Runs schtasks.exe; startup does not wait for it.
    thread::spawn(autostart::refresh);

    let shared = Arc::new(Shared::new(settings, stats));
    let _ = SHARED.set(Arc::clone(&shared));

    // cpal initializes COM per thread; keep it off the UI thread.
    let devices = thread::spawn(audio::output_device_names).join().unwrap_or_default();

    match engine::start(&shared) {
        Ok(handles) => *ENGINE.lock().unwrap_or_else(|e| e.into_inner()) = handles,
        Err(e) => {
            win::message_box(&format!("Wootlight could not start: {e}"), true);
            return;
        }
    }

    if let Err(e) = tray::run(shared, config_path, config_error, devices, open_window) {
        log!("tray failed: {e}");
        win::message_box(&format!("Wootlight could not create its tray icon: {e}"), true);
    }
    stop_engine();
    log!("stopped");
}
