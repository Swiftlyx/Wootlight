//! The main window's Win32 host: a borderless popup that feeds mouse, wheel and keys to the shared
//! `WindowCore` and draws it through Direct2D. Frames follow the monitor's refresh rate while the
//! window is visible.

use std::{cell::RefCell, sync::Arc};

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::{
            Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute},
            Gdi::{
                BeginPaint, EndPaint, GetDC, GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
                PAINTSTRUCT, ReleaseDC, ScreenToClient, ValidateRect,
            },
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI},
            Input::KeyboardAndMouse::{
                GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DOWN,
                VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, GetClientRect, HICON, HTCAPTION, HTCLIENT, ICON_BIG,
                ICON_SMALL, IDC_ARROW, IsIconic, IsWindowVisible, LoadCursorW, RegisterClassW, SIZE_MINIMIZED, SIZE_RESTORED,
                SW_HIDE, SW_MINIMIZE,
                SW_RESTORE, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetForegroundWindow, SetWindowPos,
                ShowWindow, WM_ACTIVATE, WM_APP, WM_CLOSE, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN,
                WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCHITTEST, WM_PAINT, WM_SETICON, WNDCLASSW,
                WM_SIZE, WS_EX_APPWINDOW, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU,
            },
        },
    },
    core::{BOOL, w},
};

use super::{
    canvas::Canvas,
    design::*,
    frame_clock::{FrameClock, refresh_rate},
    icon::{self, Mark},
    palette_for, seed_for,
    window_core::{self, Frame, Key, WHEEL_STEP, WindowCore},
};
use crate::engine::Shared;

/// Not exported by the Win32 bindings' WindowsAndMessaging module.
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_DISPLAYCHANGE: u32 = 0x007E;
const WM_EXITSIZEMOVE: u32 = 0x0232;
const WM_CAPTURECHANGED: u32 = 0x0215;
const WM_APP_FRAME: u32 = WM_APP + 40;

pub struct MainWindow {
    hwnd: HWND,
    canvas: Canvas,
    /// Draws only the live view when nothing else changed; far cheaper than a whole frame.
    /// Renders on the GPU and exists only while the window is visible: dropping it with its own
    /// factory releases the graphics device.
    live_canvas: Option<Canvas>,
    clock: FrameClock,
    dpi: f32,
    tracking: bool,
    /// Color of the title bar and taskbar icons.
    icon_seed: [f32; 3],
    core: WindowCore,
}

thread_local! {
    static WINDOW: RefCell<Option<MainWindow>> = const { RefCell::new(None) };
}

enum After {
    None,
    Hide,
    Minimize,
    Move(RECT),
}

impl From<window_core::After> for After {
    fn from(after: window_core::After) -> Self {
        match after {
            window_core::After::None => After::None,
            window_core::After::Hide => After::Hide,
            window_core::After::Minimize => After::Minimize,
        }
    }
}

fn lparam_point(lparam: LPARAM) -> (i32, i32) {
    ((lparam.0 & 0xFFFF) as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as i16 as i32)
}

fn register_class() {
    thread_local! { static DONE: RefCell<bool> = const { RefCell::new(false) }; }
    DONE.with(|done| {
        if *done.borrow() {
            return;
        }
        // SAFETY: registers a window class with a static window procedure.
        unsafe {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: w!("Wootlight.Window"),
                ..Default::default()
            };
            RegisterClassW(&wc);
        }
        *done.borrow_mut() = true;
    });
}

