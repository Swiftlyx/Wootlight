//! Animations for the keyboard while no music plays: rain, a thunderstorm, the Matrix, snow, a
//! starry sky, an aurora, ripples, fire, the Game of Life, a clock and a snake that plays itself.
//!
//! Most effects run on a field of 1u cells (board rows by board width), and each key shows the
//! brightest cell it covers. The starry sky, Life, the clock and the snake work on the keys
//! themselves. An effect yields a heat (0..1) per key, plus an accent (0..1) for highlights such as
//! lightning or the snake's food; the palette turns both into a color.

use std::{
    collections::VecDeque,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    layout::Layout,
    render::{Frame, SDK_COLS, SDK_ROWS, hsv_to_rgb},
    settings::{ColorMode, Settings, parse_color_mode},
};

/// Seconds for an effect to fade in when it starts.
const FADE_IN: f32 = 0.8;
/// Grid row of the number keys; 1-9 sit in columns 1-9 and 0 in column 10.
const NUMBER_ROW: usize = 1;

const BLACK: [f32; 3] = [0.0; 3];
const WHITE: [f32; 3] = [255.0; 3];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Rain,
    Storm,
    Matrix,
    Snow,
    Stars,
    Aurora,
    Ripples,
    Fire,
    Life,
    Clock,
    Snake,
}

impl Effect {
    pub const ALL: [Effect; 11] = [
        Effect::Rain,
        Effect::Storm,
        Effect::Matrix,
        Effect::Snow,
        Effect::Stars,
        Effect::Aurora,
        Effect::Ripples,
        Effect::Fire,
        Effect::Life,
        Effect::Clock,
        Effect::Snake,
    ];

    pub fn code(self) -> &'static str {
        match self {
            Effect::Rain => "rain",
            Effect::Storm => "storm",
            Effect::Matrix => "matrix",
            Effect::Snow => "snow",
            Effect::Stars => "stars",
            Effect::Aurora => "aurora",
            Effect::Ripples => "ripples",
            Effect::Fire => "fire",
            Effect::Life => "life",
            Effect::Clock => "clock",
            Effect::Snake => "snake",
        }
    }

    /// The effect named `code`; None for "off" and anything unknown.
    pub fn parse(code: &str) -> Option<Self> {
        let code = code.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|e| e.code() == code)
    }
}

/// Turns heat and accent into a color.
#[derive(Clone, Debug, PartialEq)]
pub enum Palette {
    /// Colors at rising heat from 0 to 1, and the accent color.
    Stops { stops: Vec<(f32, [f32; 3])>, accent: [f32; 3] },
    /// Hue across the board and heat as brightness; `speed` turns the hue, in turns per second.
    Rainbow { speed: f32 },
}

impl Palette {
    /// The effect's own colors.
    pub fn natural(effect: Effect) -> Self {
        let (stops, accent): (&[(f32, [f32; 3])], [f32; 3]) = match effect {
            Effect::Rain => (&[(0.0, BLACK), (0.25, [0.0, 18.0, 60.0]), (0.6, [20.0, 90.0, 255.0]), (1.0, [170.0, 225.0, 255.0])], WHITE),
            Effect::Storm => (&[(0.0, BLACK), (0.25, [0.0, 12.0, 45.0]), (0.6, [30.0, 80.0, 230.0]), (1.0, [150.0, 195.0, 255.0])], [230.0, 235.0, 255.0]),
            Effect::Matrix => (&[(0.0, BLACK), (0.3, [0.0, 45.0, 8.0]), (0.75, [0.0, 210.0, 40.0]), (1.0, [200.0, 255.0, 200.0])], WHITE),
            Effect::Snow => (&[(0.0, BLACK), (0.3, [12.0, 22.0, 50.0]), (0.6, [120.0, 145.0, 210.0]), (1.0, WHITE)], WHITE),
            Effect::Stars => (
                &[(0.0, BLACK), (0.05, [0.0, 3.0, 16.0]), (0.4, [60.0, 80.0, 170.0]), (0.8, [205.0, 210.0, 255.0]), (1.0, [255.0, 248.0, 225.0])],
                WHITE,
            ),
            Effect::Aurora => (
                &[(0.0, BLACK), (0.2, [0.0, 40.0, 30.0]), (0.5, [0.0, 200.0, 110.0]), (0.75, [40.0, 200.0, 230.0]), (1.0, [180.0, 100.0, 255.0])],
                WHITE,
            ),
            Effect::Ripples => (&[(0.0, BLACK), (0.3, [0.0, 25.0, 70.0]), (0.65, [0.0, 140.0, 230.0]), (1.0, [190.0, 245.0, 255.0])], WHITE),
            Effect::Fire => (
                &[
                    (0.0, BLACK),
                    (0.2, [60.0, 0.0, 0.0]),
                    (0.45, [200.0, 30.0, 0.0]),
                    (0.7, [255.0, 120.0, 0.0]),
                    (0.9, [255.0, 200.0, 40.0]),
                    (1.0, [255.0, 250.0, 190.0]),
                ],
                WHITE,
            ),
            Effect::Life => (&[(0.0, BLACK), (0.4, [0.0, 50.0, 90.0]), (0.9, [0.0, 210.0, 170.0]), (1.0, [170.0, 255.0, 230.0])], WHITE),
            Effect::Clock => (&[(0.0, BLACK), (0.25, [45.0, 25.0, 0.0]), (0.6, [255.0, 140.0, 20.0]), (1.0, [255.0, 235.0, 190.0])], WHITE),
            Effect::Snake => (&[(0.0, BLACK), (0.3, [0.0, 50.0, 0.0]), (0.75, [30.0, 210.0, 40.0]), (1.0, [190.0, 255.0, 110.0])], [255.0, 30.0, 30.0]),
        };
        Palette::Stops { stops: stops.to_vec(), accent }
    }

    /// Colors of a visualizer color mode: dark at no heat, its top color at full heat.
    pub fn from_mode(mode: ColorMode, hue_speed: f32) -> Self {
        let stops = match mode {
            ColorMode::Rainbow => return Palette::Rainbow { speed: 0.0 },
            ColorMode::Flow => return Palette::Rainbow { speed: hue_speed },
            ColorMode::Classic => vec![(0.0, BLACK), (0.35, [0.0, 255.0, 40.0]), (0.7, [255.0, 210.0, 0.0]), (1.0, [255.0, 0.0, 0.0])],
            ColorMode::Solid(c) => vec![(0.0, BLACK), (1.0, c)],
            ColorMode::Gradient(base, top) => vec![(0.0, BLACK), (0.45, base), (1.0, top)],
        };
        Palette::Stops { stops, accent: WHITE }
    }

