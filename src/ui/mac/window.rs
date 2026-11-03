//! The main window's AppKit host: an NSWindow with a transparent, full-size title bar and the
//! system's traffic lights, whose content view feeds mouse, trackpad and keys to the shared
//! `WindowCore` and draws it with Core Graphics. Closing the window hides it; the app stays in the
//! menu bar. Frames follow the screen's refresh rate while the window is visible.

use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use objc2::{
    MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{NSObject, NSObjectProtocol, ProtocolObject},
};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication,
    NSBackingStoreType, NSColor, NSEvent, NSEventModifierFlags, NSGraphicsContext, NSResponder, NSScreen, NSToolbar,
    NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow, NSWindowButton, NSWindowDelegate, NSWindowStyleMask,
    NSWindowTitleVisibility, NSWindowToolbarStyle,
};
use objc2_core_graphics::CGContext;
use objc2_foundation::{NSNotification, NSPoint, NSRect, NSSize, ns_string};

use super::{
    canvas::Canvas,
    design::*,
    frame_clock::{FrameClock, refresh_rate},
    palette::Color,
    palette_for, wants_dark,
    window_core::{After, Frame, Key, WHEEL_STEP, WindowCore},
};
use crate::engine::Shared;

/// Room between the traffic lights and the app mark in the header.
const MARK_GAP: f64 = 14.0;
/// Where the header draws the mark when nothing sits left of it.
const MARK_LEFT: f64 = 24.0;

/// Display refreshes seen and frames drawn, for diagnostics.
static TICKS: AtomicU64 = AtomicU64::new(0);
static DRAWS: AtomicU64 = AtomicU64::new(0);

struct Host {
    window: Retained<NSWindow>,
    view: Retained<ContentView>,
    _delegate: Retained<WindowDelegate>,
    canvas: Canvas,
    clock: FrameClock,
    dark: bool,
    core: WindowCore,
}

thread_local! {
    static WINDOW: RefCell<Option<Host>> = const { RefCell::new(None) };
}

/// Runs `f` on the window state unless it is missing or already borrowed (a nested callback).
fn with_host<R>(f: impl FnOnce(&mut Host) -> R) -> Option<R> {
    WINDOW.with(|cell| cell.try_borrow_mut().ok().and_then(|mut slot| slot.as_mut().map(f)))
}

define_class!(
    /// The window's only view: flipped, so points run from the top left like DIPs on Windows.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WootlightContentView"]
    struct ContentView;

    impl ContentView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        /// The click that activates the window also presses what it lands on, as on Windows.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            with_host(Host::draw);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let (x, y) = self.point(event);
            let drag = with_host(|h| !h.core.pointer_down(x, y) && h.core.is_drag_area(x, y)).unwrap_or(false);
            if drag {
                if let Some(window) = self.window() {
                    window.performWindowDragWithEvent(event);
                }
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let (x, y) = self.point(event);
            apply(with_host(|h| h.core.pointer_up(x, y)));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let (x, y) = self.point(event);
            with_host(|h| h.core.pointer_move(x, y));
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let (x, y) = self.point(event);
            with_host(|h| h.core.pointer_move(x, y));
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            with_host(|h| h.core.pointer_leave());
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let dy = event.scrollingDeltaY() as f32;
            if event.hasPreciseScrollingDeltas() {
                // Trackpads and Magic Mice report points, momentum included.
                with_host(|h| h.core.scroll_precise(dy));
            } else {
                // A wheel reports lines; a notch is one line, like a 120 step on Windows.
                with_host(|h| h.core.wheel(dy * WHEEL_STEP));
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let flags = event.modifierFlags();
            if flags.contains(NSEventModifierFlags::Command) {
                let chars = event.charactersIgnoringModifiers().map(|s| s.to_string()).unwrap_or_default();
                // Cmd-Q and Cmd-W are key equivalents of the main menu and never get here.
                if chars == "m" {
                    apply(Some(After::Minimize));
                }
                return;
            }
            let key = match event.keyCode() {
                48 => Key::Tab,
                53 => Key::Escape,
                36 | 76 => Key::Enter,
                49 => Key::Space,
                123 => Key::Left,
                124 => Key::Right,
                125 => Key::Down,
                126 => Key::Up,
                _ => return,
            };
            let shift = flags.contains(NSEventModifierFlags::Shift);
            apply(with_host(|h| h.core.key(key, shift)));
        }
    }
);

