//! The tray icon's right-click menu: a Material 3 menu surface on a layered window, beside the
//! taskbar, with a soft level 2 shadow. It rises 8 DIP while fading in over 150 ms.

use std::{cell::RefCell, time::Instant};

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM},
        Graphics::Gdi::{
            AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
            CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW, HBITMAP, HDC, HGDIOBJ,
            MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect, ReleaseDC, SelectObject,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            Input::KeyboardAndMouse::{
                SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME,
                VK_RETURN, VK_SPACE, VK_TAB, VK_UP,
            },
            WindowsAndMessaging::{
                CS_HREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, IDC_ARROW, KillTimer, LoadCursorW,
                PostMessageW, RegisterClassW, SW_SHOW, SetForegroundWindow, SetTimer, ShowWindow, ULW_ALPHA,
                UpdateLayeredWindow, WA_INACTIVE, WM_ACTIVATE, WM_APP, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
                WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
                WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
    core::w,
};

use super::{
    Command,
    canvas::{Align, Canvas, Rect, TextStyle},
    design,
    palette::{Color, Palette},
    post,
};

/// Not exported by the Win32 bindings' WindowsAndMessaging module.
const WM_MOUSELEAVE: u32 = 0x02A3;
const SHADOW: f32 = 16.0;
const SCREEN_GAP: f32 = 8.0;
const SLIDE: f32 = 8.0;
const APPEAR_MS: f32 = 150.0;
const PADDING: f32 = 8.0;
const HEADER: f32 = 36.0;
const DIVIDER: f32 = 9.0;
const ITEM: f32 = 40.0;
const MIN_WIDTH: f32 = 228.0;
const MAX_WIDTH: f32 = 320.0;
const GLYPH_CHECK: char = '\u{E73E}';
const WM_APP_CLOSE: u32 = WM_APP + 21;
const TIMER_APPEAR: usize = 1;

pub struct MenuEntry {
    pub text: String,
    pub glyph: char,
    pub checked: bool,
    pub danger: bool,
    /// None draws a divider.
    pub command: Option<Command>,
}

impl MenuEntry {
    pub fn item(text: &str, glyph: char, command: Command) -> Self {
        Self { text: text.into(), glyph, checked: false, danger: false, command: Some(command) }
    }
    pub fn divider() -> Self {
        Self { text: String::new(), glyph: ' ', checked: false, danger: false, command: None }
    }
}

struct Menu {
    hwnd: HWND,
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    canvas: Canvas,
    palette: Palette,
    header: String,
    entries: Vec<MenuEntry>,
    hot: Option<usize>,
    dpi: f32,
    width_px: i32,
    height_px: i32,
    surface_w: f32,
    surface_h: f32,
    origin: POINT,
    slide: (i32, i32),
    appear: f32,
    shown_at: Instant,
    tracking: bool,
}

thread_local! {
    static MENU: RefCell<Option<Menu>> = const { RefCell::new(None) };
}

fn lparam_point(lparam: LPARAM) -> (i32, i32) {
    ((lparam.0 & 0xFFFF) as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as i16 as i32)
}

pub fn is_open() -> bool {
    MENU.with(|m| m.try_borrow().map(|m| m.is_some()).unwrap_or(true))
}

pub fn close() {
    let taken = MENU.with(|m| m.try_borrow_mut().ok().and_then(|mut m| m.take()));
    if let Some(menu) = taken {
        // SAFETY: tears down the window and GDI objects this menu created.
        unsafe {
            let _ = KillTimer(Some(menu.hwnd), TIMER_APPEAR);
            let _ = DestroyWindow(menu.hwnd);
            SelectObject(menu.dc, menu.previous);
            let _ = DeleteObject(menu.bitmap.into());
            let _ = DeleteDC(menu.dc);
        }
    }
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
                style: CS_HREDRAW,
                lpfnWndProc: Some(menu_proc),
                hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: w!("Wootlight.TrayMenu"),
                ..Default::default()
            };
            RegisterClassW(&wc);
        }
        *done.borrow_mut() = true;
    });
}

