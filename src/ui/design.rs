//! Design tokens: every size, gap, radius, opacity and spring of the window and the tray menu.
//! Material 3 Expressive at desktop density; values are DIPs (1/96 inch).

// ---- Spacing: a 4 DIP grid ----
pub const SPACE_3: f32 = 12.0;
pub const SPACE_4: f32 = 16.0;

// ---- Window layout ----
pub const WINDOW_WIDTH: f32 = 460.0;
pub const WINDOW_MAX_HEIGHT: f32 = 820.0;
pub const WINDOW_MIN_HEIGHT: f32 = 560.0;
pub const HEADER_HEIGHT: f32 = 56.0;
pub const NAV_HEIGHT: f32 = 64.0;
pub const PAGE_GUTTER: f32 = SPACE_3;
pub const ITEM_PADDING: f32 = SPACE_4;
pub const CONTENT_INSET: f32 = PAGE_GUTTER + ITEM_PADDING;
pub const ICON_SIZE: f32 = 20.0;
pub const SMALL_ICON_SIZE: f32 = 14.0;

// ---- Groups: segmented lists ----
pub const SEGMENT_GAP: f32 = 2.0;
pub const GROUP_GAP: f32 = SPACE_3;
pub const SECTION_GAP: f32 = SPACE_4;
pub const SECTION_TITLE_HEIGHT: f32 = 28.0;

// ---- Rows ----
pub const ROW_HEIGHT: f32 = 48.0;
pub const CHOICE_ROW_HEIGHT: f32 = 80.0;
pub const HINT_TOP_IN_ROW: f32 = 42.0;
pub const HINT_BOTTOM_GAP: f32 = 10.0;

// ---- Controls ----
pub const CONTROL_HEIGHT: f32 = 32.0;
pub const BUTTON_HEIGHT: f32 = 40.0;
pub const ICON_BUTTON_SIZE: f32 = 40.0;
pub const SWITCH_WIDTH: f32 = 52.0;
pub const SWITCH_HEIGHT: f32 = 32.0;
pub const BUTTON_GROUP_GAP: f32 = 2.0;
pub const SWATCH_SIZE: f32 = 40.0;
pub const SLIDER_TRACK: f32 = 16.0;
pub const SLIDER_HANDLE_WIDTH: f32 = 4.0;
pub const SLIDER_HANDLE_HEIGHT: f32 = 36.0;
pub const SLIDER_GAP: f32 = 6.0;

// ---- Shape: the Material 3 corner scale ----
pub const RADIUS_EXTRA_SMALL: f32 = 4.0;
pub const RADIUS_SMALL: f32 = 8.0;
pub const RADIUS_MEDIUM: f32 = 12.0;
pub const RADIUS_LARGE: f32 = 16.0;
pub const RADIUS_GROUP: f32 = RADIUS_LARGE;
pub const RADIUS_TILE: f32 = RADIUS_EXTRA_SMALL;
pub const RADIUS_TILE_HOVERED: f32 = RADIUS_MEDIUM;
pub const RADIUS_MENU: f32 = RADIUS_LARGE;
pub const RADIUS_MENU_ITEM: f32 = RADIUS_MEDIUM;
pub const RADIUS_BUTTON_GROUP_INNER: f32 = RADIUS_SMALL;
pub const RADIUS_BUTTON_GROUP_PRESSED: f32 = RADIUS_EXTRA_SMALL;

// ---- States (Material 3 state layers) ----
pub const HOVER_OPACITY: f32 = 0.08;
pub const PRESS_OPACITY: f32 = 0.10;
pub const DISABLED_OPACITY: f32 = 0.38;
pub const FOCUS_RING_WIDTH: f32 = 3.0;
pub const FOCUS_RING_OFFSET: f32 = 2.0;
pub const SCRIM_OPACITY: f32 = 0.32;

// ---- Motion: Material 3 Expressive springs ----
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    /// Ratio: 1 never overshoots, lower bounces.
    pub damping: f32,
    /// Higher is quicker.
    pub stiffness: f32,
}

pub const SPRING_FAST_SPATIAL: Spring = Spring { damping: 0.6, stiffness: 800.0 };
pub const SPRING_FAST_EFFECTS: Spring = Spring { damping: 1.0, stiffness: 3800.0 };
pub const SPRING_DEFAULT_EFFECTS: Spring = Spring { damping: 1.0, stiffness: 1600.0 };
pub const SPRING_SCROLL: Spring = Spring { damping: 1.0, stiffness: 1200.0 };