fn create(shared: Arc<Shared>, devices: Vec<String>) -> Option<MainWindow> {
    register_class();
    let settings = shared.settings();
    let p = palette_for(&settings);
    // SAFETY: monitor queries, window creation and DWM attributes on the new window.
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let dpi = dpi_x as f32;
        let scale = dpi / 96.0;
        let work = info.rcWork;
        let work_h = (work.bottom - work.top) as f32 / scale;
        let height = (work_h - 48.0).clamp(WINDOW_MIN_HEIGHT, WINDOW_MAX_HEIGHT);
        let (w_px, h_px) = ((WINDOW_WIDTH * scale).round() as i32, (height * scale).round() as i32);
        let x = work.left + (work.right - work.left - w_px) / 2;
        let y = work.top + (work.bottom - work.top - h_px) / 2;

        let hwnd = CreateWindowExW(
            WS_EX_APPWINDOW,
            w!("Wootlight.Window"),
            w!("Wootlight"),
            WS_POPUP | WS_MINIMIZEBOX | WS_SYSMENU,
            x,
            y,
            w_px,
            h_px,
            None,
            None,
            Some(GetModuleHandleW(None).unwrap_or_default().into()),
            None,
        )
        .map_err(|e| crate::log!("window creation failed: {e}"))
        .ok()?;
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as *const _, 4);
        let dark = BOOL::from(p.dark);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
        let icon_seed = seed_for(&settings);
        set_window_icons(hwnd, icon_seed);

        let mut canvas = Canvas::new().map_err(|e| crate::log!("canvas creation failed: {e}")).ok()?;
        canvas.attach_dc(dpi, true).map_err(|e| crate::log!("render target creation failed: {e}")).ok()?;
        Some(MainWindow {
            hwnd,
            canvas,
            live_canvas: None,
            core: WindowCore::new(shared, devices, height),
            clock: FrameClock::new(hwnd, WM_APP_FRAME),
            dpi,
            tracking: false,
            icon_seed,
        })
    }
}

/// Shows the window, creating it on first use, and brings it to the front.
pub fn show(shared: &Arc<Shared>, devices: Vec<String>) {
    let hwnd = WINDOW.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = create(Arc::clone(shared), devices.clone());
        } else if let Some(w) = slot.as_mut() {
            w.core.set_devices(devices.clone());
        }
        slot.as_ref().map(|w| w.hwnd)
    });
    if let Some(hwnd) = hwnd {
        // SAFETY: shows and focuses our own window.
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            } else {
                let _ = ShowWindow(hwnd, SW_SHOW);
            }
            let _ = SetForegroundWindow(hwnd);
        }
        WINDOW.with(|cell| {
            if let Some(w) = cell.borrow_mut().as_mut() {
                w.start_frames();
            }
        });
    }
}

pub fn is_visible() -> bool {
    WINDOW.with(|cell| {
        cell.try_borrow()
            .ok()
            .and_then(|w| w.as_ref().map(|w| w.hwnd))
            // SAFETY: plain queries.
            .is_some_and(|h| unsafe { IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool() })
    })
}

