//! Direct2D and DirectWrite drawing in DIPs: shapes with per-corner radii, text in the type scale,
//! and Fluent icon glyphs. Software render targets keep GPU driver allocations out of an app that is
//! idle most of the time.

use std::{cell::RefCell, collections::HashMap};

use windows::{
    Win32::{
        Foundation::RECT,
        Graphics::{
            Direct2D::{
                Common::{
                    D2D_RECT_F, D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_BEZIER_SEGMENT, D2D1_COLOR_F,
                    D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT,
                },
                D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_DRAW_TEXT_OPTIONS_NONE,
                D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
                D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
                D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE,
                D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
                D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1RenderTarget,
                ID2D1SolidColorBrush, ID2D1StrokeStyle,
            },
            DirectWrite::{
                DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
                DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, DWRITE_TRIMMING,
                DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP,
                DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat,
            },
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Gdi::HDC,
        },
    },
    core::{BOOL, HSTRING, Interface, Result},
};
use windows_numerics::Vector2;

use super::palette::Color;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub l: f32,
    pub t: f32,
    pub r: f32,
    pub b: f32,
}

impl Rect {
    pub const fn new(l: f32, t: f32, r: f32, b: f32) -> Self {
        Self { l, t, r, b }
    }
    pub fn xywh(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { l: x, t: y, r: x + w, b: y + h }
    }
    pub fn w(&self) -> f32 {
        self.r - self.l
    }
    pub fn h(&self) -> f32 {
        self.b - self.t
    }
    pub fn cx(&self) -> f32 {
        (self.l + self.r) / 2.0
    }
    pub fn cy(&self) -> f32 {
        (self.t + self.b) / 2.0
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }
    pub fn inset(&self, dx: f32, dy: f32) -> Self {
        Self { l: self.l + dx, t: self.t + dy, r: self.r - dx, b: self.b - dy }
    }
    pub fn offset(&self, dx: f32, dy: f32) -> Self {
        Self { l: self.l + dx, t: self.t + dy, r: self.r + dx, b: self.b + dy }
    }
    pub fn intersects(&self, other: &Rect) -> bool {
        self.l < other.r && other.l < self.r && self.t < other.b && other.t < self.b
    }
    fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.l, top: self.t, right: self.r, bottom: self.b }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Corners {
    pub tl: f32,
    pub tr: f32,
    pub br: f32,
    pub bl: f32,
}