impl ContentView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSView's designated initializer.
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        // Hover follows the pointer even while another app is active, as on Windows.
        let options = NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::MouseMoved
            | NSTrackingAreaOptions::ActiveAlways
            | NSTrackingAreaOptions::InVisibleRect;
        // SAFETY: the tracking area's owner is the view that holds it.
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(mtm.alloc(), NSRect::ZERO, options, Some(&view), None)
        };
        view.addTrackingArea(&area);
        view.setWantsLayer(true);
        view
    }

    /// The event's location in points from the top left of the view.
    fn point(&self, event: &NSEvent) -> (f32, f32) {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        (p.x as f32, p.y as f32)
    }
}

define_class!(
    /// Hides instead of closing and pauses the frames while the window is in the Dock.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WootlightWindowDelegate"]
    struct WindowDelegate;

    unsafe impl NSObjectProtocol for WindowDelegate {}

    unsafe impl NSWindowDelegate for WindowDelegate {
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            hide();
            false
        }

        #[unsafe(method(windowDidMiniaturize:))]
        fn window_did_miniaturize(&self, _notification: &NSNotification) {
            with_host(Host::stop_frames);
        }

        #[unsafe(method(windowDidDeminiaturize:))]
        fn window_did_deminiaturize(&self, _notification: &NSNotification) {
            with_host(Host::start_frames);
        }

        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            with_host(|h| h.core.deactivate());
        }

        /// The window may now be on a screen with another refresh rate.
        #[unsafe(method(windowDidChangeScreen:))]
        fn window_did_change_screen(&self, _notification: &NSNotification) {
            with_host(|h| {
                if h.window.isVisible() && !h.window.isMiniaturized() {
                    h.start_frames();
                }
            });
        }

        #[unsafe(method(windowDidChangeBackingProperties:))]
        fn window_did_change_backing_properties(&self, _notification: &NSNotification) {
            with_host(|h| h.core.dirty = true);
        }
    }
);

impl WindowDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: plain NSObject initialization of our subclass.
        unsafe { msg_send![super(this), init] }
    }
}

/// Does what an input asked of the native window, outside the state borrow.
fn apply(after: Option<After>) {
    match after {
        Some(After::Hide) => hide(),
        Some(After::Minimize) => {
            if let Some(window) = with_host(|h| h.window.clone()) {
                window.miniaturize(None);
            }
        }
        Some(After::None) | None => {}
    }
}

fn ns_color(c: Color) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(c.r as f64, c.g as f64, c.b as f64, c.a as f64)
}

/// Matches the title bar (the traffic lights' inactive look) to the window's theme.
fn set_theme(window: &NSWindow, dark: bool, background: Color) {
    // SAFETY: the appearance names are AppKit constants.
    let name = unsafe { if dark { NSAppearanceNameDarkAqua } else { NSAppearanceNameAqua } };
    window.setAppearance(NSAppearance::appearanceNamed(name).as_deref());
    window.setBackgroundColor(Some(&ns_color(background)));
}

/// Room the header leaves left of its mark so the traffic lights stay clear of it.
fn traffic_lights_inset(window: &NSWindow) -> f32 {
    let right = [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton]
        .into_iter()
        .filter_map(|b| window.standardWindowButton(b))
        .map(|b| {
            let frame = b.convertRect_toView(b.bounds(), None);
            frame.origin.x + frame.size.width
        })
        .fold(0.0, f64::max);
    if right <= 0.0 { 0.0 } else { (right + MARK_GAP - MARK_LEFT).max(0.0) as f32 }
}

fn create(shared: Arc<Shared>, devices: Vec<String>) -> Option<Host> {
    let Some(mtm) = MainThreadMarker::new() else {
        crate::log!("window creation failed: not on the main thread");
        return None;
    };
    let settings = shared.settings();
    let p = palette_for(&settings);
    let work_h = NSScreen::mainScreen(mtm).map_or(WINDOW_MAX_HEIGHT as f64, |s| s.visibleFrame().size.height) as f32;
    let height = (work_h - 48.0).clamp(WINDOW_MIN_HEIGHT, WINDOW_MAX_HEIGHT);
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WINDOW_WIDTH as f64, height as f64));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::FullSizeContentView;
    // SAFETY: a new window owned by the returned `Retained`; it is not released on close.
    let window = unsafe {
        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        );
        window.setReleasedWhenClosed(false);
        window
    };
    window.setTitle(ns_string!("Wootlight"));
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    window.setTitlebarAppearsTransparent(true);
    // An empty unified toolbar makes the title bar as tall as the header, which centers the
    // traffic lights on the header's mark and name.
    let toolbar = NSToolbar::new(mtm);
    window.setToolbar(Some(&toolbar));
    window.setToolbarStyle(NSWindowToolbarStyle::Unified);
    set_theme(&window, p.dark, p.surface);

    let view = ContentView::new(mtm, rect);
    window.setContentView(Some(&view));
    window.makeFirstResponder(Some(&view));
    let delegate = WindowDelegate::new(mtm);
    window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    window.center();

    let canvas = Canvas::new().map_err(|e| crate::log!("canvas creation failed: {e}")).ok()?;
    let clock = FrameClock::new(&view, || {
        TICKS.fetch_add(1, Ordering::Relaxed);
        with_host(Host::tick);
    });
    let mut core = WindowCore::new(shared, devices, height);
    core.window_buttons = false;
    core.header_inset = traffic_lights_inset(&window);
    Some(Host { window, view, _delegate: delegate, canvas, clock, dark: p.dark, core })
}