    /// The palette the settings pick for `effect`.
    pub fn for_settings(s: &Settings, effect: Effect) -> Self {
        match s.effect_color.as_str() {
            "scheme" => Self::from_mode(s.color_mode(), s.hue_speed),
            "custom" => Self::from_mode(parse_color_mode(&s.effect_custom_color), s.hue_speed),
            _ => Self::natural(effect),
        }
    }

    /// `x` is the key's position across the board (0..1), `hue` the rainbow's rotation.
    fn color(&self, heat: f32, accent: f32, x: f32, hue: f32) -> [f32; 3] {
        let heat = heat.clamp(0.0, 1.0);
        let accent = accent.clamp(0.0, 1.0);
        let (base, accent_rgb) = match self {
            Palette::Stops { stops, accent } => (sample(stops, heat), *accent),
            Palette::Rainbow { .. } => (hsv_to_rgb((x * 0.8 + hue).rem_euclid(1.0), 1.0).map(|c| c * heat), WHITE),
        };
        [0, 1, 2].map(|i| base[i] * (1.0 - accent) + accent_rgb[i] * accent)
    }
}

fn sample(stops: &[(f32, [f32; 3])], t: f32) -> [f32; 3] {
    let Some(first) = stops.first() else { return BLACK };
    if t <= first.0 {
        return first.1;
    }
    for w in stops.windows(2) {
        let ((a, ca), (b, cb)) = (w[0], w[1]);
        if t <= b {
            let k = if b > a { (t - a) / (b - a) } else { 1.0 };
            return [0, 1, 2].map(|i| ca[i] + (cb[i] - ca[i]) * k);
        }
    }
    stops[stops.len() - 1].1
}

/// xorshift64*: plenty for animations.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }

    fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }
}

struct BoardKey {
    offset: usize,
    /// Row from the board's top row.
    row: usize,
    grid_row: usize,
    grid_col: usize,
    x0: f32,
    x1: f32,
    /// Field cells the key covers.
    cells: Vec<usize>,
}

/// The keys of one keyboard and the field of 1u cells laid over them.
struct Board {
    keys: Vec<BoardKey>,
    rows: usize,
    cols: usize,
    width: f32,
}

impl Board {
    fn new(layout: &Layout) -> Self {
        let valid: Vec<_> = layout.keys.iter().filter(|k| k.row < SDK_ROWS && k.col < SDK_COLS).collect();
        let top = valid.iter().map(|k| k.row).min().unwrap_or(0);
        let bottom = valid.iter().map(|k| k.row).max().unwrap_or(0);
        let rows = bottom - top + 1;
        let cols = (layout.width.ceil() as usize).max(1);
        let keys = valid
            .iter()
            .map(|k| {
                let row = k.row - top;
                let mut cells: Vec<usize> = (0..cols)
                    .filter(|c| {
                        let x = *c as f32 + 0.5;
                        k.x0 <= x && x < k.x1
                    })
                    .map(|c| row * cols + c)
                    .collect();
                if cells.is_empty() {
                    cells.push(row * cols + (((k.x0 + k.x1) / 2.0) as usize).min(cols - 1));
                }
                BoardKey { offset: (k.row * SDK_COLS + k.col) * 3, row, grid_row: k.row, grid_col: k.col, x0: k.x0, x1: k.x1, cells }
            })
            .collect();
        Self { keys, rows, cols, width: layout.width.max(1.0) }
    }

    fn cx(&self, k: usize) -> f32 {
        (self.keys[k].x0 + self.keys[k].x1) / 2.0
    }

    /// Keys that touch each key, in its own row and the rows next to it: Life's neighbourhood.
    fn neighbors(&self) -> Vec<Vec<usize>> {
        const TOUCH: f32 = 0.35;
        (0..self.keys.len())
            .map(|i| {
                let a = &self.keys[i];
                (0..self.keys.len())
                    .filter(|&j| {
                        let b = &self.keys[j];
                        j != i && a.row.abs_diff(b.row) <= 1 && b.x0 < a.x1 + TOUCH && a.x0 < b.x1 + TOUCH
                    })
                    .collect()
            })
            .collect()
    }

    /// The snake's moves from each key: the touching keys left and right in its row, and the
    /// nearest key straight above and below.
    fn steps(&self) -> Vec<Vec<usize>> {
        const GAP: f32 = 0.6;
        (0..self.keys.len())
            .map(|i| {
                let a = &self.keys[i];
                let cx = self.cx(i);
                let same_row = |j: &usize| *j != i && self.keys[*j].row == a.row;
                let mut out: Vec<usize> = Vec::new();
                let left = (0..self.keys.len()).filter(same_row).find(|&j| {
                    let b = &self.keys[j];
                    b.x1 <= a.x0 + 0.01 && a.x0 - b.x1 < GAP
                });
                let right = (0..self.keys.len()).filter(same_row).find(|&j| {
                    let b = &self.keys[j];
                    b.x0 >= a.x1 - 0.01 && b.x0 - a.x1 < GAP
                });
                out.extend(left);
                out.extend(right);
                for row in [a.row.checked_sub(1), Some(a.row + 1)].into_iter().flatten() {
                    let nearest = (0..self.keys.len())
                        .filter(|&j| self.keys[j].row == row)
                        .map(|j| (j, (self.cx(j) - cx).abs()))
                        .filter(|&(j, d)| d <= ((self.keys[j].x1 - self.keys[j].x0) / 2.0).max(0.8))
                        .min_by(|x, y| x.1.total_cmp(&y.1));
                    out.extend(nearest.map(|(j, _)| j));
                }
                out
            })
            .collect()
    }
}

/// What an effect draws into during one frame; everything starts dark.
struct Scene<'a> {
    board: &'a Board,
    rng: &'a mut Rng,
    /// Per cell, at `row * cols + col`.
    field: &'a mut [f32],
    field_accent: &'a mut [f32],
    /// Per key.
    heat: &'a mut [f32],
    accent: &'a mut [f32],
    /// Local hour, minute and second.
    clock: (u32, u32, u32),
}

