//! Core Graphics and Core Text drawing in points, with the same drawing API as the Direct2D canvas
//! in src/ui/canvas.rs: shapes with per-corner radii, text in the type scale in SF Pro, and SF
//! Symbols standing in for the Fluent icon glyphs.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ptr::{self, NonNull},
};

use objc2::rc::Retained;
use objc2_app_kit::{NSFont, NSFontWeightRegular, NSFontWeightSemibold, NSImage, NSImageSymbolConfiguration};
use objc2_core_foundation::{
    CFAttributedString, CFDictionary, CFIndex, CFRange, CFRetained, CFString, CFType, CGAffineTransform, Type,
    CGFloat, CGPoint, CGRect, CGSize, kCFBooleanTrue,
};
use objc2_core_graphics::{CGColor, CGColorSpace, CGContext, CGGradient, CGGradientDrawingOptions, CGImage, CGPath, kCGColorSpaceSRGB};
use objc2_core_text::{
    CTFont, CTLine, CTLineTruncationType, CTTypesetter, kCTFontAttributeName, kCTForegroundColorFromContextAttributeName,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

pub use super::geometry::{Align, Corners, Rect, TextStyle};
use super::palette::Color;

/// Size and whether the weight is semibold, in `TextStyle` order (the Windows type scale).
const STYLES: [(f32, bool); 7] = [
    (20.0, true),
    (14.0, true),
    (14.5, false),
    (13.0, false),
    (12.0, false),
    (13.0, true),
    (12.0, true),
];

/// Line height of wrapped text, in ems: the line spacing of Segoe UI, so wrapped blocks measure
/// the same as on Windows.
const LINE_HEIGHT: f32 = 1.33;

/// SF Symbols are drawn a little smaller than their nominal size so they cover about as much of
/// the em as the Fluent icons they replace.
const SYMBOL_SCALE: f32 = 0.82;

/// The SF Symbol drawn for a Segoe Fluent Icons code point (see `ui::glyph`).
pub fn symbol_name(glyph: char) -> Option<&'static str> {
    use super::glyph::*;
    Some(match glyph {
        SETTINGS => "gearshape",
        PALETTE => "paintpalette",
        EQUALIZER => "slider.vertical.3",
        CHECK => "checkmark",
        CHEVRON_RIGHT => "chevron.right",
        CHEVRON_DOWN => "chevron.down",
        MINIMIZE => "minus",
        CLOSE => "xmark",
        OPEN_WINDOW => "macwindow",
        POWER => "power",
        DOCUMENT => "doc.text",
        SYNC => "arrow.triangle.2.circlepath",
        EDIT => "pencil",
        FOLDER => "folder",
        BUG => "ant",
        ADD => "plus",
        DELETE => "trash",
        _ => return None,
    })
}

/// An SF Symbol as a template image, with an optional palette color, for menus.
pub fn symbol_image(glyph: char, size: f32) -> Option<Retained<NSImage>> {
    let name = NSString::from_str(symbol_name(glyph)?);
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(&name, None)?;
    // SAFETY: reads an AppKit constant.
    let weight = unsafe { NSFontWeightRegular };
    let config = NSImageSymbolConfiguration::configurationWithPointSize_weight(size as CGFloat, weight);
    image.imageWithSymbolConfiguration(&config)
}

fn cg_rect(r: Rect) -> CGRect {
    CGRect::new(CGPoint::new(r.l as CGFloat, r.t as CGFloat), CGSize::new(r.w().max(0.0) as CGFloat, r.h().max(0.0) as CGFloat))
}

fn srgb() -> Option<CFRetained<CGColorSpace>> {
    // SAFETY: reads a Core Graphics constant.
    CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
}

fn cg_color(c: Color) -> CFRetained<CGColor> {
    CGColor::new_srgb(c.r as CGFloat, c.g as CGFloat, c.b as CGFloat, c.a as CGFloat)
}

struct Style {
    font: CFRetained<CTFont>,
    size: f32,
    /// Half the cap height: centered text puts its baseline this far below the middle of the
    /// line, so capitals and digits sit in the middle as on Windows.
    half_cap: f32,
}

/// A rasterized symbol: an alpha mask and its size in points.
struct Symbol {
    mask: Retained<CGImage>,
    w: f32,
    h: f32,
}