/// Shows the window, creating it on first use, and brings it and the app to the front.
pub fn show(shared: &Arc<Shared>, devices: Vec<String>) {
    let window = WINDOW.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = create(Arc::clone(shared), devices.clone());
        } else if let Some(h) = slot.as_mut() {
            h.core.set_devices(devices.clone());
        }
        slot.as_ref().map(|h| h.window.clone())
    });
    let Some(window) = window else { return };
    let mtm = MainThreadMarker::from(&*window);
    // An accessory app is not active on its own; the window would open behind others.
    NSApplication::sharedApplication(mtm).activate();
    if window.isMiniaturized() {
        window.deminiaturize(None);
    }
    window.makeKeyAndOrderFront(None);
    with_host(Host::start_frames);
}

pub fn is_visible() -> bool {
    WINDOW.with(|cell| {
        cell.try_borrow()
            .ok()
            .and_then(|slot| slot.as_ref().map(|h| h.window.isVisible() && !h.window.isMiniaturized()))
            .unwrap_or(false)
    })
}

pub fn hide() {
    let window = with_host(|h| {
        h.stop_frames();
        // No release will come for a press or drag in progress.
        h.core.pointer_cancel();
        h.window.clone()
    });
    if let Some(window) = window {
        window.orderOut(None);
    }
}

/// Redraws on the next frame if anything shown outside the live view changed.
pub fn refresh(config_error: Option<String>) {
    with_host(|h| {
        let settings = h.core.shared.settings();
        let dark = wants_dark(&settings);
        if dark != h.dark {
            h.dark = dark;
            set_theme(&h.window, dark, palette_for(&settings).surface);
        }
        h.core.refresh(config_error);
    });
}

pub fn destroy() {
    let taken = WINDOW.with(|cell| cell.try_borrow_mut().ok().and_then(|mut slot| slot.take()));
    if let Some(mut h) = taken {
        h.stop_frames();
        h.window.setDelegate(None);
        h.window.close();
    }
}

/// Display refreshes seen and frames drawn since launch, for diagnostics.
#[allow(dead_code)]
pub fn frame_stats() -> (u64, u64) {
    (TICKS.load(Ordering::Relaxed), DRAWS.load(Ordering::Relaxed))
}

impl Host {
    fn start_frames(&mut self) {
        let hz = refresh_rate(&self.window);
        crate::log!("window frames at {hz} Hz");
        self.clock.start(hz);
        self.core.shared.set_ui_hz(hz);
        self.core.dirty = true;
    }

    fn stop_frames(&mut self) {
        self.clock.stop();
        self.core.shared.set_ui_hz(0);
    }

    /// A display refresh: asks AppKit for a frame when something changed.
    fn tick(&mut self) {
        match self.core.frame() {
            Frame::None => {}
            // Core Graphics redraws the whole view; a live-only frame costs the same.
            Frame::Live | Frame::Full => self.view.setNeedsDisplay(true),
        }
    }

    fn draw(&mut self) {
        let Some(context) = NSGraphicsContext::currentContext() else { return };
        let cg = context.CGContext();
        let height = self.view.bounds().size.height as f32;
        let scale = self.window.backingScaleFactor() as f32;
        // The view is flipped for its events; the canvas takes the unflipped context of a plain
        // view and flips it itself, so AppKit's flip is undone around the frame.
        CGContext::save_g_state(Some(&cg));
        CGContext::translate_ctm(Some(&cg), 0.0, height as f64);
        CGContext::scale_ctm(Some(&cg), 1.0, -1.0);
        self.canvas.attach(&cg, height);
        self.canvas.begin();
        self.core.render(&self.canvas, scale);
        self.canvas.end();
        self.canvas.detach();
        CGContext::restore_g_state(Some(&cg));
        DRAWS.fetch_add(1, Ordering::Relaxed);
    }
}
