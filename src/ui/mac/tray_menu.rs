//! The menu bar item's menu: a native NSMenu under the status item, the status line as its
//! section header, then the same entries as the Windows tray menu, each with its SF Symbol.

use std::cell::RefCell;

use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained, sel};
use objc2_app_kit::{NSControlStateValueOff, NSControlStateValueOn, NSMenu, NSMenuItem, NSStatusItem};
use objc2_foundation::{NSObject, NSString};

pub use super::menu::MenuEntry;
use super::{Command, canvas::symbol_image, post};

/// The longest status header, in characters; a longer one (a problem with a file path) ends
/// with an ellipsis so the menu keeps a reasonable width.
const HEADER_MAX_CHARS: usize = 60;
/// Point size of the symbols beside the menu items.
const SYMBOL_SIZE: f32 = 14.0;

thread_local! {
    /// The menu while it is up, so `close` can dismiss it.
    static OPEN: RefCell<Option<Retained<NSMenu>>> = const { RefCell::new(None) };
    /// The commands of the menu last built, by item tag.
    static COMMANDS: RefCell<Vec<Command>> = const { RefCell::new(Vec::new()) };
    static TARGET: RefCell<Option<Retained<MenuTarget>>> = const { RefCell::new(None) };
}

define_class!(
    /// Receives the menu items' actions.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WootlightMenuTarget"]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(choose:))]
        fn choose(&self, sender: &NSMenuItem) {
            let command = COMMANDS.with(|c| c.borrow().get(sender.tag() as usize).cloned());
            if let Some(command) = command {
                post(command);
            }
        }
    }
);

impl MenuTarget {
    fn shared(mtm: MainThreadMarker) -> Retained<Self> {
        TARGET.with(|cell| {
            cell.borrow_mut()
                .get_or_insert_with(|| {
                    let this = Self::alloc(mtm).set_ivars(());
                    // SAFETY: NSObject's designated initializer.
                    unsafe { msg_send![super(this), init] }
                })
                .clone()
        })
    }
}

pub fn is_open() -> bool {
    OPEN.with(|m| m.try_borrow().map(|m| m.is_some()).unwrap_or(true))
}

pub fn close() {
    let menu = OPEN.with(|m| m.try_borrow().ok().and_then(|m| m.clone()));
    if let Some(menu) = menu {
        menu.cancelTracking();
    }
}

fn build(mtm: MainThreadMarker, header: &str, entries: Vec<MenuEntry>) -> Retained<NSMenu> {
    let target = MenuTarget::shared(mtm);
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    let header: String = if header.chars().count() > HEADER_MAX_CHARS {
        header.chars().take(HEADER_MAX_CHARS - 1).chain(['\u{2026}']).collect()
    } else {
        header.into()
    };
    menu.addItem(&NSMenuItem::sectionHeaderWithTitle(&NSString::from_str(&header), mtm));
    let mut commands = Vec::new();
    for entry in entries {
        let Some(command) = entry.command else {
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            continue;
        };
        let key = if matches!(command, Command::Quit) { "q" } else { "" };
        // SAFETY: `choose:` is implemented by the target set below.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(&entry.text),
                Some(sel!(choose:)),
                &NSString::from_str(key),
            )
        };
        // SAFETY: the target is kept alive in TARGET for the life of the thread.
        unsafe { item.setTarget(Some(&target)) };
        item.setTag(commands.len() as isize);
        item.setImage(symbol_image(entry.glyph, SYMBOL_SIZE).as_deref());
        item.setState(if entry.checked { NSControlStateValueOn } else { NSControlStateValueOff });
        menu.addItem(&item);
        commands.push(command);
    }
    COMMANDS.with(|c| *c.borrow_mut() = commands);
    menu
}

/// Opens the menu under the status item, with the status line as its header; returns once the
/// menu closes. A chosen command is posted to the command queue.
pub fn open(status_item: &NSStatusItem, header: String, entries: Vec<MenuEntry>) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    close();
    let menu = build(mtm, &header, entries);
    OPEN.with(|m| *m.borrow_mut() = Some(menu.clone()));
    // The status item shows its menu, highlighted, as for any menu bar extra; it tracks the menu
    // in a nested event loop until the menu closes.
    status_item.setMenu(Some(&menu));
    if let Some(button) = status_item.button(mtm) {
        // SAFETY: a plain click on our own status item's button, on the main thread.
        unsafe { button.performClick(None) };
        button.highlight(false);
    }
    status_item.setMenu(None);
    OPEN.with(|m| *m.borrow_mut() = None);
}