pub struct Canvas {
    ctx: RefCell<Option<CFRetained<CGContext>>>,
    offset: Cell<(f32, f32)>,
    /// The offset in force when each clip was pushed; restoring the state restores it too.
    clips: RefCell<Vec<(f32, f32)>>,
    styles: Vec<Style>,
    symbols: RefCell<HashMap<(char, u32, u32), Option<Symbol>>>,
}

fn system_font(size: f32, semibold: bool) -> Option<CFRetained<CTFont>> {
    // SAFETY: reads AppKit constants.
    let weight = unsafe { if semibold { NSFontWeightSemibold } else { NSFontWeightRegular } };
    let font = NSFont::systemFontOfSize_weight(size as CGFloat, weight);
    // SAFETY: NSFont is toll-free bridged to CTFont; the +1 reference moves into the CFRetained.
    unsafe { NonNull::new(Retained::into_raw(font).cast::<CTFont>()).map(|p| CFRetained::from_raw(p)) }
}

impl Canvas {
    pub fn new() -> Result<Self, String> {
        let mut styles = Vec::new();
        for (size, semibold) in STYLES {
            let font = system_font(size, semibold).ok_or("the system font is unavailable")?;
            // SAFETY: plain metric query.
            let cap = unsafe { font.cap_height() } as f32;
            styles.push(Style { font, size, half_cap: (cap / 2.0 * 4.0).round() / 4.0 });
        }
        Ok(Self {
            ctx: RefCell::new(None),
            offset: Cell::new((0.0, 0.0)),
            clips: RefCell::new(Vec::new()),
            styles,
            symbols: RefCell::new(HashMap::new()),
        })
    }

    /// Draws into `ctx` until `detach`: points, origin at the top left of a surface `height` points
    /// tall (the canvas flips the context itself, so pass an unflipped context, like the one an
    /// unflipped NSView gets in drawRect). Call `begin`/`end` around each frame.
    pub fn attach(&self, ctx: &CGContext, height: f32) {
        self.detach();
        CGContext::save_g_state(Some(ctx));
        CGContext::translate_ctm(Some(ctx), 0.0, height as CGFloat);
        CGContext::scale_ctm(Some(ctx), 1.0, -1.0);
        *self.ctx.borrow_mut() = Some(ctx.retain());
        self.offset.set((0.0, 0.0));
    }

    pub fn detach(&self) {
        if let Some(ctx) = self.ctx.borrow_mut().take() {
            CGContext::restore_g_state(Some(&ctx));
        }
        self.clips.borrow_mut().clear();
    }

    fn c(&self) -> CFRetained<CGContext> {
        self.ctx.borrow().clone().expect("canvas is not attached to a context")
    }

    /// Moves everything drawn after this call by (dx, dy) points.
    pub fn set_offset(&self, dx: f32, dy: f32) {
        let (ox, oy) = self.offset.get();
        if (ox, oy) != (dx, dy) {
            CGContext::translate_ctm(Some(&self.c()), (dx - ox) as CGFloat, (dy - oy) as CGFloat);
            self.offset.set((dx, dy));
        }
    }

    pub fn begin(&self) {
        CGContext::save_g_state(Some(&self.c()));
        self.offset.set((0.0, 0.0));
    }

    /// Always true: a Core Graphics context is not lost like a Direct2D target.
    pub fn end(&self) -> bool {
        let ctx = self.c();
        for _ in self.clips.borrow_mut().drain(..) {
            CGContext::restore_g_state(Some(&ctx));
        }
        CGContext::restore_g_state(Some(&ctx));
        self.offset.set((0.0, 0.0));
        true
    }

    fn fill_color(&self, ctx: &CGContext, color: Color) {
        CGContext::set_fill_color_with_color(Some(ctx), Some(&cg_color(color)));
    }

    /// Fills the whole surface, replacing what is there (also with a translucent color).
    pub fn clear(&self, color: Color) {
        let ctx = self.c();
        let all = CGRect::new(CGPoint::new(-1e5, -1e5), CGSize::new(2e5, 2e5));
        CGContext::clear_rect(Some(&ctx), all);
        if color.a > 0.0 {
            self.fill_color(&ctx, color);
            CGContext::fill_rect(Some(&ctx), all);
        }
    }