impl Corners {
    /// Outer corners `outer` on the sides that are the group's edges, `inner` elsewhere.
    pub fn tile(first: bool, last: bool, outer: f32, inner: f32) -> Self {
        let top = if first { outer } else { inner };
        let bottom = if last { outer } else { inner };
        Self { tl: top, tr: top, br: bottom, bl: bottom }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextStyle {
    TitleLarge,
    TitleSmall,
    BodyLarge,
    BodyMedium,
    BodySmall,
    LabelLarge,
    LabelMedium,
}

/// Size, weight and whether the display optical size is used, in `TextStyle` order.
const STYLES: [(f32, DWRITE_FONT_WEIGHT, bool); 7] = [
    (20.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, true),
    (14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, false),
    (14.5, DWRITE_FONT_WEIGHT_NORMAL, false),
    (13.0, DWRITE_FONT_WEIGHT_NORMAL, false),
    (12.0, DWRITE_FONT_WEIGHT_NORMAL, false),
    (13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, false),
    (12.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, false),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Leading,
    Center,
    Trailing,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn d2d_color(c: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r, g: c.g, b: c.b, a: c.a }
}

fn v(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

pub struct Canvas {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    formats: Vec<IDWriteTextFormat>,
    /// DIPs to move centered text up so capitals and digits sit in the middle of the line.
    shifts: Vec<f32>,
    icon_family: HSTRING,
    glyph_formats: RefCell<HashMap<i32, IDWriteTextFormat>>,
    target: Option<ID2D1RenderTarget>,
    dc_target: Option<ID2D1DCRenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
}

fn resolve_family(dwrite: &IDWriteFactory, candidates: &[&str]) -> HSTRING {
    // SAFETY: plain DirectWrite queries with valid out pointers.
    unsafe {
        let mut fonts: Option<IDWriteFontCollection> = None;
        if dwrite.GetSystemFontCollection(&mut fonts, false).is_ok() {
            if let Some(fonts) = fonts {
                for name in candidates {
                    let (mut index, mut exists) = (0u32, BOOL(0));
                    if fonts.FindFamilyName(&HSTRING::from(*name), &mut index, &mut exists).is_ok() && exists.as_bool() {
                        return HSTRING::from(*name);
                    }
                }
            }
        }
    }
    HSTRING::from(*candidates.last().unwrap_or(&"Segoe UI"))
}

fn software_properties(dpi: f32) -> D2D1_RENDER_TARGET_PROPERTIES {
    D2D1_RENDER_TARGET_PROPERTIES {
        r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: dpi,
        dpiY: dpi,
        usage: D2D1_RENDER_TARGET_USAGE_NONE,
        minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
    }
}

impl Canvas {
    pub fn new() -> Result<Self> {
        // SAFETY: factory creation and text format setup with valid arguments.
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let text = resolve_family(&dwrite, &["Segoe UI Variable Text", "Segoe UI"]);
            let display = resolve_family(&dwrite, &["Segoe UI Variable Display", "Segoe UI"]);
            let icon_family = resolve_family(&dwrite, &["Segoe Fluent Icons", "Segoe MDL2 Assets"]);
            let locale = HSTRING::from("en-us");

            let mut formats = Vec::new();
            let mut shifts = Vec::new();
            for (size, weight, is_display) in STYLES {
                let family = if is_display { &display } else { &text };
                let format = dwrite.CreateTextFormat(
                    family,
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    &locale,
                )?;
                format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                // Offset of the ink of capitals and digits from the center of a centered line.
                let mut shift = 0.0;
                let probe_height = 1000.0;
                if let Ok(probe) = dwrite.CreateTextLayout(&wide("H0"), &format, 1000.0, probe_height) {
                    if let Ok(overhang) = probe.GetOverhangMetrics() {
                        let ink_top = -overhang.top;
                        let ink_bottom = probe_height + overhang.bottom;
                        shift = (((ink_top + ink_bottom) / 2.0 - probe_height / 2.0) * 2.0).round() / 2.0;
                    }
                }
                if let Ok(ellipsis) = dwrite.CreateEllipsisTrimmingSign(&format) {
                    let trimming = DWRITE_TRIMMING {
                        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                        delimiter: 0,
                        delimiterCount: 0,
                    };
                    let _ = format.SetTrimming(&trimming, &ellipsis);
                }
                formats.push(format);
                shifts.push(shift);
            }
            Ok(Self {
                d2d,
                dwrite,
                formats,
                shifts,
                icon_family,
                glyph_formats: RefCell::new(HashMap::new()),
                target: None,
                dc_target: None,
                brush: None,
            })
        }
    }

    /// A target drawn into a GDI DC each frame. Opaque targets (the window) get ClearType text;
    /// translucent ones (the menu, icons) keep alpha and use grayscale text.
    pub fn attach_dc(&mut self, dpi: f32, opaque: bool) -> Result<()> {
        self.attach_dc_with(dpi, opaque, false)
    }

    /// `gpu` renders on the graphics card and copies the result to the DC: worth it for a small
    /// area redrawn at the display refresh rate.
    pub fn attach_dc_with(&mut self, dpi: f32, opaque: bool, gpu: bool) -> Result<()> {
        self.detach();
        let mut props = software_properties(dpi);
        if opaque {
            props.pixelFormat.alphaMode = D2D1_ALPHA_MODE_IGNORE;
        }
        if gpu {
            props.r#type = D2D1_RENDER_TARGET_TYPE_DEFAULT;
        }
        // SAFETY: creates a DC render target; it is bound to a DC before each frame.
        unsafe {
            let target = self.d2d.CreateDCRenderTarget(&props)?;
            self.finish_attach(target.cast()?, opaque)?;
            self.dc_target = Some(target);
        }
        Ok(())
    }

    fn finish_attach(&mut self, target: ID2D1RenderTarget, opaque: bool) -> Result<()> {
        // SAFETY: the target was just created.
        unsafe {
            // ClearType needs an opaque background.
            target.SetTextAntialiasMode(if opaque { D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE } else { D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE });
            self.brush = Some(target.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }, None)?);
        }
        self.target = Some(target);
        Ok(())
    }

