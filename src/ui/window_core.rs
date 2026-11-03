//! The main window without its platform: the live view pinned on top (the keyboard or a plain
//! equalizer), then pages for look, sound and settings. Drawn immediate-mode: every frame lays out
//! the page and registers what can be clicked. The Win32 and AppKit hosts own the native window
//! and feed it points (DIPs) through the input interface: `pointer_*`, `wheel`, `key`, `frame`
//! and `render`.

use std::sync::Arc;

use super::{
    Command,
    canvas::{Align, Canvas, Rect, TextStyle},
    design::*,
    glyph,
    i18n::{Lang, T, effect_label, tr},
    icon::{self, Mark},
    palette::Palette,
    palette_for, post, seed_for,
    spring::Animator,
    update_label,
    wants_dark,
    widgets::*,
};
#[cfg(windows)]
use crate::engine::Preview;
use crate::{
    autostart,
    effects::Effect,
    engine::{Owner, Shared, Status},
    layout::Layout,
    render::{hsv_to_rgb, rgb_to_hs},
    sdk::KeyboardInfo,
    settings::{ColorMode, MAX_SAVED_COLORS, Settings, parse_color_mode},
    update,
};

pub(super) const KEY_GAP: f32 = 2.0;
pub(super) const F_ROW_GAP: f32 = 6.0;
pub(super) const MAX_KEY_UNIT: f32 = 30.0;
pub(super) const CARD_PADDING: f32 = 16.0;
pub(super) const STATUS_ROW: f32 = 24.0;
pub(super) const WHEEL_STEP: f32 = 56.0;
pub(super) const BAR_GAP: f32 = 3.0;
pub(super) const AXIS_HEIGHT: f32 = 18.0;
pub(super) const COLOR_TRACK: f32 = 12.0;
pub(super) const COLOR_EDIT_HEIGHT: f32 = 106.0;

// Element ids; springs and hits are keyed by them.
pub(super) const ID_MINIMIZE: u64 = 1;
pub(super) const ID_CLOSE: u64 = 2;
pub(super) const ID_ENABLED: u64 = 3;
pub(super) const ID_SCRIM: u64 = 4;
pub(super) const ID_SCROLL: u64 = 5;
pub(super) const ID_VIEW: u64 = 6;
pub(super) const ID_NAV: u64 = 10;
pub(super) const ID_SWATCH: u64 = 100;
pub(super) const ID_FILL: u64 = 200;
pub(super) const ID_COLOR_EDIT: u64 = 210;
pub(super) const ID_PRESET: u64 = 220;
pub(super) const ID_BRIGHTNESS: u64 = 230;
pub(super) const ID_SENSITIVITY: u64 = 240;
pub(super) const ID_RANGE: u64 = 250;
pub(super) const ID_EQUALIZE: u64 = 251;
pub(super) const ID_DEVICE: u64 = 252;
pub(super) const ID_RELEASE: u64 = 253;
pub(super) const ID_MEDIA_ONLY: u64 = 254;
pub(super) const ID_LANGUAGE: u64 = 260;
pub(super) const ID_THEME: u64 = 270;
pub(super) const ID_AUTOSTART: u64 = 280;
pub(super) const ID_CONFIG: u64 = 281;
pub(super) const ID_REVEAL: u64 = 282;
pub(super) const ID_QUIT: u64 = 283;
pub(super) const ID_LOG: u64 = 284;
pub(super) const ID_BACKUP: u64 = 285;
pub(super) const ID_RESTORE: u64 = 286;
pub(super) const ID_EFFECT: u64 = 290;
pub(super) const ID_EFFECT_WHEN: u64 = 292;
pub(super) const ID_SNAKE_PLAY: u64 = 296;
pub(super) const ID_EFFECT_AFTER: u64 = 300;
pub(super) const ID_EFFECT_COLORS: u64 = 310;
pub(super) const ID_EFFECT_FILL: u64 = 320;
pub(super) const ID_EFFECT_COLOR_EDIT: u64 = 330;
pub(super) const ID_UPDATES: u64 = 340;
pub(super) const ID_UPDATE_ACTION: u64 = 345;
pub(super) const ID_MENU_ITEM: u64 = 1000;

// Spring properties.
pub(super) const P_LAYER: u8 = 0;
pub(super) const P_CORNER: u8 = 1;
pub(super) const P_POSITION: u8 = 2;
pub(super) const P_SIZE: u8 = 3;
pub(super) const P_SELECTED: u8 = 4;