    pub fn fill_rect(&self, rect: Rect, color: Color) {
        if color.a <= 0.0 {
            return;
        }
        let ctx = self.c();
        self.fill_color(&ctx, color);
        CGContext::fill_rect(Some(&ctx), cg_rect(rect));
    }

    fn round_path(rect: Rect, radius: f32) -> CFRetained<CGPath> {
        let r = radius.min(rect.w() / 2.0).min(rect.h() / 2.0).max(0.0) as CGFloat;
        // SAFETY: the radius fits the rectangle; no transform.
        unsafe { CGPath::with_rounded_rect(cg_rect(rect), r, r, ptr::null()) }
    }

    pub fn fill_round(&self, rect: Rect, radius: f32, color: Color) {
        if color.a <= 0.0 || rect.w() <= 0.0 || rect.h() <= 0.0 {
            return;
        }
        let ctx = self.c();
        self.fill_color(&ctx, color);
        CGContext::add_path(Some(&ctx), Some(&Self::round_path(rect, radius)));
        CGContext::fill_path(Some(&ctx));
    }

    pub fn stroke_round(&self, rect: Rect, radius: f32, color: Color, width: f32) {
        if color.a <= 0.0 || rect.w() <= 0.0 || rect.h() <= 0.0 {
            return;
        }
        let ctx = self.c();
        CGContext::set_stroke_color_with_color(Some(&ctx), Some(&cg_color(color)));
        CGContext::set_line_width(Some(&ctx), width as CGFloat);
        CGContext::add_path(Some(&ctx), Some(&Self::round_path(rect, radius)));
        CGContext::stroke_path(Some(&ctx));
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
        let ctx = self.c();
        let p = |x: f32, y: f32| (x as CGFloat, y as CGFloat);
        // Each corner is a quarter circle as a cubic Bézier, handles at 0.5523 of the radius, as on
        // Windows.
        const HANDLE: f32 = 0.5523;
        let corner = |from: (f32, f32), at: (f32, f32), to: (f32, f32)| {
            if from == to {
                return;
            }
            let (c1x, c1y) = p(from.0 + (at.0 - from.0) * HANDLE, from.1 + (at.1 - from.1) * HANDLE);
            let (c2x, c2y) = p(to.0 + (at.0 - to.0) * HANDLE, to.1 + (at.1 - to.1) * HANDLE);
            let (x, y) = p(to.0, to.1);
            CGContext::add_curve_to_point(Some(&ctx), c1x, c1y, c2x, c2y, x, y);
        };
        let Rect { l, t, r, b } = rect;
        CGContext::begin_path(Some(&ctx));
        let (x, y) = p(l + tl, t);
        CGContext::move_to_point(Some(&ctx), x, y);
        let (x, y) = p(r - tr, t);
        CGContext::add_line_to_point(Some(&ctx), x, y);
        corner((r - tr, t), (r, t), (r, t + tr));
        let (x, y) = p(r, b - br);
        CGContext::add_line_to_point(Some(&ctx), x, y);
        corner((r, b - br), (r, b), (r - br, b));
        let (x, y) = p(l + bl, b);
        CGContext::add_line_to_point(Some(&ctx), x, y);
        corner((l + bl, b), (l, b), (l, b - bl));
        let (x, y) = p(l, t + tl);
        CGContext::add_line_to_point(Some(&ctx), x, y);
        corner((l, t + tl), (l, t), (l + tl, t));
        CGContext::close_path(Some(&ctx));
        self.fill_color(&ctx, color);
        CGContext::fill_path(Some(&ctx));
    }

    pub fn fill_circle(&self, cx: f32, cy: f32, radius: f32, color: Color) {
        if color.a <= 0.0 || radius <= 0.0 {
            return;
        }
        let ctx = self.c();
        self.fill_color(&ctx, color);
        CGContext::fill_ellipse_in_rect(Some(&ctx), cg_rect(Rect::new(cx - radius, cy - radius, cx + radius, cy + radius)));
    }

    /// A rounded rectangle filled with evenly spaced colors, top to bottom or left to right.
    pub fn fill_round_gradient(&self, rect: Rect, radius: f32, colors: &[Color], vertical: bool) {
        if colors.len() < 2 {
            if let Some(c) = colors.first() {
                self.fill_round(rect, radius, *c);
            }
            return;
        }
        let (start, end) = if vertical { ((rect.cx(), rect.t), (rect.cx(), rect.b)) } else { ((rect.l, rect.cy()), (rect.r, rect.cy())) };
        self.fill_rounds_with(&[rect], radius, start, end, colors);
    }

