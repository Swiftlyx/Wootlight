//! The window's widgets and views, drawn immediate-mode through the canvas: tiles, rows, switches,
//! button groups, sliders, swatches, the live keyboard or equalizer, the navigation bar and menus.
//! Platform-neutral: the Win32 and AppKit hosts share it through `window_core`.

use super::{
    canvas::{Align, Canvas, Corners, Rect, TextStyle},
    design::*,
    glyph,
    i18n::{Lang, T, tr},
    palette::{Color, Palette},
    spring::Animator,
    window_core::*,
};
use crate::{
    engine::{Owner, Preview, Status},
    layout::Layout,
    render::{SDK_COLS, SDK_ROWS, color_at, hsv_to_rgb},
    settings::{ColorMode, Settings},
};

pub(super) enum Row {
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

pub(super) struct Group {
    pub(super) title: Option<String>,
    pub(super) rows: Vec<Row>,
}

/// One frame's drawing context, borrowing the window's canvas, springs and hit list.
pub(super) struct Ui<'a> {
    pub(super) c: &'a Canvas,
    pub(super) p: Palette,
    pub(super) lang: Lang,
    pub(super) anim: &'a mut Animator,
    pub(super) hits: &'a mut Vec<Hit>,
    pub(super) hover: Option<u64>,
    pub(super) pressed: Option<u64>,
    pub(super) clip: Rect,
    pub(super) width: f32,
    /// Device pixels per DIP.
    pub(super) scale: f32,
    /// A dropdown menu is open: only its items and the scrim take clicks.
    pub(super) modal: bool,
}

pub(super) fn fmt_hz(lang: Lang, hz: f32, short: bool) -> String {
    let uk = lang == Lang::Uk;
    let (value, unit) = if hz >= 1000.0 { (hz / 1000.0, if uk { "кГц" } else { "kHz" }) } else { (hz, if uk { "Гц" } else { "Hz" }) };
    let n = if (value - value.round()).abs() < 0.05 { format!("{}", value.round()) } else { format!("{value:.1}") };
    if short {
        if hz >= 1000.0 { format!("{n}k") } else { n }
    } else {
        format!("{n} {unit}")
    }
}

pub(super) fn fmt_range(lang: Lang, fmin: f32, fmax: f32) -> String {
    format!("{} – {}", fmt_hz(lang, fmin, false), fmt_hz(lang, fmax, false))
}

pub(super) fn rgb_color(rgb: [f32; 3]) -> Color {
    Color::from_rgb_f32(rgb)
}

pub(super) fn rainbow_stops() -> Vec<Color> {
    (0..=6).map(|i| rgb_color(hsv_to_rgb(i as f32 / 6.0, 1.0))).collect()
}

/// Height of a grid of `count` swatches.
pub(super) fn swatches_height(count: usize) -> f32 {
    let lines = count.div_ceil(SWATCH_COLUMNS).max(1) as f32;
    lines * SWATCH_SIZE + (lines - 1.0) * SWATCH_GAP
}

/// WCAG contrast ratio of two colors.
pub(super) fn contrast(a: Color, b: Color) -> f32 {
    let luminance = |c: Color| {
        let linear = |v: f32| v.max(0.0).powf(2.2);
        0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b) + 0.05
    };
    let (x, y) = (luminance(a), luminance(b));
    x.max(y) / x.min(y)
}

/// Colors of a swatch from top to bottom (or left to right for rainbows).
pub(super) fn swatch_colors(mode: ColorMode) -> (Vec<Color>, bool) {
    match mode {
        ColorMode::Classic => (vec![Color::rgb8(255, 0, 0), Color::rgb8(255, 210, 0), Color::rgb8(0, 255, 40)], true),
        ColorMode::Gradient(bottom, top) => (vec![rgb_color(top), rgb_color(bottom)], true),
        ColorMode::Solid(rgb) => (vec![rgb_color(rgb)], true),
        ColorMode::Rainbow | ColorMode::Flow => (rainbow_stops(), false),
    }
}

