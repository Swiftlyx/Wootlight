//! Animations for the keyboard while no music plays: rain, a thunderstorm, the Matrix, snow, a
//! starry sky, an aurora, ripples, fire, a clock and a snake that plays itself.
//!
//! Most effects run on a field of 1u cells (board rows by board width), and each key shows the
//! brightest cell it covers. The starry sky, the clock and the snake work on the keys themselves.
//! Every run draws its own random parameters, and slow drifts change them as it goes. An effect yields a heat (0..1) per key, plus an accent (0..1) for highlights such as
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
    Clock,
    Snake,
}

impl Effect {
    pub const ALL: [Effect; 10] = [
        Effect::Rain,
        Effect::Storm,
        Effect::Matrix,
        Effect::Snow,
        Effect::Stars,
        Effect::Aurora,
        Effect::Ripples,
        Effect::Fire,
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

/// A direction the player steers the snake in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Steer {
    Left = 0,
    Right = 1,
    Up = 2,
    Down = 3,
}

impl Steer {
    /// In the order of `Board::moves`.
    pub const ALL: [Steer; 4] = [Steer::Left, Steer::Right, Steer::Up, Steer::Down];
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
            Effect::Rain => (&[(0.0, BLACK), (0.3, [38.0, 42.0, 50.0]), (0.65, [165.0, 172.0, 185.0]), (1.0, WHITE)], WHITE),
            Effect::Storm => (&[(0.0, BLACK), (0.3, [30.0, 33.0, 42.0]), (0.65, [150.0, 158.0, 175.0]), (1.0, [235.0, 240.0, 255.0])], WHITE),
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