    /// Rounded rectangles filled from one vertical gradient spanning `top`..`bottom`, so a color
    /// depends on height alone; `colors` run from top to bottom.
    pub fn fill_rounds_vertical(&self, rects: &[Rect], radius: f32, top: f32, bottom: f32, colors: &[Color]) {
        self.fill_rounds_with(rects, radius, (0.0, top), (0.0, bottom), colors);
    }

    /// Rounded rectangles filled from one linear gradient running from `start` to `end`, clamped
    /// beyond them; colors blend in sRGB like Direct2D's gamma 2.2 stops.
    fn fill_rounds_with(&self, rects: &[Rect], radius: f32, start: (f32, f32), end: (f32, f32), colors: &[Color]) {
        let rects: Vec<&Rect> = rects.iter().filter(|r| r.w() > 0.0 && r.h() > 0.0).collect();
        if rects.is_empty() || colors.is_empty() {
            return;
        }
        let components: Vec<CGFloat> = colors.iter().flat_map(|c| [c.r, c.g, c.b, c.a].map(|v| v as CGFloat)).collect();
        let locations: Vec<CGFloat> =
            (0..colors.len()).map(|i| i as CGFloat / (colors.len().max(2) - 1) as CGFloat).collect();
        // SAFETY: four components per location, both arrays `colors.len()` long.
        let Some(gradient) =
            (unsafe { CGGradient::with_color_components(srgb().as_deref(), components.as_ptr(), locations.as_ptr(), colors.len()) })
        else {
            return;
        };
        let ctx = self.c();
        CGContext::save_g_state(Some(&ctx));
        CGContext::begin_path(Some(&ctx));
        for rect in rects {
            CGContext::add_path(Some(&ctx), Some(&Self::round_path(*rect, radius)));
        }
        CGContext::clip(Some(&ctx));
        CGContext::draw_linear_gradient(
            Some(&ctx),
            Some(&gradient),
            CGPoint::new(start.0 as CGFloat, start.1 as CGFloat),
            CGPoint::new(end.0 as CGFloat, end.1 as CGFloat),
            CGGradientDrawingOptions::DrawsBeforeStartLocation | CGGradientDrawingOptions::DrawsAfterEndLocation,
        );
        CGContext::restore_g_state(Some(&ctx));
    }

    fn attributed(&self, text: &str, style: TextStyle) -> Option<CFRetained<CFAttributedString>> {
        let font: &CFType = &self.styles[style as usize].font;
        // SAFETY: reads Core Text and Core Foundation constants.
        let (font_key, color_key, yes) =
            unsafe { (kCTFontAttributeName, kCTForegroundColorFromContextAttributeName, kCFBooleanTrue?) };
        let yes: &CFType = yes;
        let attributes = CFDictionary::<CFString, CFType>::from_slices(&[font_key, color_key], &[font, yes]);
        let string = CFString::from_str(text);
        // SAFETY: a string and a dictionary of CF keys and values.
        unsafe { CFAttributedString::new(None, Some(&string), Some(attributes.as_opaque())) }
    }

    fn line(&self, text: &str, style: TextStyle) -> Option<CFRetained<CTLine>> {
        let attributed = self.attributed(text, style)?;
        // SAFETY: a valid attributed string.
        Some(unsafe { CTLine::with_attributed_string(&attributed) })
    }

    fn line_width(line: &CTLine) -> f32 {
        // SAFETY: plain metric query; the out pointers may be null.
        unsafe { line.typographic_bounds(ptr::null_mut(), ptr::null_mut(), ptr::null_mut()) as f32 }
    }

    /// Draws a line with its baseline starting at (x, y), in the current fill color.
    fn draw_line(ctx: &CGContext, line: &CTLine, x: f32, y: f32) {
        CGContext::save_g_state(Some(ctx));
        CGContext::translate_ctm(Some(ctx), x as CGFloat, y as CGFloat);
        // Core Text draws upright in an unflipped space.
        CGContext::scale_ctm(Some(ctx), 1.0, -1.0);
        CGContext::set_text_matrix(Some(ctx), CGAffineTransform { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 });
        CGContext::set_text_position(Some(ctx), 0.0, 0.0);
        // SAFETY: drawing into a live context.
        unsafe { line.draw(ctx) };
        CGContext::restore_g_state(Some(ctx));
    }

