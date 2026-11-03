//! Opens the main window on macOS without the menu bar app, to check the AppKit host by hand:
//! `cargo run --example window`. With `--auto` it clicks, scrolls and types through the window by
//! itself, saves screenshots to the directory given after `--shots` and quits.
//!
//! The app is a binary crate, so this example compiles the same module tree from `src/`.

#![allow(dead_code, unused_imports, unused_macros)]

#[cfg(target_os = "macos")]
#[path = "../src/mac/audio.rs"]
mod audio;
#[cfg(target_os = "macos")]
#[path = "../src/audio_common.rs"]
mod audio_common;
#[cfg(target_os = "macos")]
#[path = "../src/mac/autostart.rs"]
mod autostart;
#[cfg(target_os = "macos")]
#[path = "../src/dsp.rs"]
mod dsp;
#[cfg(target_os = "macos")]
#[path = "../src/effects.rs"]
mod effects;
#[cfg(target_os = "macos")]
#[path = "../src/engine.rs"]
mod engine;
#[cfg(target_os = "macos")]
#[path = "../src/layout.rs"]
mod layout;
#[cfg(target_os = "macos")]
#[path = "../src/logging.rs"]
mod logging;
#[cfg(target_os = "macos")]
#[path = "../src/mac/media.rs"]
mod media;
#[cfg(target_os = "macos")]
#[path = "../src/paths.rs"]
mod paths;
#[cfg(target_os = "macos")]
#[path = "../src/mac/player.rs"]
mod player;
#[cfg(target_os = "macos")]
#[path = "../src/render.rs"]
mod render;
#[cfg(target_os = "macos")]
#[path = "../src/sdk.rs"]
mod sdk;
#[cfg(target_os = "macos")]
#[path = "../src/settings.rs"]
mod settings;
#[cfg(target_os = "macos")]
#[path = "../src/shell.rs"]
mod shell;
#[cfg(target_os = "macos")]
#[path = "../src/mac/tray.rs"]
mod tray;
#[cfg(target_os = "macos")]
#[path = "../src/ui/mod.rs"]
mod ui;
#[cfg(target_os = "macos")]
#[path = "../src/mac/platform.rs"]
mod win;

#[cfg(target_os = "macos")]
fn stop_engine() {}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    harness::run();
}

#[cfg(target_os = "macos")]
mod harness {
    use std::{
        cell::{Cell, RefCell},
        path::PathBuf,
        process,
        sync::Arc,
        time::Instant,
    };