impl Scene<'_> {
    fn lift(&mut self, row: usize, col: usize, v: f32) {
        if row < self.board.rows && col < self.board.cols {
            let cell = &mut self.field[row * self.board.cols + col];
            *cell = cell.max(v);
        }
    }

    fn lift_accent(&mut self, row: usize, col: usize, v: f32) {
        if row < self.board.rows && col < self.board.cols {
            let cell = &mut self.field_accent[row * self.board.cols + col];
            *cell = cell.max(v);
        }
    }

    /// A falling streak in column `col`: brightest around `y` (in rows), fading over `tail` rows above.
    fn streak(&mut self, col: usize, y: f32, tail: f32, level: f32) {
        for row in 0..self.board.rows {
            let d = y - (row as f32 + 0.5);
            let head = (1.0 - d.abs()).max(0.0);
            let trail = if d > 0.0 { (1.0 - d / tail).max(0.0).powf(1.5) } else { 0.0 };
            self.lift(row, col, head.max(trail) * level);
        }
    }
}

trait Animation {
    fn step(&mut self, scene: &mut Scene, dt: f32);
}

/// Renders one effect at a time over a keyboard's keys.
pub struct Effects {
    board: Board,
    rng: Rng,
    current: Option<(Effect, Box<dyn Animation>)>,
    field: Vec<f32>,
    field_accent: Vec<f32>,
    heat: Vec<f32>,
    accent: Vec<f32>,
    fade: f32,
    hue: f32,
}

impl Effects {
    pub fn new(layout: &Layout) -> Self {
        let board = Board::new(layout);
        let cells = board.rows * board.cols;
        let keys = board.keys.len();
        let seed = SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
        Self {
            board,
            rng: Rng::new(seed),
            current: None,
            field: vec![0.0; cells],
            field_accent: vec![0.0; cells],
            heat: vec![0.0; keys],
            accent: vec![0.0; keys],
            fade: 0.0,
            hue: 0.0,
        }
    }

    /// The next `render` starts the effect afresh, fading in.
    pub fn restart(&mut self) {
        self.current = None;
    }

    pub fn render(&mut self, effect: Effect, palette: &Palette, brightness: f32, dt: f32, clock: (u32, u32, u32), frame: &mut Frame) {
        let Self { board, rng, current, field, field_accent, heat, accent, fade, hue } = self;
        if current.as_ref().map(|(e, _)| *e) != Some(effect) {
            *current = Some((effect, make(effect, board, rng)));
            *fade = 0.0;
        }
        *fade = (*fade + dt / FADE_IN).min(1.0);
        if let Palette::Rainbow { speed } = palette {
            *hue = (*hue + speed * dt).rem_euclid(1.0);
        }
        field.fill(0.0);
        field_accent.fill(0.0);
        heat.fill(0.0);
        accent.fill(0.0);
        if let Some((_, animation)) = current.as_mut() {
            let mut scene = Scene { board, rng, field, field_accent, heat, accent, clock };
            animation.step(&mut scene, dt);
        }

        frame.fill(0);
        let scale = brightness * *fade * *fade;
        for (k, key) in board.keys.iter().enumerate() {
            let (mut h, mut a) = (heat[k], accent[k]);
            for &c in &key.cells {
                h = h.max(field[c]);
                a = a.max(field_accent[c]);
            }
            let x = ((key.x0 + key.x1) / 2.0 / board.width).clamp(0.0, 1.0);
            let rgb = palette.color(h, a, x, *hue);
            for i in 0..3 {
                frame[key.offset + i] = (rgb[i] * scale).clamp(0.0, 255.0) as u8;
            }
        }
    }
}

fn make(effect: Effect, board: &Board, rng: &mut Rng) -> Box<dyn Animation> {
    match effect {
        Effect::Rain => Box::new(Rain::new(false)),
        Effect::Storm => Box::new(Rain::new(true)),
        Effect::Matrix => Box::new(Matrix::new(board, rng)),
        Effect::Snow => Box::new(Snow::new(board)),
        Effect::Stars => Box::new(Stars::default()),
        Effect::Aurora => Box::new(Aurora::default()),
        Effect::Ripples => Box::new(Ripples::default()),
        Effect::Fire => Box::new(Fire::new(board)),
        Effect::Life => Box::new(Life::new(board, rng)),
        // A board without number keys (UwU) shows the starry sky instead.
        Effect::Clock => match Clock::new(board) {
            Some(clock) => Box::new(clock),
            None => Box::new(Stars::default()),
        },
        Effect::Snake => Box::new(Snake::new(board, rng)),
    }
}

struct Raindrop {
    x: f32,
    y: f32,
    speed: f32,
    level: f32,
}

/// Drops fall down the board and splash on the bottom row; the storm adds wind and lightning.
struct Rain {
    drops: Vec<Raindrop>,
    /// Bottom-row splashes: cell column and age in seconds.
    splashes: Vec<(usize, f32)>,
    spawn: f32,
    lightning: Option<Lightning>,
}

const SPLASH_LIFE: f32 = 0.3;

impl Rain {
    fn new(storm: bool) -> Self {
        Self { drops: Vec::new(), splashes: Vec::new(), spawn: 0.0, lightning: storm.then(Lightning::new) }
    }
}

