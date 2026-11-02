//! Physical key geometry, used to spread spectrum strips evenly over the real keyboard.
//!
//! The SDK addresses LEDs on a 6 x 21 grid, but real boards leave many grid cells empty and keys
//! in one grid column are not stacked vertically. LED cells follow OpenRGB's Wooting matrix maps;
//! positions are in key units (u) of standard ANSI and ISO layouts, accurate to a fraction of a key.

use crate::sdk::KeyboardInfo;

const DEVICE_KEYBOARD_TKL: i32 = 1;
const DEVICE_KEYBOARD: i32 = 2;
const DEVICE_KEYBOARD_60: i32 = 3;
const DEVICE_KEYPAD_3KEY: i32 = 4;
const DEVICE_KEYBOARD_80: i32 = 5;
const LAYOUT_ISO: i32 = 1;

#[derive(Clone, Debug)]
pub struct Key {
    pub row: usize,
    pub col: usize,
    pub x0: f32,
    pub x1: f32,
}

pub struct Layout {
    pub keys: Vec<Key>,
    pub width: f32,
    /// One strip per key column of the main block.
    pub default_strips: usize,
}

#[derive(Default)]
struct Builder {
    keys: Vec<Key>,
}

impl Builder {
    fn add(&mut self, row: usize, col: usize, x0: f32, w: f32) {
        self.keys.push(Key { row, col, x0, x1: x0 + w });
    }

    fn finish(self, width: f32, default_strips: usize) -> Layout {
        Layout { keys: self.keys, width, default_strips }
    }

    /// Rows 1-5 of a standard 60% block shared by every Wooting keyboard; `x` is its left edge.
    fn main_block(&mut self, iso: bool) {
        for col in 0..=12 {
            self.add(1, col, col as f32, 1.0);
        }
        self.add(1, 13, 13.0, 2.0);
        self.add(2, 0, 0.0, 1.5);
        for col in 1..=12 {
            self.add(2, col, 0.5 + col as f32, 1.0);
        }
        self.add(2, 13, 13.5, 1.5);
        self.add(3, 0, 0.0, 1.75);
        for col in 1..=11 {
            self.add(3, col, 0.75 + col as f32, 1.0);
        }
        if iso {
            self.add(3, 12, 12.75, 1.0);
            self.add(3, 13, 13.75, 1.25);
            self.add(4, 0, 0.0, 1.25);
            self.add(4, 1, 1.25, 1.0);
        } else {
            self.add(3, 13, 12.75, 2.25);
            self.add(4, 0, 0.0, 2.25);
        }
        for col in 2..=11 {
            self.add(4, col, 0.25 + col as f32, 1.0);
        }
        self.add(4, 13, 12.25, 2.75);
        for (col, x0) in [(0, 0.0), (1, 1.25), (2, 2.5)] {
            self.add(5, col, x0, 1.25);
        }
    }

    /// Function row and the bottom-right modifiers of a tenkeyless block (Wooting One and Two).
    fn tenkeyless(&mut self, iso: bool) {
        self.add(0, 0, 0.0, 1.0);
        for (i, col) in (2..=13).enumerate() {
            let group_gap = 0.5 * (i / 4) as f32;
            self.add(0, col, 2.0 + i as f32 + group_gap, 1.0);
        }
        self.main_block(iso);
        self.add(5, 6, 3.75, 6.25);
        for (col, x0) in [(10, 10.0), (11, 11.25), (12, 12.5), (13, 13.75)] {
            self.add(5, col, x0, 1.25);
        }
        // Navigation cluster and arrows.
        for col in 14..=16 {
            let x = 15.25 + (col - 14) as f32;
            self.add(0, col, x, 1.0);
            self.add(1, col, x, 1.0);
            self.add(2, col, x, 1.0);
            self.add(5, col, x, 1.0);
        }
        self.add(4, 15, 16.25, 1.0);
    }
}

impl Layout {
    pub fn detect(info: &KeyboardInfo) -> Self {
        let iso = info.layout == LAYOUT_ISO;
        match info.device_type {
            DEVICE_KEYBOARD_TKL => Self::wooting_one(iso),
            DEVICE_KEYBOARD => Self::wooting_two(iso),
            DEVICE_KEYBOARD_60 => Self::wooting_60(iso),
            DEVICE_KEYPAD_3KEY => Self::wooting_uwu(),
            DEVICE_KEYBOARD_80 => Self::wooting_80he(iso),
            _ => Self::grid(info.rows, info.cols),
        }
    }

    /// Fallback: every grid cell is a 1u key directly under the one above.
    pub fn grid(rows: usize, cols: usize) -> Self {
        let keys = (0..rows)
            .flat_map(|row| (0..cols).map(move |col| Key { row, col, x0: col as f32, x1: col as f32 + 1.0 }))
            .collect();
        Self { keys, width: cols as f32, default_strips: cols.max(1) }
    }

    /// Wooting One: tenkeyless.
    pub fn wooting_one(iso: bool) -> Self {
        let mut b = Builder::default();
        b.tenkeyless(iso);
        b.finish(18.25, 18)
    }