pub fn hide() {
    let hwnd = WINDOW.with(|cell| {
        cell.try_borrow_mut().ok().and_then(|mut w| {
            let w = w.as_mut()?;
            w.stop_frames();
            Some(w.hwnd)
        })
    });
    if let Some(hwnd) = hwnd {
        // SAFETY: hides our own window.
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// Sets the title bar and taskbar icons in the color of `seed` and frees the ones they replace.
fn set_window_icons(hwnd: HWND, seed: [f32; 3]) {
    for (size, which) in [(32u32, ICON_SMALL), (48u32, ICON_BIG)] {
        if let Some(h) = icon::hicon(&icon::render_rgba(size, Mark::Live, seed, true), size) {
            // SAFETY: the window returns the icon it held before; nothing else references it.
            unsafe {
                let old = SendMessageW(hwnd, WM_SETICON, Some(WPARAM(which as usize)), Some(LPARAM(h.0 as isize)));
                if old.0 != 0 {
                    let _ = DestroyIcon(HICON(old.0 as _));
                }
            }
        }
    }
}

/// Redraws on the next frame if anything shown outside the live view changed.
pub fn refresh(config_error: Option<String>) {
    WINDOW.with(|cell| {
        if let Ok(mut slot) = cell.try_borrow_mut() {
            if let Some(w) = slot.as_mut() {
                let settings = w.core.shared.settings();
                let seed = seed_for(&settings);
                if seed != w.icon_seed {
                    set_window_icons(w.hwnd, seed);
                    w.icon_seed = seed;
                }
                w.core.refresh(config_error);
            }
        }
    });
}

pub fn destroy() {
    let taken = WINDOW.with(|cell| cell.try_borrow_mut().ok().and_then(|mut w| w.take()));
    if let Some(mut w) = taken {
        w.stop_frames();
        // SAFETY: destroys our own window.
        unsafe {
            let _ = DestroyWindow(w.hwnd);
        }
    }
}

impl MainWindow {
    fn scale(&self) -> f32 {
        self.dpi / 96.0
    }

    fn dip(&self, lparam: LPARAM) -> (f32, f32) {
        let (x, y) = lparam_point(lparam);
        (x as f32 / self.scale(), y as f32 / self.scale())
    }

    /// Creates the live view canvas, falling back to software rendering without a usable GPU.
    fn attach_live(&mut self) {
        self.live_canvas = Canvas::new().ok().and_then(|mut c| {
            (c.attach_dc_with(self.dpi, true, true).is_ok() || c.attach_dc(self.dpi, true).is_ok()).then_some(c)
        });
    }

    fn start_frames(&mut self) {
        if self.live_canvas.is_none() {
            self.attach_live();
        }
        self.clock.start();
        self.core.shared.set_ui_hz(refresh_rate(self.hwnd));
        self.core.dirty = true;
    }

    fn stop_frames(&mut self) {
        self.live_canvas = None;
        self.clock.stop();
        self.core.shared.set_ui_hz(0);
    }

    /// Redraws just the live view: the keys or the bars changed and nothing else did.
    fn draw_live(&mut self) {
        let preview = self.core.shared.preview();
        let (Some(area), Some(live)) = (self.core.live_area(&preview), self.live_canvas.as_ref()) else {
            self.draw_now();
            return;
        };
        self.core.last_seq = preview.seq;
        let s = self.scale();
        let px = RECT {
            left: (area.l * s).floor() as i32,
            top: (area.t * s).floor() as i32,
            right: (area.r * s).ceil() as i32,
            bottom: (area.b * s).ceil() as i32,
        };
        let mut lost = false;
        // SAFETY: draws into the DC of our own window and releases it.
        unsafe {
            let hdc = GetDC(Some(self.hwnd));
            if live.bind_dc_rect(hdc, px) {
                live.begin();
                live.set_offset(-(px.left as f32) / s, -(px.top as f32) / s);
                self.core.render_live(live, &preview, s);
                lost = !live.end();
            }
            ReleaseDC(Some(self.hwnd), hdc);
        }
        if lost {
            self.attach_live();
        }
    }

    /// Draws a frame straight to the window, outside WM_PAINT.
    fn draw_now(&mut self) {
        let mut rc = RECT::default();
        // SAFETY: draws into our own window's DC and releases it.
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
            let hdc = GetDC(Some(self.hwnd));
            if self.canvas.bind_dc(hdc, rc.right, rc.bottom) {
                self.render();
            }
            ReleaseDC(Some(self.hwnd), hdc);
            let _ = ValidateRect(Some(self.hwnd), None);
        }
    }

    /// A whole frame into the bound render target.
    fn render(&mut self) {
        self.canvas.begin();
        self.core.render(&self.canvas, self.scale());
        if !self.canvas.end() {
            let _ = self.canvas.attach_dc(self.dpi, true);
        }
    }

    fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<(LRESULT, After)> {
        let none = |r| Some((LRESULT(r), After::None));
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let mut rc = RECT::default();
                // SAFETY: paints our own window through the DC render target.
                unsafe {
                    let hdc = BeginPaint(self.hwnd, &mut ps);
                    let _ = GetClientRect(self.hwnd, &mut rc);
                    if self.canvas.bind_dc(hdc, rc.right, rc.bottom) {
                        self.render();
                    }
                    let _ = EndPaint(self.hwnd, &ps);
                }
                none(0)
            }
            WM_ERASEBKGND => none(1),
            WM_APP_FRAME => {
                match self.core.frame() {
                    Frame::Full => self.draw_now(),
                    Frame::Live => self.draw_live(),
                    Frame::None => {}
                }
                self.clock.frame_handled();
                none(0)
            }
            WM_DPICHANGED => {
                self.dpi = (wparam.0 & 0xFFFF) as f32;
                self.canvas.set_dpi(self.dpi);
                // SAFETY: lparam points to the suggested window rectangle.
                let rect = unsafe { *(lparam.0 as *const RECT) };
                Some((LRESULT(0), After::Move(rect)))
            }
            WM_SIZE => {
                // A minimized window draws nothing, so the clock and the GPU target rest meanwhile.
                match wparam.0 as u32 {
                    SIZE_MINIMIZED => self.stop_frames(),
                    // SAFETY: plain query on our own window.
                    SIZE_RESTORED if unsafe { IsWindowVisible(self.hwnd).as_bool() } => self.start_frames(),
                    _ => {}
                }
                None
            }
            WM_DISPLAYCHANGE | WM_EXITSIZEMOVE => {
                // The window may now be on a monitor with another refresh rate. WM_DISPLAYCHANGE also
                // reaches hidden windows, which must leave the analysis at its own rate.
                // SAFETY: plain queries on our own window.
                if unsafe { IsWindowVisible(self.hwnd).as_bool() && !IsIconic(self.hwnd).as_bool() } {
                    self.core.shared.set_ui_hz(refresh_rate(self.hwnd));
                }
                None
            }
            WM_CAPTURECHANGED => {
                // Capture lost without a button release (Alt+Tab, Win key, hiding the window).
                if lparam.0 != self.hwnd.0 as isize {
                    self.core.pointer_cancel();
                }
                None
            }
            WM_NCHITTEST => {
                let mut pt = POINT { x: (lparam.0 & 0xFFFF) as i16 as i32, y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32 };
                // SAFETY: converts a screen point for our own window.
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut pt);
                }
                let (x, y) = (pt.x as f32 / self.scale(), pt.y as f32 / self.scale());
                let r = if self.core.is_drag_area(x, y) { HTCAPTION } else { HTCLIENT };
                none(r as isize)
            }
            WM_MOUSEMOVE => {
                if !self.tracking {
                    let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
                    // SAFETY: requests WM_MOUSELEAVE for our own window.
                    unsafe {
                        let _ = TrackMouseEvent(&mut track);
                    }
                    self.tracking = true;
                }
                let (x, y) = self.dip(lparam);
                self.core.pointer_move(x, y);
                none(0)
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                self.core.pointer_leave();
                none(0)
            }
            WM_LBUTTONDOWN => {
                let (x, y) = self.dip(lparam);
                if self.core.pointer_down(x, y) {
                    // SAFETY: captures the mouse for a press or a drag.
                    unsafe { SetCapture(self.hwnd) };
                }
                none(0)
            }
            WM_LBUTTONUP => {
                // SAFETY: releases our own capture.
                unsafe {
                    let _ = ReleaseCapture();
                }
                let (x, y) = self.dip(lparam);
                let after = self.core.pointer_up(x, y);
                Some((LRESULT(0), after.into()))
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
                self.core.wheel(delta * WHEEL_STEP);
                none(0)
            }
            WM_KEYDOWN => {
                let key = wparam.0 as u16;
                // SAFETY: plain query.
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                let key = match key {
                    k if k == VK_TAB.0 => Key::Tab,
                    k if k == VK_ESCAPE.0 => Key::Escape,
                    k if k == VK_RETURN.0 => Key::Enter,
                    k if k == VK_SPACE.0 => Key::Space,
                    k if k == VK_LEFT.0 => Key::Left,
                    k if k == VK_RIGHT.0 => Key::Right,
                    k if k == VK_UP.0 => Key::Up,
                    k if k == VK_DOWN.0 => Key::Down,
                    _ => return none(0),
                };
                Some((LRESULT(0), self.core.key(key, shift).into()))
            }
            WM_ACTIVATE => {
                if (wparam.0 & 0xFFFF) as u32 == 0 {
                    self.core.deactivate();
                }
                None
            }
            WM_CLOSE => Some((LRESULT(0), After::Hide)),
            _ => None,
        }
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let result = WINDOW.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else { return None };
        let w = slot.as_mut().filter(|w| w.hwnd == hwnd)?;
        w.handle(msg, wparam, lparam)
    });
    match result {
        Some((r, after)) => {
            // SAFETY: window operations on our own window, outside the state borrow.
            unsafe {
                match after {
                    After::None => {}
                    After::Hide => hide(),
                    After::Minimize => {
                        let _ = ShowWindow(hwnd, SW_MINIMIZE);
                    }
                    After::Move(rect) => {
                        let _ = SetWindowPos(hwnd, None, rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top, SWP_NOZORDER | SWP_NOACTIVATE);
                        let dpi = GetDpiForWindow(hwnd) as f32;
                        WINDOW.with(|cell| {
                            if let Ok(mut slot) = cell.try_borrow_mut() {
                                if let Some(w) = slot.as_mut() {
                                    w.dpi = dpi;
                                    let _ = w.canvas.attach_dc(dpi, true);
                                    if w.live_canvas.is_some() {
                                        w.attach_live();
                                    }
                                    w.core.dirty = true;
                                }
                            }
                        });
                    }
                }
            }
            r
        }
        // SAFETY: default handling for everything else.
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
