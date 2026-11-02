//! The main window: the live view pinned on top (the keyboard or a plain equalizer), then pages
//! for look, sound and settings. Drawn immediate-mode: every frame lays out the page and registers
//! what can be clicked. Frames follow the monitor's refresh rate while the window is visible.

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
    Command,
    canvas::{Align, Canvas, Corners, Rect, TextStyle},
    design::*,
    frame_clock::{FrameClock, refresh_rate},
    glyph,
    i18n::{Lang, T, tr},
    icon::{self, Mark},
    palette::{Color, Palette},
    palette_for, post, seed_for,
    spring::Animator,
    wants_dark,
};
use crate::{
    autostart,
    engine::{Owner, Preview, Shared, Status},
    layout::Layout,
    render::{SDK_COLS, SDK_ROWS, color_at, hsv_to_rgb, rgb_to_hs},
    sdk::KeyboardInfo,
    settings::{ColorMode, MAX_SAVED_COLORS, Settings, parse_color_mode},
};

/// Not exported by the Win32 bindings' WindowsAndMessaging module.
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_DISPLAYCHANGE: u32 = 0x007E;
const WM_EXITSIZEMOVE: u32 = 0x0232;
const WM_CAPTURECHANGED: u32 = 0x0215;
const WM_APP_FRAME: u32 = WM_APP + 40;

const KEY_GAP: f32 = 2.0;
const F_ROW_GAP: f32 = 6.0;
const MAX_KEY_UNIT: f32 = 30.0;
const CARD_PADDING: f32 = 16.0;
const STATUS_ROW: f32 = 24.0;
const WHEEL_STEP: f32 = 56.0;
const BAR_GAP: f32 = 3.0;
const AXIS_HEIGHT: f32 = 18.0;
const COLOR_TRACK: f32 = 12.0;
const COLOR_EDIT_HEIGHT: f32 = 106.0;

// Element ids; springs and hits are keyed by them.
const ID_MINIMIZE: u64 = 1;
const ID_CLOSE: u64 = 2;
const ID_ENABLED: u64 = 3;
const ID_SCRIM: u64 = 4;
const ID_SCROLL: u64 = 5;
const ID_VIEW: u64 = 6;
const ID_NAV: u64 = 10;
const ID_SWATCH: u64 = 100;
const ID_FILL: u64 = 200;
const ID_COLOR_EDIT: u64 = 210;
const ID_PRESET: u64 = 220;
const ID_BRIGHTNESS: u64 = 230;
const ID_SENSITIVITY: u64 = 240;
const ID_RANGE: u64 = 250;
const ID_EQUALIZE: u64 = 251;
const ID_DEVICE: u64 = 252;
const ID_RELEASE: u64 = 253;
const ID_MEDIA_ONLY: u64 = 254;
const ID_LANGUAGE: u64 = 260;
const ID_THEME: u64 = 270;
const ID_AUTOSTART: u64 = 280;
const ID_CONFIG: u64 = 281;
const ID_REVEAL: u64 = 282;
const ID_QUIT: u64 = 283;
const ID_MENU_ITEM: u64 = 1000;

// Spring properties.
const P_LAYER: u8 = 0;
const P_CORNER: u8 = 1;
const P_POSITION: u8 = 2;
const P_SIZE: u8 = 3;
const P_SELECTED: u8 = 4;

const COLORS: [(&str, T); 12] = [
    ("classic", T::ColorClassic),
    ("rainbow", T::ColorRainbow),
    ("flow", T::ColorFlow),
    ("#ffffff", T::ColorWhite),
    ("#00ffaa", T::ColorSolidTurquoise),
    ("#9020ff", T::ColorSolidPurple),
    ("#c8c8c8>#ff2a8a", T::ColorPink),
    ("#c8c8c8>#ff1010", T::ColorRed),
    ("#c8c8c8>#00ffc0", T::ColorTurquoise),
    ("#c8c8c8>#9020ff", T::ColorPurple),
    ("#c8c8c8>#ff6000", T::ColorOrange),
    ("#c8c8c8>#1060ff", T::ColorBlue),
];
/// The swatch after the presets: the user's own color.
const CUSTOM: usize = COLORS.len();
/// The first saved custom color; saved colors follow the custom swatch.
const SAVED: usize = CUSTOM + 1;
const SWATCH_COLUMNS: usize = 6;
const SWATCH_GAP: f32 = 12.0;
/// Below this contrast with the tile a swatch gets an outline.
const SWATCH_MIN_CONTRAST: f32 = 1.5;
const BASE_WHITE: [f32; 3] = [200.0, 200.0, 200.0];
/// From the narrowest to the widest top frequency.
const RANGES: [(f32, f32); 4] = [(20.0, 5000.0), (30.0, 10000.0), (40.0, 16000.0), (20.0, 20000.0)];
/// (range_db, headroom_db) for low, medium and high sensitivity.
const SENSITIVITY: [(f32, f32, T); 3] =
    [(36.0, 8.0, T::SensitivityLow), (30.0, 3.0, T::SensitivityMedium), (24.0, 0.0, T::SensitivityHigh)];
