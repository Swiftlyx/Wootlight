//! The menu bar item and the NSApplication run loop around the shared app shell (see `shell`).
//!
//! Commands and status item clicks are handled by a run loop observer just before the main thread
//! waits, so they run outside any AppKit callback; a 500 ms timer drives the shell's checks.

use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    path::PathBuf,
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use objc2::{
    MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObjectProtocol, ProtocolObject},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSApplicationTerminateReply, NSEvent,
    NSEventModifierFlags, NSEventType, NSMenu, NSMenuItem, NSScreen, NSStatusItem, NSView,
    NSWorkspace, NSWorkspaceDidWakeNotification, NSWorkspaceWillPowerOffNotification, NSWorkspaceWillSleepNotification,
};
use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFRetained, CFRunLoop, CFRunLoopActivity, CFRunLoopObserver, CFRunLoopTimer,
    kCFRunLoopCommonModes,
};
use objc2_foundation::{NSNotification, NSObject, NSPoint, NSSize, NSString};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::{
    engine::Shared,
    log,
    shell::App,
    ui::{
        self, Command,
        icon::{self, Mark},
        tray_menu, window,
    },
    win,
};

const TIMER_SECONDS: f64 = 0.5;
/// The mark's size in the menu bar, in points.
const ICON_POINTS: f64 = 18.0;
/// How long sleep waits for the keyboard thread to hand the lighting back.
const SLEEP_GRACE: Duration = Duration::from_millis(300);

/// Status item clicks, with whether Control was held, as tray-icon reports them.
static CLICKS: Mutex<Vec<(MouseButton, bool)>> = Mutex::new(Vec::new());
/// Set by SIGTERM or SIGINT; the timer turns it into a clean quit.
static SIGNALLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static STATUS_ITEM: RefCell<Option<Retained<NSStatusItem>>> = const { RefCell::new(None) };
    static SHELL: RefCell<Option<Retained<ShellObject>>> = const { RefCell::new(None) };
    /// The engine state before system sleep paused the visualizer, until wake.
    static PAUSED_FOR_SLEEP: Cell<bool> = const { Cell::new(false) };
    static QUITTING: Cell<bool> = const { Cell::new(false) };
}

define_class!(
    /// The application delegate; also receives the Quit menu item and power notifications.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WootlightShell"]
    struct ShellObject;

    unsafe impl NSObjectProtocol for ShellObject {}

    unsafe impl NSApplicationDelegate for ShellObject {
        /// A quit from outside (logout, shutdown, a quit Apple event): restore the lighting first.
        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _sender: &NSApplication) -> NSApplicationTerminateReply {
            log!("quit requested by the system");
            crate::stop_engine();
            log!("stopped");
            NSApplicationTerminateReply::TerminateNow
        }
    }

    impl ShellObject {
        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            ui::post(Command::Quit);
        }

        #[unsafe(method(willPowerOff:))]
        fn will_power_off(&self, _notification: &NSNotification) {
            log!("session ending");
            crate::stop_engine();
        }

        #[unsafe(method(willSleep:))]
        fn will_sleep(&self, _notification: &NSNotification) {
            log!("system going to sleep");
            pause_for_sleep();
        }

        #[unsafe(method(didWake:))]
        fn did_wake(&self, _notification: &NSNotification) {
            log!("system woke up");
            resume_after_sleep();
        }
    }
);