pub(super) const COLORS: [(&str, T); 12] = [
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
pub(super) const CUSTOM: usize = COLORS.len();
/// The first saved custom color; saved colors follow the custom swatch.
pub(super) const SAVED: usize = CUSTOM + 1;
pub(super) const SWATCH_COLUMNS: usize = 6;
pub(super) const SWATCH_GAP: f32 = 12.0;
/// Below this contrast with the tile a swatch gets an outline.
pub(super) const SWATCH_MIN_CONTRAST: f32 = 1.5;
pub(super) const BASE_WHITE: [f32; 3] = [200.0, 200.0, 200.0];
/// From the narrowest to the widest top frequency.
pub(super) const RANGES: [(f32, f32); 4] = [(20.0, 5000.0), (30.0, 10000.0), (40.0, 16000.0), (20.0, 20000.0)];
/// (range_db, headroom_db) for low, medium and high sensitivity.
pub(super) const SENSITIVITY: [(f32, f32, T); 3] =
    [(36.0, 8.0, T::SensitivityLow), (30.0, 3.0, T::SensitivityMedium), (24.0, 0.0, T::SensitivityHigh)];
pub(super) const LANGUAGES: [(&str, &str); 2] = [("en", "English"), ("uk", "Українська")];
/// Frequencies labeled under the equalizer view.
pub(super) const AXIS_TICKS: [f32; 9] = [50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0];
/// Idle times an effect can wait for, in seconds.
pub(super) const EFFECT_WAITS: [f32; 4] = [30.0, 60.0, 120.0, 300.0];
/// Values of `updates`, in the order of the buttons.
pub(super) const UPDATE_MODES: [(&str, T); 3] = [("auto", T::UpdatesAuto), ("notify", T::UpdatesNotify), ("off", T::UpdatesOff)];
/// Values of `effect_color`, in the order of the buttons.
pub(super) const EFFECT_COLOR_MODES: [(&str, T); 3] = [("natural", T::EffectNatural), ("scheme", T::EffectScheme), ("custom", T::EffectCustom)];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Page {
    Look,
    Sound,
    Effects,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum View {
    Keyboard,
    Equalizer,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Flag {
    Enabled,
    Equalize,
    Release,
    SnakePlay,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Drop {
    Range,
    Device,
    Language,
    Effect,
}

/// Which color of the custom scheme a control edits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Part {
    Solid = 0,
    Top = 1,
    Bottom = 2,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum SliderKind {
    Brightness,
    Hue(Part),
    Saturation(Part),
    /// The same for the effects' own color.
    EffectHue(Part),
    EffectSaturation(Part),
}

#[derive(Clone, Debug)]
pub(super) enum Action {
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
    OpenLog,
    BackUp,
    Restore,
    Quit,
    /// Shows the effect always (true) or only when idle.
    EffectWhen(bool),
    /// Seconds of idle time before the effect starts.
    EffectAfter(f32),
    EffectColors(&'static str),
    /// Gradient (true) or solid effect color.
    EffectFill(bool),
    Updates(&'static str),
    CheckUpdates,
    InstallUpdate,
}

pub(super) struct Hit {
    pub(super) id: u64,
    pub(super) rect: Rect,
    pub(super) radius: f32,
    pub(super) action: Action,
}

/// The user's own color scheme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Custom {
    Solid([f32; 3]),
    Gradient { bottom: [f32; 3], top: [f32; 3] },
}

impl Custom {
    pub(super) fn parse(code: &str) -> Self {
        match parse_color_mode(code) {
            ColorMode::Solid(c) => Custom::Solid(c),
            ColorMode::Gradient(bottom, top) => Custom::Gradient { bottom, top },
            _ => Custom::Gradient { bottom: BASE_WHITE, top: [255.0, 176.0, 0.0] },
        }
    }

    pub(super) fn code(self) -> String {
        let hex = |c: [f32; 3]| format!("#{:02x}{:02x}{:02x}", c[0].round() as u8, c[1].round() as u8, c[2].round() as u8);
        match self {
            Custom::Solid(c) => hex(c),
            Custom::Gradient { bottom, top } => format!("{}>{}", hex(bottom), hex(top)),
        }
    }

    pub(super) fn part(self, part: Part) -> [f32; 3] {
        match (self, part) {
            (Custom::Solid(c), _) => c,
            (Custom::Gradient { bottom, .. }, Part::Bottom) => bottom,
            (Custom::Gradient { top, .. }, _) => top,
        }
    }

    pub(super) fn with_part(self, part: Part, rgb: [f32; 3]) -> Self {
        match (self, part) {
            (Custom::Solid(_), _) => Custom::Solid(rgb),
            (Custom::Gradient { top, .. }, Part::Bottom) => Custom::Gradient { bottom: rgb, top },
            (Custom::Gradient { bottom, .. }, _) => Custom::Gradient { bottom, top: rgb },
        }
    }
}

/// Code of swatch `index`: a preset, the custom color, then the saved colors.
pub(super) fn swatch_code(s: &Settings, index: usize) -> Option<&str> {
    match index {
        i if i < CUSTOM => Some(COLORS[i].0),
        CUSTOM => Some(&s.custom_color),
        i => s.saved_colors.get(i - SAVED).map(String::as_str),
    }
}

/// The swatch of the current color: `picked` while it still matches, otherwise the custom color,
/// a saved color or a preset, in that order. A color matching nothing shows as custom.
pub(super) fn selected_swatch(s: &Settings, picked: Option<usize>) -> usize {
    let mode = s.color_mode();
    let matches = |i: usize| swatch_code(s, i).is_some_and(|code| parse_color_mode(code) == mode);
    picked
        .filter(|&i| matches(i))
        .or_else(|| (CUSTOM..SAVED + s.saved_colors.len()).chain(0..CUSTOM).find(|&i| matches(i)))
        .unwrap_or(CUSTOM)
}

/// Stores `code` in the custom or saved swatch `index` and makes it the current color.
pub(super) fn set_own_color(s: &mut Settings, index: usize, code: String) {
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
pub(super) enum SaveState {
    Ready,
    Saved,
    Full,
}

/// Whether the custom color can be added to the saved colors.
pub(super) fn save_state(s: &Settings) -> SaveState {
    let mode = parse_color_mode(&s.custom_color);
    if s.saved_colors.iter().any(|code| parse_color_mode(code) == mode) {
        SaveState::Saved
    } else if s.saved_colors.len() >= MAX_SAVED_COLORS {
        SaveState::Full
    } else {
        SaveState::Ready
    }
}

/// What a live-only redraw reuses from the last full frame (Windows; macOS redraws in full).
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) struct LiveCache {
    pub(super) area: Rect,
    pub(super) owner: Owner,
    pub(super) keyboard: Option<KeyboardInfo>,
    pub(super) layout: Layout,
    pub(super) palette: Palette,
    pub(super) mode: ColorMode,
    pub(super) lang: Lang,
}

/// Everything besides the live view that the window shows; it redraws when this changes.
#[derive(PartialEq)]
pub(super) struct Shown {
    settings: u64,
    status: Status,
    dark: bool,
    autostart: bool,
    config_error: Option<String>,
    update: update::State,
}

/// What the host does with its native window after an input.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum After {
    None,
    Hide,
    Minimize,
}

/// Keys the window reacts to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Key {
    Tab,
    Escape,
    Enter,
    Space,
    Left,
    Right,
    Up,
    Down,
}

/// What a display refresh should draw.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Frame {
    /// Nothing changed.
    None,
    /// Only the live view has a new picture.
    Live,
    /// Everything.
    Full,
}

/// The window's state; the host owns the native window, the canvases and the frame clock.
pub(super) struct WindowCore {
    pub(super) live: Option<LiveCache>,
    shown: Option<Shown>,
    autostart: bool,
    /// Window height in DIPs; the width is `WINDOW_WIDTH`.
    pub(super) height: f32,
    pub(super) shared: Arc<Shared>,
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
    pub(super) last_seq: u64,
    pub(super) dirty: bool,
    moving: bool,
    /// A slider being dragged and the settings it edits, saved on release.
    dragging: Option<(SliderKind, Settings)>,
    /// Last hue per custom part, kept while its saturation is zero.
    hues: [f32; 3],
    /// The same for the effects' own color.
    effect_hues: [f32; 3],
    /// The swatch picked or edited last: it stays selected while the color matches it, even when
    /// another swatch has the same color.
    picked: Option<usize>,
    /// The header draws minimize and close buttons; off where the system draws its own.
    pub(super) window_buttons: bool,
    /// Room left of the header's mark, for buttons the system draws there.
    pub(super) header_inset: f32,
}

impl WindowCore {
    pub(super) fn new(shared: Arc<Shared>, devices: Vec<String>, height: f32) -> Self {
        Self {
            live: None,
            shown: None,
            autostart: autostart::is_enabled(),
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
            last_seq: u64::MAX,
            dirty: true,
            moving: false,
            dragging: None,
            hues: [0.45; 3],
            effect_hues: [0.6; 3],
            picked: None,
            window_buttons: true,
            header_inset: 0.0,
        }
    }

    pub(super) fn set_devices(&mut self, devices: Vec<String>) {
        self.devices = devices;
        self.dirty = true;
    }

    /// Redraws on the next frame if anything shown outside the live view changed.
    pub(super) fn refresh(&mut self, config_error: Option<String>) {
        let settings = self.shared.settings();
        let shown = Shown {
            settings: self.shared.settings_version(),
            status: self.shared.status(),
            dark: wants_dark(&settings),
            autostart: autostart::is_enabled(),
            config_error,
            update: update::state(),
        };
        if self.shown.as_ref() != Some(&shown) {
            self.autostart = shown.autostart;
            self.config_error = shown.config_error.clone();
            self.shown = Some(shown);
            self.dirty = true;
        }
    }

    /// What the next display refresh draws.
    pub(super) fn frame(&self) -> Frame {
        let new_picture = self.shared.preview_seq() != self.last_seq;
        // An open dropdown dims the whole window, the live view included.
        if self.dirty || self.moving || (new_picture && self.dropdown.is_some()) {
            Frame::Full
        } else if new_picture {
            Frame::Live
        } else {
            Frame::None
        }
    }

    /// Whether a press at this point drags the window: the header outside its buttons.
    pub(super) fn is_drag_area(&self, x: f32, y: f32) -> bool {
        let over_button = self.hits.iter().any(|h| (h.id == ID_MINIMIZE || h.id == ID_CLOSE) && h.rect.contains(x, y));
        y < HEADER_HEIGHT && !over_button && self.dropdown.is_none()
    }

    pub(super) fn pointer_move(&mut self, x: f32, y: f32) {
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
    }

    /// The pointer left the window.
    pub(super) fn pointer_leave(&mut self) {
        if self.hover.take().is_some() {
            self.dirty = true;
        }
    }

    /// A primary button press; true if it pressed an element, so the host captures the pointer.
    pub(super) fn pointer_down(&mut self, x: f32, y: f32) -> bool {
        self.show_focus = false;
        let mut pressed = false;
        if let Some((id, action, rect)) = self.hit_at(x, y).map(|h| (h.id, h.action.clone(), h.rect)) {
            self.pressed = Some(id);
            self.focus = Some(id);
            pressed = true;
            if let Action::Slider(kind) = action {
                self.dragging = Some((kind, self.shared.settings()));
                self.drag(kind, x, rect, false);
            }
        } else if self.dropdown.is_some() {
            self.dropdown = None;
        }
        self.dirty = true;
        pressed
    }

    /// A primary button release: a click if it ends where the press started.
    pub(super) fn pointer_up(&mut self, x: f32, y: f32) -> After {
        let pressed = self.pressed;
        let dragged = self.dragging.is_some();
        self.release_pointer();
        if dragged {
            return After::None;
        }
        let hit = self.hit_at(x, y).filter(|h| Some(h.id) == pressed).map(|h| (h.action.clone(), h.rect));
        if let Some((action, rect)) = hit {
            return self.act(action, rect);
        }
        After::None
    }

    /// The pointer capture was lost without a release (app switch, the window hidden).
    pub(super) fn pointer_cancel(&mut self) {
        if self.pressed.is_some() || self.dragging.is_some() {
            self.release_pointer();
        }
    }

    /// Scrolls the page by `dy` DIPs; positive moves towards the top, like a wheel turned up.
    pub(super) fn wheel(&mut self, dy: f32) {
        if self.dropdown.is_none() {
            self.scroll = (self.scroll - dy).clamp(0.0, self.max_scroll());
            self.dirty = true;
        }
    }

    /// Scrolls by `dy` DIPs without the spring: a trackpad already moves smoothly.
    #[cfg_attr(windows, allow(dead_code))]
    pub(super) fn scroll_precise(&mut self, dy: f32) {
        if self.dropdown.is_none() {
            self.scroll = (self.scroll - dy).clamp(0.0, self.max_scroll());
            self.anim.set((ID_SCROLL, P_POSITION), self.scroll);
            self.dirty = true;
        }
    }

    pub(super) fn key(&mut self, key: Key, shift: bool) -> After {
        match key {
            Key::Tab => self.focus_step(if shift { -1 } else { 1 }),
            Key::Escape => {
                if self.dropdown.take().is_none() {
                    return After::Hide;
                }
                self.dirty = true;
            }
            Key::Enter | Key::Space => {
                if let Some((action, rect)) = self.focus.and_then(|f| self.hits.iter().find(|h| h.id == f)).map(|h| (h.action.clone(), h.rect)) {
                    return self.act(action, rect);
                }
            }
            Key::Left | Key::Right | Key::Up | Key::Down => {
                let step = if matches!(key, Key::Left | Key::Up) { -1.0 } else { 1.0 };
                let slider = self.focus.and_then(|f| self.hits.iter().find(|h| h.id == f)).and_then(|h| match h.action {
                    Action::Slider(kind) => Some(kind),
                    _ => None,
                });
                match slider {
                    Some(kind) if matches!(key, Key::Left | Key::Right) => {
                        let s = self.shared.settings();
                        let value = (self.slider_value(&s, kind) + step * 0.05).clamp(0.0, 1.0);
                        let s = self.with_slider(s, kind, value);
                        post(Command::Apply { settings: Box::new(s), save: true });
                        self.dirty = true;
                    }
                    _ => self.focus_step(step as i32),
                }
            }
        }
        After::None
    }

    /// The window lost activation: an open menu closes.
    pub(super) fn deactivate(&mut self) {
        if self.dropdown.take().is_some() {
            self.dirty = true;
        }
    }

    /// The live view area of the last full frame while a live-only redraw can reuse it.
    #[cfg(windows)]
    pub(super) fn live_area(&self, preview: &Preview) -> Option<Rect> {
        let fresh = |c: &&LiveCache| c.owner == preview.owner && c.keyboard == preview.keyboard && c.area.w() > 0.0;
        self.live.as_ref().filter(fresh).map(|c| c.area)
    }

    /// Draws just the live view (the keys or the bars) over its background; `live_area` must be
    /// `Some`. `scale` is device pixels per DIP.
    #[cfg(windows)]
    pub(super) fn render_live(&mut self, c: &Canvas, preview: &Preview, scale: f32) {
        let Some(cache) = self.live.as_ref() else { return };
        let area = cache.area;
        let mut no_hits = Vec::new();
        c.clear(cache.palette.surface_container_high);
        let mut ui = Ui {
            c,
            p: cache.palette,
            lang: cache.lang,
            anim: &mut self.anim,
            hits: &mut no_hits,
            hover: None,
            pressed: None,
            clip: area,
            width: WINDOW_WIDTH,
            scale,
            modal: false,
        };
        draw_live_view(&mut ui, preview, &cache.layout, cache.mode, self.view, area, None);
    }

    pub(super) fn hit_at(&self, x: f32, y: f32) -> Option<&Hit> {
        self.hits.iter().rev().find(|h| h.rect.contains(x, y))
    }

    pub(super) fn update_settings(&self, f: impl FnOnce(&mut Settings)) {
        let mut s = self.shared.settings();
        f(&mut s);
        post(Command::Apply { settings: Box::new(s), save: true });
    }

    /// Hue and saturation of a custom color part, keeping the hue while the color is white.
    pub(super) fn hue_sat(&self, rgb: [f32; 3], part: Part) -> (f32, f32) {
        hue_sat_with(&self.hues, rgb, part)
    }

    /// The same for the effects' own color.
    pub(super) fn effect_hue_sat(&self, rgb: [f32; 3], part: Part) -> (f32, f32) {
        hue_sat_with(&self.effect_hues, rgb, part)
    }

    /// The selected swatch and its color, as the color editor shows them.
    pub(super) fn edited(&self, s: &Settings) -> (usize, Custom) {
        let index = selected_swatch(s, self.picked);
        (index, Custom::parse(swatch_code(s, index).unwrap_or_default()))
    }

    pub(super) fn act(&mut self, action: Action, rect: Rect) -> After {
        self.dirty = true;
        match action {
            Action::Page(page) => {
                if self.page != page {
                    self.page = page;
                    self.scroll = 0.0;
                    self.anim.set((ID_SCROLL, P_POSITION), 0.0);
                    self.shared.set_effect_preview(page == Page::Effects);
                }
            }
            Action::View(view) => self.view = view,
            Action::Minimize => return After::Minimize,
            Action::Close => return After::Hide,
            Action::Toggle(flag) => match flag {
                Flag::Enabled => post(Command::ToggleEnabled),
                Flag::Equalize => self.update_settings(|s| s.equalize = if s.equalize > 0.0 { 0.0 } else { 1.0 }),
                Flag::Release => self.update_settings(|s| s.release_when_silent = !s.release_when_silent),
                Flag::SnakePlay => self.update_settings(|s| s.snake_play = !s.snake_play),
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
                    Drop::Effect => {
                        let code = index.checked_sub(1).and_then(|i| Effect::ALL.get(i)).map_or("off", |e| e.code());
                        self.update_settings(|s| s.effect = code.into());
                    }
                }
            }
            Action::Dismiss => self.dropdown = None,
            Action::Slider(_) => {}
            Action::Autostart => post(Command::ToggleAutostart),
            Action::OpenConfig => post(Command::OpenConfig),
            Action::RevealConfig => post(Command::RevealConfig),
            Action::OpenLog => post(Command::OpenLog),
            Action::BackUp => post(Command::BackUpSettings),
            Action::Restore => post(Command::RestoreSettings),
            Action::Quit => post(Command::Quit),
            Action::EffectWhen(always) => self.update_settings(|s| s.effect_when = if always { "always" } else { "idle" }.into()),
            Action::EffectAfter(secs) => self.update_settings(|s| s.effect_after_sec = secs),
            Action::EffectColors(code) => self.update_settings(|s| s.effect_color = code.into()),
            Action::Updates(mode) => {
                self.update_settings(|s| s.updates = mode.into());
                if mode != "off" {
                    post(Command::CheckUpdates);
                }
            }
            Action::CheckUpdates => post(Command::CheckUpdates),
            Action::InstallUpdate => post(Command::InstallUpdate),
            Action::EffectFill(gradient) => self.update_settings(|s| {
                let custom = match (Custom::parse(&s.effect_custom_color), gradient) {
                    (Custom::Solid(c), true) => Custom::Gradient { bottom: c, top: [255.0; 3] },
                    (Custom::Gradient { bottom, .. }, false) => Custom::Solid(bottom),
                    (c, _) => c,
                };
                s.effect_custom_color = custom.code();
            }),
        }
        After::None
    }

    /// Applies a slider at pointer `x`; `save` writes the settings file (on release).
    pub(super) fn drag(&mut self, kind: SliderKind, x: f32, rect: Rect, save: bool) {
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
    pub(super) fn release_pointer(&mut self) {
        self.pressed = None;
        if let Some((_, s)) = self.dragging.take() {
            post(Command::Apply { settings: Box::new(s), save: true });
        }
        self.dirty = true;
    }

    pub(super) fn slider_value(&self, s: &Settings, kind: SliderKind) -> f32 {
        match kind {
            SliderKind::Brightness => s.brightness,
            SliderKind::Hue(part) => self.hue_sat(self.edited(s).1.part(part), part).0,
            SliderKind::Saturation(part) => self.hue_sat(self.edited(s).1.part(part), part).1,
            SliderKind::EffectHue(part) => self.effect_hue_sat(Custom::parse(&s.effect_custom_color).part(part), part).0,
            SliderKind::EffectSaturation(part) => self.effect_hue_sat(Custom::parse(&s.effect_custom_color).part(part), part).1,
        }
    }

    /// `s` with slider `kind` set to `value` (0..1).
    pub(super) fn with_slider(&mut self, mut s: Settings, kind: SliderKind, value: f32) -> Settings {
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
            SliderKind::EffectHue(part) | SliderKind::EffectSaturation(part) => {
                let custom = Custom::parse(&s.effect_custom_color);
                let (mut h, mut sat) = self.effect_hue_sat(custom.part(part), part);
                if matches!(kind, SliderKind::EffectHue(_)) {
                    h = value;
                    self.effect_hues[part as usize] = value;
                } else {
                    sat = value;
                }
                s.effect_custom_color = custom.with_part(part, hsv_to_rgb(h, sat)).code();
            }
        }
        s
    }

    pub(super) fn page_groups(&self, lang: Lang, s: &Settings) -> Vec<Group> {
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
                        rows.push(Row::ColorEdit { id: ID_COLOR_EDIT + 2 * i as u64, label: t(*label).into(), part: *part, rgb, hue, sat, effect: false });
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
            Page::Effects => {
                let effect = s.effect();
                let mut rows = vec![Row::Dropdown {
                    id: ID_EFFECT,
                    label: t(T::Effect).into(),
                    hint: Some(t(T::EffectHint).into()),
                    value: t(effect_label(effect)).into(),
                    drop: Drop::Effect,
                }];
                if effect.is_none() {
                    return vec![Group { title: Some(t(T::SectionEffect).into()), rows }];
                }
                if effect == Some(Effect::Snake) {
                    rows.push(Row::Switch {
                        id: ID_SNAKE_PLAY,
                        label: t(T::SnakePlay).into(),
                        hint: Some(t(T::SnakePlayHint).into()),
                        on: s.snake_play,
                        action: Action::Toggle(Flag::SnakePlay),
                    });
                }
                let always = s.effect_always();
                rows.push(Row::Choice {
                    id: ID_EFFECT_WHEN,
                    label: t(T::EffectWhen).into(),
                    hint: Some(t(if always { T::EffectAlwaysHint } else { T::EffectIdleHint }).into()),
                    options: vec![t(T::EffectWhenIdle).into(), t(T::EffectWhenAlways).into()],
                    selected: Some(always as usize),
                    actions: vec![Action::EffectWhen(false), Action::EffectWhen(true)],
                });
                if !always {
                    rows.push(Row::Choice {
                        id: ID_EFFECT_AFTER,
                        label: t(T::EffectAfter).into(),
                        hint: Some(effect_stop_hint(lang, s.effect_after_sec, self.shared.status().dim_after)),
                        options: EFFECT_WAITS.iter().map(|w| fmt_wait(lang, *w)).collect(),
                        selected: EFFECT_WAITS.iter().position(|w| (w - s.effect_after_sec).abs() < 0.5),
                        actions: EFFECT_WAITS.iter().map(|w| Action::EffectAfter(*w)).collect(),
                    });
                }
                let mode = EFFECT_COLOR_MODES.iter().position(|(code, _)| *code == s.effect_color);
                let mut colors = vec![Row::Choice {
                    id: ID_EFFECT_COLORS,
                    label: t(T::EffectColors).into(),
                    hint: Some(t(T::EffectColorsHint).into()),
                    options: EFFECT_COLOR_MODES.iter().map(|(_, k)| t(*k).to_string()).collect(),
                    selected: mode,
                    actions: EFFECT_COLOR_MODES.iter().map(|(code, _)| Action::EffectColors(code)).collect(),
                }];
                if s.effect_color == "custom" {
                    let custom = Custom::parse(&s.effect_custom_color);
                    let gradient = matches!(custom, Custom::Gradient { .. });
                    colors.push(Row::Choice {
                        id: ID_EFFECT_FILL,
                        label: t(T::Fill).into(),
                        hint: None,
                        options: vec![t(T::FillSolid).into(), t(T::FillGradient).into()],
                        selected: Some(gradient as usize),
                        actions: vec![Action::EffectFill(false), Action::EffectFill(true)],
                    });
                    let parts: &[(Part, T)] =
                        if gradient { &[(Part::Bottom, T::EffectBase), (Part::Top, T::EffectHighlight)] } else { &[(Part::Solid, T::ColorSingle)] };
                    for (i, (part, label)) in parts.iter().enumerate() {
                        let rgb = custom.part(*part);
                        let (hue, sat) = self.effect_hue_sat(rgb, *part);
                        colors.push(Row::ColorEdit { id: ID_EFFECT_COLOR_EDIT + 2 * i as u64, label: t(*label).into(), part: *part, rgb, hue, sat, effect: true });
                    }
                }
                vec![Group { title: Some(t(T::SectionEffect).into()), rows }, Group { title: Some(t(T::SectionEffectColors).into()), rows: colors }]
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
                let state = update::state();
                let action_row = match &state {
                    update::State::Available { version, .. } | update::State::Ready { version, .. } => {
                        Row::Button { id: ID_UPDATE_ACTION, glyph: glyph::SYNC, label: update_label(lang, version, true), action: Some(Action::InstallUpdate), danger: false }
                    }
                    busy => {
                        let idle = !matches!(busy, update::State::Checking | update::State::Downloading { .. });
                        Row::Button { id: ID_UPDATE_ACTION, glyph: glyph::SYNC, label: t(T::UpdateCheck).into(), action: idle.then_some(Action::CheckUpdates), danger: false }
                    }
                };
                groups.push(Group {
                    title: Some(t(T::SectionUpdates).into()),
                    rows: vec![
                        Row::Choice {
                            id: ID_UPDATES,
                            label: t(T::Updates).into(),
                            hint: Some(update_hint(lang, &s.updates, &state)),
                            options: UPDATE_MODES.iter().map(|(_, k)| t(*k).to_string()).collect(),
                            selected: UPDATE_MODES.iter().position(|(code, _)| *code == s.updates),
                            actions: UPDATE_MODES.iter().map(|(code, _)| Action::Updates(code)).collect(),
                        },
                        action_row,
                    ],
                });
                groups.push(Group {
                    title: None,
                    rows: vec![
                        Row::Nav { id: ID_CONFIG, glyph: glyph::DOCUMENT, title: t(T::SettingsFile).into(), subtitle: t(T::SettingsFileHint).into(), action: Action::OpenConfig },
                        Row::Nav { id: ID_REVEAL, glyph: glyph::FOLDER, title: t(T::ShowInFolder).into(), subtitle: t(T::ShowInFolderHint).into(), action: Action::RevealConfig },
                        Row::Nav { id: ID_BACKUP, glyph: glyph::SAVE, title: t(T::BackUp).into(), subtitle: t(T::BackUpHint).into(), action: Action::BackUp },
                        Row::Nav { id: ID_RESTORE, glyph: glyph::HISTORY, title: t(T::Restore).into(), subtitle: t(T::RestoreHint).into(), action: Action::Restore },
                        Row::Nav { id: ID_LOG, glyph: glyph::BUG, title: t(T::Log).into(), subtitle: t(T::LogHint).into(), action: Action::OpenLog },
                        Row::Info { label: t(T::Version).into(), value: format!("{} · SDK 1.8.0", env!("CARGO_PKG_VERSION")) },
                    ],
                });
                groups
            }
        }
    }

    /// Draws a whole frame; the host calls the canvas's `begin` before and `end` after.
    /// `scale` is device pixels per DIP.
    pub(super) fn render(&mut self, c: &Canvas, scale: f32) {
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

        self.hits.clear();
        c.clear(p.surface);
        let modal = self.dropdown.is_some();
        let mut ui = Ui {
            scale,
            c,
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
        let inset = self.header_inset;
        icon::draw_mark(ui.c, 24.0 + inset, 16.0, 24.0, mark, seed_for(&settings), p.dark);
        ui.c.text(ui.t(T::AppName), Rect::new(58.0 + inset, 0.0, width - 120.0, HEADER_HEIGHT), TextStyle::TitleLarge, p.on_surface, Align::Leading);
        if self.window_buttons {
            ui.icon_button(ID_MINIMIZE, width - PAGE_GUTTER - 60.0, HEADER_HEIGHT / 2.0, glyph::MINIMIZE, 12.0, Action::Minimize);
            ui.icon_button(ID_CLOSE, width - PAGE_GUTTER - 20.0, HEADER_HEIGHT / 2.0, glyph::CLOSE, 12.0, Action::Close);
        }

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
                c.stroke_round(h.rect.inset(-o, -o), h.radius + o, p.secondary, FOCUS_RING_WIDTH);
            }
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

    pub(super) fn max_scroll(&self) -> f32 {
        (self.content_h - self.page_view.h()).max(0.0)
    }

    pub(super) fn focus_step(&mut self, dir: i32) {
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
}

/// Hue and saturation of `rgb`, keeping the remembered hue of `part` while the color is white.
fn hue_sat_with(hues: &[f32; 3], rgb: [f32; 3], part: Part) -> (f32, f32) {
    let (h, s) = rgb_to_hs(rgb);
    if s < 0.02 { (hues[part as usize], s) } else { (h, s) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::palette::Color;

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