const LANGUAGES: [(&str, &str); 2] = [("en", "English"), ("uk", "Українська")];
/// Frequencies labeled under the equalizer view.
const AXIS_TICKS: [f32; 9] = [50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    Look,
    Sound,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Keyboard,
    Equalizer,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Flag {
    Enabled,
    Equalize,
    Release,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Drop {
    Range,
    Device,
    Language,
}

/// Which color of the custom scheme a control edits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Part {
    Solid = 0,
    Top = 1,
    Bottom = 2,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SliderKind {
    Brightness,
    Hue(Part),
    Saturation(Part),
}

#[derive(Clone, Debug)]
enum Action {
    Page(Page),
    View(View),
    Minimize,
    Close,
    Toggle(Flag),
    Color(usize),
    Fill(bool),
    Sensitivity(f32, f32),
    MediaOnly(bool),
    Theme(&'static str),
    Open(Drop),
    Pick(Drop, usize),
    Dismiss,
    Slider(SliderKind),
    SaveColor,
    /// Removes the saved color of a swatch index.
    DeleteColor(usize),
    Autostart,
    OpenConfig,
    RevealConfig,
    Quit,
}

struct Hit {
    id: u64,
    rect: Rect,
    radius: f32,
    action: Action,
}

/// The user's own color scheme.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Custom {
    Solid([f32; 3]),
    Gradient { bottom: [f32; 3], top: [f32; 3] },
}

impl Custom {
    fn parse(code: &str) -> Self {
        match parse_color_mode(code) {
            ColorMode::Solid(c) => Custom::Solid(c),
            ColorMode::Gradient(bottom, top) => Custom::Gradient { bottom, top },
            _ => Custom::Gradient { bottom: BASE_WHITE, top: [255.0, 176.0, 0.0] },
        }
    }

    fn code(self) -> String {
        let hex = |c: [f32; 3]| format!("#{:02x}{:02x}{:02x}", c[0].round() as u8, c[1].round() as u8, c[2].round() as u8);
        match self {
            Custom::Solid(c) => hex(c),
            Custom::Gradient { bottom, top } => format!("{}>{}", hex(bottom), hex(top)),
        }
    }

    fn part(self, part: Part) -> [f32; 3] {
        match (self, part) {
            (Custom::Solid(c), _) => c,
            (Custom::Gradient { bottom, .. }, Part::Bottom) => bottom,
            (Custom::Gradient { top, .. }, _) => top,
        }
    }

    fn with_part(self, part: Part, rgb: [f32; 3]) -> Self {
        match (self, part) {
            (Custom::Solid(_), _) => Custom::Solid(rgb),
            (Custom::Gradient { top, .. }, Part::Bottom) => Custom::Gradient { bottom: rgb, top },
            (Custom::Gradient { bottom, .. }, _) => Custom::Gradient { bottom, top: rgb },
        }
    }
}

/// Code of swatch `index`: a preset, the custom color, then the saved colors.
fn swatch_code(s: &Settings, index: usize) -> Option<&str> {
    match index {
        i if i < CUSTOM => Some(COLORS[i].0),
        CUSTOM => Some(&s.custom_color),
        i => s.saved_colors.get(i - SAVED).map(String::as_str),
    }
}

/// The swatch of the current color: `picked` while it still matches, otherwise the custom color,
/// a saved color or a preset, in that order. A color matching nothing shows as custom.
fn selected_swatch(s: &Settings, picked: Option<usize>) -> usize {
    let mode = s.color_mode();
    let matches = |i: usize| swatch_code(s, i).is_some_and(|code| parse_color_mode(code) == mode);
    picked
        .filter(|&i| matches(i))
        .or_else(|| (CUSTOM..SAVED + s.saved_colors.len()).chain(0..CUSTOM).find(|&i| matches(i)))
        .unwrap_or(CUSTOM)
}

/// Stores `code` in the custom or saved swatch `index` and makes it the current color.
fn set_own_color(s: &mut Settings, index: usize, code: String) {
    match index {
        CUSTOM => s.custom_color = code.clone(),
        i if i >= SAVED => match s.saved_colors.get_mut(i - SAVED) {
            Some(slot) => *slot = code.clone(),
            None => return,
        },
        _ => return,
    }
    s.color = code;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SaveState {
    Ready,
    Saved,
    Full,
}

/// Whether the custom color can be added to the saved colors.
fn save_state(s: &Settings) -> SaveState {
    let mode = parse_color_mode(&s.custom_color);
    if s.saved_colors.iter().any(|code| parse_color_mode(code) == mode) {
        SaveState::Saved
    } else if s.saved_colors.len() >= MAX_SAVED_COLORS {
        SaveState::Full
    } else {
        SaveState::Ready
    }
}

enum Row {
    Switch { id: u64, label: String, hint: Option<String>, on: bool, action: Action },
    Choice { id: u64, label: String, hint: Option<String>, options: Vec<String>, selected: Option<usize>, actions: Vec<Action> },
    Dropdown { id: u64, label: String, hint: Option<String>, value: String, drop: Drop },
    Slider { id: u64, label: String, value: f32 },
    ColorEdit { id: u64, label: String, part: Part, rgb: [f32; 3], hue: f32, sat: f32 },
    Swatches { swatches: Vec<ColorMode>, selected: usize, caption: String },
    Nav { id: u64, glyph: char, title: String, subtitle: String, action: Action },
    /// A whole-tile command; without an action it is shown disabled.
    Button { id: u64, glyph: char, label: String, action: Option<Action>, danger: bool },
    Info { label: String, value: String },
}

struct Group {
    title: Option<String>,
    rows: Vec<Row>,
}

/// One frame's drawing context, borrowing the window's canvas, springs and hit list.
struct Ui<'a> {
    c: &'a Canvas,
    p: Palette,
    lang: Lang,
    anim: &'a mut Animator,
    hits: &'a mut Vec<Hit>,
    hover: Option<u64>,
    pressed: Option<u64>,
    clip: Rect,
    width: f32,
    /// Device pixels per DIP.
    scale: f32,
    /// A dropdown menu is open: only its items and the scrim take clicks.
    modal: bool,
}

fn fmt_hz(lang: Lang, hz: f32, short: bool) -> String {
    let uk = lang == Lang::Uk;
    let (value, unit) = if hz >= 1000.0 { (hz / 1000.0, if uk { "кГц" } else { "kHz" }) } else { (hz, if uk { "Гц" } else { "Hz" }) };
    let n = if (value - value.round()).abs() < 0.05 { format!("{}", value.round()) } else { format!("{value:.1}") };
    if short {
        if hz >= 1000.0 { format!("{n}k") } else { n }
    } else {
        format!("{n} {unit}")
    }
}

fn fmt_range(lang: Lang, fmin: f32, fmax: f32) -> String {
    format!("{} – {}", fmt_hz(lang, fmin, false), fmt_hz(lang, fmax, false))
}

fn rgb_color(rgb: [f32; 3]) -> Color {
    Color::from_rgb_f32(rgb)
}

fn rainbow_stops() -> Vec<Color> {
    (0..=6).map(|i| rgb_color(hsv_to_rgb(i as f32 / 6.0, 1.0))).collect()
}

/// Height of a grid of `count` swatches.
fn swatches_height(count: usize) -> f32 {
    let lines = count.div_ceil(SWATCH_COLUMNS).max(1) as f32;
    lines * SWATCH_SIZE + (lines - 1.0) * SWATCH_GAP
}

/// WCAG contrast ratio of two colors.
fn contrast(a: Color, b: Color) -> f32 {
    let luminance = |c: Color| {
        let linear = |v: f32| v.max(0.0).powf(2.2);
        0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b) + 0.05
    };
    let (x, y) = (luminance(a), luminance(b));
    x.max(y) / x.min(y)
}

/// Colors of a swatch from top to bottom (or left to right for rainbows).
fn swatch_colors(mode: ColorMode) -> (Vec<Color>, bool) {
    match mode {
        ColorMode::Classic => (vec![Color::rgb8(255, 0, 0), Color::rgb8(255, 210, 0), Color::rgb8(0, 255, 40)], true),
        ColorMode::Gradient(bottom, top) => (vec![rgb_color(top), rgb_color(bottom)], true),
        ColorMode::Solid(rgb) => (vec![rgb_color(rgb)], true),
        ColorMode::Rainbow | ColorMode::Flow => (rainbow_stops(), false),
    }
}

impl Ui<'_> {
    fn t(&self, key: T) -> &'static str {
        tr(self.lang, key)
    }

    /// Rounds a coordinate to the nearest device pixel.
    fn snap(&self, v: f32) -> f32 {
        (v * self.scale).round() / self.scale
    }

    fn hit(&mut self, id: u64, rect: Rect, radius: f32, action: Action, overlay: bool) {
        if self.modal && !overlay {
            return;
        }
        let clipped = Rect::new(rect.l.max(self.clip.l), rect.t.max(self.clip.t), rect.r.min(self.clip.r), rect.b.min(self.clip.b));
        if clipped.w() > 0.0 && clipped.h() > 0.0 {
            self.hits.push(Hit { id, rect: clipped, radius, action });
        }
    }

    /// Hover and press state layer opacity for an element.
    fn layer(&mut self, id: u64) -> f32 {
        let target = if self.modal {
            0.0
        } else {
            let hover = if self.hover == Some(id) { HOVER_OPACITY } else { 0.0 };
            let press = if self.pressed == Some(id) && self.hover == Some(id) { PRESS_OPACITY } else { 0.0 };
            hover + press
        };
        self.anim.get((id, P_LAYER), target, SPRING_FAST_EFFECTS)
    }

    fn icon_button(&mut self, id: u64, cx: f32, cy: f32, g: char, size: f32, action: Action) {
        let layer = self.layer(id);
        let r = ICON_BUTTON_SIZE / 2.0;
        self.c.fill_circle(cx, cy, r, self.p.on_surface.alpha(layer));
        self.c.glyph(g, cx, cy, size, self.p.on_surface_variant);
        self.hit(id, Rect::new(cx - r, cy - r, cx + r, cy + r), r, action, false);
    }

    fn section_title(&mut self, title: &str, y: f32) -> f32 {
        self.c.text(title, Rect::new(CONTENT_INSET, y, self.width - CONTENT_INSET, y + SECTION_TITLE_HEIGHT), TextStyle::TitleSmall, self.p.primary, Align::Leading);
        y + SECTION_TITLE_HEIGHT
    }

    fn hint_height(&self, hint: &Option<String>) -> f32 {
        hint.as_ref().map_or(0.0, |h| self.c.measure_height(h, TextStyle::BodySmall, self.width - 2.0 * CONTENT_INSET))
    }

    fn draw_hint(&self, hint: &Option<String>, y: f32) {
        if let Some(h) = hint {
            self.c.text_wrapped(h, Rect::new(CONTENT_INSET, y, self.width - CONTENT_INSET, y + 200.0), TextStyle::BodySmall, self.p.on_surface_variant);
        }
    }

    fn row_height(&self, row: &Row) -> f32 {
        match row {
            Row::Switch { hint, .. } | Row::Dropdown { hint, .. } => match hint {
                Some(_) => HINT_TOP_IN_ROW + self.hint_height(hint) + HINT_BOTTOM_GAP,
                None => ROW_HEIGHT,
            },
            Row::Choice { hint, .. } => match hint {
                Some(_) => 76.0 + self.hint_height(hint) + HINT_BOTTOM_GAP,
                None => CHOICE_ROW_HEIGHT,
            },
            Row::Slider { .. } => CHOICE_ROW_HEIGHT,
            Row::ColorEdit { .. } => COLOR_EDIT_HEIGHT,
            Row::Swatches { swatches, .. } => 16.0 + swatches_height(swatches.len()) + 12.0 + 20.0 + 12.0,
            Row::Nav { .. } => 60.0,
            Row::Button { .. } | Row::Info { .. } => ROW_HEIGHT,
        }
    }

    /// Whole-tile rows react as one: hover rounds their inner corners (the M3 Expressive morph).
    fn row_action(row: &Row) -> Option<(u64, Action)> {
        match row {
            Row::Switch { id, action, .. } | Row::Nav { id, action, .. } => Some((*id, action.clone())),
            Row::Button { id, action: Some(action), .. } => Some((*id, action.clone())),
            _ => None,
        }
    }

    fn groups(&mut self, groups: &[Group], mut y: f32) -> f32 {
        for (gi, group) in groups.iter().enumerate() {
            if let Some(title) = &group.title {
                if gi > 0 {
                    y += SECTION_GAP - GROUP_GAP;
                }
                y = self.section_title(title, y);
            }
            let n = group.rows.len();
            for (i, row) in group.rows.iter().enumerate() {
                let h = self.row_height(row);
                let rect = Rect::new(PAGE_GUTTER, y, self.width - PAGE_GUTTER, y + h);
                if rect.intersects(&self.clip) {
                    self.tile(Self::row_action(row), rect, i == 0, i + 1 == n);
                    self.row(row, rect);
                }
                y += h + if i + 1 < n { SEGMENT_GAP } else { 0.0 };
            }
            y += GROUP_GAP;
        }
        y
    }

    fn tile(&mut self, reactive: Option<(u64, Action)>, rect: Rect, first: bool, last: bool) {
        let inner = match &reactive {
            Some((id, _)) => {
                let hovered = !self.modal && self.hover == Some(*id);
                self.anim.get((*id, P_CORNER), if hovered { RADIUS_TILE_HOVERED } else { RADIUS_TILE }, SPRING_FAST_SPATIAL)
            }
            None => RADIUS_TILE,
        };
        let corners = Corners::tile(first, last, RADIUS_GROUP, inner);
        self.c.fill_shape(rect, corners, self.p.surface_container);
        if let Some((id, action)) = reactive {
            let layer = self.layer(id);
            self.c.fill_shape(rect, corners, self.p.on_surface.alpha(layer));
            self.hit(id, rect, inner, action, false);
        }
    }

    fn row(&mut self, row: &Row, rect: Rect) {
        let p = self.p;
        let right = rect.r - ITEM_PADDING;
        let label_rect = |r: f32| Rect::new(CONTENT_INSET, rect.t, r, rect.t + ROW_HEIGHT);
        match row {
            Row::Switch { id, label, hint, on, .. } => {
                self.c.text(label, label_rect(right - SWITCH_WIDTH - 12.0), TextStyle::BodyLarge, p.on_surface, Align::Leading);
                self.switch(*id, right, rect.t + ROW_HEIGHT / 2.0, *on);
                self.draw_hint(hint, rect.t + HINT_TOP_IN_ROW);
            }
            Row::Choice { id, label, hint, options, selected, actions } => {
                self.c.text(label, Rect::new(CONTENT_INSET, rect.t + 6.0, right, rect.t + 34.0), TextStyle::BodyLarge, p.on_surface, Align::Leading);
                let group = Rect::new(CONTENT_INSET, rect.t + 36.0, right, rect.t + 36.0 + CONTROL_HEIGHT);
                self.button_group(*id, group, options, *selected, actions);
                self.draw_hint(hint, rect.t + 76.0);
            }
            Row::Dropdown { id, label, hint, value, drop } => {
                let w = (self.c.measure(value, TextStyle::BodyMedium) + 44.0).clamp(96.0, rect.w() * 0.62);
                self.c.text(label, label_rect(right - w - 12.0), TextStyle::BodyLarge, p.on_surface, Align::Leading);
                let field = Rect::new(right - w, rect.t + 8.0, right, rect.t + 8.0 + CONTROL_HEIGHT);
                let layer = self.layer(*id);
                self.c.fill_round(field, RADIUS_SMALL, p.surface_container_highest);
                self.c.fill_round(field, RADIUS_SMALL, p.on_surface.alpha(layer));
                self.c.text(value, Rect::new(field.l + 12.0, field.t, field.r - 30.0, field.b), TextStyle::BodyMedium, p.on_surface, Align::Leading);
                self.c.glyph(glyph::CHEVRON_DOWN, field.r - 16.0, field.cy(), 12.0, p.on_surface_variant);
                self.hit(*id, field, RADIUS_SMALL, Action::Open(*drop), false);
                self.draw_hint(hint, rect.t + HINT_TOP_IN_ROW);
            }
            Row::Slider { id, label, value } => {
                self.c.text(label, Rect::new(CONTENT_INSET, rect.t + 6.0, right - 60.0, rect.t + 34.0), TextStyle::BodyLarge, p.on_surface, Align::Leading);
                let pct = format!("{}%", (value * 100.0).round());
                self.c.text(&pct, Rect::new(right - 60.0, rect.t + 6.0, right, rect.t + 34.0), TextStyle::BodyMedium, p.on_surface_variant, Align::Trailing);
                self.slider(*id, CONTENT_INSET, right, rect.t + 54.0, *value);
            }
            Row::ColorEdit { id, label, part, rgb, hue, sat } => {
                self.c.text(label, Rect::new(CONTENT_INSET, rect.t + 6.0, right - 110.0, rect.t + 34.0), TextStyle::BodyLarge, p.on_surface, Align::Leading);
                let code = Custom::Solid(*rgb).code();
                self.c.text(&code, Rect::new(right - 84.0, rect.t + 6.0, right, rect.t + 34.0), TextStyle::BodyMedium, p.on_surface_variant, Align::Trailing);
                let chip = Rect::new(right - 106.0, rect.t + 10.0, right - 86.0, rect.t + 30.0);
                self.c.fill_round(chip, RADIUS_SMALL, rgb_color(*rgb));
                self.c.stroke_round(chip.inset(0.5, 0.5), RADIUS_SMALL - 0.5, p.outline_variant, 1.0);
                self.color_slider(*id, CONTENT_INSET, right, rect.t + 52.0, *hue, &rainbow_stops(), SliderKind::Hue(*part));
                let pure = rgb_color(hsv_to_rgb(*hue, 1.0));
                self.color_slider(*id + 1, CONTENT_INSET, right, rect.t + 84.0, *sat, &[Color::rgb8(255, 255, 255), pure], SliderKind::Saturation(*part));
            }
            Row::Swatches { swatches, selected, caption } => {
                let cols = SWATCH_COLUMNS as f32;
                let gap = (right - CONTENT_INSET - cols * SWATCH_SIZE) / (cols - 1.0);
                for (i, mode) in swatches.iter().enumerate() {
                    let (col, line) = ((i % SWATCH_COLUMNS) as f32, (i / SWATCH_COLUMNS) as f32);
                    // Whole pixels give every circle the same edge.
                    let x = self.snap(CONTENT_INSET + col * (SWATCH_SIZE + gap));
                    let y = self.snap(rect.t + 16.0 + line * (SWATCH_SIZE + SWATCH_GAP));
                    self.swatch(i, Rect::xywh(x, y, SWATCH_SIZE, SWATCH_SIZE), *mode, *selected == i);
                }
                let cy = rect.t + 16.0 + swatches_height(swatches.len()) + 12.0;
                self.c.text(caption, Rect::new(CONTENT_INSET, cy, right, cy + 20.0), TextStyle::BodySmall, p.on_surface_variant, Align::Center);
            }
            Row::Button { glyph: g, label, action, danger, .. } => {
                let color = match (action, danger) {
                    (None, _) => p.on_surface.alpha(DISABLED_OPACITY),
                    (Some(_), true) => p.error,
                    (Some(_), false) => p.primary,
                };
                self.c.glyph(*g, CONTENT_INSET + ICON_SIZE / 2.0, rect.cy(), ICON_SIZE, color);
                let text_l = CONTENT_INSET + ICON_SIZE + SPACE_3;
                self.c.text(label, Rect::new(text_l, rect.t, right, rect.b), TextStyle::BodyLarge, color, Align::Leading);
            }
            Row::Nav { glyph: g, title, subtitle, .. } => {
                self.c.glyph(*g, CONTENT_INSET + ICON_SIZE / 2.0, rect.cy(), ICON_SIZE, p.on_surface_variant);
                let text_l = CONTENT_INSET + ICON_SIZE + SPACE_3;
                self.c.text(title, Rect::new(text_l, rect.t + 10.0, right - 24.0, rect.t + 31.0), TextStyle::BodyLarge, p.on_surface, Align::Leading);
                self.c.text(subtitle, Rect::new(text_l, rect.t + 31.0, right - 24.0, rect.t + 50.0), TextStyle::BodySmall, p.on_surface_variant, Align::Leading);
                self.c.glyph(glyph::CHEVRON_RIGHT, right - SMALL_ICON_SIZE / 2.0, rect.cy(), SMALL_ICON_SIZE, p.on_surface_variant);
            }
            Row::Info { label, value } => {
                self.c.text(label, label_rect(right - 160.0), TextStyle::BodyLarge, p.on_surface, Align::Leading);
                self.c.text(value, Rect::new(right - 200.0, rect.t, right, rect.t + ROW_HEIGHT), TextStyle::BodyMedium, p.on_surface_variant, Align::Trailing);
            }
        }
    }

    fn switch(&mut self, id: u64, right: f32, cy: f32, on: bool) {
        let p = self.p;
        let pos = self.anim.get((id, P_POSITION), on as u8 as f32, SPRING_FAST_SPATIAL);
        let mix = self.anim.get((id, P_SELECTED), on as u8 as f32, SPRING_DEFAULT_EFFECTS);
        let pressed = !self.modal && self.pressed == Some(id) && self.hover == Some(id);
        let target = if pressed { 28.0 } else if on { 24.0 } else { 16.0 };
        let size = self.anim.get((id, P_SIZE), target, SPRING_FAST_SPATIAL);
        let track = Rect::new(right - SWITCH_WIDTH, cy - SWITCH_HEIGHT / 2.0, right, cy + SWITCH_HEIGHT / 2.0);
        self.c.fill_round(track, SWITCH_HEIGHT / 2.0, p.surface_container_highest.mix(p.primary, mix));
        if mix < 1.0 {
            self.c.stroke_round(track.inset(1.0, 1.0), SWITCH_HEIGHT / 2.0 - 1.0, p.outline.alpha(1.0 - mix), 2.0);
        }
        let cx = track.l + SWITCH_HEIGHT / 2.0 + pos * (SWITCH_WIDTH - SWITCH_HEIGHT);
        self.c.fill_circle(cx, cy, size / 2.0, p.outline.mix(p.on_primary, mix));
        if size > 20.0 {
            self.c.glyph(glyph::CHECK, cx, cy, 14.0, p.primary.alpha(mix));
        }
    }

    fn button_group(&mut self, base: u64, rect: Rect, labels: &[String], selected: Option<usize>, actions: &[Action]) {
        let p = self.p;
        let n = labels.len().max(1);
        let w = (rect.w() - BUTTON_GROUP_GAP * (n - 1) as f32) / n as f32;
        let full = rect.h() / 2.0;
        for (i, label) in labels.iter().enumerate() {
            let id = base + i as u64;
            let l = rect.l + i as f32 * (w + BUTTON_GROUP_GAP);
            let r = Rect::new(l, rect.t, l + w, rect.b);
            let is_selected = selected == Some(i);
            let sel = self.anim.get((id, P_SELECTED), is_selected as u8 as f32, SPRING_DEFAULT_EFFECTS);
            let pressed = !self.modal && self.pressed == Some(id) && self.hover == Some(id);
            let inner_target = if pressed { RADIUS_BUTTON_GROUP_PRESSED } else if is_selected { full } else { RADIUS_BUTTON_GROUP_INNER };
            let inner = self.anim.get((id, P_CORNER), inner_target, SPRING_FAST_SPATIAL);
            let left = if i == 0 { full } else { inner };
            let right = if i + 1 == n { full } else { inner };
            let corners = Corners { tl: left, bl: left, tr: right, br: right };
            self.c.fill_shape(r, corners, p.surface_container_highest.mix(p.primary, sel));
            let layer = self.layer(id);
            self.c.fill_shape(r, corners, p.on_surface.mix(p.on_primary, sel).alpha(layer));
            self.c.text(label, r.inset(6.0, 0.0), TextStyle::LabelMedium, p.on_surface_variant.mix(p.on_primary, sel), Align::Center);
            if let Some(action) = actions.get(i) {
                self.hit(id, r, inner, action.clone(), false);
            }
        }
    }

    fn slider(&mut self, id: u64, left: f32, right: f32, cy: f32, value: f32) {
        let p = self.p;
        let t = value.clamp(0.0, 1.0);
        let held = self.pressed == Some(id);
        let handle_w = self.anim.get((id, P_SIZE), if held { 2.0 } else { SLIDER_HANDLE_WIDTH }, SPRING_FAST_SPATIAL);
        let hx = left + t * (right - left);
        let (top, bottom) = (cy - SLIDER_TRACK / 2.0, cy + SLIDER_TRACK / 2.0);
        let round = SLIDER_TRACK / 2.0;
        let gap_l = hx - SLIDER_HANDLE_WIDTH / 2.0 - SLIDER_GAP;
        let gap_r = hx + SLIDER_HANDLE_WIDTH / 2.0 + SLIDER_GAP;
        if gap_l > left {
            self.c.fill_shape(Rect::new(left, top, gap_l, bottom), Corners { tl: round, bl: round, tr: 2.0, br: 2.0 }, p.primary);
        }
        if gap_r < right {
            self.c.fill_shape(Rect::new(gap_r, top, right, bottom), Corners { tl: 2.0, bl: 2.0, tr: round, br: round }, p.secondary_container);
            self.c.fill_circle(right - round, cy, 2.0, p.on_secondary_container);
        }
        self.c.fill_round(
            Rect::new(hx - handle_w / 2.0, cy - SLIDER_HANDLE_HEIGHT / 2.0, hx + handle_w / 2.0, cy + SLIDER_HANDLE_HEIGHT / 2.0),
            handle_w / 2.0,
            p.primary,
        );
        self.hit(id, Rect::new(left - 8.0, cy - 20.0, right + 8.0, cy + 20.0), round, Action::Slider(SliderKind::Brightness), false);
    }

    /// A slider over a color gradient: hue or saturation of a custom color.
    #[allow(clippy::too_many_arguments)]
    fn color_slider(&mut self, id: u64, left: f32, right: f32, cy: f32, value: f32, colors: &[Color], kind: SliderKind) {
        let p = self.p;
        let track = Rect::new(left, cy - COLOR_TRACK / 2.0, right, cy + COLOR_TRACK / 2.0);
        self.c.fill_round_gradient(track, COLOR_TRACK / 2.0, colors, false);
        let held = self.pressed == Some(id);
        let w = self.anim.get((id, P_SIZE), if held { 4.0 } else { 6.0 }, SPRING_FAST_SPATIAL);
        let hx = left + value.clamp(0.0, 1.0) * (right - left);
        let handle = Rect::new(hx - w / 2.0, cy - 13.0, hx + w / 2.0, cy + 13.0);
        self.c.fill_round(handle.inset(-1.5, -1.5), w / 2.0 + 1.5, p.surface);
        self.c.fill_round(handle, w / 2.0, p.on_surface);
        self.hit(id, Rect::new(left - 8.0, cy - 14.0, right + 8.0, cy + 14.0), COLOR_TRACK / 2.0, Action::Slider(kind), false);
    }

    fn swatch(&mut self, index: usize, rect: Rect, mode: ColorMode, selected: bool) {
        let p = self.p;
        let id = ID_SWATCH + index as u64;
        // A chosen swatch turns from a circle into a rounded square.
        let morph = self.anim.get((id, P_CORNER), selected as u8 as f32, SPRING_FAST_SPATIAL);
        let radius = SWATCH_SIZE / 2.0 - morph * (SWATCH_SIZE / 2.0 - RADIUS_MEDIUM);
        let layer = self.layer(id);
        self.c.fill_circle(rect.cx(), rect.cy(), SWATCH_SIZE / 2.0 + 4.0, p.on_surface.alpha(layer));
        let (colors, vertical) = swatch_colors(mode);
        // An outline only where the swatch would blend into the tile: a hairline over every
        // colored edge reads as a jagged rim.
        if colors.iter().any(|c| contrast(*c, p.surface_container) < SWATCH_MIN_CONTRAST) {
            self.c.fill_round(rect.inset(-1.0, -1.0), radius + 1.0, p.outline_variant);
        }
        self.c.fill_round_gradient(rect, radius, &colors, vertical);
        let mark = if selected {
            Some(glyph::CHECK)
        } else if index == CUSTOM {
            Some(glyph::EDIT)
        } else if mode == ColorMode::Flow {
            Some(glyph::SYNC)
        } else {
            None
        };
        if let Some(g) = mark {
            self.c.fill_circle(rect.cx(), rect.cy(), 11.0, p.surface.alpha(0.85));
            self.c.glyph(g, rect.cx(), rect.cy(), 14.0, p.on_surface);
        }
        self.hit(id, rect, radius, Action::Color(index), false);
    }
}