impl Animation for Rain {
    fn step(&mut self, s: &mut Scene, dt: f32) {
        let (rows, cols) = (s.board.rows as f32, s.board.cols as f32);
        let storm = self.lightning.is_some();
        // Drops per column per second, and cells of sideways drift per row of fall.
        let (rate, wind) = if storm { (0.75, 0.3) } else { (0.4, 0.0) };
        self.spawn += rate * cols * dt;
        while self.spawn >= 1.0 {
            self.spawn -= 1.0;
            let speed = if storm { s.rng.range(9.0, 13.0) } else { s.rng.range(6.0, 9.0) };
            // Upwind of the board too, so the wind blows drops onto every column.
            let x = s.rng.range(-wind * rows, cols);
            self.drops.push(Raindrop { x, y: -s.rng.range(0.0, 1.0), speed, level: s.rng.range(0.5, 0.8) });
        }
        let mut landed = Vec::new();
        self.drops.retain_mut(|d| {
            d.y += d.speed * dt;
            d.x += wind * d.speed * dt;
            if d.y < rows + 0.3 {
                return true;
            }
            if (0.0..cols).contains(&d.x) {
                landed.push(d.x as usize);
            }
            false
        });
        self.splashes.extend(landed.into_iter().map(|col| (col, 0.0)));
        for d in &self.drops {
            if (0.0..cols).contains(&d.x) {
                s.streak(d.x as usize, d.y, 1.3, d.level);
            }
        }
        self.splashes.retain_mut(|(_, age)| {
            *age += dt;
            *age < SPLASH_LIFE
        });
        let bottom = s.board.rows - 1;
        for &(col, age) in &self.splashes {
            let k = 1.0 - age / SPLASH_LIFE;
            s.lift(bottom, col, 0.55 * k);
            s.lift(bottom, col.wrapping_sub(1), 0.3 * k);
            s.lift(bottom, col + 1, 0.3 * k);
        }
        if let Some(lightning) = &mut self.lightning {
            lightning.step(s, dt);
        }
    }
}

/// A thunderstorm's flashes: one to three pulses over the whole sky and a jagged bolt.
struct Lightning {
    next: f32,
    since: f32,
    /// Start of each pulse, in seconds after the first.
    pulses: Vec<f32>,
    bolt: Vec<(usize, usize)>,
}

impl Lightning {
    fn new() -> Self {
        Self { next: 2.0, since: 0.0, pulses: Vec::new(), bolt: Vec::new() }
    }

    fn step(&mut self, s: &mut Scene, dt: f32) {
        self.next -= dt;
        if self.next <= 0.0 {
            self.next = s.rng.range(3.0, 9.0);
            self.since = 0.0;
            self.pulses = vec![0.0];
            if s.rng.chance(0.6) {
                self.pulses.push(s.rng.range(0.1, 0.18));
            }
            if s.rng.chance(0.3) {
                self.pulses.push(s.rng.range(0.25, 0.4));
            }
            self.bolt.clear();
            let mut col = s.rng.below(s.board.cols) as i32;
            for row in 0..s.board.rows {
                self.bolt.push((row, col.clamp(0, s.board.cols as i32 - 1) as usize));
                col += s.rng.below(3) as i32 - 1;
            }
        }
        self.since += dt;
        let level = self
            .pulses
            .iter()
            .enumerate()
            .filter(|(_, at)| self.since >= **at)
            .map(|(i, at)| (-(self.since - at) / 0.09).exp() * if i == 0 { 1.0 } else { 0.75 })
            .fold(0.0, f32::max);
        if level < 0.01 {
            return;
        }
        for row in 0..s.board.rows {
            for col in 0..s.board.cols {
                s.lift_accent(row, col, level * 0.45);
            }
        }
        for &(row, col) in &self.bolt {
            s.lift_accent(row, col, level);
        }
    }
}

struct Stream {
    y: f32,
    speed: f32,
    tail: f32,
    wait: f32,
}

/// Code streams falling at their own speed, one per column, over flickering glyphs.
struct Matrix {
    streams: Vec<Stream>,
    flicker: Vec<f32>,
    flicker_in: f32,
}

impl Matrix {
    fn new(board: &Board, rng: &mut Rng) -> Self {
        let streams = (0..board.cols)
            .map(|_| {
                let mut stream = Stream { y: 0.0, speed: 0.0, tail: 0.0, wait: 0.0 };
                Self::reset(&mut stream, board.rows, rng);
                stream.y = -rng.range(0.0, board.rows as f32 * 1.5);
                stream
            })
            .collect();
        Self { streams, flicker: vec![1.0; board.rows * board.cols], flicker_in: 0.0 }
    }

    fn reset(stream: &mut Stream, rows: usize, rng: &mut Rng) {
        stream.y = -rng.range(0.0, 1.5);
        stream.speed = rng.range(3.5, 8.5);
        stream.tail = rng.range(2.5, rows as f32 + 1.0);
        stream.wait = rng.range(0.0, 1.8);
    }
}

impl Animation for Matrix {
    fn step(&mut self, s: &mut Scene, dt: f32) {
        let (rows, cols) = (s.board.rows, s.board.cols);
        self.flicker_in -= dt;
        if self.flicker_in <= 0.0 {
            self.flicker_in = 0.07;
            for f in &mut self.flicker {
                if s.rng.chance(0.3) {
                    *f = s.rng.range(0.55, 1.0);
                }
            }
        }
        for (col, stream) in self.streams.iter_mut().enumerate() {
            if stream.wait > 0.0 {
                stream.wait -= dt;
                continue;
            }
            stream.y += stream.speed * dt;
            if stream.y - stream.tail > rows as f32 {
                Self::reset(stream, rows, s.rng);
                continue;
            }
            for row in 0..rows {
                let d = stream.y - (row as f32 + 0.5);
                let level = if d.abs() < 0.5 {
                    1.0
                } else if d > 0.0 && d < stream.tail {
                    0.8 * (1.0 - d / stream.tail) * self.flicker[row * cols + col]
                } else {
                    0.0
                };
                s.lift(row, col, level);
            }
        }
    }
}

struct Flake {
    x: f32,
    y: f32,
    speed: f32,
    phase: f32,
    sway: f32,
}

impl Flake {
    fn column(&self) -> f32 {
        self.x + self.phase.sin() * self.sway
    }
}

/// Flakes drift down and settle on the bottom row, which melts slowly.
struct Snow {
    flakes: Vec<Flake>,
    /// Snow lying on each column of the bottom row, 0..1.
    pile: Vec<f32>,
    spawn: f32,
}

impl Snow {
    fn new(board: &Board) -> Self {
        Self { flakes: Vec::new(), pile: vec![0.0; board.cols], spawn: 0.0 }
    }
}