impl Ui<'_> {
    pub(super) fn t(&self, key: T) -> &'static str {
        tr(self.lang, key)
    }

    /// Rounds a coordinate to the nearest device pixel.
    pub(super) fn snap(&self, v: f32) -> f32 {
        (v * self.scale).round() / self.scale
    }

    pub(super) fn hit(&mut self, id: u64, rect: Rect, radius: f32, action: Action, overlay: bool) {
        if self.modal && !overlay {
            return;
        }
        let clipped = Rect::new(rect.l.max(self.clip.l), rect.t.max(self.clip.t), rect.r.min(self.clip.r), rect.b.min(self.clip.b));
        if clipped.w() > 0.0 && clipped.h() > 0.0 {
            self.hits.push(Hit { id, rect: clipped, radius, action });
        }
    }

    /// Hover and press state layer opacity for an element.
    pub(super) fn layer(&mut self, id: u64) -> f32 {
        let target = if self.modal {
            0.0
        } else {
            let hover = if self.hover == Some(id) { HOVER_OPACITY } else { 0.0 };
            let press = if self.pressed == Some(id) && self.hover == Some(id) { PRESS_OPACITY } else { 0.0 };
            hover + press
        };
        self.anim.get((id, P_LAYER), target, SPRING_FAST_EFFECTS)
    }

    pub(super) fn icon_button(&mut self, id: u64, cx: f32, cy: f32, g: char, size: f32, action: Action) {
        let layer = self.layer(id);
        let r = ICON_BUTTON_SIZE / 2.0;
        self.c.fill_circle(cx, cy, r, self.p.on_surface.alpha(layer));
        self.c.glyph(g, cx, cy, size, self.p.on_surface_variant);
        self.hit(id, Rect::new(cx - r, cy - r, cx + r, cy + r), r, action, false);
    }

    pub(super) fn section_title(&mut self, title: &str, y: f32) -> f32 {
        self.c.text(title, Rect::new(CONTENT_INSET, y, self.width - CONTENT_INSET, y + SECTION_TITLE_HEIGHT), TextStyle::TitleSmall, self.p.primary, Align::Leading);
        y + SECTION_TITLE_HEIGHT
    }

    pub(super) fn hint_height(&self, hint: &Option<String>) -> f32 {
        hint.as_ref().map_or(0.0, |h| self.c.measure_height(h, TextStyle::BodySmall, self.width - 2.0 * CONTENT_INSET))
    }

    pub(super) fn draw_hint(&self, hint: &Option<String>, y: f32) {
        if let Some(h) = hint {
            self.c.text_wrapped(h, Rect::new(CONTENT_INSET, y, self.width - CONTENT_INSET, y + 200.0), TextStyle::BodySmall, self.p.on_surface_variant);
        }
    }

    pub(super) fn row_height(&self, row: &Row) -> f32 {
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
    pub(super) fn row_action(row: &Row) -> Option<(u64, Action)> {
        match row {
            Row::Switch { id, action, .. } | Row::Nav { id, action, .. } => Some((*id, action.clone())),
            Row::Button { id, action: Some(action), .. } => Some((*id, action.clone())),
            _ => None,
        }
    }

    pub(super) fn groups(&mut self, groups: &[Group], mut y: f32) -> f32 {
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

    pub(super) fn tile(&mut self, reactive: Option<(u64, Action)>, rect: Rect, first: bool, last: bool) {
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

    pub(super) fn row(&mut self, row: &Row, rect: Rect) {
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

    pub(super) fn switch(&mut self, id: u64, right: f32, cy: f32, on: bool) {
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

    pub(super) fn button_group(&mut self, base: u64, rect: Rect, labels: &[String], selected: Option<usize>, actions: &[Action]) {
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

    pub(super) fn slider(&mut self, id: u64, left: f32, right: f32, cy: f32, value: f32) {
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
    pub(super) fn color_slider(&mut self, id: u64, left: f32, right: f32, cy: f32, value: f32, colors: &[Color], kind: SliderKind) {
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

    pub(super) fn swatch(&mut self, index: usize, rect: Rect, mode: ColorMode, selected: bool) {
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

/// Key unit, height of the live view, first grid row and number of rows for this layout.
pub(super) fn view_metrics(layout: &Layout, inner_w: f32) -> (f32, f32, usize, usize) {
    let top_row = layout.keys.iter().map(|k| k.row).min().unwrap_or(0);
    let bottom_row = layout.keys.iter().map(|k| k.row).max().unwrap_or(0);
    let rows = bottom_row - top_row + 1;
    let u = (inner_w / layout.width).min(MAX_KEY_UNIT);
    (u, rows as f32 * u + f_row_gap(top_row, rows), top_row, rows)
}

/// The gap after the function row only exists on boards that have one.
pub(super) fn f_row_gap(top_row: usize, rows: usize) -> f32 {
    if top_row == 0 && rows >= 6 { F_ROW_GAP } else { 0.0 }
}

/// Draws the pinned card; returns its bottom edge and the area of the live view.
pub(super) fn draw_preview(ui: &mut Ui, preview: &Preview, layout: &Layout, s: &Settings, status: &Status, view: View, top: f32) -> (f32, Rect) {
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
pub(super) fn draw_live_view(ui: &mut Ui, preview: &Preview, layout: &Layout, mode: ColorMode, view: View, area: Rect, axis: Option<(f32, f32)>) {
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

pub(super) fn draw_keys(ui: &mut Ui, layout: &Layout, preview: &Preview, area: Rect, u: f32, top_row: usize, rows: usize) {
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
pub(super) fn draw_bars(ui: &mut Ui, preview: &Preview, mode: ColorMode, bars_area: Rect, rows: usize) {
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
pub(super) fn draw_axis(ui: &mut Ui, fmin: f32, fmax: f32, bars_area: Rect, bottom: f32) {
    let span = (fmax / fmin).ln();
    for f in AXIS_TICKS.iter().filter(|f| **f >= fmin * 1.15 && **f <= fmax / 1.15) {
        let x = bars_area.l + (f / fmin).ln() / span * bars_area.w();
        ui.c.text(&fmt_hz(ui.lang, *f, true), Rect::new(x - 24.0, bars_area.b + 2.0, x + 24.0, bottom), TextStyle::BodySmall, ui.p.on_surface_variant, Align::Center);
    }
}

pub(super) fn draw_nav(ui: &mut Ui, page: Page, height: f32) {
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
pub(super) fn dropdown_items(drop: Drop, s: &Settings, devices: &[String], lang: Lang) -> (Vec<String>, Option<usize>) {
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

pub(super) fn draw_dropdown(ui: &mut Ui, drop: Drop, anchor: Rect, items: &(Vec<String>, Option<usize>), hot: Option<usize>) {
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