    /// The snake's moves from each key, in the order of `Steer::ALL`: the touching keys left and
    /// right in its row, and the nearest key straight above and below.
    fn moves(&self) -> Vec<[Option<usize>; 4]> {
        const GAP: f32 = 0.6;
        (0..self.keys.len())
            .map(|i| {
                let a = &self.keys[i];
                let cx = self.cx(i);
                let same_row = |j: &usize| *j != i && self.keys[*j].row == a.row;
                let left = (0..self.keys.len()).filter(same_row).find(|&j| {
                    let b = &self.keys[j];
                    b.x1 <= a.x0 + 0.01 && a.x0 - b.x1 < GAP
                });
                let right = (0..self.keys.len()).filter(same_row).find(|&j| {
                    let b = &self.keys[j];
                    b.x0 >= a.x1 - 0.01 && b.x0 - a.x1 < GAP
                });
                let vertical = |row: Option<usize>| {
                    let row = row?;
                    (0..self.keys.len())
                        .filter(|&j| self.keys[j].row == row)
                        .map(|j| (j, (self.cx(j) - cx).abs()))
                        .filter(|&(j, d)| d <= ((self.keys[j].x1 - self.keys[j].x0) / 2.0).max(0.8))
                        .min_by(|x, y| x.1.total_cmp(&y.1))
                        .map(|(j, _)| j)
                };
                [left, right, vertical(a.row.checked_sub(1)), vertical(Some(a.row + 1))]
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

    /// An arrow key the player pressed; only the snake listens.
    fn steer(&mut self, _direction: Steer) {}
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

    /// Passes an arrow key press to the effect: the snake turns that way.
    pub fn steer(&mut self, direction: Steer) {
        if let Some((_, animation)) = self.current.as_mut() {
            animation.steer(direction);
        }
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
        Effect::Rain => Box::new(Rain::new(false, rng)),
        Effect::Storm => Box::new(Rain::new(true, rng)),
        Effect::Matrix => Box::new(Matrix::new(board, rng)),
        Effect::Snow => Box::new(Snow::new(board, rng)),
        Effect::Stars => Box::new(Stars::new(rng)),
        Effect::Aurora => Box::new(Aurora::new(rng)),
        Effect::Ripples => Box::new(Ripples::new(rng)),
        Effect::Fire => Box::new(Fire::new(board, rng)),
        // A board without number keys (UwU) shows the starry sky instead.
        Effect::Clock => match Clock::new(board) {
            Some(clock) => Box::new(clock),
            None => Box::new(Stars::new(rng)),
        },
        Effect::Snake => Box::new(Snake::new(board, rng)),
    }
}

/// A value that wanders smoothly between `lo` and `hi`, heading for a new random target every
/// `every` seconds or so: what makes every run of an effect its own.
struct Drift {
    value: f32,
    target: f32,
    lo: f32,
    hi: f32,
    every: f32,
    next: f32,
}

impl Drift {
    fn new(lo: f32, hi: f32, every: f32, rng: &mut Rng) -> Self {
        let value = rng.range(lo, hi);
        Self { value, target: value, lo, hi, every, next: rng.range(0.5, 1.5) * every }
    }

    fn step(&mut self, dt: f32, rng: &mut Rng) -> f32 {
        self.next -= dt;
        if self.next <= 0.0 {
            self.target = rng.range(self.lo, self.hi);
            self.next = rng.range(0.5, 1.5) * self.every;
        }
        self.value += (self.target - self.value) * (1.0 - (-dt / (self.every * 0.4)).exp());
        self.value
    }
}

fn either_way(rng: &mut Rng) -> f32 {
    if rng.chance(0.5) { 1.0 } else { -1.0 }
}

struct Raindrop {
    x: f32,
    y: f32,
    speed: f32,
    level: f32,
    tail: f32,
}

/// Drops fall down the board and splash on the bottom row. Showers come and go, and every one has
/// its own wind; the storm blows harder and adds lightning.
struct Rain {
    drops: Vec<Raindrop>,
    /// Bottom-row splashes: cell column and age in seconds.
    splashes: Vec<(usize, f32)>,
    spawn: f32,
    /// How hard it rains, as a share of the full rate.
    shower: Drift,
    /// This shower's sideways drift in cells per row of fall, and the gusts around it.
    wind: f32,
    gust: Drift,
    lightning: Option<Lightning>,
}

const SPLASH_LIFE: f32 = 0.3;

impl Rain {
    fn new(storm: bool, rng: &mut Rng) -> Self {
        let side = either_way(rng);
        let wind = side * if storm { rng.range(0.2, 0.45) } else { rng.range(0.0, 0.12) };
        Self {
            drops: Vec::new(),
            splashes: Vec::new(),
            spawn: 0.0,
            shower: Drift::new(0.35, 1.3, 6.0, rng),
            wind,
            gust: Drift::new(-0.08, 0.08, 3.0, rng),
            lightning: storm.then(|| Lightning::new(rng)),
        }
    }
}

impl Animation for Rain {
    fn step(&mut self, s: &mut Scene, dt: f32) {
        let (rows, cols) = (s.board.rows as f32, s.board.cols as f32);
        let storm = self.lightning.is_some();
        // Drops per column per second.
        let rate = if storm { 0.75 } else { 0.4 } * self.shower.step(dt, s.rng);
        let wind = self.wind + self.gust.step(dt, s.rng);
        self.spawn += rate * cols * dt;
        while self.spawn >= 1.0 {
            self.spawn -= 1.0;
            let speed = if storm { s.rng.range(9.0, 13.0) } else { s.rng.range(6.0, 9.0) };
            // Upwind of the board too, so the wind blows drops onto every column.
            let x = if wind >= 0.0 { s.rng.range(-wind * rows, cols) } else { s.rng.range(0.0, cols - wind * rows) };
            let drop = Raindrop { x, y: -s.rng.range(0.0, 1.0), speed, level: s.rng.range(0.45, 0.85), tail: s.rng.range(0.9, 1.7) };
            self.drops.push(drop);
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
                s.streak(d.x as usize, d.y, d.tail, d.level);
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

/// A thunderstorm's flashes: one to three pulses over the whole sky, with a jagged bolt when the
/// strike is near; distant ones only brighten the sky a little.
struct Lightning {
    next: f32,
    since: f32,
    /// Start of each pulse, in seconds after the first.
    pulses: Vec<f32>,
    bolt: Vec<(usize, usize)>,
    /// 1 for a strike nearby, less for a distant one.
    strength: f32,
}

impl Lightning {
    fn new(rng: &mut Rng) -> Self {
        Self { next: rng.range(1.5, 5.0), since: 0.0, pulses: Vec::new(), bolt: Vec::new(), strength: 1.0 }
    }

    fn step(&mut self, s: &mut Scene, dt: f32) {
        self.next -= dt;
        if self.next <= 0.0 {
            self.next = s.rng.range(3.0, 10.0);
            self.since = 0.0;
            self.pulses = vec![0.0];
            if s.rng.chance(0.6) {
                self.pulses.push(s.rng.range(0.1, 0.18));
            }
            if s.rng.chance(0.3) {
                self.pulses.push(s.rng.range(0.25, 0.4));
            }
            self.strength = if s.rng.chance(0.35) { s.rng.range(0.3, 0.55) } else { 1.0 };
            self.bolt.clear();
            if self.strength >= 1.0 {
                let mut col = s.rng.below(s.board.cols) as i32;
                for row in 0..s.board.rows {
                    self.bolt.push((row, col.clamp(0, s.board.cols as i32 - 1) as usize));
                    col += s.rng.below(3) as i32 - 1;
                }
            }
        }
        self.since += dt;
        let level = self
            .pulses
            .iter()
            .enumerate()
            .filter(|(_, at)| self.since >= **at)
            .map(|(i, at)| (-(self.since - at) / 0.09).exp() * if i == 0 { 1.0 } else { 0.75 })
            .fold(0.0, f32::max)
            * self.strength;
        if level < 0.03 {
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

/// Code streams falling at their own speed, one per column, over flickering glyphs; the whole rain
/// of code speeds up and slows down a little over time.
struct Matrix {
    streams: Vec<Stream>,
    flicker: Vec<f32>,
    flicker_in: f32,
    tempo: Drift,
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
        Self { streams, flicker: vec![1.0; board.rows * board.cols], flicker_in: 0.0, tempo: Drift::new(0.85, 1.15, 4.0, rng) }
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
        let tempo = self.tempo.step(dt, s.rng);
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
            stream.y += stream.speed * tempo * dt;
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

/// Flakes drift down in flurries that come and go, blown by a changing wind, and pile up. A column
/// grows to half the board's height; then its top row melts away and the pile grows again.
struct Snow {
    flakes: Vec<Flake>,
    /// Snow lying on each column, in rows.
    depth: Vec<f32>,
    /// How far the top row of each column has melted, 0..1, while it melts.
    melting: Vec<Option<f32>>,
    spawn: f32,
    fall: Drift,
    /// Cells per second.
    wind: Drift,
}

/// Seconds for the top row of a full column to melt.
const MELT_TIME: f32 = 3.0;
/// Rows a landing flake adds to its column.
const FLAKE_DEPTH: f32 = 0.12;

impl Snow {
    fn new(board: &Board, rng: &mut Rng) -> Self {
        Self {
            flakes: Vec::new(),
            depth: vec![0.0; board.cols],
            melting: vec![None; board.cols],
            spawn: 0.0,
            fall: Drift::new(0.4, 1.6, 7.0, rng),
            wind: Drift::new(-0.35, 0.35, 5.0, rng),
        }
    }
}

impl Animation for Snow {
    fn step(&mut self, s: &mut Scene, dt: f32) {
        let (rows, cols) = (s.board.rows as f32, s.board.cols as f32);
        let full = (rows / 2.0).max(1.0);
        let wind = self.wind.step(dt, s.rng);
        self.spawn += 0.12 * self.fall.step(dt, s.rng) * cols * dt;
        while self.spawn >= 1.0 {
            self.spawn -= 1.0;
            let flake = Flake { x: s.rng.range(0.0, cols), y: -0.5, speed: s.rng.range(0.9, 1.6), phase: s.rng.range(0.0, 6.3), sway: s.rng.range(0.2, 0.6) };
            self.flakes.push(flake);
        }
        let depth = &mut self.depth;
        self.flakes.retain_mut(|f| {
            f.y += f.speed * dt;
            f.x += wind * dt;
            f.phase += 1.7 * dt;
            let x = f.column();
            if !(0.0..cols).contains(&x) {
                return f.y < rows + 1.0;
            }
            let col = x as usize;
            // A flake settles on top of the snow already lying in its column.
            if f.y < rows - depth[col] - 0.5 {
                return true;
            }
            depth[col] = (depth[col] + FLAKE_DEPTH).min(full);
            false
        });
        let bottom = s.board.rows - 1;
        for col in 0..s.board.cols {
            match self.melting[col] {
                None if self.depth[col] >= full - 1e-3 => self.melting[col] = Some(0.0),
                Some(melt) if melt + dt / MELT_TIME >= 1.0 => {
                    self.depth[col] = (self.depth[col] - 1.0).max(0.0);
                    self.melting[col] = None;
                }
                Some(melt) => self.melting[col] = Some(melt + dt / MELT_TIME),
                None => {}
            }
            let depth = self.depth[col];
            let layers = depth.ceil() as usize;
            for layer in 0..layers.min(s.board.rows) {
                let mut fill = (depth - layer as f32).clamp(0.0, 1.0);
                // The top row of a melting column fades away.
                if layer + 1 == layers {
                    fill *= 1.0 - self.melting[col].unwrap_or(0.0);
                }
                s.lift(bottom - layer, col, fill * 0.7);
            }
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

/// Position in cells and rows, velocity in cells and rows per second, and tail length.
struct ShootingStar {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    tail: f32,
}

/// A dim night sky where single keys light up and fade, more of them at some times than others,
/// and now and then a shooting star: level, slanting down or up a row or two, or straight down.
struct Stars {
    twinkles: Vec<Twinkle>,
    spawn: f32,
    density: Drift,
    shooting: Option<ShootingStar>,
    next_shooting: f32,
}

/// Heat of the night sky behind the stars.
const NIGHT: f32 = 0.05;

impl Stars {
    fn new(rng: &mut Rng) -> Self {
        Self { twinkles: Vec::new(), spawn: 0.0, density: Drift::new(0.6, 1.5, 8.0, rng), shooting: None, next_shooting: rng.range(3.0, 9.0) }
    }

    fn launch(rows: f32, cols: f32, rng: &mut Rng) -> ShootingStar {
        let kind = rng.unit();
        if kind < 0.2 {
            // Straight down from the top, drifting a little sideways.
            return ShootingStar { x: rng.range(0.5, cols - 0.5), y: -1.0, vx: rng.range(-0.8, 0.8), vy: rng.range(6.0, 9.0), tail: 2.5 };
        }
        let speed = rng.range(10.0, 16.0) * either_way(rng);
        let flight = (cols + 8.0) / speed.abs();
        // Half of the rest fly level; the others drift one or two rows on the way, mostly down.
        let shift = if kind < 0.6 { 0.0 } else { rng.range(1.0, 2.0) * if rng.chance(0.75) { 1.0 } else { -1.0 } };
        let (top, bottom) = if shift >= 0.0 { (0.5, (rows - 0.5 - shift).max(0.5)) } else { ((0.5 - shift).min(rows - 0.5), rows - 0.5) };
        let y = if shift == 0.0 { rng.range(0.5, (rows * 0.6).max(0.6)) } else { rng.range(top, bottom.max(top)) };
        ShootingStar { x: if speed > 0.0 { -4.0 } else { cols + 4.0 }, y, vx: speed, vy: shift / flight, tail: 3.5 }
    }
}

impl Animation for Stars {
    fn step(&mut self, s: &mut Scene, dt: f32) {
        let n = s.board.keys.len();
        self.spawn += n as f32 * 0.035 * self.density.step(dt, s.rng) * dt;
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

        let (rows, cols) = (s.board.rows as f32, s.board.cols as f32);
        self.next_shooting -= dt;
        if self.shooting.is_none() && self.next_shooting <= 0.0 {
            self.next_shooting = s.rng.range(5.0, 14.0);
            self.shooting = Some(Self::launch(rows, cols, s.rng));
        }
        let mut gone = false;
        if let Some(star) = &mut self.shooting {
            star.x += star.vx * dt;
            star.y += star.vy * dt;
            gone = star.x < -6.0 || star.x > cols + 6.0 || star.y > rows + 4.0 || star.y < -4.0;
            let speed = star.vx.hypot(star.vy).max(1e-3);
            let (dx, dy) = (star.vx / speed, star.vy / speed);
            for row in 0..s.board.rows {
                for col in 0..s.board.cols {
                    let (rx, ry) = (star.x - (col as f32 + 0.5), star.y - (row as f32 + 0.5));
                    // Distance behind the head along the flight, and off its line.
                    let along = rx * dx + ry * dy;
                    let off = (rx * dy - ry * dx).abs();
                    if off >= 0.7 {
                        continue;
                    }
                    let v = if along.abs() < 0.5 {
                        1.0
                    } else if along > 0.0 && along < star.tail {
                        0.8 * (1.0 - along / star.tail)
                    } else {
                        0.0
                    };
                    s.lift(row, col, v * (1.0 - off / 0.7));
                }
            }
        }
        if gone {
            self.shooting = None;
        }
    }
}

/// Slow curtains of light that sway across the board; every run has its own waves, and the
/// curtain rises, sinks and brightens over time.
struct Aurora {
    t: f32,
    /// Spatial frequency, phase and speed of the three waves.
    k: [f32; 3],
    phase: [f32; 3],
    speed: [f32; 3],
    /// Where the curtain hangs, as a share of the board's height, and how bright it is.
    height: Drift,
    glow: Drift,
}

impl Aurora {
    fn new(rng: &mut Rng) -> Self {
        let k = [rng.range(0.35, 0.65), rng.range(0.12, 0.22), rng.range(0.7, 1.1)];
        let phase = [rng.range(0.0, 6.3), rng.range(0.0, 6.3), rng.range(0.0, 6.3)];
        let speed = [rng.range(0.4, 0.8) * either_way(rng), rng.range(0.15, 0.3) * either_way(rng), rng.range(0.9, 1.6) * either_way(rng)];
        Self { t: 0.0, k, phase, speed, height: Drift::new(0.2, 0.45, 9.0, rng), glow: Drift::new(0.7, 1.0, 6.0, rng) }
    }
}

impl Animation for Aurora {
    fn step(&mut self, s: &mut Scene, dt: f32) {
        self.t += dt;
        let (t, k, p, w) = (self.t, self.k, self.phase, self.speed);
        let center = self.height.step(dt, s.rng);
        let glow = self.glow.step(dt, s.rng);
        let rows = s.board.rows as f32;
        for row in 0..s.board.rows {
            let y = (row as f32 + 0.5) / rows;
            let hang = (-((y - center) / 0.4).powi(2)).exp();
            for col in 0..s.board.cols {
                let x = col as f32 + 0.5;
                let curtain = 0.5 + 0.5 * (x * k[0] + t * w[0] + p[0] + 1.8 * (x * k[1] - t * w[1] + p[1]).sin()).sin();
                let shimmer = 0.6 + 0.4 * (x * k[2] - t * w[2] + row as f32 * 0.7 + p[2]).sin();
                s.lift(row, col, curtain * curtain * shimmer * hang * glow);
            }
        }
    }
}

struct Ring {
    x: f32,
    y: f32,
    /// Seconds; negative while it waits to start.
    age: f32,
    life: f32,
    /// Cells per second.
    speed: f32,
    width: f32,
    strength: f32,
}

/// Drops fall on random keys and send rings outwards, each its own size and speed, sometimes two
/// close together; the rain on the water comes faster and slower.
struct Ripples {
    rings: Vec<Ring>,
    next: f32,
    pace: Drift,
}

impl Ripples {
    fn new(rng: &mut Rng) -> Self {
        Self { rings: Vec::new(), next: 0.0, pace: Drift::new(0.5, 1.6, 6.0, rng) }
    }

    fn drop_at(x: f32, y: f32, delay: f32, rng: &mut Rng) -> Ring {
        Ring { x, y, age: -delay, life: rng.range(1.6, 2.6), speed: rng.range(3.5, 5.5), width: rng.range(0.55, 0.9), strength: rng.range(0.6, 1.0) }
    }
}

impl Animation for Ripples {
    fn step(&mut self, s: &mut Scene, dt: f32) {
        let pace = self.pace.step(dt, s.rng);
        self.next -= dt;
        if self.next <= 0.0 {
            self.next = s.rng.range(0.5, 1.2) * pace;
            let (x, y) = (s.rng.range(0.0, s.board.cols as f32), s.rng.range(0.0, s.board.rows as f32));
            self.rings.push(Self::drop_at(x, y, 0.0, s.rng));
            if s.rng.chance(0.2) {
                let (dx, dy, delay) = (s.rng.range(-1.5, 1.5), s.rng.range(-1.0, 1.0), s.rng.range(0.12, 0.3));
                self.rings.push(Self::drop_at(x + dx, y + dy, delay, s.rng));
            }
        }
        self.rings.retain_mut(|r| {
            r.age += dt;
            r.age < r.life
        });
        for row in 0..s.board.rows {
            for col in 0..s.board.cols {
                let (x, y) = (col as f32 + 0.5, row as f32 + 0.5);
                let mut v = 0.0;
                for r in self.rings.iter().filter(|r| r.age >= 0.0) {
                    let d = ((x - r.x).powi(2) + (y - r.y).powi(2)).sqrt();
                    let fade = 1.0 - r.age / r.life;
                    v += (-((d - r.age * r.speed) / r.width).powi(2)).exp() * fade * r.strength;
                    if r.age < 0.3 {
                        v += (-(d * d) / 0.5).exp() * (1.0 - r.age / 0.3) * r.strength;
                    }
                }
                s.lift(row, col, v.min(1.0));
            }
        }
    }
}

/// Flames rising from the bottom row, burning higher and lower over time, with sparks.
struct Fire {
    cells: Vec<f32>,
    tick: f32,
    /// Column, height in rows and rising speed of each spark.
    sparks: Vec<(f32, f32, f32)>,
    strength: Drift,
}

/// Seconds between steps of the flame simulation.
const FIRE_TICK: f32 = 0.04;

impl Fire {
    fn new(board: &Board, rng: &mut Rng) -> Self {
        Self { cells: vec![0.0; board.rows * board.cols], tick: 0.0, sparks: Vec::new(), strength: Drift::new(0.75, 1.05, 4.0, rng) }
    }
}

impl Animation for Fire {
    fn step(&mut self, s: &mut Scene, dt: f32) {
        let (rows, cols) = (s.board.rows, s.board.cols);
        let bottom = rows - 1;
        let strength = self.strength.step(dt, s.rng);
        self.tick = (self.tick + dt).min(FIRE_TICK * 4.0);
        while self.tick >= FIRE_TICK {
            self.tick -= FIRE_TICK;
            for col in 0..cols {
                let fuel = s.rng.range(0.55, 1.0) * strength;
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
            if s.rng.chance(0.25 * strength) {
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

/// The time, steady: the hour on F1-F12, the minutes on the number keys (tens in the effect's
/// color, ones in its accent), and the seconds filling the hour row from left to right. A board
/// without a function row shows the hour on 1-0, - and =, and the minutes on Q-P.
struct Clock {
    /// Keys of the hours 1-12.
    hours: [usize; 12],
    /// Keys of the minute digits 0-9.
    digits: [usize; 10],
    t: f32,
    second: u32,
    /// Seconds since `second` began, for a smooth fill.
    since: f32,
}

impl Clock {
    fn new(board: &Board) -> Option<Self> {
        let at = |row: usize, col: usize| board.keys.iter().position(|k| k.grid_row == row && k.grid_col == col);
        // The function row: every key of grid row 0 but Esc, from the left.
        let mut f_keys: Vec<usize> = (0..board.keys.len()).filter(|&k| board.keys[k].grid_row == 0 && board.keys[k].grid_col != 0).collect();
        f_keys.sort_by(|a, b| board.keys[*a].x0.total_cmp(&board.keys[*b].x0));
        let (hours, digit_row): (Vec<usize>, usize) = if f_keys.len() >= 12 {
            (f_keys[..12].to_vec(), NUMBER_ROW)
        } else {
            ((1..=12).map(|col| at(NUMBER_ROW, col)).collect::<Option<_>>()?, NUMBER_ROW + 1)
        };
        let mut digits = [0; 10];
        for (digit, key) in digits.iter_mut().enumerate() {
            *key = at(digit_row, if digit == 0 { 10 } else { digit })?;
        }
        Some(Self { hours: hours.try_into().ok()?, digits, t: 0.0, second: u32::MAX, since: 0.0 })
    }
}

impl Animation for Clock {
    fn step(&mut self, s: &mut Scene, dt: f32) {
        self.t += dt;
        let (hour, minute, second) = s.clock;
        if second == self.second {
            self.since = (self.since + dt).min(1.0);
        } else {
            self.second = second;
            self.since = 0.0;
        }
        for &k in self.hours.iter().chain(&self.digits) {
            s.heat[k] = 0.06;
        }
        let filled = (second as f32 + self.since) / 60.0 * self.hours.len() as f32;
        for (i, &k) in self.hours.iter().enumerate() {
            s.heat[k] = s.heat[k].max((filled - i as f32).clamp(0.0, 1.0) * 0.25);
        }
        // 1-12 o'clock on keys 0-11; midnight and noon on the twelfth.
        s.heat[self.hours[((hour + 11) % 12) as usize]] = 1.0;
        let (tens, ones) = (self.digits[(minute / 10 % 10) as usize], self.digits[(minute % 10) as usize]);
        s.heat[tens] = 1.0;
        // One key for both digits breathes between their two colors.
        s.accent[ones] = if ones == tens { 0.85 * (0.5 + 0.5 * (self.t * 2.0).sin()) } else { 0.85 };
    }
}

/// A snake that plays itself: it heads for the food by a short free path, takes only moves after
/// which its tail stays reachable, so it does not wall itself in, and strays from the shortest way
/// by a taste for detours drawn anew for every game. It starts slow and speeds up as the game goes
/// on, and the faster it goes, the more it slips, so it loses now and then. The player can take
/// over with the arrow keys; left alone for `PLAYER_PATIENCE`, it plays on its own again. A new
/// game starts when it crashes or fills half the board.
struct Snake {
    moves: Vec<[Option<usize>; 4]>,
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
    /// Noise the AI adds to path lengths, in steps: how far it strays from the shortest way.
    wander: f32,
    /// The direction of the last step.
    heading: Option<Steer>,
    /// The direction the player steers in and when they pressed it (in `t`).
    steer: Option<(Steer, f32)>,
    /// Steps since the snake last ate.
    hungry: u32,
    /// Seconds this game has run, and the food eaten in it: both speed the snake up.
    age: f32,
    eaten: u32,
}

/// Seconds per step at the start of a game and at full speed.
const SNAKE_SLOW: f32 = 0.32;
const SNAKE_FAST: f32 = 0.09;
/// Seconds of play until full speed; each food eaten counts as `SNAKE_RAMP / 50`.
const SNAKE_RAMP: f32 = 75.0;
/// Chance per step that the AI slips and takes a random turn, at the start and at full speed.
const SLIP_SLOW: f32 = 0.002;
const SLIP_FAST: f32 = 0.02;
const SNAKE_OVER: f32 = 0.9;
/// Seconds without an arrow press after which the snake plays on its own again.
const PLAYER_PATIENCE: f32 = 6.0;
/// Steps without food after which the AI takes risky moves to reach it.
const HUNGER_LIMIT: u32 = 120;

impl Snake {
    fn new(board: &Board, rng: &mut Rng) -> Self {
        let moves = board.moves();
        let steps = moves.iter().map(|m| m.iter().flatten().copied().collect()).collect();
        let mut snake = Self {
            moves,
            steps,
            body: VecDeque::new(),
            grow: 0,
            food: None,
            timer: 0.0,
            t: 0.0,
            over: 0.0,
            wander: 0.0,
            heading: None,
            steer: None,
            hungry: 0,
            age: 0.0,
            eaten: 0,
        };
        snake.reset(rng);
        snake
    }

    fn reset(&mut self, rng: &mut Rng) {
        self.body.clear();
        self.body.push_back(rng.below(self.steps.len()));
        self.grow = 2;
        self.over = 0.0;
        self.wander = rng.range(0.0, 2.5);
        self.heading = None;
        self.steer = None;
        self.hungry = 0;
        self.age = 0.0;
        self.eaten = 0;
        self.place_food(rng);
    }

    /// How far the game has sped up, 0 at the start and 1 at full speed.
    fn haste(&self) -> f32 {
        (self.age / SNAKE_RAMP + self.eaten as f32 / 50.0).min(1.0)
    }

    /// Seconds per step now.
    fn interval(&self) -> f32 {
        SNAKE_SLOW + (SNAKE_FAST - SNAKE_SLOW) * self.haste()
    }

    /// A careless turn: any neighbour but the neck, the body included, which ends the game.
    fn slip(&self, rng: &mut Rng) -> Option<usize> {
        let head = *self.body.front()?;
        let neck = self.body.get(1).copied();
        let turns: Vec<usize> = self.steps[head].iter().copied().filter(|&n| Some(n) != neck).collect();
        let next = *turns.get(rng.below(turns.len()))?;
        (!self.blocked(next)).then_some(next)
    }

    /// Food goes on a free key with at least two ways in, so it is never a dead end.
    fn place_food(&mut self, rng: &mut Rng) {
        let free: Vec<usize> = (0..self.steps.len()).filter(|k| !self.body.contains(k)).collect();
        let open: Vec<usize> = free.iter().copied().filter(|&k| self.steps[k].len() >= 2).collect();
        let pick = if open.is_empty() { &free } else { &open };
        self.food = (!pick.is_empty()).then(|| pick[rng.below(pick.len())]);
    }

    /// Whether the head cannot enter key `k`: the body, except the tail end that moves on.
    fn blocked(&self, k: usize) -> bool {
        let moving_tail = usize::from(self.grow == 0);
        self.body.iter().take(self.body.len() - moving_tail.min(self.body.len())).any(|&b| b == k)
    }

    /// Steps from every key to `goal` over keys the head may enter; `usize::MAX` where it cannot get.
    fn distances(&self, goal: usize) -> Vec<usize> {
        let mut dist = vec![usize::MAX; self.steps.len()];
        dist[goal] = 0;
        let mut queue = VecDeque::from([goal]);
        while let Some(k) = queue.pop_front() {
            for &n in &self.steps[k] {
                if dist[n] == usize::MAX && !self.blocked(n) {
                    dist[n] = dist[k] + 1;
                    queue.push_back(n);
                }
            }
        }
        dist
    }

    /// Whether, after moving the head to `next`, a free path still leads from the head to the tail.
    fn safe_after(&self, next: usize) -> bool {
        let keeps_tail = self.grow > 0 || Some(next) == self.food;
        let mut body: Vec<usize> = std::iter::once(next).chain(self.body.iter().copied()).collect();
        if !keeps_tail {
            body.pop();
        }
        let tail = *body.last().unwrap_or(&next);
        if tail == next {
            return true;
        }
        let mut closed = vec![false; self.steps.len()];
        for &b in &body[..body.len() - 1] {
            closed[b] = true;
        }
        let mut queue = VecDeque::from([next]);
        while let Some(k) = queue.pop_front() {
            for &n in &self.steps[k] {
                if n == tail {
                    return true;
                }
                if !closed[n] {
                    closed[n] = true;
                    queue.push_back(n);
                }
            }
        }
        false
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

    /// The AI's next key: the free neighbour closest to the food, with this game's noise, and not
    /// one that cuts the head off from the tail while a safe one exists, unless it has gone hungry.
    fn choose(&self, rng: &mut Rng) -> Option<usize> {
        let risk = if self.hungry < HUNGER_LIMIT { 1000.0 } else { 0.0 };
        let head = *self.body.front()?;
        let to_food = self.food.map(|food| self.distances(food));
        let candidates: Vec<usize> = self.steps[head].iter().copied().filter(|&n| !self.blocked(n)).collect();
        candidates
            .into_iter()
            .map(|n| {
                let dist = to_food.as_ref().map_or(usize::MAX, |d| d[n]);
                let mut score = if dist == usize::MAX { 60.0 } else { dist as f32 };
                score += rng.range(0.0, self.wander);
                if !self.safe_after(n) {
                    score += risk;
                }
                // With the food out of reach, more room ahead wins.
                score -= self.room(n) as f32 * 0.05;
                (n, score)
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(n, _)| n)
    }

    /// The player's next key: the pressed direction, or straight on when it points back into the
    /// neck. None for a wall or the body: the player loses this game.
    fn player_move(&mut self, direction: Steer) -> Option<usize> {
        let head = *self.body.front()?;
        let neck = self.body.get(1).copied();
        let pressed = self.moves[head][direction as usize];
        let next = if pressed.is_some() && pressed == neck { self.heading.and_then(|h| self.moves[head][h as usize]) } else { pressed };
        next.filter(|&n| !self.blocked(n))
    }

    fn advance(&mut self, rng: &mut Rng) {
        let player = self.steer.filter(|(_, at)| self.t - at < PLAYER_PATIENCE).map(|(direction, _)| direction);
        if player.is_none() {
            self.steer = None;
        }
        let head = self.body.front().copied();
        let slips = rng.chance(SLIP_SLOW + (SLIP_FAST - SLIP_SLOW) * self.haste());
        let next = match player {
            Some(direction) => self.player_move(direction),
            None if slips => self.slip(rng),
            None => self.choose(rng),
        };
        let Some(next) = next else {
            self.over = SNAKE_OVER;
            return;
        };
        self.heading = head.and_then(|h| Steer::ALL.into_iter().find(|d| self.moves[h][*d as usize] == Some(next)));
        let ate = Some(next) == self.food;
        self.hungry = if ate { 0 } else { self.hungry + 1 };
        self.body.push_front(next);
        if self.grow > 0 {
            self.grow -= 1;
        } else {
            self.body.pop_back();
        }
        if ate {
            self.grow += 1;
            self.eaten += 1;
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
            self.age += dt;
            self.timer = (self.timer + dt).min(SNAKE_SLOW * 2.0);
            while self.timer >= self.interval() && self.over <= 0.0 {
                self.timer -= self.interval();
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

    fn steer(&mut self, direction: Steer) {
        if self.over <= 0.0 {
            self.steer = Some((direction, self.t));
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

    /// The keys the clock lights at 9:47:30: (grid row, grid column) of full heat and of the accent.
    fn clock_keys(layout: &Layout) -> (Vec<(usize, usize)>, Vec<(usize, usize)>) {
        let board = Board::new(layout);
        let mut clock = Clock::new(&board).expect("a board with number keys");
        let n = board.keys.len();
        let (mut field, mut field_accent) = (vec![0.0; board.rows * board.cols], vec![0.0; board.rows * board.cols]);
        let (mut heat, mut accent) = (vec![0.0; n], vec![0.0; n]);
        let mut rng = Rng::new(1);
        let mut scene = Scene { board: &board, rng: &mut rng, field: &mut field, field_accent: &mut field_accent, heat: &mut heat, accent: &mut accent, clock: (9, 47, 30) };
        clock.step(&mut scene, 0.0);
        let cell = |k: usize| (board.keys[k].grid_row, board.keys[k].grid_col);
        ((0..n).filter(|&k| heat[k] >= 1.0).map(cell).collect(), (0..n).filter(|&k| accent[k] > 0.5).map(cell).collect())
    }

    #[test]
    fn clock_shows_hours_on_f_keys_and_minutes_on_digits() {
        // F9 is grid column 10 on the 80HE; 4 and 7 are columns 4 and 7 of the number row.
        assert_eq!(clock_keys(&Layout::wooting_80he(false)), (vec![(0, 10), (NUMBER_ROW, 4)], vec![(NUMBER_ROW, 7)]));
        assert_eq!(clock_keys(&Layout::wooting_two(false)), (vec![(0, 10), (NUMBER_ROW, 4)], vec![(NUMBER_ROW, 7)]));
        // Without a function row: 9 o'clock on the 9 key, the minutes on R and U.
        assert_eq!(clock_keys(&Layout::wooting_60(false)), (vec![(NUMBER_ROW, 9), (NUMBER_ROW + 1, 4)], vec![(NUMBER_ROW + 1, 7)]));
        assert!(Clock::new(&Board::new(&Layout::wooting_uwu())).is_none());
    }

    #[test]
    fn snow_piles_up_to_half_the_board_and_melts_a_row() {
        let layout = Layout::wooting_80he(false);
        let board = Board::new(&layout);
        let mut rng = Rng::new(5);
        let mut snow = Snow::new(&board, &mut rng);
        let n = board.keys.len();
        let (mut field, mut field_accent) = (vec![0.0; board.rows * board.cols], vec![0.0; board.rows * board.cols]);
        let (mut heat, mut accent) = (vec![0.0; n], vec![0.0; n]);
        let (mut deepest, mut melted) = (0.0f32, false);
        for _ in 0..(60 * 600) {
            let mut scene = Scene { board: &board, rng: &mut rng, field: &mut field, field_accent: &mut field_accent, heat: &mut heat, accent: &mut accent, clock: (0, 0, 0) };
            let before: f32 = snow.depth.iter().sum();
            snow.step(&mut scene, 1.0 / 60.0);
            melted |= snow.depth.iter().sum::<f32>() < before - 0.9;
            deepest = snow.depth.iter().fold(deepest, |m, d| m.max(*d));
        }
        assert!((deepest - board.rows as f32 / 2.0).abs() < 1e-3, "piles up to half the board ({deepest})");
        assert!(melted, "a full column melts a row");
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
            assert!(eaten > 150, "the snake finds its food ({eaten})");
        }
    }

    #[test]
    fn the_snake_speeds_up_and_can_lose() {
        let board = Board::new(&Layout::wooting_80he(false));
        let mut rng = Rng::new(11);
        let mut snake = Snake::new(&board, &mut rng);
        let start = snake.interval();
        snake.age = SNAKE_RAMP;
        assert!(snake.interval() < start / 3.0, "{} then {}", start, snake.interval());
        let mut lost = 0;
        for _ in 0..20_000 {
            if snake.over > 0.0 {
                lost += usize::from(snake.body.len() * 2 < snake.steps.len());
                snake.reset(&mut rng);
                snake.age = SNAKE_RAMP;
            }
            snake.advance(&mut rng);
        }
        assert!(lost > 0, "at full speed the snake loses sometimes");
    }

    #[test]
    fn every_game_takes_its_own_way() {
        let board = Board::new(&Layout::wooting_80he(false));
        let path = |seed: u64| {
            let mut rng = Rng::new(seed);
            let mut snake = Snake::new(&board, &mut rng);
            (0..300).map(|_| {
                snake.advance(&mut rng);
                snake.body.front().copied()
            }).collect::<Vec<_>>()
        };
        assert_ne!(path(1), path(2));
    }

    #[test]
    fn arrows_steer_the_snake_and_walls_end_the_game() {
        let board = Board::new(&Layout::wooting_80he(false));
        let mut rng = Rng::new(3);
        let mut snake = Snake::new(&board, &mut rng);
        // Somewhere in the middle of the board, with no key to its left on the far side.
        let start = (0..board.keys.len()).find(|&k| snake.moves[k].iter().all(Option::is_some)).unwrap();
        snake.body = VecDeque::from([start]);
        snake.grow = 0;
        snake.food = None;
        snake.steer(Steer::Right);
        snake.advance(&mut rng);
        assert_eq!(snake.body.front().copied(), snake.moves[start][Steer::Right as usize]);
        assert_eq!(snake.heading, Some(Steer::Right));
        // Back into the neck is ignored: the snake goes on to the right.
        let head = snake.body[0];
        snake.body.push_back(start);
        snake.steer(Steer::Left);
        snake.advance(&mut rng);
        assert_eq!(snake.body.front().copied(), snake.moves[head][Steer::Right as usize]);
        // Leftwards from the left edge of a row there is only the wall.
        let edge = (0..board.keys.len()).find(|&k| snake.moves[k][Steer::Left as usize].is_none()).unwrap();
        snake.body = VecDeque::from([edge]);
        snake.steer(Steer::Left);
        snake.advance(&mut rng);
        assert!(snake.over > 0.0, "the wall ends the game");
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

#[cfg(test)]
mod preview {
    use super::*;
    use crate::render::FRAME_LEN;

    /// Writes a contact sheet of every effect on an 80HE to the BMP file in `WOOTLIGHT_SHEET`:
    /// `cargo test contact_sheet -- --ignored`.
    #[test]
    #[ignore = "writes an image for a person to look at"]
    fn contact_sheet() {
        let path = std::env::var("WOOTLIGHT_SHEET").expect("WOOTLIGHT_SHEET");
        let layout = Layout::wooting_80he(false);
        let (u, pad) = (10usize, 6usize);
        let (kw, kh) = ((layout.width * u as f32) as usize + 2 * pad, 6 * u + 2 * pad);
        let shots = 4;
        let (w, h) = (kw * shots, kh * Effect::ALL.len());
        let mut pixels = vec![[18u8, 16, 20]; w * h];
        for (row, effect) in Effect::ALL.into_iter().enumerate() {
            let mut effects = Effects::new(&layout);
            let palette = Palette::natural(effect);
            let every = if effect == Effect::Snow { 120.0 } else { 3.0 };
            let mut frame = [0u8; FRAME_LEN];
            let mut t = 0.0;
            for shot in 0..shots {
                while t < every * (shot + 1) as f32 {
                    effects.render(effect, &palette, 1.0, 1.0 / 60.0, (21, 47, 30), &mut frame);
                    t += 1.0 / 60.0;
                }
                for key in &layout.keys {
                    let i = (key.row * SDK_COLS + key.col) * 3;
                    let led = [frame[i], frame[i + 1], frame[i + 2]];
                    let color = if led.iter().all(|v| *v == 0) { [40, 38, 44] } else { led };
                    let (x0, x1) = (pad + (key.x0 * u as f32) as usize + 1, pad + (key.x1 * u as f32) as usize - 1);
                    let (y0, y1) = (pad + key.row * u + 1, pad + (key.row + 1) * u - 1);
                    for y in y0..y1 {
                        for x in x0..x1 {
                            pixels[(row * kh + y) * w + shot * kw + x] = color;
                        }
                    }
                }
            }
        }
        let stride = (w * 3).div_ceil(4) * 4;
        let size = 54 + stride * h;
        let mut bmp = Vec::with_capacity(size);
        bmp.extend_from_slice(b"BM");
        for v in [size as u32, 0, 54, 40, w as u32, h as u32] {
            bmp.extend_from_slice(&v.to_le_bytes());
        }
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&24u16.to_le_bytes());
        for v in [0u32, (stride * h) as u32, 2835, 2835, 0, 0] {
            bmp.extend_from_slice(&v.to_le_bytes());
        }
        for y in (0..h).rev() {
            let line: Vec<u8> = pixels[y * w..(y + 1) * w].iter().flat_map(|c| [c[2], c[1], c[0]]).collect();
            bmp.extend_from_slice(&line);
            bmp.resize(bmp.len() + stride - w * 3, 0);
        }
        std::fs::write(path, bmp).unwrap();
    }
}