impl ShellObject {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

fn shared() -> Option<Arc<Shared>> {
    APP.with(|a| a.try_borrow().ok().and_then(|a| a.as_ref().map(|app| Arc::clone(&app.shared))))
}

/// Hands the lighting back to the keyboard for sleep without touching the saved settings.
fn pause_for_sleep() {
    let Some(shared) = shared() else { return };
    let mut s = shared.settings();
    if s.enabled {
        s.enabled = false;
        shared.set_settings(s);
        PAUSED_FOR_SLEEP.set(true);
        thread::sleep(SLEEP_GRACE);
    }
}

fn resume_after_sleep() {
    if !PAUSED_FOR_SLEEP.replace(false) {
        return;
    }
    let Some(shared) = shared() else { return };
    let mut s = shared.settings();
    s.enabled = true;
    shared.set_settings(s);
}

fn status_button_view() -> Option<Retained<NSView>> {
    let mtm = MainThreadMarker::new()?;
    STATUS_ITEM.with(|s| s.borrow().as_ref().and_then(|item| item.button(mtm)).map(|b| Retained::into_super(Retained::into_super(Retained::into_super(b)))))
}

/// The menu bar mark's size in pixels: 18 points at the scale of the menu bar's screen.
pub fn icon_size() -> u32 {
    let scale = MainThreadMarker::new().and_then(|mtm| {
        status_button_view()
            .and_then(|v| v.window())
            .map(|w| w.backingScaleFactor())
            .or_else(|| NSScreen::mainScreen(mtm).map(|s| s.backingScaleFactor()))
    });
    (ICON_POINTS * scale.unwrap_or(2.0)).round() as u32
}

/// Shows straight-alpha RGBA pixels `size` pixels square as the menu bar mark, 18 points tall.
pub fn set_icon(tray: &TrayIcon, rgba: Vec<u8>, size: u32) {
    let Ok(icon) = Icon::from_rgba(rgba, size, size) else { return };
    let _ = tray.set_icon(Some(icon));
    // tray-icon sizes the image by its pixels; show every pixel at the screen's scale instead.
    let Some(mtm) = MainThreadMarker::new() else { return };
    if let Some(image) = tray.ns_status_item().and_then(|item| item.button(mtm)).and_then(|b| b.image()) {
        image.setSize(NSSize::new(ICON_POINTS, ICON_POINTS));
    }
}

/// Makes queued `ui::Command`s run on the next pass of the run loop; callable from the UI thread.
pub fn wake() {
    if let Some(main) = CFRunLoop::main() {
        main.wake_up();
    }
}

/// Runs `f` on the app unless it is already in use further up the stack (a nested event loop).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

fn app_busy() -> bool {
    APP.with(|a| a.try_borrow_mut().is_err())
}

fn quit(mtm: MainThreadMarker) {
    if QUITTING.replace(true) {
        return;
    }
    log!("quitting");
    let app = NSApplication::sharedApplication(mtm);
    app.stop(None);
    // `stop` takes effect after the next event; post one so `run` returns now.
    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        0,
        0,
        0,
    );
    if let Some(event) = event {
        app.postEvent_atStart(&event, true);
    }
}

unsafe extern "C-unwind" fn open_menu(_timer: *mut CFRunLoopTimer, _info: *mut c_void) {
    let Some((header, entries)) = with_app(|app| (app.status_text(), app.menu_entries())) else { return };
    let Some(item) = STATUS_ITEM.with(|s| s.borrow().clone()) else { return };
    // Runs a nested event loop until the menu closes; the app is not borrowed meanwhile.
    tray_menu::open(&item, header, entries);
}

/// Opens the menu from a one-shot timer of its own: a run loop callout does not run again while
/// it is still running, so a menu opened from the observer or the 500 ms timer would stall them.
fn schedule_menu() {
    let Some(main) = CFRunLoop::main() else { return };
    // SAFETY: a plain callback without context; the run loop retains the timer until it fires.
    unsafe {
        if let Some(timer) = CFRunLoopTimer::new(None, CFAbsoluteTimeGetCurrent(), 0.0, 0, 0, Some(open_menu), ptr::null_mut()) {
            main.add_timer(Some(&timer), kCFRunLoopCommonModes);
        }
    }
}

/// Handles status item clicks and queued commands.
fn drain() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    if QUITTING.get() || app_busy() {
        return;
    }
    let clicks = std::mem::take(&mut *CLICKS.lock().unwrap_or_else(|e| e.into_inner()));
    for (button, control) in clicks {
        match button {
            // Left click toggles the window; right click or Control-click shows the menu.
            MouseButton::Left if !control => {
                if !tray_menu::is_open() {
                    ui::post(Command::ToggleWindow);
                }
            }
            MouseButton::Left | MouseButton::Right => schedule_menu(),
            _ => {}
        }
    }
    while !app_busy() {
        let Some(command) = ui::take_command() else { break };
        if with_app(|app| app.execute(command)) == Some(false) {
            quit(mtm);
            return;
        }
    }
}

unsafe extern "C-unwind" fn before_waiting(_observer: *mut CFRunLoopObserver, _activity: CFRunLoopActivity, _info: *mut c_void) {
    drain();
}

unsafe extern "C-unwind" fn on_timer(_timer: *mut CFRunLoopTimer, _info: *mut c_void) {
    if SIGNALLED.swap(false, Ordering::SeqCst) {
        log!("terminate signal received");
        ui::post(Command::Quit);
    }
    if win::open_requested() {
        ui::post(Command::OpenWindow);
    }
    if !QUITTING.get() {
        with_app(App::on_timer);
    }
    drain();
}


extern "C" fn on_signal(_signal: libc::c_int) {
    SIGNALLED.store(true, Ordering::SeqCst);
}

fn on_tray_event(event: TrayIconEvent) {
    if let TrayIconEvent::Click { button, button_state: MouseButtonState::Down, .. } = event {
        let control = NSEvent::modifierFlags_class().contains(NSEventModifierFlags::Control);
        CLICKS.lock().unwrap_or_else(|e| e.into_inner()).push((button, control));
        wake();
    }
}

