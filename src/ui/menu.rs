//! An entry of the tray or menu bar menu, shown by either platform's `tray_menu`.

use super::Command;

pub struct MenuEntry {
    pub text: String,
    pub glyph: char,
    pub checked: bool,
    pub danger: bool,
    /// None draws a divider.
    pub command: Option<Command>,
}

impl MenuEntry {
    pub fn item(text: &str, glyph: char, command: Command) -> Self {
        Self { text: text.into(), glyph, checked: false, danger: false, command: Some(command) }
    }
    pub fn divider() -> Self {
        Self { text: String::new(), glyph: ' ', checked: false, danger: false, command: None }
    }
}