/// Opens the menu next to the tray icon at `icon`, with the status line as its header.
pub fn open(icon: RECT, cursor: POINT, header: String, entries: Vec<MenuEntry>, palette: Palette) {
    close();
    register_class();
    let Ok(mut canvas) = Canvas::new() else { return };

    // SAFETY: monitor queries and creation of the layered window and its backing DIB.
    unsafe {
        let monitor = MonitorFromRect(&icon, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let dpi = dpi_x as f32;
        let scale = dpi / 96.0;
        let px = |dip: f32| (dip * scale).round() as i32;
        if canvas.attach_dc(dpi, false).is_err() {
            return;
        }

        let mut widest = canvas.measure(&header, TextStyle::LabelMedium) + 32.0;
        for e in entries.iter().filter(|e| e.command.is_some()) {
            widest = widest.max(canvas.measure(&e.text, TextStyle::BodyLarge) + 52.0 + 44.0);
        }
        let surface_w = widest.ceil().clamp(MIN_WIDTH, MAX_WIDTH);
        let surface_h = 2.0 * PADDING
            + HEADER
            + entries.iter().map(|e| if e.command.is_some() { ITEM } else { DIVIDER }).sum::<f32>();

        let width_px = px(surface_w + 2.0 * SHADOW);
        let height_px = px(surface_h + 2.0 * SHADOW);
        let (sw, sh) = (px(surface_w), px(surface_h));
        let work = info.rcWork;
        let screen = info.rcMonitor;
        let gap = px(SCREEN_GAP);
        // Beside the taskbar, on whichever edge it is.
        let mut x = cursor.x - sw / 2;
        let mut y = work.bottom - gap - sh;
        let mut slide = (0, px(SLIDE));
        if work.top > screen.top {
            y = work.top + gap;
            slide = (0, -px(SLIDE));
        } else if work.left > screen.left {
            x = work.left + gap;
            y = cursor.y - sh / 2;
            slide = (-px(SLIDE), 0);
        } else if work.right < screen.right {
            x = work.right - gap - sw;
            y = cursor.y - sh / 2;
            slide = (px(SLIDE), 0);
        } else if work.bottom == screen.bottom {
            y = icon.top - gap - sh;
        }
        x = x.clamp(work.left + gap, (work.left + gap).max(work.right - gap - sw));
        y = y.clamp(work.top + gap, (work.top + gap).max(work.bottom - gap - sh));
        let origin = POINT { x: x - px(SHADOW), y: y - px(SHADOW) };

        let Ok(hwnd) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            w!("Wootlight.TrayMenu"),
            w!(""),
            WS_POPUP,
            origin.x,
            origin.y,
            width_px,
            height_px,
            None,
            None,
            Some(GetModuleHandleW(None).unwrap_or_default().into()),
            None,
        ) else {
            return;
        };

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width_px,
                biHeight: -height_px,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let screen_dc = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen_dc));
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(Some(screen_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0);
        ReleaseDC(None, screen_dc);
        let Ok(bitmap) = bitmap else {
            let _ = DestroyWindow(hwnd);
            let _ = DeleteDC(dc);
            return;
        };
        let previous = SelectObject(dc, bitmap.into());

        let menu = Menu {
            hwnd,
            dc,
            bitmap,
            previous,
            canvas,
            palette,
            header,
            entries,
            hot: None,
            dpi,
            width_px,
            height_px,
            surface_w,
            surface_h,
            origin,
            slide,
            appear: 0.0,
            shown_at: Instant::now(),
            tracking: false,
        };
        menu.render();
        MENU.with(|m| *m.borrow_mut() = Some(menu));
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        SetTimer(Some(hwnd), TIMER_APPEAR, 10, None);
    }
}

impl Menu {
    fn render(&self) {
        let c = &self.canvas;
        let p = &self.palette;
        if !c.bind_dc(self.dc, self.width_px, self.height_px) {
            return;
        }
        c.begin();
        c.clear(Color::TRANSPARENT);
        let surface = Rect::new(SHADOW, SHADOW, SHADOW + self.surface_w, SHADOW + self.surface_h);
        let radius = design::RADIUS_MENU;
        // Level 2 elevation as a soft shadow, offset slightly downwards: it sits on any desktop.
        for layer in 1..=5 {
            let grow = layer as f32 * 2.0;
            c.fill_round(
                Rect::new(surface.l - grow, surface.t - grow + 2.0, surface.r + grow, surface.b + grow + 2.0),
                radius + grow,
                Color { r: 0.0, g: 0.0, b: 0.0, a: 0.04 },
            );
        }
        c.fill_round(surface, radius, p.surface_container);
        c.stroke_round(surface.inset(0.5, 0.5), radius - 0.5, p.outline_variant, 1.0);

        let (left, right) = (surface.l, surface.r);
        let mut y = surface.t + PADDING;
        c.text(&self.header, Rect::new(left + 16.0, y, right - 16.0, y + HEADER), TextStyle::LabelMedium, p.on_surface_variant, Align::Leading);
        y += HEADER;
        for (i, e) in self.entries.iter().enumerate() {
            if e.command.is_none() {
                c.fill_rect(Rect::new(left, y + 4.0, right, y + 5.0), p.outline_variant);
                y += DIVIDER;
                continue;
            }
            let row = Rect::new(left + 6.0, y, right - 6.0, y + ITEM);
            if self.hot == Some(i) {
                c.fill_round(row, design::RADIUS_MENU_ITEM, p.on_surface.alpha(design::HOVER_OPACITY));
            }
            let mid = y + ITEM / 2.0;
            let content = if e.danger { p.error } else { p.on_surface };
            c.glyph(e.glyph, left + 28.0, mid, 18.0, if e.danger { p.error } else { p.on_surface_variant });
            c.text(&e.text, Rect::new(left + 52.0, y, right - 44.0, y + ITEM), TextStyle::BodyLarge, content, Align::Leading);
            if e.checked {
                c.glyph(GLYPH_CHECK, right - 26.0, mid, 16.0, p.primary);
            }
            y += ITEM;
        }
        c.end();

        let k = 1.0 - self.appear;
        let dest = POINT {
            x: self.origin.x + (self.slide.0 as f32 * k).round() as i32,
            y: self.origin.y + (self.slide.1 as f32 * k).round() as i32,
        };
        let size = SIZE { cx: self.width_px, cy: self.height_px };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: (self.appear * 255.0).round() as u8,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        // SAFETY: pushes the DIB to the layered window.
        unsafe {
            let _ = UpdateLayeredWindow(self.hwnd, None, Some(&dest), Some(&size), Some(self.dc), Some(&src), Default::default(), Some(&blend), ULW_ALPHA);
        }
    }