    /// Wooting Two, Two LE and Two HE: full size with four extra keys above the numpad.
    pub fn wooting_two(iso: bool) -> Self {
        let mut b = Builder::default();
        b.tenkeyless(iso);
        for col in 17..=20 {
            let x = 18.5 + (col - 17) as f32;
            b.add(0, col, x, 1.0);
            b.add(1, col, x, 1.0);
        }
        for col in 17..=19 {
            let x = 18.5 + (col - 17) as f32;
            b.add(2, col, x, 1.0);
            b.add(3, col, x, 1.0);
            b.add(4, col, x, 1.0);
        }
        b.add(2, 20, 21.5, 1.0);
        b.add(4, 20, 21.5, 1.0);
        b.add(5, 18, 18.5, 2.0);
        b.add(5, 19, 20.5, 1.0);
        b.finish(22.5, 22)
    }

    /// Wooting 60HE family: no function row, so the board starts at grid row 1.
    pub fn wooting_60(iso: bool) -> Self {
        let mut b = Builder::default();
        b.main_block(iso);
        for col in 4..=8 {
            b.add(5, col, 3.75 + (col - 4) as f32 * 1.25, 1.25);
        }
        for (col, x0) in [(10, 10.0), (11, 11.25), (12, 12.5), (13, 13.75)] {
            b.add(5, col, x0, 1.25);
        }
        b.finish(15.0, 15)
    }

    /// Wooting UwU: three keys inside a ring of light bar LEDs.
    pub fn wooting_uwu() -> Self {
        const CELLS: [(usize, usize); 17] = [
            (0, 0), (0, 2), (0, 4), (0, 6),
            (1, 0), (1, 6),
            (2, 0), (2, 1), (2, 3), (2, 5), (2, 6),
            (3, 0), (3, 6),
            (4, 1), (4, 2), (4, 4), (4, 5),
        ];
        let mut b = Builder::default();
        for (row, col) in CELLS {
            b.add(row, col, col as f32, 1.0);
        }
        b.finish(7.0, 7)
    }

    /// Wooting 80HE: an 80% board with a 2-wide navigation column and gaps in the F row.
    pub fn wooting_80he(iso: bool) -> Self {
        let mut b = Builder::default();
        // Row 0: Esc, F1-F4, F5-F8, F9-F12, Mode, PrtSc, Pause.
        b.add(0, 0, 0.0, 1.0);
        for (i, col) in (1..=8).chain(10..=13).enumerate() {
            let group_gap = 0.25 * (i / 4) as f32;
            b.add(0, col, 1.25 + i as f32 + group_gap, 1.0);
        }
        b.add(0, 14, 14.25, 1.0);
        b.add(0, 15, 15.25, 1.0);
        b.add(0, 16, 16.25, 1.0);

        // Rows 1-4 follow the main block, but Backspace, backslash, Enter and Shift sit in column 14.
        b.main_block(iso);
        for key in b.keys.iter_mut().filter(|k| k.col == 13 && (1..=4).contains(&k.row)) {
            key.col = 14;
        }
        b.add(1, 15, 15.25, 1.0);
        b.add(1, 16, 16.25, 1.0);
        b.add(2, 15, 15.25, 1.0);
        b.add(2, 16, 16.25, 1.0);
        b.add(4, 15, 15.25, 1.0);

        // Row 5: five spacebar LEDs (cols 4-8), right Alt, Win, Fn, arrows.
        for col in 4..=8 {
            b.add(5, col, 3.75 + (col - 4) as f32 * 1.25, 1.25);
        }
        for (col, x0) in [(10, 10.0), (11, 11.25), (12, 12.5)] {
            b.add(5, col, x0, 1.25);
        }
        b.add(5, 14, 14.25, 1.0);
        b.add(5, 15, 15.25, 1.0);
        b.add(5, 16, 16.25, 1.0);
        b.finish(17.25, 17)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No two keys of a board share a grid cell, and rows do not overlap within a row.
    fn assert_consistent(layout: &Layout) {
        let mut cells: Vec<(usize, usize)> = layout.keys.iter().map(|k| (k.row, k.col)).collect();
        cells.sort_unstable();
        let n = cells.len();
        cells.dedup();
        assert_eq!(cells.len(), n, "duplicate grid cells");
        for row in 0..6 {
            let mut spans: Vec<(f32, f32)> = layout.keys.iter().filter(|k| k.row == row).map(|k| (k.x0, k.x1)).collect();
            spans.sort_by(|a, b| a.0.total_cmp(&b.0));
            for w in spans.windows(2) {
                assert!(w[0].1 <= w[1].0 + 1e-3, "row {row}: {:?} overlaps {:?}", w[0], w[1]);
            }
            assert!(spans.iter().all(|s| s.1 <= layout.width + 1e-3), "row {row} wider than the board");
        }
    }

    #[test]
    fn every_layout_is_consistent() {
        for iso in [false, true] {
            assert_consistent(&Layout::wooting_one(iso));
            assert_consistent(&Layout::wooting_two(iso));
            assert_consistent(&Layout::wooting_60(iso));
            assert_consistent(&Layout::wooting_80he(iso));
        }
        assert_consistent(&Layout::wooting_uwu());
    }

    #[test]
    fn key_counts_match_the_boards() {
        assert_eq!(Layout::wooting_one(false).keys.len(), 87);
        assert_eq!(Layout::wooting_two(false).keys.len(), 87 + 17 + 4);
        assert_eq!(Layout::wooting_60(false).keys.len(), 61 + 4);
    }
}