    pub fn detach(&mut self) {
        self.brush = None;
        self.target = None;
        self.dc_target = None;
    }

    pub fn bind_dc(&self, dc: HDC, width: i32, height: i32) -> bool {
        self.bind_dc_rect(dc, RECT { left: 0, top: 0, right: width, bottom: height })
    }

    /// Binds to part of a DC; the drawing origin is the rectangle's top-left corner.
    pub fn bind_dc_rect(&self, dc: HDC, rect: RECT) -> bool {
        // SAFETY: the DC stays selected while this frame is drawn.
        self.dc_target.as_ref().is_some_and(|t| unsafe { t.BindDC(dc, &rect).is_ok() })
    }

    /// Moves everything drawn after this call by (dx, dy) DIPs.
    pub fn set_offset(&self, dx: f32, dy: f32) {
        let m = windows_numerics::Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: dx, M32: dy };
        // SAFETY: plain setter between begin() and end().
        unsafe { self.t().SetTransform(&m) };
    }

    pub fn set_dpi(&self, dpi: f32) {
        if let Some(t) = &self.target {
            // SAFETY: plain setter.
            unsafe { t.SetDpi(dpi, dpi) };
        }
    }

    fn t(&self) -> &ID2D1RenderTarget {
        self.target.as_ref().expect("canvas has no render target")
    }

    fn brush(&self, color: Color) -> &ID2D1SolidColorBrush {
        let brush = self.brush.as_ref().expect("canvas has no brush");
        // SAFETY: plain setter.
        unsafe { brush.SetColor(&d2d_color(color)) };
        brush
    }

    pub fn begin(&self) {
        // SAFETY: matched by end().
        unsafe { self.t().BeginDraw() };
        self.set_offset(0.0, 0.0);
    }

    /// False when the target was lost and must be recreated.
    pub fn end(&self) -> bool {
        // SAFETY: matched with begin().
        match unsafe { self.t().EndDraw(None, None) } {
            Ok(()) => true,
            Err(e) => {
                crate::log!("EndDraw failed: {e}");
                false
            }
        }
    }

    pub fn clear(&self, color: Color) {
        // SAFETY: drawing between begin() and end().
        unsafe { self.t().Clear(Some(&d2d_color(color))) };
    }

    pub fn fill_rect(&self, rect: Rect, color: Color) {
        if color.a <= 0.0 {
            return;
        }
        // SAFETY: drawing between begin() and end().
        unsafe { self.t().FillRectangle(&rect.d2d(), self.brush(color)) };
    }

    pub fn fill_round(&self, rect: Rect, radius: f32, color: Color) {
        if color.a <= 0.0 || rect.w() <= 0.0 || rect.h() <= 0.0 {
            return;
        }
        let radius = radius.min(rect.w() / 2.0).min(rect.h() / 2.0).max(0.0);
        let rounded = D2D1_ROUNDED_RECT { rect: rect.d2d(), radiusX: radius, radiusY: radius };
        // SAFETY: drawing between begin() and end().
        unsafe { self.t().FillRoundedRectangle(&rounded, self.brush(color)) };
    }

    pub fn stroke_round(&self, rect: Rect, radius: f32, color: Color, width: f32) {
        if color.a <= 0.0 {
            return;
        }
        let radius = radius.min(rect.w() / 2.0).min(rect.h() / 2.0).max(0.0);
        let rounded = D2D1_ROUNDED_RECT { rect: rect.d2d(), radiusX: radius, radiusY: radius };
        // SAFETY: drawing between begin() and end().
        unsafe { self.t().DrawRoundedRectangle(&rounded, self.brush(color), width, None::<&ID2D1StrokeStyle>) };
    }

    /// A rectangle with its own radius per corner, e.g. a tile in a group.
    pub fn fill_shape(&self, rect: Rect, corners: Corners, color: Color) {
        let limit = rect.w().min(rect.h()) / 2.0;
        if limit <= 0.0 || color.a <= 0.0 {
            return;
        }
        let c = |r: f32| r.clamp(0.0, limit);
        let (tl, tr, br, bl) = (c(corners.tl), c(corners.tr), c(corners.br), c(corners.bl));
        if tl == tr && tr == br && br == bl {
            if tl <= 0.0 {
                self.fill_rect(rect, color);
            } else {
                self.fill_round(rect, tl, color);
            }
            return;
        }
        // SAFETY: builds and fills a path geometry between begin() and end().
        unsafe {
            let Ok(path) = self.d2d.CreatePathGeometry() else { return };
            let Ok(sink) = path.Open() else { return };
            // Each corner is a quarter circle as a cubic Bézier, handles at 0.5523 of the radius.
            const HANDLE: f32 = 0.5523;
            let corner = |from: (f32, f32), at: (f32, f32), to: (f32, f32)| {
                if from == to {
                    return;
                }
                sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                    point1: v(from.0 + (at.0 - from.0) * HANDLE, from.1 + (at.1 - from.1) * HANDLE),
                    point2: v(to.0 + (at.0 - to.0) * HANDLE, to.1 + (at.1 - to.1) * HANDLE),
                    point3: v(to.0, to.1),
                });
            };
            let Rect { l, t, r, b } = rect;
            sink.BeginFigure(v(l + tl, t), D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLine(v(r - tr, t));
            corner((r - tr, t), (r, t), (r, t + tr));
            sink.AddLine(v(r, b - br));
            corner((r, b - br), (r, b), (r - br, b));
            sink.AddLine(v(l + bl, b));
            corner((l + bl, b), (l, b), (l, b - bl));
            sink.AddLine(v(l, t + tl));
            corner((l, t + tl), (l, t), (l + tl, t));
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            if sink.Close().is_ok() {
                self.t().FillGeometry(&path, self.brush(color), None);
            }
        }
    }

    pub fn fill_circle(&self, cx: f32, cy: f32, radius: f32, color: Color) {
        if color.a <= 0.0 || radius <= 0.0 {
            return;
        }
        let ellipse = D2D1_ELLIPSE { point: v(cx, cy), radiusX: radius, radiusY: radius };
        // SAFETY: drawing between begin() and end().
        unsafe { self.t().FillEllipse(&ellipse, self.brush(color)) };
    }

    /// A rounded rectangle filled with evenly spaced colors, top to bottom or left to right.
    pub fn fill_round_gradient(&self, rect: Rect, radius: f32, colors: &[Color], vertical: bool) {
        if colors.len() < 2 {
            if let Some(c) = colors.first() {
                self.fill_round(rect, radius, *c);
            }
            return;
        }
        let (start, end) =
            if vertical { (v(rect.cx(), rect.t), v(rect.cx(), rect.b)) } else { (v(rect.l, rect.cy()), v(rect.r, rect.cy())) };
        self.fill_rounds_with(&[rect], radius, start, end, colors);
    }

    /// Rounded rectangles filled from one vertical gradient spanning `top`..`bottom`, so a color
    /// depends on height alone; `colors` run from top to bottom.
    pub fn fill_rounds_vertical(&self, rects: &[Rect], radius: f32, top: f32, bottom: f32, colors: &[Color]) {
        self.fill_rounds_with(rects, radius, v(0.0, top), v(0.0, bottom), colors);
    }

    /// Rounded rectangles filled from one linear gradient running from `start` to `end`.
    fn fill_rounds_with(&self, rects: &[Rect], radius: f32, start: Vector2, end: Vector2, colors: &[Color]) {
        if rects.is_empty() || colors.is_empty() {
            return;
        }
        let stops: Vec<D2D1_GRADIENT_STOP> = colors
            .iter()
            .enumerate()
            .map(|(i, c)| D2D1_GRADIENT_STOP { position: i as f32 / (colors.len().max(2) - 1) as f32, color: d2d_color(*c) })
            .collect();
        // SAFETY: one short-lived gradient brush between begin() and end().
        unsafe {
            let Ok(collection) = self.t().CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else {
                return;
            };
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: start, endPoint: end };
            let Ok(brush) = self.t().CreateLinearGradientBrush(&props, None, &collection) else { return };
            for rect in rects {
                let r = radius.min(rect.w() / 2.0).min(rect.h() / 2.0).max(0.0);
                let rounded = D2D1_ROUNDED_RECT { rect: rect.d2d(), radiusX: r, radiusY: r };
                self.t().FillRoundedRectangle(&rounded, &brush);
            }
        }
    }

    fn format(&self, style: TextStyle, align: Align, wrap: bool) -> &IDWriteTextFormat {
        let format = &self.formats[style as usize];
        // SAFETY: plain setters on a format owned by this canvas.
        unsafe {
            let _ = format.SetTextAlignment(match align {
                Align::Leading => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Trailing => DWRITE_TEXT_ALIGNMENT_TRAILING,
            });
            let _ = format.SetWordWrapping(if wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP });
        }
        format
    }

    /// Text vertically centered in `rect`; single lines end with an ellipsis when cut.
    pub fn text(&self, text: &str, rect: Rect, style: TextStyle, color: Color, align: Align) {
        self.draw_text(text, rect, style, color, align, false);
    }

    /// Wrapping text, top-aligned within the rectangle's height.
    pub fn text_wrapped(&self, text: &str, rect: Rect, style: TextStyle, color: Color) {
        let height = self.measure_height(text, style, rect.w());
        self.draw_text(text, Rect::new(rect.l, rect.t, rect.r, rect.t + height), style, color, Align::Leading, true);
    }

    fn draw_text(&self, text: &str, rect: Rect, style: TextStyle, color: Color, align: Align, wrap: bool) {
        if text.is_empty() || color.a <= 0.0 {
            return;
        }
        let shift = self.shifts[style as usize];
        let shifted = Rect::new(rect.l, rect.t - shift, rect.r, rect.b - shift);
        // SAFETY: drawing between begin() and end().
        unsafe {
            self.t().DrawText(
                &wide(text),
                self.format(style, align, wrap),
                &shifted.d2d(),
                self.brush(color),
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    pub fn measure(&self, text: &str, style: TextStyle) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        // SAFETY: a temporary layout for measuring.
        unsafe {
            let format = self.format(style, Align::Leading, false);
            let Ok(layout) = self.dwrite.CreateTextLayout(&wide(text), format, 10000.0, 100.0) else { return 0.0 };
            let mut metrics = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut metrics).is_err() {
                return 0.0;
            }
            metrics.widthIncludingTrailingWhitespace
        }
    }

    pub fn measure_height(&self, text: &str, style: TextStyle, width: f32) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        // SAFETY: a temporary layout for measuring.
        unsafe {
            let format = self.format(style, Align::Leading, true);
            let Ok(layout) = self.dwrite.CreateTextLayout(&wide(text), format, width.max(1.0), 10000.0) else {
                return 0.0;
            };
            let mut metrics = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut metrics).is_err() {
                return 0.0;
            }
            metrics.height
        }
    }

    /// A Fluent icon centered on a point.
    pub fn glyph(&self, glyph: char, cx: f32, cy: f32, size: f32, color: Color) {
        if color.a <= 0.0 {
            return;
        }
        let key = (size * 4.0).round() as i32;
        let mut formats = self.glyph_formats.borrow_mut();
        if !formats.contains_key(&key) {
            // SAFETY: creates a text format for the icon font.
            let created = unsafe {
                self.dwrite.CreateTextFormat(
                    &self.icon_family,
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    &HSTRING::from("en-us"),
                )
            };
            let Ok(format) = created else { return };
            // SAFETY: plain setters.
            unsafe {
                let _ = format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            }
            formats.insert(key, format);
        }
        let format = &formats[&key];
        let mut buf = [0u16; 2];
        let rect = Rect::new(cx - size, cy - size, cx + size, cy + size);
        // SAFETY: drawing between begin() and end(); the text mode is restored right after.
        unsafe {
            // ClearType fringes icons with color, most visibly over colored fills.
            let mode = self.t().GetTextAntialiasMode();
            self.t().SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            self.t().DrawText(
                glyph.encode_utf16(&mut buf),
                format,
                &rect.d2d(),
                self.brush(color),
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            self.t().SetTextAntialiasMode(mode);
        }
    }

    pub fn push_clip(&self, rect: Rect) {
        // SAFETY: matched by pop_clip().
        unsafe { self.t().PushAxisAlignedClip(&rect.d2d(), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
    }

    pub fn pop_clip(&self) {
        // SAFETY: matched with push_clip().
        unsafe { self.t().PopAxisAlignedClip() };
    }
}