    fn entry_at(&self, px: i32, py: i32) -> Option<usize> {
        let scale = self.dpi / 96.0;
        let (x, y) = (px as f32 / scale, py as f32 / scale);
        if x < SHADOW || x > SHADOW + self.surface_w {
            return None;
        }
        let mut top = SHADOW + PADDING + HEADER;
        for (i, e) in self.entries.iter().enumerate() {
            let h = if e.command.is_some() { ITEM } else { DIVIDER };
            if y >= top && y < top + h {
                return e.command.is_some().then_some(i);
            }
            top += h;
        }
        None
    }

    fn step(&mut self, dir: i32) {
        let n = self.entries.len() as i32;
        let mut index = self.hot.map_or(-1, |h| h as i32);
        for _ in 0..n {
            index = if index < 0 { if dir > 0 { 0 } else { n - 1 } } else { (index + dir + n) % n };
            if self.entries[index as usize].command.is_some() {
                self.hot = Some(index as usize);
                self.render();
                return;
            }
        }
    }

    fn inside_surface(&self, px: i32, py: i32) -> bool {
        let scale = self.dpi / 96.0;
        let (x, y) = (px as f32 / scale, py as f32 / scale);
        x >= SHADOW && y >= SHADOW && x <= SHADOW + self.surface_w && y <= SHADOW + self.surface_h
    }
}

fn choose(index: Option<usize>) {
    let command = MENU.with(|m| {
        m.try_borrow().ok().and_then(|m| {
            let menu = m.as_ref()?;
            menu.entries.get(index?)?.command.clone()
        })
    });
    if let Some(command) = command {
        close();
        post(command);
    }
}

unsafe extern "system" fn menu_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    enum After {
        None,
        Close,
        Choose(Option<usize>),
    }
    let mut after = After::None;
    let handled = MENU.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else { return false };
        let Some(menu) = slot.as_mut().filter(|m| m.hwnd == hwnd) else { return false };
        match msg {
            WM_TIMER => {
                let t = (menu.shown_at.elapsed().as_secs_f32() * 1000.0 / APPEAR_MS).clamp(0.0, 1.0);
                // Emphasized decelerate, approximated by a cubic ease-out.
                menu.appear = 1.0 - (1.0 - t).powi(3);
                menu.render();
                if t >= 1.0 {
                    // SAFETY: stops this window's own timer.
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_APPEAR);
                    }
                }
            }
            WM_ACTIVATE => {
                // The menu closes like any menu once focus goes elsewhere.
                if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE {
                    // SAFETY: posts to this window; a posted WM_CLOSE would read as a quit request.
                    unsafe {
                        let _ = PostMessageW(Some(hwnd), WM_APP_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
            }
            WM_APP_CLOSE => after = After::Close,
            WM_MOUSEMOVE => {
                if !menu.tracking {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    // SAFETY: requests WM_MOUSELEAVE for this window.
                    unsafe {
                        let _ = TrackMouseEvent(&mut track);
                    }
                    menu.tracking = true;
                }
                let (x, y) = lparam_point(lparam);
                let hot = menu.entry_at(x, y);
                if hot != menu.hot {
                    menu.hot = hot;
                    menu.render();
                }
            }
            WM_MOUSELEAVE => {
                menu.tracking = false;
                if menu.hot.take().is_some() {
                    menu.render();
                }
            }
            WM_LBUTTONUP | WM_RBUTTONUP => {
                let (x, y) = lparam_point(lparam);
                if let Some(i) = menu.entry_at(x, y) {
                    after = After::Choose(Some(i));
                }
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                // A press on the shadow counts as outside the menu.
                let (x, y) = lparam_point(lparam);
                if !menu.inside_surface(x, y) {
                    after = After::Close;
                }
            }
            WM_KEYDOWN => match wparam.0 as u16 {
                k if k == VK_ESCAPE.0 => after = After::Close,
                k if k == VK_UP.0 => menu.step(-1),
                k if k == VK_DOWN.0 || k == VK_TAB.0 => menu.step(1),
                k if k == VK_HOME.0 => {
                    menu.hot = None;
                    menu.step(1);
                }
                k if k == VK_END.0 => {
                    menu.hot = None;
                    menu.step(-1);
                }
                k if k == VK_RETURN.0 || k == VK_SPACE.0 => after = After::Choose(menu.hot),
                _ => return false,
            },
            _ => return false,
        }
        true
    });
    match after {
        After::Close => close(),
        After::Choose(i) => choose(i),
        After::None => {}
    }
    if handled {
        LRESULT(0)
    } else {
        // SAFETY: default handling for everything else.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }
}
