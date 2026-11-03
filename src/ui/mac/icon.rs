//! The app mark (see `mark`) rendered with Core Graphics for the menu bar and the window, and the
//! light or dark appearance it is drawn for.

use std::{cell::RefCell, ffi::c_void};

use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSView};
use objc2_core_graphics::{CGBitmapContextCreate, CGImageAlphaInfo, CGImageByteOrderInfo};
use objc2_foundation::{NSArray, NSString, NSUserDefaults};

use super::{canvas::Canvas, palette::Color};
pub use super::mark::{Mark, draw_mark};

thread_local! {
    static CANVAS: RefCell<Option<Canvas>> = const { RefCell::new(None) };
    /// A view in the menu bar (the status item's button), whose appearance follows the menu bar.
    static MENU_BAR_VIEW: RefCell<Option<Retained<NSView>>> = const { RefCell::new(None) };
}

/// Releases the icon canvas; kept for parity with Windows, where the order of teardown matters.
pub fn release_canvas() {
    let taken = CANVAS.with(|cell| cell.try_borrow_mut().ok().and_then(|mut slot| slot.take()));
    drop(taken);
}

/// Lets `taskbar_is_dark` follow the menu bar, which takes its tone from the wallpaper.
pub fn set_menu_bar_view(view: Option<Retained<NSView>>) {
    MENU_BAR_VIEW.with(|cell| *cell.borrow_mut() = view);
}

fn is_dark(appearance: &NSAppearance) -> bool {
    // SAFETY: reads AppKit constants.
    let names = unsafe { NSArray::from_slice(&[NSAppearanceNameAqua, NSAppearanceNameDarkAqua]) };
    appearance
        .bestMatchFromAppearancesWithNames(&names)
        // SAFETY: reads an AppKit constant.
        .is_some_and(|name| &*name == unsafe { NSAppearanceNameDarkAqua })
}

/// The system-wide setting, readable from any thread and before the app runs.
fn interface_style_dark() -> bool {
    let style = NSUserDefaults::standardUserDefaults().stringForKey(&NSString::from_str("AppleInterfaceStyle"));
    style.is_some_and(|s| s.to_string().eq_ignore_ascii_case("dark"))
}

/// Whether the menu bar is dark.
pub fn taskbar_is_dark() -> bool {
    if MainThreadMarker::new().is_some() {
        if let Some(dark) = MENU_BAR_VIEW.with(|cell| cell.borrow().as_ref().map(|v| is_dark(&v.effectiveAppearance()))) {
            return dark;
        }
    }
    apps_prefer_dark()
}

/// Whether apps use the dark appearance.
pub fn apps_prefer_dark() -> bool {
    match MainThreadMarker::new() {
        Some(mtm) => is_dark(&NSApplication::sharedApplication(mtm).effectiveAppearance()),
        None => interface_style_dark(),
    }
}

/// Renders the mark to straight-alpha RGBA pixels, one point per pixel.
pub fn render_rgba(size: u32, mark: Mark, seed: [f32; 3], dark_background: bool) -> Vec<u8> {
    let n = size as usize;
    let mut out = vec![0u8; n * n * 4];
    CANVAS.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Canvas::new().ok();
        }
        let Some(canvas) = slot.as_ref() else { return };
        let info = CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0;
        // SAFETY: `out` outlives the context, which writes n x n premultiplied RGBA pixels into it.
        let Some(ctx) = (unsafe {
            CGBitmapContextCreate(out.as_mut_ptr() as *mut c_void, n, n, 8, n * 4, srgb().as_deref(), info)
        }) else {
            return;
        };
        canvas.attach(&ctx, size as f32);
        canvas.begin();
        canvas.clear(Color::TRANSPARENT);
        draw_mark(canvas, 0.0, 0.0, size as f32, mark, seed, dark_background);
        canvas.end();
        canvas.detach();
    });
    for px in out.chunks_exact_mut(4) {
        // Premultiplied to straight alpha.
        let a = px[3] as u32;
        if a != 0 && a != 255 {
            for v in &mut px[..3] {
                *v = ((*v as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    out
}

fn srgb() -> Option<objc2_core_foundation::CFRetained<objc2_core_graphics::CGColorSpace>> {
    // SAFETY: reads a Core Graphics constant.
    objc2_core_graphics::CGColorSpace::with_name(Some(unsafe { objc2_core_graphics::kCGColorSpaceSRGB }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_renders_opaque_pixels() {
        let rgba = render_rgba(32, Mark::Live, [255.0, 42.0, 138.0], true);
        let opaque = rgba.chunks_exact(4).filter(|p| p[3] > 200).count();
        assert!(opaque > 300, "only {opaque} opaque pixels");
        // The center column of the middle bar is the dark ink, the cap around it is light; rows
        // count from the top as on Windows.
        let px = |x: usize, y: usize| &rgba[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4];
        assert!(px(16, 20)[0] < 100, "{:?}", px(16, 20));
        assert!(px(4, 16)[0] > 150, "{:?}", px(4, 16));
        // The bars stand on the bottom: above the tallest one the cap shows.
        assert!(px(16, 4)[0] > 150, "{:?}", px(16, 4));
    }
}