impl Animation for Snow {
    fn step(&mut self, s: &mut Scene, dt: f32) {
        let (rows, cols) = (s.board.rows as f32, s.board.cols as f32);
        self.spawn += 0.12 * cols * dt;
        while self.spawn >= 1.0 {
            self.spawn -= 1.0;
            let flake = Flake { x: s.rng.range(0.0, cols), y: -0.5, speed: s.rng.range(0.9, 1.6), phase: s.rng.range(0.0, 6.3), sway: s.rng.range(0.2, 0.6) };
            self.flakes.push(flake);
        }
        let mut landed = Vec::new();
        self.flakes.retain_mut(|f| {
            f.y += f.speed * dt;
            f.phase += 1.7 * dt;
            if f.y < rows - 0.5 {
                return true;
            }
            landed.push(f.column());
            false
        });
        for x in landed {
            if let Some(pile) = self.pile.get_mut(x.clamp(0.0, cols - 1.0) as usize) {
                *pile = (*pile + 0.3).min(1.0);
            }
        }
        let bottom = s.board.rows - 1;
        for (col, pile) in self.pile.iter_mut().enumerate() {
            *pile = (*pile - 0.03 * dt).max(0.0);
            s.lift(bottom, col, *pile * 0.55);
        }
        for f in &self.flakes {
            let x = f.column();
            if !(0.0..cols).contains(&x) {
                continue;
            }
            for row in 0..s.board.rows {
                let v = (1.0 - (f.y - (row as f32 + 0.5)).abs()).max(0.0);
                s.lift(row, x as usize, v * 0.9);
            }
        }
    }
}

struct Twinkle {
    key: usize,
    age: f32,
    rise: f32,
    hold: f32,
    fall: f32,
    peak: f32,
}

struct ShootingStar {
    row: usize,
    x: f32,
    /// Cells per second; negative moves left.
    speed: f32,
}

/// A dim night sky where single keys light up and fade, and now and then a shooting star.
#[derive(Default)]
struct Stars {
    twinkles: Vec<Twinkle>,
    spawn: f32,
    shooting: Option<ShootingStar>,
    next_shooting: f32,
}

/// Heat of the night sky behind the stars.
const NIGHT: f32 = 0.05;

impl Animation for Stars {
    fn step(&mut self, s: &mut Scene, dt: f32) {
        let n = s.board.keys.len();
        self.spawn += n as f32 * 0.035 * dt;
        while self.spawn >= 1.0 {
            self.spawn -= 1.0;
            let key = s.rng.below(n);
            if !self.twinkles.iter().any(|t| t.key == key) {
                let twinkle = Twinkle {
                    key,
                    age: 0.0,
                    rise: s.rng.range(0.4, 1.2),
                    hold: s.rng.range(0.2, 1.0),
                    fall: s.rng.range(0.8, 2.0),
                    peak: s.rng.range(0.45, 1.0),
                };
                self.twinkles.push(twinkle);
            }
        }
        s.heat.fill(NIGHT);
        self.twinkles.retain_mut(|t| {
            t.age += dt;
            t.age < t.rise + t.hold + t.fall
        });
        for t in &self.twinkles {
            let v = if t.age < t.rise {
                t.age / t.rise
            } else if t.age < t.rise + t.hold {
                1.0
            } else {
                1.0 - (t.age - t.rise - t.hold) / t.fall
            };
            s.heat[t.key] = s.heat[t.key].max(NIGHT + (t.peak - NIGHT) * v * v);
        }

        self.next_shooting -= dt;
        if self.shooting.is_none() && self.next_shooting <= 0.0 {
            self.next_shooting = s.rng.range(6.0, 16.0);
            let row = s.rng.below(s.board.rows.div_ceil(2));
            let speed = s.rng.range(10.0, 16.0) * if s.rng.chance(0.5) { 1.0 } else { -1.0 };
            let x = if speed > 0.0 { -1.0 } else { s.board.cols as f32 + 1.0 };
            self.shooting = Some(ShootingStar { row, x, speed });
        }
        let cols = s.board.cols as f32;
        let mut gone = false;
        if let Some(star) = &mut self.shooting {
            star.x += star.speed * dt;
            gone = star.x < -5.0 || star.x > cols + 5.0;
            for col in 0..s.board.cols {
                // Positive behind the head, where the tail is.
                let d = (star.x - (col as f32 + 0.5)) * star.speed.signum();
                let v = if d.abs() < 0.5 {
                    1.0
                } else if d > 0.0 && d < 3.5 {
                    0.8 * (1.0 - d / 3.5)
                } else {
                    0.0
                };
                s.lift(star.row, col, v);
            }
        }
        if gone {
            self.shooting = None;
        }
    }
}

/// Slow curtains of light that sway across the upper rows.
#[derive(Default)]
struct Aurora {
    t: f32,
}

impl Animation for Aurora {
    fn step(&mut self, s: &mut Scene, dt: f32) {
        self.t += dt;
        let t = self.t;
        let rows = s.board.rows as f32;
        for row in 0..s.board.rows {
            let y = (row as f32 + 0.5) / rows;
            // Brightest a third of the way down, as a curtain hangs.
            let hang = (-((y - 0.3) / 0.4).powi(2)).exp();
            for col in 0..s.board.cols {
                let x = col as f32 + 0.5;
                let curtain = 0.5 + 0.5 * (x * 0.5 + t * 0.6 + 1.8 * (x * 0.17 - t * 0.23).sin()).sin();
                let shimmer = 0.6 + 0.4 * (x * 0.9 - t * 1.3 + row as f32 * 0.7).sin();
                s.lift(row, col, curtain * curtain * shimmer * hang);
            }
        }
    }
}

struct Ring {
    x: f32,
    y: f32,
    age: f32,
}

/// Drops fall on random keys and send rings outwards.
#[derive(Default)]
struct Ripples {
    rings: Vec<Ring>,
    next: f32,
}

const RING_LIFE: f32 = 2.2;
/// Cells per second.
const RING_SPEED: f32 = 4.5;