/// What a live-only redraw reuses from the last full frame.
struct LiveCache {
    area: Rect,
    owner: Owner,
    keyboard: Option<KeyboardInfo>,
    layout: Layout,
    palette: Palette,
    mode: ColorMode,
    lang: Lang,
}

/// Everything besides the live view that the window shows; it redraws when this changes.
#[derive(PartialEq)]
struct Shown {
    settings: u64,
    status: Status,
    dark: bool,
    autostart: bool,
    config_error: Option<String>,
}

pub struct MainWindow {
    hwnd: HWND,
    canvas: Canvas,
    /// Draws only the live view when nothing else changed; far cheaper than a whole frame.
    /// Renders on the GPU and exists only while the window is visible: dropping it with its own
    /// factory releases the graphics device.
    live_canvas: Option<Canvas>,
    live: Option<LiveCache>,
    shown: Option<Shown>,
    autostart: bool,
    clock: FrameClock,
    dpi: f32,
    height: f32,
    shared: Arc<Shared>,
    devices: Vec<String>,
    config_error: Option<String>,
    anim: Animator,
    hits: Vec<Hit>,
    hover: Option<u64>,
    pressed: Option<u64>,
    focus: Option<u64>,
    show_focus: bool,
    page: Page,
    view: View,
    scroll: f32,
    content_h: f32,
    page_view: Rect,
    dropdown: Option<(Drop, Rect)>,
    menu_hot: Option<usize>,
    tracking: bool,
    last_seq: u64,
    dirty: bool,
    moving: bool,
    /// A slider being dragged and the settings it edits, saved on release.
    dragging: Option<(SliderKind, Settings)>,
    /// Last hue per custom part, kept while its saturation is zero.
    hues: [f32; 3],
    /// Color of the title bar and taskbar icons.
    icon_seed: [f32; 3],
    /// The swatch picked or edited last: it stays selected while the color matches it, even when
    /// another swatch has the same color.
    picked: Option<usize>,
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
            live: None,
            shown: None,
            autostart: autostart::is_enabled(),
            clock: FrameClock::new(hwnd, WM_APP_FRAME),
            dpi,
            height,
            shared,
            devices,
            config_error: None,
            anim: Animator::default(),
            hits: Vec::new(),
            hover: None,
            pressed: None,
            focus: None,
            show_focus: false,
            page: Page::Look,
            view: View::Keyboard,
            scroll: 0.0,
            content_h: 0.0,
            page_view: Rect::default(),
            dropdown: None,
            menu_hot: None,
            tracking: false,
            last_seq: u64::MAX,
            dirty: true,
            moving: false,
            dragging: None,
            hues: [0.45; 3],
            icon_seed,
            picked: None,
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
            w.devices = devices.clone();
            w.dirty = true;
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
                let settings = w.shared.settings();
                let seed = seed_for(&settings);
                if seed != w.icon_seed {
                    set_window_icons(w.hwnd, seed);
                    w.icon_seed = seed;
                }
                let shown = Shown {
                    settings: w.shared.settings_version(),
                    status: w.shared.status(),
                    dark: wants_dark(&settings),
                    autostart: autostart::is_enabled(),
                    config_error,
                };
                if w.shown.as_ref() != Some(&shown) {
                    w.autostart = shown.autostart;
                    w.config_error = shown.config_error.clone();
                    w.shown = Some(shown);
                    w.dirty = true;
                }
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
        self.shared.set_ui_hz(refresh_rate(self.hwnd));
        self.dirty = true;
    }

    fn stop_frames(&mut self) {
        self.live_canvas = None;
        self.clock.stop();
        self.shared.set_ui_hz(0);
    }

    fn hit_at(&self, x: f32, y: f32) -> Option<&Hit> {
        self.hits.iter().rev().find(|h| h.rect.contains(x, y))
    }

    fn update_settings(&self, f: impl FnOnce(&mut Settings)) {
        let mut s = self.shared.settings();
        f(&mut s);
        post(Command::Apply { settings: Box::new(s), save: true });
    }

    /// Hue and saturation of a custom color part, keeping the hue while the color is white.
    fn hue_sat(&self, rgb: [f32; 3], part: Part) -> (f32, f32) {
        let (h, s) = rgb_to_hs(rgb);
        if s < 0.02 { (self.hues[part as usize], s) } else { (h, s) }
    }

    /// The selected swatch and its color, as the color editor shows them.
    fn edited(&self, s: &Settings) -> (usize, Custom) {
        let index = selected_swatch(s, self.picked);
        (index, Custom::parse(swatch_code(s, index).unwrap_or_default()))
    }

    fn act(&mut self, action: Action, rect: Rect) -> After {
        self.dirty = true;
        match action {
            Action::Page(page) => {
                if self.page != page {
                    self.page = page;
                    self.scroll = 0.0;
                    self.anim.set((ID_SCROLL, P_POSITION), 0.0);
                }
            }
            Action::View(view) => self.view = view,
            Action::Minimize => return After::Minimize,
            Action::Close => return After::Hide,
            Action::Toggle(flag) => match flag {
                Flag::Enabled => post(Command::ToggleEnabled),
                Flag::Equalize => self.update_settings(|s| s.equalize = if s.equalize > 0.0 { 0.0 } else { 1.0 }),
                Flag::Release => self.update_settings(|s| s.release_when_silent = !s.release_when_silent),
            },
            Action::Color(index) => {
                self.picked = Some(index);
                self.update_settings(|s| {
                    if let Some(code) = swatch_code(s, index).map(str::to_owned) {
                        s.color = code;
                    }
                });
            }
            Action::Fill(gradient) => {
                let (index, custom) = self.edited(&self.shared.settings());
                if index >= CUSTOM {
                    self.picked = Some(index);
                    let custom = match (custom, gradient) {
                        (Custom::Solid(c), true) => Custom::Gradient { bottom: BASE_WHITE, top: c },
                        (Custom::Gradient { top, .. }, false) => Custom::Solid(top),
                        (c, _) => c,
                    };
                    self.update_settings(|s| set_own_color(s, index, custom.code()));
                }
            }
            Action::SaveColor => {
                self.picked = Some(CUSTOM);
                self.update_settings(|s| {
                    if save_state(s) == SaveState::Ready {
                        s.saved_colors.push(s.custom_color.clone());
                    }
                });
            }
            Action::DeleteColor(index) => {
                self.picked = Some(CUSTOM);
                self.update_settings(|s| {
                    if let Some(i) = index.checked_sub(SAVED).filter(|&i| i < s.saved_colors.len()) {
                        // The color stays on the keyboard, now as the custom color.
                        s.custom_color = s.saved_colors.remove(i);
                        s.color = s.custom_color.clone();
                    }
                });
            }
            Action::Sensitivity(range, headroom) => self.update_settings(|s| {
                s.range_db = range;
                s.headroom_db = headroom;
            }),
            Action::MediaOnly(on) => self.update_settings(|s| s.media_only = on),
            Action::Theme(code) => self.update_settings(|s| s.theme = code.into()),
            Action::Open(drop) => {
                self.dropdown = Some((drop, rect));
                self.menu_hot = None;
            }
            Action::Pick(drop, index) => {
                self.dropdown = None;
                match drop {
                    Drop::Range => {
                        if let Some((fmin, fmax)) = RANGES.get(index) {
                            self.update_settings(|s| {
                                s.fmin = *fmin;
                                s.fmax = *fmax;
                            });
                        }
                    }
                    Drop::Device => {
                        let device = if index == 0 { String::new() } else { self.devices.get(index - 1).cloned().unwrap_or_default() };
                        self.update_settings(|s| s.audio_device = device);
                    }
                    Drop::Language => {
                        if let Some((code, _)) = LANGUAGES.get(index) {
                            self.update_settings(|s| s.language = (*code).into());
                        }
                    }
                }
            }
            Action::Dismiss => self.dropdown = None,
            Action::Slider(_) => {}
            Action::Autostart => post(Command::ToggleAutostart),
            Action::OpenConfig => post(Command::OpenConfig),
            Action::RevealConfig => post(Command::RevealConfig),
            Action::Quit => post(Command::Quit),
        }
        After::None
    }

    /// Applies a slider at pointer `x`; `save` writes the settings file (on release).
    fn drag(&mut self, kind: SliderKind, x: f32, rect: Rect, save: bool) {
        let left = rect.l + 8.0;
        let right = rect.r - 8.0;
        let value = ((x - left) / (right - left)).clamp(0.0, 1.0);
        let base = self.dragging.as_ref().map_or_else(|| self.shared.settings(), |(_, s)| s.clone());
        let s = self.with_slider(base, kind, value);
        self.dragging = Some((kind, s.clone()));
        post(Command::Apply { settings: Box::new(s), save });
        self.dirty = true;
    }

    /// Ends a press or drag; a drag in progress is saved.
    fn release_pointer(&mut self) {
        self.pressed = None;
        if let Some((_, s)) = self.dragging.take() {
            post(Command::Apply { settings: Box::new(s), save: true });
        }
        self.dirty = true;
    }

    fn slider_value(&self, s: &Settings, kind: SliderKind) -> f32 {
        match kind {
            SliderKind::Brightness => s.brightness,
            SliderKind::Hue(part) => self.hue_sat(self.edited(s).1.part(part), part).0,
            SliderKind::Saturation(part) => self.hue_sat(self.edited(s).1.part(part), part).1,
        }
    }

    /// `s` with slider `kind` set to `value` (0..1).
    fn with_slider(&mut self, mut s: Settings, kind: SliderKind, value: f32) -> Settings {
        match kind {
            SliderKind::Brightness => s.brightness = (value * 100.0).round() / 100.0,
            SliderKind::Hue(part) | SliderKind::Saturation(part) => {
                let (index, custom) = self.edited(&s);
                if index < CUSTOM {
                    return s;
                }
                // Keeps the edited swatch selected while its color passes another swatch's.
                self.picked = Some(index);
                let (mut h, mut sat) = self.hue_sat(custom.part(part), part);
                if matches!(kind, SliderKind::Hue(_)) {
                    h = value;
                    self.hues[part as usize] = value;
                } else {
                    sat = value;
                }
                set_own_color(&mut s, index, custom.with_part(part, hsv_to_rgb(h, sat)).code());
            }
        }
        s
    }

    fn page_groups(&self, lang: Lang, s: &Settings) -> Vec<Group> {
        let t = |k| tr(lang, k);
        match self.page {
            Page::Look => {
                let (selected, custom) = self.edited(s);
                let swatches = COLORS
                    .iter()
                    .map(|(code, _)| *code)
                    .chain([s.custom_color.as_str()])
                    .chain(s.saved_colors.iter().map(String::as_str))
                    .map(parse_color_mode)
                    .collect();
                let caption = match selected {
                    i if i < CUSTOM => t(COLORS[i].1).to_string(),
                    CUSTOM => t(T::ColorCustom).to_string(),
                    i => format!("{} {}", t(T::ColorSaved), i - CUSTOM),
                };
                let mut rows = vec![Row::Swatches { swatches, selected, caption }];
                if selected >= CUSTOM {
                    let gradient = matches!(custom, Custom::Gradient { .. });
                    rows.push(Row::Choice {
                        id: ID_FILL,
                        label: t(T::Fill).into(),
                        hint: None,
                        options: vec![t(T::FillSolid).into(), t(T::FillGradient).into()],
                        selected: Some(gradient as usize),
                        actions: vec![Action::Fill(false), Action::Fill(true)],
                    });
                    let parts: &[(Part, T)] =
                        if gradient { &[(Part::Top, T::ColorTop), (Part::Bottom, T::ColorBottom)] } else { &[(Part::Solid, T::ColorSingle)] };
                    for (i, (part, label)) in parts.iter().enumerate() {
                        let rgb = custom.part(*part);
                        let (hue, sat) = self.hue_sat(rgb, *part);
                        rows.push(Row::ColorEdit { id: ID_COLOR_EDIT + 2 * i as u64, label: t(*label).into(), part: *part, rgb, hue, sat });
                    }
                    rows.push(if selected == CUSTOM {
                        let state = save_state(s);
                        let (glyph, label) = match state {
                            SaveState::Ready => (glyph::ADD, T::SavePreset),
                            SaveState::Saved => (glyph::CHECK, T::PresetSaved),
                            SaveState::Full => (glyph::ADD, T::PresetsFull),
                        };
                        let action = (state == SaveState::Ready).then_some(Action::SaveColor);
                        Row::Button { id: ID_PRESET, glyph, label: t(label).into(), action, danger: false }
                    } else {
                        Row::Button { id: ID_PRESET, glyph: glyph::DELETE, label: t(T::DeletePreset).into(), action: Some(Action::DeleteColor(selected)), danger: true }
                    });
                }
                rows.push(Row::Slider { id: ID_BRIGHTNESS, label: t(T::Brightness).into(), value: s.brightness });
                vec![Group { title: Some(t(T::SectionColors).into()), rows }]
            }
            Page::Sound => {
                let sensitivity = SENSITIVITY.iter().position(|(r, h, _)| (r - s.range_db).abs() < 0.01 && (h - s.headroom_db).abs() < 0.01);
                let range = RANGES.iter().position(|(a, b)| (a - s.fmin).abs() < 0.5 && (b - s.fmax).abs() < 0.5);
                let range_text = match range {
                    Some(_) => fmt_range(lang, s.fmin, s.fmax),
                    None => format!("{}: {}", t(T::FrequencyCustom), fmt_range(lang, s.fmin, s.fmax)),
                };
                let device = if s.audio_device.is_empty() { t(T::WindowsDefault).to_string() } else { s.audio_device.clone() };
                vec![
                    Group {
                        title: Some(t(T::SectionAnalysis).into()),
                        rows: vec![
                            Row::Choice {
                                id: ID_SENSITIVITY,
                                label: t(T::Sensitivity).into(),
                                hint: Some(if sensitivity.is_some() { t(T::SensitivityHint) } else { t(T::SensitivityCustom) }.into()),
                                options: SENSITIVITY.iter().map(|(_, _, k)| t(*k).to_string()).collect(),
                                selected: sensitivity,
                                actions: SENSITIVITY.iter().map(|(r, h, _)| Action::Sensitivity(*r, *h)).collect(),
                            },
                            Row::Dropdown { id: ID_RANGE, label: t(T::FrequencyRange).into(), hint: Some(t(T::FrequencyHint).into()), value: range_text, drop: Drop::Range },
                            Row::Switch { id: ID_EQUALIZE, label: t(T::Equalize).into(), hint: Some(t(T::EqualizeHint).into()), on: s.equalize > 0.0, action: Action::Toggle(Flag::Equalize) },
                        ],
                    },
                    Group {
                        title: Some(t(T::SectionAudio).into()),
                        rows: vec![
                            Row::Dropdown { id: ID_DEVICE, label: t(T::AudioSource).into(), hint: None, value: device, drop: Drop::Device },
                            Row::Choice {
                                id: ID_MEDIA_ONLY,
                                label: t(T::ReactTo).into(),
                                hint: Some(t(T::ReactHint).into()),
                                options: vec![t(T::ReactAnySound).into(), t(T::ReactMedia).into()],
                                selected: Some(s.media_only as usize),
                                actions: vec![Action::MediaOnly(false), Action::MediaOnly(true)],
                            },
                            Row::Switch { id: ID_RELEASE, label: t(T::ReleaseWhenSilent).into(), hint: Some(t(T::ReleaseHint).into()), on: s.release_when_silent, action: Action::Toggle(Flag::Release) },
                        ],
                    },
                ]
            }
            Page::Settings => {
                let mut groups = Vec::new();
                if let Some(e) = &self.config_error {
                    groups.push(Group { title: None, rows: vec![Row::Info { label: t(T::ConfigError).into(), value: e.clone() }] });
                }
                let language = LANGUAGES.iter().find(|(code, _)| *code == s.language).map_or(LANGUAGES[0].1, |(_, name)| name);
                groups.push(Group {
                    title: None,
                    rows: vec![
                        Row::Dropdown { id: ID_LANGUAGE, label: t(T::Language).into(), hint: None, value: language.into(), drop: Drop::Language },
                        Row::Choice {
                            id: ID_THEME,
                            label: t(T::Theme).into(),
                            hint: None,
                            options: vec![t(T::ThemeAuto).into(), t(T::ThemeLight).into(), t(T::ThemeDark).into()],
                            selected: ["auto", "light", "dark"].iter().position(|m| *m == s.theme),
                            actions: vec![Action::Theme("auto"), Action::Theme("light"), Action::Theme("dark")],
                        },
                        Row::Switch { id: ID_AUTOSTART, label: t(T::StartWithWindows).into(), hint: None, on: self.autostart, action: Action::Autostart },
                    ],
                });
                groups.push(Group {
                    title: None,
                    rows: vec![
                        Row::Nav { id: ID_CONFIG, glyph: glyph::DOCUMENT, title: t(T::SettingsFile).into(), subtitle: t(T::SettingsFileHint).into(), action: Action::OpenConfig },
                        Row::Nav { id: ID_REVEAL, glyph: glyph::FOLDER, title: t(T::ShowInFolder).into(), subtitle: t(T::ShowInFolderHint).into(), action: Action::RevealConfig },
                        Row::Info { label: t(T::Version).into(), value: format!("{} · SDK 1.8.0", env!("CARGO_PKG_VERSION")) },
                    ],
                });
                groups
            }
        }
    }

    fn render(&mut self) {
        let settings = self.shared.settings();
        let status = self.shared.status();
        let preview = self.shared.preview();
        self.last_seq = preview.seq;
        let p = palette_for(&settings);
        let lang = Lang::from_code(&settings.language);
        let width = WINDOW_WIDTH;
        let height = self.height;
        let page = self.page;
        let view = self.view;
        let groups = self.page_groups(lang, &settings);
        let layout = preview.keyboard.as_ref().map_or_else(|| Layout::wooting_80he(false), Layout::detect);
        let dropdown = self.dropdown.map(|(drop, anchor)| (drop, anchor, dropdown_items(drop, &settings, &self.devices, lang)));
        let menu_hot = self.menu_hot;
        let scroll_target = self.scroll;
        let scale = self.scale();

        self.hits.clear();
        self.canvas.begin();
        self.canvas.clear(p.surface);
        let modal = self.dropdown.is_some();
        let mut ui = Ui {
            scale,
            c: &self.canvas,
            p,
            lang,
            anim: &mut self.anim,
            hits: &mut self.hits,
            hover: self.hover,
            pressed: self.pressed,
            clip: Rect::new(0.0, 0.0, width, height),
            width,
            modal,
        };

        // Header: the mark, the name, and the window buttons.
        let mark = if !settings.enabled { Mark::Paused } else if preview.owner == Owner::Live { Mark::Live } else { Mark::Idle };
        icon::draw_mark(ui.c, 24.0, 16.0, 24.0, mark, seed_for(&settings), p.dark);
        ui.c.text(ui.t(T::AppName), Rect::new(58.0, 0.0, width - 120.0, HEADER_HEIGHT), TextStyle::TitleLarge, p.on_surface, Align::Leading);
        ui.icon_button(ID_MINIMIZE, width - PAGE_GUTTER - 60.0, HEADER_HEIGHT / 2.0, glyph::MINIMIZE, 12.0, Action::Minimize);
        ui.icon_button(ID_CLOSE, width - PAGE_GUTTER - 20.0, HEADER_HEIGHT / 2.0, glyph::CLOSE, 12.0, Action::Close);

        let (card_bottom, live_area) = draw_preview(&mut ui, &preview, &layout, &settings, &status, view, HEADER_HEIGHT);

        // The page scrolls under the pinned card.
        let page_view = Rect::new(0.0, card_bottom + GROUP_GAP, width, height - NAV_HEIGHT);
        let scroll = ui.anim.get((ID_SCROLL, P_POSITION), scroll_target, SPRING_SCROLL);
        ui.clip = page_view;
        ui.c.push_clip(page_view);
        let top = page_view.t - scroll;
        let mut end = ui.groups(&groups, top);
        if page == Page::Settings {
            let r = Rect::new(PAGE_GUTTER, end, width - PAGE_GUTTER, end + BUTTON_HEIGHT);
            let layer = ui.layer(ID_QUIT);
            ui.c.fill_round(r, BUTTON_HEIGHT / 2.0, p.error.alpha(0.12));
            ui.c.fill_round(r, BUTTON_HEIGHT / 2.0, p.error.alpha(layer));
            ui.c.glyph(glyph::POWER, r.cx() - ui.c.measure(ui.t(T::Quit), TextStyle::LabelLarge) / 2.0 - 14.0, r.cy(), 16.0, p.error);
            ui.c.text(ui.t(T::Quit), r.offset(10.0, 0.0), TextStyle::LabelLarge, p.error, Align::Center);
            ui.hit(ID_QUIT, r, BUTTON_HEIGHT / 2.0, Action::Quit, false);
            end += BUTTON_HEIGHT + GROUP_GAP;
        }
        ui.c.pop_clip();
        ui.clip = Rect::new(0.0, 0.0, width, height);
        let content_h = end - top;

        draw_nav(&mut ui, page, height);
        if let Some((drop, anchor, items)) = &dropdown {
            draw_dropdown(&mut ui, *drop, *anchor, items, menu_hot);
        }
        // Keyboard focus ring, following the focused element's shape.
        if self.show_focus {
            if let Some(h) = self.focus.and_then(|f| self.hits.iter().find(|h| h.id == f)) {
                let o = FOCUS_RING_OFFSET + FOCUS_RING_WIDTH / 2.0;
                self.canvas.stroke_round(h.rect.inset(-o, -o), h.radius + o, p.secondary, FOCUS_RING_WIDTH);
            }
        }
        if !self.canvas.end() {
            let _ = self.canvas.attach_dc(self.dpi, true);
        }
        self.page_view = page_view;
        self.content_h = content_h;
        self.live = Some(LiveCache {
            area: live_area,
            owner: preview.owner,
            keyboard: preview.keyboard,
            layout,
            palette: p,
            mode: settings.color_mode(),
            lang,
        });
        self.moving = self.anim.take_moving();
        self.dirty = false;
    }

    /// Redraws just the live view: the keys or the bars changed and nothing else did.
    fn draw_live(&mut self) {
        let preview = self.shared.preview();
        let fresh = |c: &&LiveCache| c.owner == preview.owner && c.keyboard == preview.keyboard && c.area.w() > 0.0;
        let (Some(cache), Some(live)) = (self.live.as_ref().filter(fresh), self.live_canvas.as_ref()) else {
            self.draw_now();
            return;
        };
        self.last_seq = preview.seq;
        let s = self.scale();
        let area = cache.area;
        let px = RECT {
            left: (area.l * s).floor() as i32,
            top: (area.t * s).floor() as i32,
            right: (area.r * s).ceil() as i32,
            bottom: (area.b * s).ceil() as i32,
        };
        let mut no_hits = Vec::new();
        let mut lost = false;
        // SAFETY: draws into the DC of our own window and releases it.
        unsafe {
            let hdc = GetDC(Some(self.hwnd));
            if live.bind_dc_rect(hdc, px) {
                live.begin();
                live.set_offset(-(px.left as f32) / s, -(px.top as f32) / s);
                live.clear(cache.palette.surface_container_high);
                let mut ui = Ui {
                    c: live,
                    p: cache.palette,
                    lang: cache.lang,
                    anim: &mut self.anim,
                    hits: &mut no_hits,
                    hover: None,
                    pressed: None,
                    clip: area,
                    width: WINDOW_WIDTH,
                    scale: s,
                    modal: false,
                };
                draw_live_view(&mut ui, &preview, &cache.layout, cache.mode, self.view, area, None);
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

    fn max_scroll(&self) -> f32 {
        (self.content_h - self.page_view.h()).max(0.0)
    }

    fn focus_step(&mut self, dir: i32) {
        let ids: Vec<u64> = self.hits.iter().filter(|h| h.id != ID_SCRIM).map(|h| h.id).collect();
        if ids.is_empty() {
            return;
        }
        let pos = self.focus.and_then(|f| ids.iter().position(|i| *i == f));
        let n = ids.len() as i32;
        let next = match pos {
            Some(i) => (i as i32 + dir).rem_euclid(n),
            None if dir > 0 => 0,
            None => n - 1,
        };
        self.focus = Some(ids[next as usize]);
        self.show_focus = true;
        self.dirty = true;
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
                let new_picture = self.shared.preview_seq() != self.last_seq;
                // An open dropdown dims the whole window, the live view included.
                if self.dirty || self.moving || (new_picture && self.dropdown.is_some()) {
                    self.draw_now();
                } else if new_picture {
                    self.draw_live();
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
                    self.shared.set_ui_hz(refresh_rate(self.hwnd));
                }
                None
            }
            WM_CAPTURECHANGED => {
                // Capture lost without a button release (Alt+Tab, Win key, hiding the window).
                if lparam.0 != self.hwnd.0 as isize && (self.pressed.is_some() || self.dragging.is_some()) {
                    self.release_pointer();
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
                let over_button = self.hits.iter().any(|h| (h.id == ID_MINIMIZE || h.id == ID_CLOSE) && h.rect.contains(x, y));
                let r = if y < HEADER_HEIGHT && !over_button && self.dropdown.is_none() { HTCAPTION } else { HTCLIENT };
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
                if let (Some(id), Some((kind, _))) = (self.pressed, self.dragging.as_ref()) {
                    let kind = *kind;
                    if let Some(rect) = self.hits.iter().find(|h| h.id == id).map(|h| h.rect) {
                        self.drag(kind, x, rect, false);
                    }
                }
                let hover = self.hit_at(x, y).map(|h| h.id);
                self.menu_hot = hover.filter(|h| *h >= ID_MENU_ITEM).map(|h| (h - ID_MENU_ITEM) as usize);
                if hover != self.hover {
                    self.hover = hover;
                    self.dirty = true;
                }
                none(0)
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                if self.hover.take().is_some() {
                    self.dirty = true;
                }
                none(0)
            }
            WM_LBUTTONDOWN => {
                let (x, y) = self.dip(lparam);
                self.show_focus = false;
                if let Some((id, action, rect)) = self.hit_at(x, y).map(|h| (h.id, h.action.clone(), h.rect)) {
                    self.pressed = Some(id);
                    self.focus = Some(id);
                    // SAFETY: captures the mouse for a press or a drag.
                    unsafe { SetCapture(self.hwnd) };
                    if let Action::Slider(kind) = action {
                        self.dragging = Some((kind, self.shared.settings()));
                        self.drag(kind, x, rect, false);
                    }
                } else if self.dropdown.is_some() {
                    self.dropdown = None;
                }
                self.dirty = true;
                none(0)
            }
            WM_LBUTTONUP => {
                // SAFETY: releases our own capture.
                unsafe {
                    let _ = ReleaseCapture();
                }
                let (x, y) = self.dip(lparam);
                let pressed = self.pressed;
                let dragged = self.dragging.is_some();
                self.release_pointer();
                if dragged {
                    return none(0);
                }
                let hit = self.hit_at(x, y).filter(|h| Some(h.id) == pressed).map(|h| (h.action.clone(), h.rect));
                if let Some((action, rect)) = hit {
                    let after = self.act(action, rect);
                    return Some((LRESULT(0), after));
                }
                none(0)
            }
            WM_MOUSEWHEEL => {
                if self.dropdown.is_none() {
                    let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
                    self.scroll = (self.scroll - delta * WHEEL_STEP).clamp(0.0, self.max_scroll());
                    self.dirty = true;
                }
                none(0)
            }
            WM_KEYDOWN => {
                let key = wparam.0 as u16;
                // SAFETY: plain query.
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                if key == VK_TAB.0 {
                    self.focus_step(if shift { -1 } else { 1 });
                } else if key == VK_ESCAPE.0 {
                    if self.dropdown.take().is_none() {
                        return Some((LRESULT(0), After::Hide));
                    }
                    self.dirty = true;
                } else if key == VK_RETURN.0 || key == VK_SPACE.0 {
                    if let Some((action, rect)) = self.focus.and_then(|f| self.hits.iter().find(|h| h.id == f)).map(|h| (h.action.clone(), h.rect)) {
                        let after = self.act(action, rect);
                        return Some((LRESULT(0), after));
                    }
                } else if key == VK_LEFT.0 || key == VK_RIGHT.0 || key == VK_UP.0 || key == VK_DOWN.0 {
                    let step = if key == VK_LEFT.0 || key == VK_UP.0 { -1.0 } else { 1.0 };
                    let slider = self.focus.and_then(|f| self.hits.iter().find(|h| h.id == f)).and_then(|h| match h.action {
                        Action::Slider(kind) => Some(kind),
                        _ => None,
                    });
                    match slider {
                        Some(kind) if key == VK_LEFT.0 || key == VK_RIGHT.0 => {
                            let s = self.shared.settings();
                            let value = (self.slider_value(&s, kind) + step * 0.05).clamp(0.0, 1.0);
                            let s = self.with_slider(s, kind, value);
                            post(Command::Apply { settings: Box::new(s), save: true });
                            self.dirty = true;
                        }
                        _ => self.focus_step(step as i32),
                    }
                }
                none(0)
            }
            WM_ACTIVATE => {
                if (wparam.0 & 0xFFFF) as u32 == 0 && self.dropdown.take().is_some() {
                    self.dirty = true;
                }
                None
            }
            WM_CLOSE => Some((LRESULT(0), After::Hide)),
            _ => None,
        }
    }
}

/// Key unit, height of the live view, first grid row and number of rows for this layout.
fn view_metrics(layout: &Layout, inner_w: f32) -> (f32, f32, usize, usize) {
    let top_row = layout.keys.iter().map(|k| k.row).min().unwrap_or(0);
    let bottom_row = layout.keys.iter().map(|k| k.row).max().unwrap_or(0);
    let rows = bottom_row - top_row + 1;
    let u = (inner_w / layout.width).min(MAX_KEY_UNIT);
    (u, rows as f32 * u + f_row_gap(top_row, rows), top_row, rows)
}

/// The gap after the function row only exists on boards that have one.
fn f_row_gap(top_row: usize, rows: usize) -> f32 {
    if top_row == 0 && rows >= 6 { F_ROW_GAP } else { 0.0 }
}

/// Draws the pinned card; returns its bottom edge and the area of the live view.
fn draw_preview(ui: &mut Ui, preview: &Preview, layout: &Layout, s: &Settings, status: &Status, view: View, top: f32) -> (f32, Rect) {
    let p = ui.p;
    let inner_w = ui.width - 2.0 * PAGE_GUTTER - 2.0 * CARD_PADDING;
    let (_, area_h, _, _) = view_metrics(layout, inner_w);
    let card_h = CARD_PADDING + CONTROL_HEIGHT + 12.0 + area_h + 12.0 + STATUS_ROW + 14.0;
    let card = Rect::new(PAGE_GUTTER, top, ui.width - PAGE_GUTTER, top + card_h);
    ui.c.fill_round(card, RADIUS_GROUP, p.surface_container_high);
    let left = card.l + CARD_PADDING;
    let right = card.r - CARD_PADDING;

    // Top row: which view, and the master switch.
    let toggle = Rect::new(left, card.t + CARD_PADDING, left + 240.0, card.t + CARD_PADDING + CONTROL_HEIGHT);
    let labels = [ui.t(T::ViewKeyboard).to_string(), ui.t(T::ViewEqualizer).to_string()];
    ui.button_group(ID_VIEW, toggle, &labels, Some((view == View::Equalizer) as usize), &[Action::View(View::Keyboard), Action::View(View::Equalizer)]);
    let sw_cy = toggle.cy();
    ui.switch(ID_ENABLED, right, sw_cy, s.enabled);
    let sw = Rect::new(right - SWITCH_WIDTH, sw_cy - SWITCH_HEIGHT / 2.0, right, sw_cy + SWITCH_HEIGHT / 2.0);
    ui.hit(ID_ENABLED, sw, SWITCH_HEIGHT / 2.0, Action::Toggle(Flag::Enabled), false);

    let area = Rect::new(left, toggle.b + 12.0, right, toggle.b + 12.0 + area_h);
    draw_live_view(ui, preview, layout, s.color_mode(), view, area, Some((s.fmin, s.fmax)));

    // Status line: who drives the lighting, and the keyboard with the audio source.
    let sy = area.b + 12.0;
    let row = Rect::new(left, sy, right, sy + STATUS_ROW);
    let (state, dot) = match (s.enabled, preview.owner) {
        (false, _) => (T::StatePaused, p.outline),
        (true, Owner::Live) => (T::StateLive, p.primary),
        (true, Owner::Idle) => (T::StateIdle, p.tertiary),
        (true, Owner::NoKeyboard) => (T::StateNoKeyboard, p.error),
        (true, _) => (T::StateStarting, p.outline),
    };
    let label = ui.t(state);
    let chip_w = ui.c.measure(label, TextStyle::LabelMedium) + 34.0;
    let chip = Rect::new(row.l, row.t, row.l + chip_w, row.b);
    ui.c.stroke_round(chip.inset(0.5, 0.5), STATUS_ROW / 2.0 - 0.5, p.outline_variant, 1.0);
    ui.c.fill_circle(chip.l + 14.0, chip.cy(), 4.0, dot);
    ui.c.text(label, Rect::new(chip.l + 24.0, chip.t, chip.r - 8.0, chip.b), TextStyle::LabelMedium, p.on_surface, Align::Leading);
    let detail = match (&status.keyboard, &status.audio) {
        (Some(k), Some(a)) => format!("{k} · {a}"),
        (Some(k), None) => format!("{k} · {}", ui.t(T::NoAudio)),
        (None, _) => ui.t(T::ConnectKeyboard).to_string(),
    };
    ui.c.text(&detail, Rect::new(chip.r + 10.0, row.t, row.r, row.b), TextStyle::BodySmall, p.on_surface_variant, Align::Leading);
    // Between full frames only the moving part is redrawn; the frequency axis stays as it is.
    let live = if view == View::Equalizer { Rect::new(area.l, area.t, area.r, area.b - AXIS_HEIGHT) } else { area };
    (card.b, live)
}

/// The keys or the bars; `axis` (fmin, fmax) also draws what does not move: the frequency axis.
fn draw_live_view(ui: &mut Ui, preview: &Preview, layout: &Layout, mode: ColorMode, view: View, area: Rect, axis: Option<(f32, f32)>) {
    match view {
        View::Keyboard => {
            let (u, _, top_row, rows) = view_metrics(layout, area.w());
            draw_keys(ui, layout, preview, area, u, top_row, rows);
        }
        View::Equalizer => {
            let rows = view_metrics(layout, area.w()).3;
            let bars = if axis.is_some() { Rect::new(area.l, area.t, area.r, area.b - AXIS_HEIGHT) } else { area };
            draw_bars(ui, preview, mode, bars, rows);
            if let Some((fmin, fmax)) = axis {
                draw_axis(ui, fmin, fmax, bars, area.b);
            }
        }
    }
}

fn draw_keys(ui: &mut Ui, layout: &Layout, preview: &Preview, area: Rect, u: f32, top_row: usize, rows: usize) {
    let unlit = ui.p.surface_container_highest;
    let live = preview.owner == Owner::Live;
    let f_gap = f_row_gap(top_row, rows);
    let left = area.l + (area.w() - layout.width * u) / 2.0;
    for key in &layout.keys {
        let rel = key.row - top_row;
        let y = area.t + rel as f32 * u + if rel > 0 { f_gap } else { 0.0 };
        let r = Rect::new(left + key.x0 * u + KEY_GAP / 2.0, y + KEY_GAP / 2.0, left + key.x1 * u - KEY_GAP / 2.0, y + u - KEY_GAP / 2.0);
        let mut color = unlit;
        if live && key.row < SDK_ROWS && key.col < SDK_COLS {
            let i = (key.row * SDK_COLS + key.col) * 3;
            let rgb = [preview.frame[i], preview.frame[i + 1], preview.frame[i + 2]];
            let peak = *rgb.iter().max().unwrap_or(&0) as f32;
            if peak > 0.0 {
                // Light on a keycap: the LED's hue at full strength, blended by its brightness.
                let k = 255.0 / peak;
                let led = Color::rgb8((rgb[0] as f32 * k) as u8, (rgb[1] as f32 * k) as u8, (rgb[2] as f32 * k) as u8);
                color = unlit.mix(led, peak / 255.0);
            }
        }
        ui.c.fill_round(r, RADIUS_EXTRA_SMALL, color);
    }
}

/// A plain spectrum view of the same bars the keyboard shows, with its key rows as guides.
fn draw_bars(ui: &mut Ui, preview: &Preview, mode: ColorMode, bars_area: Rect, rows: usize) {
    let p = ui.p;
    // One guide per key row, so a bar crossing a line lights the next row up.
    for level in 1..rows {
        let y = bars_area.b - bars_area.h() * level as f32 / rows as f32;
        ui.c.fill_rect(Rect::new(bars_area.l, y - 0.5, bars_area.r, y + 0.5), p.outline_variant.alpha(0.45));
    }
    let n = preview.bars.len().max(1);
    let w = (bars_area.w() - BAR_GAP * (n - 1) as f32) / n as f32;
    let playing = matches!(preview.owner, Owner::Live | Owner::Idle | Owner::NoKeyboard);
    let radius = RADIUS_EXTRA_SMALL.min(w / 2.0);
    let column = |i: usize| bars_area.l + i as f32 * (w + BAR_GAP);
    for i in 0..n {
        ui.c.fill_round(Rect::new(column(i), bars_area.t, column(i) + w, bars_area.b), radius, p.surface_container_highest.alpha(0.5));
    }
    let filled = |i: usize, bar: f32| {
        let h = if playing { bar.clamp(0.0, 1.0) } else { 0.0 };
        (h > 0.002).then(|| Rect::new(column(i), bars_area.b - h * bars_area.h(), column(i) + w, bars_area.b))
    };
    match mode {
        // Color by band: each bar is one color.
        ColorMode::Rainbow | ColorMode::Flow => {
            for (i, bar) in preview.bars.iter().enumerate() {
                if let Some(rect) = filled(i, *bar) {
                    ui.c.fill_round(rect, radius, rgb_color(color_at(mode, 0.0, i, n, preview.hue_offset)));
                }
            }
        }
        // Color by height, as the keys get it from their row: one gradient for every bar.
        _ => {
            let rects: Vec<Rect> = preview.bars.iter().enumerate().filter_map(|(i, b)| filled(i, *b)).collect();
            let colors: Vec<Color> = (0..=8).map(|k| rgb_color(color_at(mode, 1.0 - k as f32 / 8.0, 0, n, 0.0))).collect();
            ui.c.fill_rounds_vertical(&rects, radius, bars_area.t, bars_area.b, &colors);
        }
    }
}

/// Frequency labels on the same log scale as the bands.
fn draw_axis(ui: &mut Ui, fmin: f32, fmax: f32, bars_area: Rect, bottom: f32) {
    let span = (fmax / fmin).ln();
    for f in AXIS_TICKS.iter().filter(|f| **f >= fmin * 1.15 && **f <= fmax / 1.15) {
        let x = bars_area.l + (f / fmin).ln() / span * bars_area.w();
        ui.c.text(&fmt_hz(ui.lang, *f, true), Rect::new(x - 24.0, bars_area.b + 2.0, x + 24.0, bottom), TextStyle::BodySmall, ui.p.on_surface_variant, Align::Center);
    }
}

fn draw_nav(ui: &mut Ui, page: Page, height: f32) {
    let p = ui.p;
    let bar = Rect::new(0.0, height - NAV_HEIGHT, ui.width, height);
    ui.c.fill_rect(bar, p.surface_container);
    let items = [(Page::Look, glyph::PALETTE, T::NavLook), (Page::Sound, glyph::EQUALIZER, T::NavSound), (Page::Settings, glyph::SETTINGS, T::NavSettings)];
    let w = ui.width / items.len() as f32;
    for (i, (target, g, label)) in items.iter().enumerate() {
        let id = ID_NAV + i as u64;
        let cell = Rect::new(i as f32 * w, bar.t, (i + 1) as f32 * w, bar.b);
        let open = ui.anim.get((id, P_SELECTED), (page == *target) as u8 as f32, SPRING_FAST_SPATIAL).max(0.0);
        let pill_w = 56.0 * open.min(1.2);
        let pill = Rect::new(cell.cx() - pill_w / 2.0, bar.t + 6.0, cell.cx() + pill_w / 2.0, bar.t + 38.0);
        ui.c.fill_round(pill, 16.0, p.secondary_container.alpha(open.min(1.0)));
        let layer = ui.layer(id);
        ui.c.fill_round(Rect::new(cell.cx() - 28.0, bar.t + 6.0, cell.cx() + 28.0, bar.t + 38.0), 16.0, p.on_surface.alpha(layer));
        let mix = open.clamp(0.0, 1.0);
        ui.c.glyph(*g, cell.cx(), pill.cy(), ICON_SIZE, p.on_surface_variant.mix(p.on_secondary_container, mix));
        ui.c.text(ui.t(*label), Rect::new(cell.l, bar.t + 40.0, cell.r, bar.t + 58.0), TextStyle::LabelMedium, p.on_surface_variant.mix(p.on_surface, mix), Align::Center);
        ui.hit(id, Rect::new(cell.l, bar.t, cell.r, bar.b), 16.0, Action::Page(*target), false);
    }
}

/// Items of a dropdown menu and the index of the current one.
fn dropdown_items(drop: Drop, s: &Settings, devices: &[String], lang: Lang) -> (Vec<String>, Option<usize>) {
    match drop {
        Drop::Range => (
            RANGES.iter().map(|(a, b)| fmt_range(lang, *a, *b)).collect(),
            RANGES.iter().position(|(a, b)| (a - s.fmin).abs() < 0.5 && (b - s.fmax).abs() < 0.5),
        ),
        Drop::Device => {
            let mut items = vec![tr(lang, T::WindowsDefault).to_string()];
            items.extend(devices.iter().cloned());
            let current = if s.audio_device.is_empty() { Some(0) } else { devices.iter().position(|d| *d == s.audio_device).map(|i| i + 1) };
            (items, current)
        }
        Drop::Language => (
            LANGUAGES.iter().map(|(_, name)| name.to_string()).collect(),
            LANGUAGES.iter().position(|(code, _)| *code == s.language),
        ),
    }
}

fn draw_dropdown(ui: &mut Ui, drop: Drop, anchor: Rect, items: &(Vec<String>, Option<usize>), hot: Option<usize>) {
    let p = ui.p;
    let (labels, current) = items;
    ui.c.fill_rect(Rect::new(0.0, 0.0, ui.width, ui.clip.b), Color::rgb8(0, 0, 0).alpha(SCRIM_OPACITY));
    ui.hit(ID_SCRIM, Rect::new(0.0, 0.0, ui.width, ui.clip.b), 0.0, Action::Dismiss, true);
    let widest = labels.iter().map(|l| ui.c.measure(l, TextStyle::BodyLarge)).fold(0.0, f32::max);
    let w = (widest + 16.0 + 44.0).clamp(200.0, ui.width - 2.0 * PAGE_GUTTER);
    let h = 16.0 + labels.len() as f32 * 40.0;
    let x = (anchor.r - w).clamp(PAGE_GUTTER, ui.width - PAGE_GUTTER - w);
    let below = anchor.b + 4.0;
    let y = if below + h <= ui.clip.b - 8.0 { below } else { (anchor.t - 4.0 - h).max(8.0) };
    let menu = Rect::xywh(x, y, w, h);
    ui.c.fill_round(menu, RADIUS_MENU, p.surface_container_highest);
    for (i, label) in labels.iter().enumerate() {
        let row = Rect::new(menu.l + 6.0, menu.t + 8.0 + i as f32 * 40.0, menu.r - 6.0, menu.t + 8.0 + (i + 1) as f32 * 40.0);
        if *current == Some(i) {
            ui.c.fill_round(row, RADIUS_MENU_ITEM, p.secondary_container);
        }
        if hot == Some(i) {
            ui.c.fill_round(row, RADIUS_MENU_ITEM, p.on_surface.alpha(HOVER_OPACITY));
        }
        let color = if *current == Some(i) { p.on_secondary_container } else { p.on_surface };
        ui.c.text(label, Rect::new(row.l + 10.0, row.t, row.r - 36.0, row.b), TextStyle::BodyLarge, color, Align::Leading);
        if *current == Some(i) {
            ui.c.glyph(glyph::CHECK, row.r - 18.0, row.cy(), 16.0, p.on_secondary_container);
        }
        ui.hit(ID_MENU_ITEM + i as u64, row, RADIUS_MENU_ITEM, Action::Pick(drop, i), true);
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
                                    w.dirty = true;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(color: &str, custom: &str, saved: &[&str]) -> Settings {
        Settings {
            color: color.into(),
            custom_color: custom.into(),
            saved_colors: saved.iter().map(|c| c.to_string()).collect(),
            ..Settings::default()
        }
    }

    #[test]
    fn presets_follow_the_requested_order() {
        let order: Vec<&str> = COLORS.iter().map(|(code, _)| *code).collect();
        assert_eq!(&order[..6], ["classic", "rainbow", "flow", "#ffffff", "#00ffaa", "#9020ff"]);
        assert!(order[6..].iter().all(|code| code.starts_with("#c8c8c8>")));
        assert_eq!(COLORS.len() % SWATCH_COLUMNS, 0, "presets fill whole rows");
    }

    #[test]
    fn custom_wins_then_saved_then_presets() {
        let s = settings("#00ffaa", "#00ffaa", &["#00ffaa"]);
        assert_eq!(selected_swatch(&s, None), CUSTOM);
        let s = settings("#00ffaa", "#123456", &["#00ffaa"]);
        assert_eq!(selected_swatch(&s, None), SAVED);
        let s = settings("#00ffaa", "#123456", &[]);
        assert_eq!(selected_swatch(&s, None), 4);
        let s = settings("#abcdef", "#123456", &[]);
        assert_eq!(selected_swatch(&s, None), CUSTOM);
    }

    #[test]
    fn picked_swatch_stays_selected_while_it_matches() {
        let s = settings("#00ffaa", "#00ffaa", &["#00ffaa"]);
        assert_eq!(selected_swatch(&s, Some(SAVED)), SAVED);
        assert_eq!(selected_swatch(&s, Some(4)), 4);
        // A pick that no longer matches falls back to the usual order.
        assert_eq!(selected_swatch(&s, Some(0)), CUSTOM);
        assert_eq!(selected_swatch(&s, Some(SAVED + 5)), CUSTOM);
    }

    #[test]
    fn editing_changes_only_the_edited_swatch() {
        let mut s = settings("#c8c8c8>#ff0000", "#111111", &["#c8c8c8>#ff0000", "#222222"]);
        set_own_color(&mut s, SAVED, "#c8c8c8>#00ff00".into());
        assert_eq!(s.saved_colors, ["#c8c8c8>#00ff00", "#222222"]);
        assert_eq!((s.color.as_str(), s.custom_color.as_str()), ("#c8c8c8>#00ff00", "#111111"));
        set_own_color(&mut s, 3, "#ffffff".into());
        set_own_color(&mut s, SAVED + 9, "#ffffff".into());
        assert_eq!(s.color, "#c8c8c8>#00ff00", "presets and missing swatches stay untouched");
    }

    #[test]
    fn saving_stops_at_duplicates_and_the_limit() {
        assert_eq!(save_state(&settings("x", "#123456", &[])), SaveState::Ready);
        assert_eq!(save_state(&settings("x", "#123456", &["#123456"])), SaveState::Saved);
        let full: Vec<String> = (0..MAX_SAVED_COLORS).map(|i| format!("#0000{i:02x}")).collect();
        let full: Vec<&str> = full.iter().map(String::as_str).collect();
        assert_eq!(save_state(&settings("x", "#123456", &full)), SaveState::Full);
    }

    #[test]
    fn only_low_contrast_swatches_get_an_outline() {
        let dark_tile = Color::rgb8(0x2b, 0x29, 0x30);
        let light_tile = Color::rgb8(0xf3, 0xed, 0xf7);
        assert!(contrast(Color::rgb8(255, 255, 255), light_tile) < SWATCH_MIN_CONTRAST);
        assert!(contrast(Color::rgb8(255, 255, 255), dark_tile) > SWATCH_MIN_CONTRAST);
        assert!(contrast(Color::rgb8(0, 0, 255), dark_tile) > SWATCH_MIN_CONTRAST);
        assert!(contrast(Color::rgb8(20, 20, 20), dark_tile) < SWATCH_MIN_CONTRAST);
    }
}