    /// Text vertically centered in `rect` (capitals and digits centered, as on Windows); single
    /// lines end with an ellipsis when cut.
    pub fn text(&self, text: &str, rect: Rect, style: TextStyle, color: Color, align: Align) {
        if text.is_empty() || color.a <= 0.0 {
            return;
        }
        let Some(mut line) = self.line(text, style) else { return };
        let mut width = Self::line_width(&line);
        let avail = rect.w().max(0.0);
        if width > avail {
            let Some(token) = self.line("\u{2026}", style) else { return };
            // SAFETY: valid lines; returns None when not even the ellipsis fits.
            match unsafe { line.truncated_line(avail as f64, CTLineTruncationType::End, Some(&token)) } {
                Some(cut) => line = cut,
                None => return,
            }
            width = Self::line_width(&line);
        }
        let x = match align {
            Align::Leading => rect.l,
            Align::Center => rect.l + (avail - width) / 2.0,
            Align::Trailing => rect.r - width,
        };
        let baseline = rect.cy() + self.styles[style as usize].half_cap;
        let ctx = self.c();
        CGContext::save_g_state(Some(&ctx));
        CGContext::clip_to_rect(Some(&ctx), cg_rect(Rect::new(rect.l, rect.t - 1e4, rect.r, rect.b + 1e4)));
        self.fill_color(&ctx, color);
        Self::draw_line(&ctx, &line, x, baseline);
        CGContext::restore_g_state(Some(&ctx));
    }

    /// The lines `text` wraps into at `width`.
    fn wrap(&self, text: &str, style: TextStyle, width: f32) -> Vec<CFRetained<CTLine>> {
        let Some(attributed) = self.attributed(text, style) else { return Vec::new() };
        // SAFETY: a valid attributed string; ranges stay within it.
        unsafe {
            let typesetter = CTTypesetter::with_attributed_string(&attributed);
            let length = attributed.length();
            let mut lines = Vec::new();
            let mut start: CFIndex = 0;
            while start < length {
                let count = typesetter.suggest_line_break(start, width.max(1.0) as f64).max(1);
                lines.push(typesetter.line(CFRange { location: start, length: count }));
                start += count;
            }
            lines
        }
    }

    fn line_height(&self, style: TextStyle) -> f32 {
        (self.styles[style as usize].size * LINE_HEIGHT).round()
    }

    /// Wrapping text, top-aligned within the rectangle's height.
    pub fn text_wrapped(&self, text: &str, rect: Rect, style: TextStyle, color: Color) {
        if text.is_empty() || color.a <= 0.0 {
            return;
        }
        let lh = self.line_height(style);
        let half_cap = self.styles[style as usize].half_cap;
        let ctx = self.c();
        CGContext::save_g_state(Some(&ctx));
        self.fill_color(&ctx, color);
        for (i, line) in self.wrap(text, style, rect.w()).iter().enumerate() {
            let baseline = rect.t + i as f32 * lh + lh / 2.0 + half_cap;
            Self::draw_line(&ctx, line, rect.l, baseline);
        }
        CGContext::restore_g_state(Some(&ctx));
    }

