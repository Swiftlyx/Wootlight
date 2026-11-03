//! Calls the window back once per display refresh while running, so it animates at the refresh
//! rate of the screen that shows it (e.g. 120 Hz ProMotion) instead of a fixed timer. The ticks
//! come from the view's CADisplayLink (macOS 14+), which follows the window across screens and
//! fires on the main run loop.

use objc2::{
    DefinedClass, Encode, Encoding, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObject},
    sel,
};
use objc2_app_kit::{NSView, NSWindow};
use objc2_foundation::{NSRunLoop, NSRunLoopCommonModes};

const FALLBACK_HZ: u32 = 60;

/// Maximum refresh rate of the screen showing `window`.
pub fn refresh_rate(window: &NSWindow) -> u32 {
    window
        .screen()
        .map(|s| s.maximumFramesPerSecond())
        .filter(|hz| *hz > 1)
        .map_or(FALLBACK_HZ, |hz| hz as u32)
}

/// Core Animation's CAFrameRateRange.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct FrameRateRange {
    minimum: f32,
    maximum: f32,
    preferred: f32,
}

// SAFETY: matches the C layout and type encoding of CAFrameRateRange.
unsafe impl Encode for FrameRateRange {
    const ENCODING: Encoding = Encoding::Struct("CAFrameRateRange", &[f32::ENCODING, f32::ENCODING, f32::ENCODING]);
}

struct TargetIvars {
    tick: Box<dyn Fn()>,
}

define_class!(
    /// Receives the display link's callbacks.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WootlightFrameTarget"]
    #[ivars = TargetIvars]
    struct Target;

    impl Target {
        #[unsafe(method(tick:))]
        fn tick(&self, _link: &AnyObject) {
            (self.ivars().tick)();
        }
    }
);

pub struct FrameClock {
    /// The CADisplayLink; it retains its target.
    link: Retained<AnyObject>,
    running: bool,
    /// Kept alive here too, in case the link holds it weakly.
    _target: Retained<Target>,
}

impl FrameClock {
    /// A paused clock that calls `tick` on every refresh of the screen showing `view` once started.
    pub fn new(view: &NSView, tick: impl Fn() + 'static) -> Self {
        let mtm = MainThreadMarker::from(view);
        let target = Target::alloc(mtm).set_ivars(TargetIvars { tick: Box::new(tick) });
        // SAFETY: plain NSObject initialization of our subclass.
        let target: Retained<Target> = unsafe { msg_send![super(target), init] };
        // SAFETY: `tick:` is implemented by `Target` and takes the display link; the link is
        // paused before it joins the main run loop, in the modes that keep it ticking while a
        // window moves or a menu tracks.
        let link: Retained<AnyObject> = unsafe {
            let link: Retained<AnyObject> = msg_send![view, displayLinkWithTarget: &*target, selector: sel!(tick:)];
            let _: () = msg_send![&*link, setPaused: true];
            let _: () = msg_send![&*link, addToRunLoop: &*NSRunLoop::mainRunLoop(), forMode: NSRunLoopCommonModes];
            link
        };
        Self { link, running: false, _target: target }
    }

    /// Starts ticking, asking for `hz` frames per second.
    pub fn start(&mut self, hz: u32) {
        let hz = hz as f32;
        let range = FrameRateRange { minimum: hz.min(60.0), maximum: hz, preferred: hz };
        // SAFETY: plain property setters on our display link.
        unsafe {
            let _: () = msg_send![&*self.link, setPreferredFrameRateRange: range];
            let _: () = msg_send![&*self.link, setPaused: false];
        }
        self.running = true;
    }

    pub fn stop(&mut self) {
        if self.running {
            // SAFETY: plain property setter on our display link.
            unsafe {
                let _: () = msg_send![&*self.link, setPaused: true];
            }
            self.running = false;
        }
    }
}

impl Drop for FrameClock {
    fn drop(&mut self) {
        // SAFETY: removes the link from the run loop and releases its target.
        unsafe {
            let _: () = msg_send![&*self.link, invalidate];
        }
    }
}
