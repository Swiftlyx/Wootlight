//! The app shell on the UI thread: the tray icon, the command queue, settings and the message loop.

use std::{fs, path::PathBuf, ptr, sync::Arc, thread, time::SystemTime};

use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::UI::{
    HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi},
    WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, IsWindow, MSG, SM_CXSMICON, SetTimer, TranslateMessage, WM_CLOSE, WM_TIMER,
    },
};

use crate::{
    audio, autostart,
    engine::{Owner, Shared},
    log,
    settings::Settings,
    ui::{
        self, Command, glyph,
        i18n::{Lang, T, tr},
        icon::{self, Mark},
        tray_menu::{self, MenuEntry},
        window,
    },
    win,
};

const TIMER_MS: u32 = 500;
const TOOLTIP_MAX_CHARS: usize = 120;

fn modified(path: &PathBuf) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Output devices, listed off the UI thread: cpal initializes COM per thread.
fn devices() -> Vec<String> {
    thread::spawn(audio::output_device_names).join().unwrap_or_default()
}

fn tray_icon_size() -> u32 {
    // SAFETY: plain metric queries.
    unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()).max(16) as u32 }
}

struct App {
    shared: Arc<Shared>,
    config_path: PathBuf,
    config_mtime: Option<SystemTime>,
    config_error: Option<String>,
    tray: TrayIcon,
    devices: Vec<String>,
    shown_status: String,
    shown_icon: Option<(Mark, [u32; 3], bool)>,
}

impl App {
    fn lang(&self) -> Lang {
        Lang::from_code(&self.shared.settings().language)
    }

    fn mark(&self) -> Mark {
        let s = self.shared.settings();
        if !s.enabled || self.config_error.is_some() {
            Mark::Paused
        } else if self.shared.preview().owner == Owner::Live {
            Mark::Live
        } else {
            Mark::Idle
        }
    }

    /// The short status line for the tray tooltip and the menu header.
    fn status_text(&self) -> String {
        let lang = self.lang();
        let s = self.shared.settings();
        let st = self.shared.status();
        if let Some(e) = &self.config_error {
            return format!("{}: {e}", tr(lang, T::ConfigError));
        }
        if !s.enabled {
            return tr(lang, T::StatePaused).into();
        }
        if let Some(problem) = &st.problem {
            return problem.clone();
        }
        match (&st.keyboard, self.shared.preview().owner) {
            (Some(k), Owner::Idle) => format!("{k} · {}", tr(lang, T::StateIdle)),
            (Some(k), _) => format!("{k} · {}", tr(lang, T::StateLive)),
            (None, Owner::Starting) => tr(lang, T::StateStarting).into(),
            (None, _) => tr(lang, T::ConnectKeyboard).into(),
        }
    }

    fn refresh(&mut self) {
        let text = self.status_text();
        if text != self.shown_status {
            let tooltip: String = format!("Wootlight\n{text}").chars().take(TOOLTIP_MAX_CHARS).collect();
            let _ = self.tray.set_tooltip(Some(tooltip));
            self.shown_status = text;
        }
        let settings = self.shared.settings();
        let seed = ui::seed_for(&settings);
        let dark = icon::taskbar_is_dark();
        let key = (self.mark(), seed.map(|v| v as u32), dark);
        if self.shown_icon != Some(key) {
            let size = tray_icon_size();
            if let Ok(icon) = Icon::from_rgba(icon::render_rgba(size, key.0, seed, dark), size, size) {
                let _ = self.tray.set_icon(Some(icon));
            }
            self.shown_icon = Some(key);
        }
        window::refresh(self.config_error.clone());
    }

    fn apply(&mut self, settings: Settings, save: bool) {
        let settings = settings.sanitized();
        if save {
            if let Err(e) = settings.save(&self.config_path) {
                log!("saving settings failed: {e}");
            }
            self.config_mtime = modified(&self.config_path);
            self.config_error = None;
        }
        self.shared.set_settings(settings);
        // A slider drag applies many times a second and redraws the window itself; the tray icon
        // and tooltip follow once the value is saved (or on the next timer tick).
        if save {
            self.refresh();
        }
    }

    fn open_window(&mut self) {
        self.devices = devices();
        window::show(&self.shared, self.devices.clone());
    }

    /// Returns false when the app should quit.
    fn execute(&mut self, command: Command) -> bool {
        match command {
            Command::OpenWindow => self.open_window(),
            Command::ToggleWindow => {
                if window::is_visible() {
                    window::hide();
                } else {
                    self.open_window();
                }
            }
            Command::ToggleEnabled => {
                let mut s = self.shared.settings();
                s.enabled = !s.enabled;
                self.apply(s, true);
            }
            Command::ToggleAutostart => {
                if !autostart::set_enabled(!autostart::is_enabled()) {
                    log!("changing autostart failed");
                }
                log!("autostart: {}", autostart::is_enabled());
                self.refresh();
            }
            Command::Apply { settings, save } => self.apply(*settings, save),
            Command::OpenConfig => {
                if modified(&self.config_path).is_none() {
                    let _ = self.shared.settings().save(&self.config_path);
                    self.config_mtime = modified(&self.config_path);
                }
                win::open_file(&self.config_path);
            }
            Command::RevealConfig => win::reveal_file(&self.config_path),
            Command::Quit => return false,
        }
        true
    }