/// The app menu: never shown for a menu bar app, but its key equivalents work in the window.
fn main_menu(mtm: MainThreadMarker, shell: &ShellObject) -> Retained<NSMenu> {
    let item = |title: &str, action, key: &str| {
        // SAFETY: the actions exist on the shell object or the responder chain.
        unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(key),
            )
        }
    };
    let app_menu = NSMenu::new(mtm);
    let quit = item("Quit Wootlight", sel!(quit:), "q");
    // SAFETY: the shell object lives in SHELL for the whole run.
    unsafe { quit.setTarget(Some(shell)) };
    app_menu.addItem(&quit);
    // Sent to the key window through the responder chain.
    app_menu.addItem(&item("Close Window", sel!(performClose:), "w"));
    let app_item = NSMenuItem::new(mtm);
    app_item.setSubmenu(Some(&app_menu));
    let menu = NSMenu::new(mtm);
    menu.addItem(&app_item);
    menu
}

fn observe_power(shell: &ShellObject) {
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    // SAFETY: the selectors are implemented by the shell object, which outlives the observation
    // (it is removed before the object is released).
    unsafe {
        center.addObserver_selector_name_object(shell, sel!(willPowerOff:), Some(NSWorkspaceWillPowerOffNotification), None);
        center.addObserver_selector_name_object(shell, sel!(willSleep:), Some(NSWorkspaceWillSleepNotification), None);
        center.addObserver_selector_name_object(shell, sel!(didWake:), Some(NSWorkspaceDidWakeNotification), None);
    }
}

/// The run loop hooks: the command observer and the timer, in the common modes so they also run
/// while a menu tracks or a window resizes.
fn install_hooks() -> Result<(CFRetained<CFRunLoopObserver>, CFRetained<CFRunLoopTimer>), String> {
    let main = CFRunLoop::main().ok_or("no main run loop")?;
    // SAFETY: plain callbacks without context; the run loop retains what it is given.
    unsafe {
        let observer = CFRunLoopObserver::new(None, CFRunLoopActivity::BeforeWaiting.0, true, 0, Some(before_waiting), ptr::null_mut())
            .ok_or("cannot create the run loop observer")?;
        let timer =
            CFRunLoopTimer::new(None, CFAbsoluteTimeGetCurrent() + TIMER_SECONDS, TIMER_SECONDS, 0, 0, Some(on_timer), ptr::null_mut())
                .ok_or("cannot create the timer")?;
        main.add_observer(Some(&observer), kCFRunLoopCommonModes);
        main.add_timer(Some(&timer), kCFRunLoopCommonModes);
        Ok((observer, timer))
    }
}

/// Runs the menu bar app until the user quits or the session ends.
pub fn run(
    shared: Arc<Shared>,
    config_path: PathBuf,
    config_error: Option<String>,
    devices: Vec<String>,
    open_window: bool,
) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("the menu bar must run on the main thread")?;
    ui::init_ui_thread();
    let ns_app = NSApplication::sharedApplication(mtm);
    ns_app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let shell = ShellObject::new(mtm);
    ns_app.setDelegate(Some(ProtocolObject::from_ref(&*shell)));
    ns_app.setMainMenu(Some(&main_menu(mtm, &shell)));
    observe_power(&shell);
    SHELL.with(|s| *s.borrow_mut() = Some(shell.clone()));

    let settings = shared.settings();
    let size = (ICON_POINTS * NSScreen::mainScreen(mtm).map_or(2.0, |s| s.backingScaleFactor())).round() as u32;
    let first_icon = Icon::from_rgba(icon::render_rgba(size, Mark::Idle, ui::seed_for(&settings), icon::taskbar_is_dark()), size, size)
        .map_err(|e| e.to_string())?;
    TrayIconEvent::set_event_handler(Some(on_tray_event));
    let tray = TrayIconBuilder::new()
        .with_menu_on_left_click(false)
        .with_menu_on_right_click(false)
        .with_tooltip("Wootlight")
        .with_icon(first_icon)
        .build()
        .map_err(|e| e.to_string())?;
    let status_item = tray.ns_status_item();
    STATUS_ITEM.with(|s| *s.borrow_mut() = status_item);
    icon::set_menu_bar_view(status_button_view());

    APP.with(|a| *a.borrow_mut() = Some(App::new(shared, config_path, config_error, tray, devices)));
    with_app(App::refresh);
    if open_window {
        with_app(App::open_window);
    }

    let (observer, timer) = install_hooks()?;
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as libc::sighandler_t);
    }

    ns_app.run();

    observer.invalidate();
    timer.invalidate();
    // SAFETY: removes the observer added in `observe_power`.
    unsafe { NSWorkspace::sharedWorkspace().notificationCenter().removeObserver(&shell) };
    tray_menu::close();
    window::destroy();
    TrayIconEvent::set_event_handler(None::<fn(TrayIconEvent)>);
    // The status item goes with the app; then the icon canvas.
    let app = APP.with(|a| a.borrow_mut().take());
    drop(app);
    STATUS_ITEM.with(|s| s.borrow_mut().take());
    icon::set_menu_bar_view(None);
    icon::release_canvas();
    ns_app.setDelegate(None);
    SHELL.with(|s| s.borrow_mut().take());
    Ok(())
}
