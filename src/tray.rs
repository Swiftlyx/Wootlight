//! The tray icon and the Win32 message loop around the shared app shell (see `shell`).

use std::{path::PathBuf, ptr, sync::Arc};

use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::UI::{
    HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi},
    WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, IsWindow, MSG, SM_CXSMICON, SetTimer, TranslateMessage, WM_CLOSE, WM_TIMER,
    },
};

use crate::{
    engine::Shared,
    log,
    shell::App,
    ui::{
        self, Command,
        icon::{self, Mark},
        tray_menu, window,
    },
    win,
};

const TIMER_MS: u32 = 500;

/// The tray icon's size in pixels.
pub fn icon_size() -> u32 {
    // SAFETY: plain metric queries.
    unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()).max(16) as u32 }
}

/// Shows straight-alpha RGBA pixels `size` pixels square as the tray icon.
pub fn set_icon(tray: &TrayIcon, rgba: Vec<u8>, size: u32) {
    if let Ok(icon) = Icon::from_rgba(rgba, size, size) {
        let _ = tray.set_icon(Some(icon));
    }
}

fn open_menu(app: &App, rect: tray_icon::Rect, position: tray_icon::dpi::PhysicalPosition<f64>) {
    let entries = app.menu_entries();
    let icon_rect = windows::Win32::Foundation::RECT {
        left: rect.position.x as i32,
        top: rect.position.y as i32,
        right: rect.position.x as i32 + rect.size.width as i32,
        bottom: rect.position.y as i32 + rect.size.height as i32,
    };
    let cursor = windows::Win32::Foundation::POINT { x: position.x as i32, y: position.y as i32 };
    tray_menu::open(icon_rect, cursor, app.status_text(), entries, ui::palette_for(&app.shared.settings()));
}

fn on_tray_event(app: &mut App, event: TrayIconEvent) {
    match event {
        TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } => {
            if !tray_menu::is_open() {
                app.execute(Command::ToggleWindow);
            }
        }
        TrayIconEvent::Click { button: MouseButton::Right, button_state: MouseButtonState::Up, rect, position, .. } => {
            open_menu(app, rect, position);
        }
        _ => {}
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
    let size = icon_size();
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

    let mut app = App::new(shared, config_path, config_error, tray, devices);
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
                on_tray_event(&mut app, event);
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