    use objc2::{
        DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
        rc::Retained,
        runtime::{AnyObject, NSObject},
        sel,
    };
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBitmapImageFileType, NSEvent, NSEventModifierFlags, NSEventType, NSWindow,
    };
    use objc2_core_graphics::{CGEvent, CGScrollEventUnit};
    use objc2_foundation::{NSDictionary, NSPoint, NSString, NSTimer};

    use crate::{
        engine::Shared,
        settings::Settings,
        ui::{self, Command, window},
    };

    struct State {
        shared: Arc<Shared>,
        auto: bool,
        shots: PathBuf,
        start: Instant,
        step: Cell<usize>,
        shown: Cell<bool>,
        next_at: Cell<f64>,
        mark: Cell<(f64, u64, u64)>,
    }

    thread_local! {
        static STATE: RefCell<Option<Arc<State>>> = const { RefCell::new(None) };
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "WootlightHarnessTimer"]
        struct Ticker;

        impl Ticker {
            #[unsafe(method(tick:))]
            fn tick(&self, _timer: &AnyObject) {
                let state = STATE.with(|s| s.borrow().clone());
                if let Some(state) = state {
                    // Opened from the run loop, as the menu bar app does.
                    if !state.shown.replace(true) {
                        window::show(&state.shared, vec!["Studio Display Speakers".into(), "AirPods Pro".into()]);
                    }
                    pump(&state);
                    if state.auto {
                        script(&state);
                    }
                }
            }
        }
    );

    pub fn run() {
        let mtm = MainThreadMarker::new().expect("main thread");
        let args: Vec<String> = std::env::args().collect();
        let auto = args.iter().any(|a| a == "--auto");
        let shots = args
            .iter()
            .position(|a| a == "--shots")
            .and_then(|i| args.get(i + 1))
            .map_or_else(std::env::temp_dir, PathBuf::from);
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        let shared = Arc::new(Shared::new(Settings::default(), false));
        ui::init_ui_thread();
        let state = Arc::new(State {
            shared,
            auto,
            shots,
            start: Instant::now(),
            step: Cell::new(0),
            shown: Cell::new(false),
            next_at: Cell::new(1.5),
            mark: Cell::new((0.0, 0, 0)),
        });
        STATE.with(|s| *s.borrow_mut() = Some(state));
        let ticker = Ticker::alloc(mtm).set_ivars(());
        // SAFETY: plain NSObject initialization; the timer keeps its target alive.
        let ticker: Retained<Ticker> = unsafe { msg_send![super(ticker), init] };
        // SAFETY: `tick:` takes the timer.
        unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                1.0 / 30.0,
                &ticker,
                sel!(tick:),
                None,
                true,
            );
        }
        app.run();
    }

    /// What the menu bar app does with queued commands.
    fn pump(state: &State) {
        while let Some(command) = ui::take_command() {
            match command {
                Command::Apply { settings, .. } => {
                    state.shared.set_settings(*settings);
                    window::refresh(None);
                }
                Command::ToggleEnabled => {
                    let mut s = state.shared.settings();
                    s.enabled = !s.enabled;
                    state.shared.set_settings(s);
                    window::refresh(None);
                }
                Command::Quit => process::exit(0),
                other => println!("command: {other:?}"),
            }
        }
    }

    fn main_window() -> Option<Retained<NSWindow>> {
        let mtm = MainThreadMarker::new()?;
        let windows = NSApplication::sharedApplication(mtm).windows();
        windows.iter().find(|w| w.title().to_string() == "Wootlight")
    }

    /// A click at `x`, `y` points from the window's top left, through the window's event dispatch.
    fn click(x: f64, y: f64) {
        let Some(w) = main_window() else { return };
        let h = w.contentView().map_or(0.0, |v| v.bounds().size.height);
        let at = NSPoint::new(x, h - y);
        for kind in [NSEventType::LeftMouseDown, NSEventType::LeftMouseUp] {
            let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                kind,
                at,
                NSEventModifierFlags::empty(),
                0.0,
                w.windowNumber(),
                None,
                0,
                1,
                1.0,
            );
            if let Some(e) = event {
                w.sendEvent(&e);
            }
        }
    }

    fn key(code: u16, chars: &str, flags: NSEventModifierFlags) {
        let Some(w) = main_window() else { return };
        let s = NSString::from_str(chars);
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            NSEventType::KeyDown,
            NSPoint::new(0.0, 0.0),
            flags,
            0.0,
            w.windowNumber(),
            None,
            &s,
            &s,
            false,
            code,
        );
        if let Some(e) = event {
            w.sendEvent(&e);
        }
    }

    /// A scroll of `dy` (pixels if `precise`, else lines) delivered to the content view.
    fn scroll(dy: i32, precise: bool) {
        let Some(w) = main_window() else { return };
        let unit = if precise { CGScrollEventUnit::Pixel } else { CGScrollEventUnit::Line };
        let Some(cg) = CGEvent::new_scroll_wheel_event2(None, unit, 1, dy, 0, 0) else { return };
        let Some(event) = NSEvent::eventWithCGEvent(&cg) else { return };
        println!("scroll {dy} precise={} deltaY={}", event.hasPreciseScrollingDeltas(), event.scrollingDeltaY());
        if let Some(view) = w.contentView() {
            // SAFETY: the content view implements scrollWheel: with an NSEvent.
            unsafe {
                let _: () = msg_send![&*view, scrollWheel: &*event];
            }
        }
    }

    fn shot(state: &State, name: &str) {
        let Some(w) = main_window() else { return };
        let path = state.shots.join(format!("{name}.png"));
        let ok = process::Command::new("screencapture")
            .args(["-x", "-o", &format!("-l{}", w.windowNumber())])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            println!("screenshot {} saved", path.display());
            return;
        }
        // Without Screen Recording access (or with the screen locked) the window draws itself,
        // title bar included, into a bitmap instead.
        // SAFETY: the content view sits in the window's frame view.
        let Some(frame_view) = w.contentView().and_then(|v| unsafe { v.superview() }) else { return };
        let bounds = frame_view.bounds();
        let Some(rep) = frame_view.bitmapImageRepForCachingDisplayInRect(bounds) else { return };
        frame_view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
        // SAFETY: PNG takes no properties.
        let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) };
        let ok = data.is_some_and(|d| d.writeToFile_atomically(&NSString::from_str(&path.to_string_lossy()), true));
        println!("offscreen render {} {}", path.display(), if ok { "saved" } else { "failed" });
    }

    fn stats(state: &State, label: &str) {
        let now = state.start.elapsed().as_secs_f64();
        let (t0, ticks0, draws0) = state.mark.get();
        let (ticks, draws) = window::frame_stats();
        let dt = now - t0;
        println!(
            "{label}: {:.0} ticks/s, {:.0} frames/s over {dt:.2}s; visible={}",
            (ticks - ticks0) as f64 / dt,
            (draws - draws0) as f64 / dt,
            window::is_visible()
        );
        state.mark.set((now, ticks, draws));
    }

    /// The automatic walk through the window, one step per due time.
    fn script(state: &State) {
        let now = state.start.elapsed().as_secs_f64();
        if now < state.next_at.get() {
            return;
        }
        let step = state.step.get();
        state.step.set(step + 1);
        let mut wait = 0.6;
        let nav_y = |h: f64| h - 32.0;
        let h = main_window().and_then(|w| w.contentView()).map_or(0.0, |v| v.bounds().size.height);
        let none = NSEventModifierFlags::empty();
        match step {
            0 => {
                stats(state, "idle start");
                shot(state, "1-look");
            }
            1 => {
                stats(state, "idle (nothing changes, expect ~0 frames/s)");
                println!("clicking Sound tab");
                click(230.0, nav_y(h));
            }
            2 => shot(state, "2-sound"),
            3 => {
                scroll(-120, true);
                wait = 0.3;
            }
            4 => {
                scroll(-3, false);
            }
            5 => shot(state, "3-sound-scrolled"),
            6 => {
                println!("clicking Settings tab");
                click(383.0, nav_y(h));
            }
            7 => shot(state, "4-settings"),
            8 => {
                // Three steps forward and one back: the focus ring lands on the second element.
                key(48, "\t", none);
                key(48, "\t", none);
                key(48, "\t", none);
                key(48, "\t", NSEventModifierFlags::Shift);
            }
            9 => shot(state, "5-focus"),
            10 => {
                // Animation: page switches keep the springs busy.
                state.mark.set((now, window::frame_stats().0, window::frame_stats().1));
                click(77.0, nav_y(h));
                wait = 0.12;
            }
            11..=24 => {
                let x = [77.0, 230.0, 383.0][step % 3];
                click(x, nav_y(h));
                wait = 0.12;
            }
            25 => {
                stats(state, "animating");
                shot(state, "6-after-animation");
            }
            26 => {
                println!("Escape hides the window");
                key(53, "\u{1b}", none);
            }
            27 => {
                stats(state, "hidden");
                let s = state.shared.clone();
                window::show(&s, Vec::new());
            }
            28 => {
                println!("visible after show: {}", window::is_visible());
                println!("closing through the traffic light");
                if let Some(w) = main_window() {
                    w.performClose(None);
                }
            }
            29 => {
                println!("visible after close: {}", window::is_visible());
                let s = state.shared.clone();
                window::show(&s, Vec::new());
            }
            30 => {
                println!("Cmd+M minimizes");
                key(46, "m", NSEventModifierFlags::Command);
                wait = 1.0;
            }
            31 => {
                println!("visible after minimize: {}", window::is_visible());
                let s = state.shared.clone();
                window::show(&s, Vec::new());
                wait = 1.0;
            }
            32 => {
                println!("visible after restore: {}", window::is_visible());
                window::destroy();
                println!("done");
                process::exit(0);
            }
            _ => {}
        }
        state.next_at.set(now + wait);
    }
}