impl Animation for Ripples {
    fn step(&mut self, s: &mut Scene, dt: f32) {
        self.next -= dt;
        if self.next <= 0.0 {
            self.next = s.rng.range(0.5, 1.6);
            let ring = Ring { x: s.rng.range(0.0, s.board.cols as f32), y: s.rng.range(0.0, s.board.rows as f32), age: 0.0 };
            self.rings.push(ring);
        }
        self.rings.retain_mut(|r| {
            r.age += dt;
            r.age < RING_LIFE
        });
        for row in 0..s.board.rows {
            for col in 0..s.board.cols {
                let (x, y) = (col as f32 + 0.5, row as f32 + 0.5);
                let mut v = 0.0;
                for r in &self.rings {
                    let d = ((x - r.x).powi(2) + (y - r.y).powi(2)).sqrt();
                    let fade = 1.0 - r.age / RING_LIFE;
                    v += (-((d - r.age * RING_SPEED) / 0.75).powi(2)).exp() * fade;
                    if r.age < 0.3 {
                        v += (-(d * d) / 0.5).exp() * (1.0 - r.age / 0.3);
                    }
                }
                s.lift(row, col, v.min(1.0));
            }
        }
    }
}

/// Flames rising from the bottom row, with sparks.
struct Fire {
    cells: Vec<f32>,
    tick: f32,
    /// Column, height in rows and rising speed of each spark.
    sparks: Vec<(f32, f32, f32)>,
}

/// Seconds between steps of the flame simulation.
const FIRE_TICK: f32 = 0.04;

impl Fire {
    fn new(board: &Board) -> Self {
        Self { cells: vec![0.0; board.rows * board.cols], tick: 0.0, sparks: Vec::new() }
    }
}

impl Animation for Fire {
    fn step(&mut self, s: &mut Scene, dt: f32) {
        let (rows, cols) = (s.board.rows, s.board.cols);
        let bottom = rows - 1;
        self.tick = (self.tick + dt).min(FIRE_TICK * 4.0);
        while self.tick >= FIRE_TICK {
            self.tick -= FIRE_TICK;
            for col in 0..cols {
                let fuel = s.rng.range(0.55, 1.0);
                let cell = &mut self.cells[bottom * cols + col];
                *cell = *cell * 0.5 + fuel * 0.5;
            }
            for row in (0..bottom).rev() {
                for col in 0..cols {
                    let below = (row + 1) * cols;
                    let (l, m, r) = (self.cells[below + col.saturating_sub(1)], self.cells[below + col], self.cells[below + (col + 1).min(cols - 1)]);
                    let cool = s.rng.range(0.0, 0.12);
                    self.cells[row * cols + col] = ((l + 2.0 * m + r) / 4.0 * 0.74 - cool).max(0.0);
                }
            }
            if s.rng.chance(0.25) {
                self.sparks.push((s.rng.range(0.0, cols as f32), bottom as f32, s.rng.range(2.0, 3.5)));
            }
        }
        self.sparks.retain_mut(|(_, y, speed)| {
            *y -= *speed * dt;
            *y > -0.5
        });
        for row in 0..rows {
            for col in 0..cols {
                s.lift(row, col, self.cells[row * cols + col]);
            }
        }
        for &(x, y, _) in &self.sparks {
            let col = (x as usize).min(cols - 1);
            for row in 0..rows {
                let v = (1.0 - (y - (row as f32 + 0.5)).abs()).max(0.0);
                s.lift(row, col, v * 0.95);
            }
        }
    }
}

/// Conway's Game of Life on the keys: a key with three lit neighbours lights up, a lit one with two
/// or three stays lit. It starts over from a random board when it dies out or repeats itself.
struct Life {
    neighbors: Vec<Vec<usize>>,
    alive: Vec<bool>,
    level: Vec<f32>,
    timer: f32,
    generation: u32,
    history: VecDeque<u64>,
    repeats: u32,
}

const LIFE_STEP: f32 = 0.5;
const LIFE_MAX_GENERATIONS: u32 = 150;

impl Life {
    fn new(board: &Board, rng: &mut Rng) -> Self {
        let n = board.keys.len();
        let mut life = Self {
            neighbors: board.neighbors(),
            alive: vec![false; n],
            level: vec![0.0; n],
            timer: 0.0,
            generation: 0,
            history: VecDeque::new(),
            repeats: 0,
        };
        life.seed(rng);
        life
    }

    fn seed(&mut self, rng: &mut Rng) {
        for alive in &mut self.alive {
            *alive = rng.chance(0.35);
        }
        self.generation = 0;
        self.history.clear();
        self.repeats = 0;
    }

    /// FNV-1a over the lit keys.
    fn hash(&self) -> u64 {
        self.alive.iter().fold(0xcbf2_9ce4_8422_2325, |h, &a| (h ^ a as u64).wrapping_mul(0x0100_0000_01b3))
    }
}

impl Animation for Life {
    fn step(&mut self, s: &mut Scene, dt: f32) {
        self.timer += dt;
        if self.timer >= LIFE_STEP {
            self.timer = (self.timer - LIFE_STEP).min(LIFE_STEP);
            let next: Vec<bool> = (0..self.alive.len())
                .map(|k| {
                    let lit = self.neighbors[k].iter().filter(|&&j| self.alive[j]).count();
                    lit == 3 || (self.alive[k] && lit == 2)
                })
                .collect();
            self.alive = next;
            self.generation += 1;
            let hash = self.hash();
            if self.history.contains(&hash) {
                self.repeats += 1;
            }
            self.history.push_back(hash);
            if self.history.len() > 12 {
                self.history.pop_front();
            }
            let population = self.alive.iter().filter(|a| **a).count();
            if population < 3 || self.repeats > 6 || self.generation > LIFE_MAX_GENERATIONS {
                self.seed(s.rng);
            }
        }
        let k = 1.0 - (-dt / 0.15).exp();
        for (i, level) in self.level.iter_mut().enumerate() {
            let target = if self.alive[i] { 0.9 } else { 0.0 };
            *level += (target - *level) * k;
            s.heat[i] = s.heat[i].max(*level);
        }
    }
}

/// The time spelled on the number keys, hours then minutes, while the seconds of the minute fill
/// the function row.
struct Clock {
    /// Key of each digit 0-9.
    digits: [usize; 10],
    /// Function-row keys from left to right; empty on boards without one.
    f_row: Vec<usize>,
    t: f32,
}

const CLOCK_CYCLE: f32 = 7.0;
/// When each of the four digits lights up within a cycle, in seconds.
const CLOCK_DIGITS_AT: [f32; 4] = [0.0, 0.85, 2.3, 3.15];
const CLOCK_DIGIT_ON: f32 = 0.6;