    pub fn measure(&self, text: &str, style: TextStyle) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        self.line(text, style).map_or(0.0, |line| Self::line_width(&line))
    }

    pub fn measure_height(&self, text: &str, style: TextStyle, width: f32) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        self.wrap(text, style, width).len() as f32 * self.line_height(style)
    }

    /// Pixels per point of the attached context.
    fn device_scale(ctx: &CGContext) -> f32 {
        let m = CGContext::user_space_to_device_space_transform(Some(ctx));
        (m.a * m.a + m.b * m.b).sqrt().max(1.0) as f32
    }

    fn rasterize(glyph: char, size: f32, scale: f32) -> Option<Symbol> {
        let image = symbol_image(glyph, size * SYMBOL_SCALE)?;
        let NSSize { width, height } = image.size();
        let mut rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width * scale as CGFloat, height * scale as CGFloat));
        // SAFETY: a valid rect pointer; no reference context or hints.
        let mask = unsafe { image.CGImageForProposedRect_context_hints(&mut rect, None, None) }?;
        Some(Symbol { mask, w: width as f32, h: height as f32 })
    }

    /// A Fluent icon glyph, drawn as the matching SF Symbol centered on a point.
    pub fn glyph(&self, glyph: char, cx: f32, cy: f32, size: f32, color: Color) {
        if color.a <= 0.0 || size <= 0.0 {
            return;
        }
        let ctx = self.c();
        let scale = Self::device_scale(&ctx);
        let key = (glyph, (size * 4.0).round() as u32, (scale * 4.0).round() as u32);
        let mut symbols = self.symbols.borrow_mut();
        let Some(symbol) = symbols.entry(key).or_insert_with(|| Self::rasterize(glyph, size, scale)) else { return };
        let (w, h) = (symbol.w as CGFloat, symbol.h as CGFloat);
        CGContext::save_g_state(Some(&ctx));
        // Images draw upright in an unflipped space.
        CGContext::translate_ctm(Some(&ctx), cx as CGFloat - w / 2.0, cy as CGFloat + h / 2.0);
        CGContext::scale_ctm(Some(&ctx), 1.0, -1.0);
        let bounds = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w, h));
        CGContext::clip_to_mask(Some(&ctx), bounds, Some(&symbol.mask));
        self.fill_color(&ctx, color);
        CGContext::fill_rect(Some(&ctx), bounds);
        CGContext::restore_g_state(Some(&ctx));
    }

    pub fn push_clip(&self, rect: Rect) {
        let ctx = self.c();
        CGContext::save_g_state(Some(&ctx));
        CGContext::clip_to_rect(Some(&ctx), cg_rect(rect));
        self.clips.borrow_mut().push(self.offset.get());
    }

    pub fn pop_clip(&self) {
        let Some(pushed) = self.clips.borrow_mut().pop() else { return };
        let ctx = self.c();
        // Restoring brings back the transform of the push; keep the offset set since then.
        CGContext::restore_g_state(Some(&ctx));
        let now = self.offset.get();
        self.offset.set(pushed);
        self.set_offset(now.0, now.1);
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::c_void;

    use objc2::AllocAnyThread;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep};
    use objc2_core_graphics::{CGBitmapContextCreate, CGBitmapContextCreateImage, CGImageAlphaInfo, CGImageByteOrderInfo};
    use objc2_foundation::NSDictionary;

    use super::*;
    use crate::ui::glyph;

    const W: usize = 420;
    const H: usize = 320;
    const SCALE: usize = 2;

    struct Bitmap {
        pixels: Vec<u8>,
        ctx: CFRetained<CGContext>,
    }

    impl Bitmap {
        /// A surface W x H points at 2 pixels per point, like a Retina window.
        fn new() -> Self {
            let (pw, ph) = (W * SCALE, H * SCALE);
            let mut pixels = vec![0u8; pw * ph * 4];
            let info = CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0;
            // SAFETY: `pixels` lives in the returned struct beside the context; its heap buffer
            // does not move.
            let ctx =
                unsafe { CGBitmapContextCreate(pixels.as_mut_ptr() as *mut c_void, pw, ph, 8, pw * 4, srgb().as_deref(), info) }
                    .expect("bitmap context");
            CGContext::scale_ctm(Some(&ctx), SCALE as CGFloat, SCALE as CGFloat);
            Self { pixels, ctx }
        }

        /// The pixel at (x, y) points from the top left.
        fn at(&self, x: f32, y: f32) -> [u8; 4] {
            let (px, py) = ((x * SCALE as f32) as usize, (y * SCALE as f32) as usize);
            let i = (py * W * SCALE + px) * 4;
            [self.pixels[i], self.pixels[i + 1], self.pixels[i + 2], self.pixels[i + 3]]
        }

        /// The center, in points, of the pixels darker than mid gray inside `area`.
        fn ink_center(&self, area: Rect) -> Option<(f32, f32)> {
            let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
            let s = SCALE as f32;
            for py in (area.t * s) as usize..(area.b * s) as usize {
                for px in (area.l * s) as usize..(area.r * s) as usize {
                    let i = (py * W * SCALE + px) * 4;
                    if self.pixels[i + 1] < 128 {
                        sx += (px as f32 + 0.5) / s;
                        sy += (py as f32 + 0.5) / s;
                        n += 1.0;
                    }
                }
            }
            (n > 0.0).then(|| (sx / n, sy / n))
        }

        /// Saves the surface as target/<name> to look at.
        fn save_png(&self, name: &str) {
            let image = CGBitmapContextCreateImage(Some(&self.ctx)).expect("image");
            let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &image);
            // SAFETY: an empty properties dictionary.
            let data =
                unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }.expect("png");
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(name), data.to_vec()).unwrap();
        }
    }

    const RED: Color = Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
    const BLUE: Color = Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 };
    const INK: Color = Color { r: 0.1, g: 0.1, b: 0.12, a: 1.0 };

    #[test]
    fn draws_shapes_text_and_symbols() {
        let b = Bitmap::new();
        let c = Canvas::new().unwrap();
        c.attach(&b.ctx, H as f32);
        c.begin();
        c.clear(Color::rgb8(255, 255, 255));

        // Shapes along the top.
        c.fill_rect(Rect::new(10.0, 10.0, 50.0, 50.0), RED);
        c.fill_round(Rect::new(60.0, 10.0, 100.0, 50.0), 12.0, BLUE);
        c.fill_shape(Rect::new(110.0, 10.0, 150.0, 50.0), Corners { tl: 0.0, tr: 16.0, br: 16.0, bl: 16.0 }, INK);
        c.fill_circle(180.0, 30.0, 20.0, RED);
        c.stroke_round(Rect::new(210.0, 10.0, 250.0, 50.0), 10.0, BLUE, 2.0);
        c.fill_round_gradient(Rect::new(260.0, 10.0, 300.0, 50.0), 8.0, &[RED, BLUE], true);
        c.fill_rounds_vertical(&[Rect::new(310.0, 30.0, 320.0, 50.0), Rect::new(325.0, 10.0, 335.0, 50.0)], 3.0, 10.0, 50.0, &[RED, BLUE]);
        // A clip and an offset.
        c.push_clip(Rect::new(350.0, 10.0, 370.0, 50.0));
        c.set_offset(5.0, 0.0);
        c.fill_rect(Rect::new(340.0, 10.0, 410.0, 20.0), BLUE);
        c.pop_clip();
        c.fill_rect(Rect::new(340.0, 40.0, 350.0, 50.0), RED);
        c.set_offset(0.0, 0.0);

        // The type scale, each line beside a guide at the middle of its rectangle.
        let styles = [
            TextStyle::TitleLarge,
            TextStyle::TitleSmall,
            TextStyle::BodyLarge,
            TextStyle::BodyMedium,
            TextStyle::BodySmall,
            TextStyle::LabelLarge,
            TextStyle::LabelMedium,
        ];
        for (i, style) in styles.iter().enumerate() {
            let row = Rect::new(10.0, 64.0 + i as f32 * 26.0, 200.0, 88.0 + i as f32 * 26.0);
            c.fill_rect(Rect::new(row.l, row.cy() - 0.25, row.l + 4.0, row.cy() + 0.25), RED);
            c.text("H0 Wootlight", Rect::new(row.l + 8.0, row.t, row.r, row.b), *style, INK, Align::Leading);
        }
        c.text("Centered", Rect::new(210.0, 64.0, 410.0, 88.0), TextStyle::BodyLarge, INK, Align::Center);
        c.text("Trailing", Rect::new(210.0, 90.0, 410.0, 114.0), TextStyle::BodyLarge, INK, Align::Trailing);
        c.text("A long line cut with an ellipsis at the end", Rect::new(210.0, 116.0, 330.0, 140.0), TextStyle::BodyMedium, INK, Align::Leading);
        let para = "Wrapped text flows over several lines and starts at the top of its rectangle.";
        let h = c.measure_height(para, TextStyle::BodyMedium, 190.0);
        c.fill_rect(Rect::new(210.0, 150.0, 400.0, 150.0 + h), Color::rgb8(230, 236, 255));
        c.text_wrapped(para, Rect::new(210.0, 150.0, 400.0, 250.0), TextStyle::BodyMedium, INK);

        // Every glyph, centered on a crosshair.
        let glyphs = [
            glyph::SETTINGS,
            glyph::PALETTE,
            glyph::EQUALIZER,
            glyph::CHECK,
            glyph::CHEVRON_RIGHT,
            glyph::CHEVRON_DOWN,
            glyph::MINIMIZE,
            glyph::CLOSE,
            glyph::OPEN_WINDOW,
            glyph::POWER,
            glyph::DOCUMENT,
            glyph::SYNC,
            glyph::EDIT,
            glyph::FOLDER,
            glyph::BUG,
            glyph::ADD,
            glyph::DELETE,
        ];
        for (i, g) in glyphs.iter().enumerate() {
            let (cx, cy) = (20.0 + i as f32 * 23.0, 290.0);
            c.fill_rect(Rect::new(cx - 10.0, cy - 0.25, cx + 10.0, cy + 0.25), Color::rgb8(255, 170, 170));
            c.fill_rect(Rect::new(cx - 0.25, cy - 10.0, cx + 0.25, cy + 10.0), Color::rgb8(255, 170, 170));
            c.glyph(*g, cx, cy, 16.0, INK);
        }
        assert!(c.end());
        c.detach();
        b.save_png("canvas-test.png");

        // Shapes.
        assert_eq!(b.at(30.0, 30.0), [255, 0, 0, 255]);
        assert_eq!(b.at(61.0, 11.0), [255, 255, 255, 255], "rounded corner stays empty");
        assert_eq!(b.at(80.0, 30.0)[2], 255);
        assert!(b.at(110.5, 10.5)[0] < 60, "square top-left corner is filled");
        assert_eq!(b.at(149.5, 49.5), [255, 255, 255, 255], "round bottom-right corner stays empty");
        assert!(b.at(280.0, 12.0)[0] > 200 && b.at(280.0, 48.0)[2] > 200, "gradient runs red to blue down");
        // One gradient over both bars: the short bar's top matches the tall bar at that height.
        let (short, tall) = (b.at(315.0, 31.0), b.at(330.0, 31.0));
        assert!(short.iter().zip(tall).all(|(a, b)| a.abs_diff(b) <= 8), "{short:?} {tall:?}");
        // The clip keeps the offset fill inside 350..370; the offset survives the pop.
        assert_eq!(b.at(352.0, 15.0)[2], 255);
        assert_eq!(b.at(375.0, 15.0), [255, 255, 255, 255]);
        assert_eq!(b.at(343.0, 45.0), [255, 255, 255, 255]);
        assert_eq!(b.at(352.0, 45.0)[0], 255);

        // Capitals and digits sit on the middle of their line.
        for i in 0..styles.len() {
            let top = 64.0 + i as f32 * 26.0;
            let (_, y) = b.ink_center(Rect::new(18.0, top, 36.0, top + 24.0)).expect("text ink");
            assert!((y - (top + 12.0)).abs() <= 1.0, "style {i}: ink center {y}, line center {}", top + 12.0);
        }
        let (x, _) = b.ink_center(Rect::new(210.0, 64.0, 410.0, 88.0)).unwrap();
        assert!((x - 310.0).abs() < 3.0, "centered text at {x}");
        let (x, _) = b.ink_center(Rect::new(210.0, 90.0, 410.0, 114.0)).unwrap();
        assert!(x > 360.0, "trailing text at {x}");
        assert!(b.ink_center(Rect::new(331.0, 116.0, 410.0, 140.0)).is_none(), "cut text stays in its rectangle");
        assert!(h >= 3.0 * 17.0, "wrapped height {h}");
        assert!(b.ink_center(Rect::new(210.0, 150.0 + h - 17.0, 400.0, 150.0 + h)).is_some(), "last line drawn");

        // Symbols land on their points.
        for (i, g) in glyphs.iter().enumerate() {
            let (cx, cy) = (20.0 + i as f32 * 23.0, 290.0);
            let (x, y) = b.ink_center(Rect::new(cx - 11.0, cy - 11.0, cx + 11.0, cy + 11.0)).expect("glyph ink");
            assert!((x - cx).abs() < 2.5 && (y - cy).abs() < 2.5, "glyph {:X} centered at ({x}, {y})", *g as u32);
        }
        assert!(c.measure("Wootlight", TextStyle::BodyLarge) > 40.0);
    }
}