    fn open_menu(&mut self, rect: tray_icon::Rect, position: tray_icon::dpi::PhysicalPosition<f64>) {
        let lang = self.lang();
        let s = self.shared.settings();
        let mut visualizer = MenuEntry::item(tr(lang, T::Visualizer), glyph::EQUALIZER, Command::ToggleEnabled);
        visualizer.checked = s.enabled;
        let mut autostart_item = MenuEntry::item(tr(lang, T::StartWithWindows), glyph::POWER, Command::ToggleAutostart);
        autostart_item.checked = autostart::is_enabled();
        let mut quit = MenuEntry::item(tr(lang, T::Quit), glyph::CLOSE, Command::Quit);
        quit.danger = true;
        let entries = vec![
            MenuEntry::item(tr(lang, T::OpenApp), glyph::OPEN_WINDOW, Command::OpenWindow),
            visualizer,
            autostart_item,
            MenuEntry::divider(),
            quit,
        ];
        let icon_rect = windows::Win32::Foundation::RECT {
            left: rect.position.x as i32,
            top: rect.position.y as i32,
            right: rect.position.x as i32 + rect.size.width as i32,
            bottom: rect.position.y as i32 + rect.size.height as i32,
        };
        let cursor = windows::Win32::Foundation::POINT { x: position.x as i32, y: position.y as i32 };
        tray_menu::open(icon_rect, cursor, self.status_text(), entries, ui::palette_for(&s));
    }

    fn on_tray_event(&mut self, event: TrayIconEvent) {
        match event {
            TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } => {
                if !tray_menu::is_open() {
                    self.execute(Command::ToggleWindow);
                }
            }
            TrayIconEvent::Click { button: MouseButton::Right, button_state: MouseButtonState::Up, rect, position, .. } => {
                self.open_menu(rect, position);
            }
            _ => {}
        }
    }

    fn on_timer(&mut self) {
        let mtime = modified(&self.config_path);
        if mtime != self.config_mtime {
            self.config_mtime = mtime;
            if mtime.is_none() {
                // Recreate a deleted file from the current settings.
                self.apply(self.shared.settings(), true);
            } else {
                match Settings::load(&self.config_path) {
                    Ok(s) => {
                        self.config_error = None;
                        if s != self.shared.settings() {
                            log!("settings reloaded from file");
                            self.apply(s, false);
                        }
                    }
                    Err(e) => {
                        log!("settings file error: {e}");
                        self.config_error = Some(e);
                    }
                }
            }
        }
        self.refresh();
    }
}

/// Runs the tray until the user quits or the session ends.
pub fn run(
    shared: Arc<Shared>,
    config_path: PathBuf,
    config_error: Option<String>,
    devices: Vec<String>,
    open_window: bool,
) -> Result<(), String> {
    ui::init_ui_thread();
    let settings = shared.settings();
    let size = tray_icon_size();
    let first_icon = Icon::from_rgba(icon::render_rgba(size, Mark::Idle, ui::seed_for(&settings), icon::taskbar_is_dark()), size, size)
        .map_err(|e| e.to_string())?;
    let tray = TrayIconBuilder::new()
        .with_menu_on_left_click(false)
        .with_menu_on_right_click(false)
        .with_tooltip("Wootlight")
        .with_icon(first_icon)
        .build()
        .map_err(|e| e.to_string())?;

    if !win::create_session_window() {
        log!("session window not created; lighting may not restore on logoff");
    }

    let mut app = App {
        shared,
        config_mtime: modified(&config_path),
        config_path,
        config_error,
        tray,
        devices,
        shown_status: String::new(),
        shown_icon: None,
    };
    app.refresh();
    if open_window {
        app.open_window();
    }


    // SAFETY: standard Win32 message loop on the thread that owns the tray and windows.
    unsafe {
        SetTimer(ptr::null_mut(), 0, TIMER_MS, None);
        let mut msg: MSG = std::mem::zeroed();
        'run: loop {
            let result = GetMessageW(&mut msg, ptr::null_mut(), 0, 0);
            if result == 0 || result == -1 {
                break;
            }
            // `taskkill` posts WM_CLOSE to our top-level windows; any of them means quit.
            if msg.message == WM_CLOSE {
                log!("close requested");
                break;
            }
            if msg.message == WM_TIMER && msg.hwnd.is_null() {
                if IsWindow(app.tray.window_handle() as _) == 0 {
                    log!("tray window lost, exiting");
                    break;
                }
                app.on_timer();
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
            while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                app.on_tray_event(event);
            }
            while let Some(command) = ui::take_command() {
                if !app.execute(command) {
                    break 'run;
                }
            }
        }
    }
    tray_menu::close();
    window::destroy();
    // The tray icon is the last user of the icon canvas.
    drop(app);
    icon::release_canvas();
    Ok(())
}