impl Clock {
    fn new(board: &Board) -> Option<Self> {
        let find = |col: usize| board.keys.iter().position(|k| k.grid_row == NUMBER_ROW && k.grid_col == col);
        let mut digits = [0; 10];
        for (digit, key) in digits.iter_mut().enumerate() {
            *key = find(if digit == 0 { 10 } else { digit })?;
        }
        let mut f_row: Vec<usize> = (0..board.keys.len()).filter(|&k| board.keys[k].grid_row == NUMBER_ROW - 1).collect();
        f_row.sort_by(|a, b| board.keys[*a].x0.total_cmp(&board.keys[*b].x0));
        Some(Self { digits, f_row, t: 0.0 })
    }
}

impl Animation for Clock {
    fn step(&mut self, s: &mut Scene, dt: f32) {
        self.t = (self.t + dt) % CLOCK_CYCLE;
        let (hour, minute, second) = s.clock;
        let shown = [hour / 10, hour % 10, minute / 10, minute % 10];
        for &k in &self.digits {
            s.heat[k] = 0.07;
        }
        for (i, at) in CLOCK_DIGITS_AT.iter().enumerate() {
            let age = self.t - at;
            if (0.0..CLOCK_DIGIT_ON).contains(&age) {
                let v = if age < 0.08 { age / 0.08 } else { 1.0 - ((age - 0.08) / (CLOCK_DIGIT_ON - 0.08)).powi(2) };
                let k = self.digits[shown[i] as usize % 10];
                s.heat[k] = s.heat[k].max(v);
            }
        }
        let filled = second as f32 / 60.0 * self.f_row.len() as f32;
        for (i, &k) in self.f_row.iter().enumerate() {
            s.heat[k] = s.heat[k].max((filled - i as f32).clamp(0.0, 1.0) * 0.22);
        }
    }
}

/// A snake that takes the shortest free path to its food, grows, and starts over when it traps
/// itself or fills half the board.
struct Snake {
    steps: Vec<Vec<usize>>,
    /// Head first.
    body: VecDeque<usize>,
    /// Steps left during which the tail stays put.
    grow: usize,
    food: Option<usize>,
    timer: f32,
    t: f32,
    /// Seconds left of the flash before a new game.
    over: f32,
}

const SNAKE_STEP: f32 = 0.16;
const SNAKE_OVER: f32 = 0.9;

impl Snake {
    fn new(board: &Board, rng: &mut Rng) -> Self {
        let mut snake = Self { steps: board.steps(), body: VecDeque::new(), grow: 0, food: None, timer: 0.0, t: 0.0, over: 0.0 };
        snake.reset(rng);
        snake
    }

    fn reset(&mut self, rng: &mut Rng) {
        self.body.clear();
        self.body.push_back(rng.below(self.steps.len()));
        self.grow = 2;
        self.over = 0.0;
        self.place_food(rng);
    }

    fn place_food(&mut self, rng: &mut Rng) {
        let free: Vec<usize> = (0..self.steps.len()).filter(|k| !self.body.contains(k)).collect();
        self.food = (!free.is_empty()).then(|| free[rng.below(free.len())]);
    }

    /// Whether the head cannot enter key `k`: the body, except the tail end that moves on.
    fn blocked(&self, k: usize) -> bool {
        let moving_tail = usize::from(self.grow == 0);
        self.body.iter().take(self.body.len() - moving_tail.min(self.body.len())).any(|&b| b == k)
    }

    /// The first step of a shortest free path from the head to `goal`.
    fn path_step(&self, goal: usize) -> Option<usize> {
        let head = *self.body.front()?;
        let mut from = vec![usize::MAX; self.steps.len()];
        from[head] = head;
        let mut queue = VecDeque::from([head]);
        while let Some(k) = queue.pop_front() {
            if k == goal {
                let mut step = k;
                while from[step] != head {
                    step = from[step];
                }
                return Some(step);
            }
            for &n in &self.steps[k] {
                if from[n] == usize::MAX && !self.blocked(n) {
                    from[n] = k;
                    queue.push_back(n);
                }
            }
        }
        None
    }

    /// Free keys reachable from `start`.
    fn room(&self, start: usize) -> usize {
        let mut seen = vec![false; self.steps.len()];
        seen[start] = true;
        let mut queue = VecDeque::from([start]);
        let mut count = 0;
        while let Some(k) = queue.pop_front() {
            count += 1;
            for &n in &self.steps[k] {
                if !seen[n] && !self.blocked(n) {
                    seen[n] = true;
                    queue.push_back(n);
                }
            }
        }
        count
    }

    /// The free neighbour of the head with the most room behind it.
    fn roomiest_step(&self) -> Option<usize> {
        let head = *self.body.front()?;
        self.steps[head].iter().copied().filter(|&n| !self.blocked(n)).max_by_key(|&n| self.room(n))
    }

    fn advance(&mut self, rng: &mut Rng) {
        let Some(food) = self.food else {
            self.over = SNAKE_OVER;
            return;
        };
        let Some(next) = self.path_step(food).or_else(|| self.roomiest_step()) else {
            self.over = SNAKE_OVER;
            return;
        };
        self.body.push_front(next);
        if self.grow > 0 {
            self.grow -= 1;
        } else {
            self.body.pop_back();
        }
        if next == food {
            self.grow += 1;
            if self.body.len() * 2 >= self.steps.len() {
                self.over = SNAKE_OVER;
                return;
            }
            self.place_food(rng);
        }
    }
}

