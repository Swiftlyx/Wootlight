//! The window, the tray menu and the drawing they share. Everything here runs on the UI thread.

pub mod canvas;
pub mod design;
pub mod frame_clock;
pub mod i18n;
pub mod icon;
pub mod palette;
pub mod spring;
pub mod tray_menu;
pub mod window;

use std::{
    cell::RefCell,
    collections::VecDeque,
    sync::atomic::{AtomicU32, Ordering},
};

use windows::Win32::{
    Foundation::{LPARAM, WPARAM},
    System::Threading::GetCurrentThreadId,
    UI::WindowsAndMessaging::{PostThreadMessageW, WM_APP},
};

use crate::settings::{ColorMode, Settings};
use palette::{NEUTRAL_SEED, Palette, make_palette};

/// Posted to the UI thread when a command is queued.
pub const WM_APP_COMMAND: u32 = WM_APP + 20;

/// What the window and the menu ask the app to do.
#[derive(Clone, Debug)]
pub enum Command {
    OpenWindow,
    ToggleWindow,
    ToggleEnabled,
    ToggleAutostart,
    /// New settings; `save` is false while a slider is being dragged.
    Apply { settings: Box<Settings>, save: bool },
    OpenConfig,
    RevealConfig,
    OpenLog,
    Quit,
}

static UI_THREAD: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static QUEUE: RefCell<VecDeque<Command>> = const { RefCell::new(VecDeque::new()) };
    static PALETTE: RefCell<Option<(([u32; 3], bool), Palette)>> = const { RefCell::new(None) };
}

pub fn init_ui_thread() {
    // SAFETY: plain query.
    UI_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
}

/// Queues a command; it runs from the message loop, outside any window procedure.
pub fn post(command: Command) {
    QUEUE.with(|q| q.borrow_mut().push_back(command));
    // SAFETY: wakes the UI thread's message loop.
    unsafe {
        let _ = PostThreadMessageW(UI_THREAD.load(Ordering::SeqCst), WM_APP_COMMAND, WPARAM(0), LPARAM(0));
    }
}

pub fn take_command() -> Option<Command> {
    QUEUE.with(|q| q.borrow_mut().pop_front())
}

/// The interface takes its tone from the visualizer's accent color.
pub fn seed_for(settings: &Settings) -> [f32; 3] {
    match settings.color_mode() {
        ColorMode::Classic => [40.0, 200.0, 90.0],
        ColorMode::Gradient(_, top) => top,
        ColorMode::Solid(rgb) => rgb,
        ColorMode::Rainbow | ColorMode::Flow => NEUTRAL_SEED,
    }
}

pub fn wants_dark(settings: &Settings) -> bool {
    match settings.theme.as_str() {
        "light" => false,
        "dark" => true,
        _ => icon::apps_prefer_dark(),
    }
}

pub fn palette_for(settings: &Settings) -> Palette {
    let seed = seed_for(settings);
    let dark = wants_dark(settings);
    let key = (seed.map(|v| v as u32), dark);
    PALETTE.with(|cell| {
        let mut slot = cell.borrow_mut();
        match slot.as_ref() {
            Some((k, p)) if *k == key => *p,
            _ => {
                let p = make_palette(seed, dark);
                *slot = Some((key, p));
                p
            }
        }
    })
}

/// Segoe Fluent Icons code points.
pub mod glyph {
    pub const SETTINGS: char = '\u{E713}';
    pub const PALETTE: char = '\u{E790}';
    pub const EQUALIZER: char = '\u{E9E9}';
    pub const CHECK: char = '\u{E73E}';
    pub const CHEVRON_RIGHT: char = '\u{E76C}';
    pub const CHEVRON_DOWN: char = '\u{E70D}';
    pub const MINIMIZE: char = '\u{E921}';
    pub const CLOSE: char = '\u{E8BB}';
    pub const OPEN_WINDOW: char = '\u{E8A7}';
    pub const POWER: char = '\u{E7E8}';
    pub const DOCUMENT: char = '\u{E8A5}';
    pub const SYNC: char = '\u{E895}';
    pub const EDIT: char = '\u{E70F}';
    pub const FOLDER: char = '\u{E838}';
    pub const BUG: char = '\u{EBE8}';
    pub const ADD: char = '\u{E710}';
    pub const DELETE: char = '\u{E74D}';
}
