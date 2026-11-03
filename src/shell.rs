//! The app shell both platforms share: commands, the settings file watcher, the status line, the
//! watchdog and the tray icon. Each platform's `tray` module owns the event loop around it.

use std::{fs, path::PathBuf, sync::Arc, thread, time::SystemTime};

use tray_icon::TrayIcon;

use crate::{
    audio, autostart,
    engine::{Owner, Shared, Watchdog},
    log, logging,
    settings::Settings,
    tray,
    ui::{
        self, Command, glyph,
        i18n::{Lang, T, effect_label, tr},
        icon::{self, Mark},
        menu::MenuEntry,
        window,
    },
    update, win,
};

const TOOLTIP_MAX_CHARS: usize = 120;

fn modified(path: &PathBuf) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Output devices, listed off the UI thread: cpal initializes COM per thread.
fn devices() -> Vec<String> {
    thread::spawn(audio::output_device_names).join().unwrap_or_default()
}

/// Saves the current settings where the user chooses. The dialog runs on its own thread.
fn back_up(settings: Settings, lang: Lang) {
    let spawned = thread::Builder::new().name("backup".into()).spawn(move || {
        let (year, month, day) = win::local_date();
        let name = format!("Wootlight settings {year:04}-{month:02}-{day:02}.toml");
        let Some(target) = win::choose_save_path(tr(lang, T::BackUp), tr(lang, T::SettingsFiles), &name) else { return };
        // Written from the settings in use, so the backup holds every key with its comment.
        match settings.save(&target) {
            Ok(()) => log!("settings backed up to {}", target.display()),
            Err(e) => {
                log!("backing up the settings to {} failed: {e}", target.display());
                win::message_box(&format!("{}: {e}", tr(lang, T::BackUpFailed)), true);
            }
        }
    });
    if let Err(e) = spawned {
        log!("backup failed to start: {e}");
    }
}

/// Replaces the settings file with a backup the user picks; the file watcher then applies it.
fn restore(config_path: PathBuf, lang: Lang) {
    let spawned = thread::Builder::new().name("restore".into()).spawn(move || {
        let Some(source) = win::choose_open_path(tr(lang, T::Restore), tr(lang, T::SettingsFiles)) else { return };
        let restored = Settings::load_backup(&source).and_then(|s| s.save(&config_path).map_err(|e| e.to_string()));
        match restored {
            Ok(()) => log!("settings restored from {}", source.display()),
            Err(e) => {
                log!("restoring the settings from {} failed: {e}", source.display());
                win::message_box(&format!("{}: {e}", tr(lang, T::RestoreFailed)), true);
            }
        }
    });
    if let Err(e) = spawned {
        log!("restore failed to start: {e}");
    }
}

pub struct App {
    pub shared: Arc<Shared>,
    config_path: PathBuf,
    config_mtime: Option<SystemTime>,
    config_error: Option<String>,
    pub tray: TrayIcon,
    devices: Vec<String>,
    shown_status: String,
    shown_icon: Option<(Mark, [u32; 3], bool, u32)>,
    watchdog: Watchdog,
}

impl App {
    pub fn new(shared: Arc<Shared>, config_path: PathBuf, config_error: Option<String>, tray: TrayIcon, devices: Vec<String>) -> Self {
        Self {
            shared,
            config_mtime: modified(&config_path),
            config_path,
            config_error,
            tray,
            devices,
            shown_status: String::new(),
            shown_icon: None,
            watchdog: Watchdog::new(),
        }
    }

    pub fn lang(&self) -> Lang {
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
    pub fn status_text(&self) -> String {
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
            (Some(k), Owner::Effect) => format!("{k} · {}", tr(lang, effect_label(s.effect()))),
            (Some(k), _) => format!("{k} · {}", tr(lang, T::StateLive)),
            (None, Owner::Starting) => tr(lang, T::StateStarting).into(),
            (None, _) => tr(lang, T::ConnectKeyboard).into(),
        }
    }

    /// The menu under the status header: open, the visualizer and autostart toggles, quit.
    pub fn menu_entries(&self) -> Vec<MenuEntry> {
        let lang = self.lang();
        let s = self.shared.settings();
        let mut visualizer = MenuEntry::item(tr(lang, T::Visualizer), glyph::EQUALIZER, Command::ToggleEnabled);
        visualizer.checked = s.enabled;
        let mut autostart_item = MenuEntry::item(tr(lang, T::StartWithWindows), glyph::POWER, Command::ToggleAutostart);
        autostart_item.checked = autostart::is_enabled();
        let mut quit = MenuEntry::item(tr(lang, T::Quit), glyph::CLOSE, Command::Quit);
        quit.danger = true;
        let mut entries = vec![MenuEntry::item(tr(lang, T::OpenApp), glyph::OPEN_WINDOW, Command::OpenWindow)];
        if let update::State::Available { version, .. } | update::State::Ready { version, .. } = update::state() {
            entries.push(MenuEntry::item(&ui::update_label(lang, &version, false), glyph::SYNC, Command::InstallUpdate));
        }
        entries.extend([
            visualizer,
            autostart_item,
            MenuEntry::divider(),
            quit,
        ]);
        entries
    }

    pub fn refresh(&mut self) {
        let text = self.status_text();
        if text != self.shown_status {
            let tooltip: String = format!("Wootlight\n{text}").chars().take(TOOLTIP_MAX_CHARS).collect();
            let _ = self.tray.set_tooltip(Some(tooltip));
            self.shown_status = text;
        }
        let settings = self.shared.settings();
        let seed = ui::seed_for(&settings);
        let dark = icon::taskbar_is_dark();
        let size = tray::icon_size();
        let key = (self.mark(), seed.map(|v| v as u32), dark, size);
        if self.shown_icon != Some(key) {
            tray::set_icon(&self.tray, icon::render_rgba(size, key.0, seed, dark), size);
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

    pub fn open_window(&mut self) {
        self.devices = devices();
        window::show(&self.shared, self.devices.clone());
    }

    /// Returns false when the app should quit.
    pub fn execute(&mut self, command: Command) -> bool {
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
            Command::OpenLog => win::open_file(&logging::path()),
            Command::BackUpSettings => back_up(self.shared.settings(), self.lang()),
            Command::RestoreSettings => restore(self.config_path.clone(), self.lang()),
            Command::CheckUpdates => update::check_now(),
            Command::InstallUpdate => {
                if update::install(window::is_visible()) {
                    return false;
                }
                window::refresh(self.config_error.clone());
            }
            Command::Quit => return false,
        }
        true
    }

    pub fn on_timer(&mut self) {
        self.watchdog.check(&self.shared);
        // Music playing or the window open is not a moment to restart.
        let quiet = !window::is_visible() && self.shared.preview().owner != Owner::Live;
        if update::on_tick(&self.shared.settings().updates, quiet) {
            ui::post(Command::Quit);
        }
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