impl Animation for Snake {
    fn step(&mut self, s: &mut Scene, dt: f32) {
        self.t += dt;
        if self.over > 0.0 {
            self.over -= dt;
            if self.over <= 0.0 {
                self.reset(s.rng);
            }
        } else {
            self.timer = (self.timer + dt).min(SNAKE_STEP * 4.0);
            while self.timer >= SNAKE_STEP && self.over <= 0.0 {
                self.timer -= SNAKE_STEP;
                self.advance(s.rng);
            }
        }
        let flash = (self.over / SNAKE_OVER).clamp(0.0, 1.0);
        let n = self.body.len().max(1) as f32;
        for (i, &k) in self.body.iter().enumerate() {
            s.heat[k] = s.heat[k].max(if i == 0 { 1.0 } else { 0.85 - 0.5 * i as f32 / n });
            s.accent[k] = s.accent[k].max(flash);
        }
        if let Some(food) = self.food.filter(|_| self.over <= 0.0) {
            s.accent[food] = s.accent[food].max(0.55 + 0.45 * (self.t * 7.0).sin().abs());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::FRAME_LEN;

    fn layouts() -> Vec<Layout> {
        let mut all = Vec::new();
        for iso in [false, true] {
            all.extend([Layout::wooting_one(iso), Layout::wooting_two(iso), Layout::wooting_60(iso), Layout::wooting_80he(iso)]);
        }
        all.extend([Layout::wooting_uwu(), Layout::grid(6, 21)]);
        all
    }

    fn run(effects: &mut Effects, effect: Effect, seconds: f32) -> Frame {
        let mut frame = [0; FRAME_LEN];
        let palette = Palette::natural(effect);
        let mut t = 0.0;
        while t < seconds {
            effects.render(effect, &palette, 1.0, 1.0 / 60.0, (12, 34, 56), &mut frame);
            t += 1.0 / 60.0;
        }
        frame
    }

    #[test]
    fn codes_round_trip() {
        for effect in Effect::ALL {
            assert_eq!(Effect::parse(effect.code()), Some(effect));
        }
        assert_eq!(Effect::parse(" Rain "), Some(Effect::Rain));
        assert_eq!(Effect::parse("off"), None);
    }

    #[test]
    fn every_effect_lights_every_layout() {
        for layout in layouts() {
            for effect in Effect::ALL {
                let mut effects = Effects::new(&layout);
                // The Matrix and ripples may wait a moment before the first light.
                let lit = (0..10).any(|_| run(&mut effects, effect, 1.0).iter().any(|v| *v > 0));
                assert!(lit, "{effect:?} stays dark on a {}-key board", layout.keys.len());
            }
        }
    }

    #[test]
    fn effects_fade_in_from_dark() {
        let layout = Layout::wooting_80he(false);
        let mut effects = Effects::new(&layout);
        let mut frame = [0; FRAME_LEN];
        let palette = Palette::natural(Effect::Aurora);
        effects.render(Effect::Aurora, &palette, 1.0, 0.0, (0, 0, 0), &mut frame);
        assert!(frame.iter().all(|v| *v == 0), "nothing shows before time passes");
        let faded = *run(&mut effects, Effect::Aurora, 0.2).iter().max().unwrap();
        let full = *run(&mut effects, Effect::Aurora, 3.0).iter().max().unwrap();
        assert!(faded < full, "{faded} < {full}");
    }

    #[test]
    fn clock_spells_hours_then_minutes() {
        let layout = Layout::wooting_80he(false);
        let board = Board::new(&layout);
        let mut clock = Clock::new(&board).expect("an 80HE has number keys");
        let n = board.keys.len();
        let (mut field, mut field_accent) = (vec![0.0; board.rows * board.cols], vec![0.0; board.rows * board.cols]);
        let mut brightest_at = |t: f32| {
            let (mut heat, mut accent) = (vec![0.0; n], vec![0.0; n]);
            let mut rng = Rng::new(1);
            let mut scene = Scene { board: &board, rng: &mut rng, field: &mut field, field_accent: &mut field_accent, heat: &mut heat, accent: &mut accent, clock: (9, 47, 30) };
            clock.t = t;
            clock.step(&mut scene, 0.0);
            let k = (0..n).max_by(|a, b| heat[*a].total_cmp(&heat[*b])).unwrap();
            (board.keys[k].grid_row, board.keys[k].grid_col)
        };
        assert_eq!(brightest_at(0.2), (NUMBER_ROW, 10), "0 of 09");
        assert_eq!(brightest_at(1.05), (NUMBER_ROW, 9));
        assert_eq!(brightest_at(2.5), (NUMBER_ROW, 4));
        assert_eq!(brightest_at(3.35), (NUMBER_ROW, 7));
        assert!(Clock::new(&Board::new(&Layout::wooting_uwu())).is_none());
    }

    #[test]
    fn snake_never_crosses_itself() {
        for layout in [Layout::wooting_80he(false), Layout::wooting_60(true), Layout::wooting_two(false)] {
            let board = Board::new(&layout);
            let mut rng = Rng::new(7);
            let mut snake = Snake::new(&board, &mut rng);
            let mut eaten = 0;
            for _ in 0..5000 {
                if snake.over > 0.0 {
                    snake.reset(&mut rng);
                }
                let food = snake.food;
                snake.advance(&mut rng);
                eaten += usize::from(snake.food != food);
                let mut cells: Vec<usize> = snake.body.iter().copied().collect();
                cells.sort_unstable();
                cells.dedup();
                assert_eq!(cells.len(), snake.body.len(), "the snake overlaps itself");
                for pair in snake.body.iter().zip(snake.body.iter().skip(1)) {
                    assert!(snake.steps[*pair.1].contains(pair.0), "the snake jumped");
                }
            }
            assert!(eaten > 20, "the snake finds its food ({eaten})");
        }
    }

    #[test]
    fn life_neighbours_are_mutual() {
        for layout in layouts() {
            let neighbors = Board::new(&layout).neighbors();
            for (k, list) in neighbors.iter().enumerate() {
                assert!(list.len() <= 10, "key {k} has {} neighbours", list.len());
                assert!(list.iter().all(|j| neighbors[*j].contains(&k)));
            }
        }
    }

    #[test]
    fn palettes_run_from_dark_to_their_top_color() {
        let p = Palette::from_mode(ColorMode::Gradient([200.0, 200.0, 200.0], [255.0, 0.0, 0.0]), 0.0);
        assert_eq!(p.color(0.0, 0.0, 0.5, 0.0), BLACK);
        assert_eq!(p.color(1.0, 0.0, 0.5, 0.0), [255.0, 0.0, 0.0]);
        assert_eq!(p.color(0.0, 1.0, 0.5, 0.0), WHITE, "a full accent is the accent color");
        let solid = Palette::from_mode(ColorMode::Solid([0.0, 100.0, 200.0]), 0.0);
        assert_eq!(solid.color(0.5, 0.0, 0.0, 0.0), [0.0, 50.0, 100.0]);
        assert_eq!(Palette::from_mode(ColorMode::Flow, 0.1), Palette::Rainbow { speed: 0.1 });
    }
}
