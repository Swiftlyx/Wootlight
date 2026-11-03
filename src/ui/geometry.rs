//! Platform-neutral drawing types shared by the Direct2D and Core Graphics canvases.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub l: f32,
    pub t: f32,
    pub r: f32,
    pub b: f32,
}

impl Rect {
    pub const fn new(l: f32, t: f32, r: f32, b: f32) -> Self {
        Self { l, t, r, b }
    }
    pub fn xywh(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { l: x, t: y, r: x + w, b: y + h }
    }
    pub fn w(&self) -> f32 {
        self.r - self.l
    }
    pub fn h(&self) -> f32 {
        self.b - self.t
    }
    pub fn cx(&self) -> f32 {
        (self.l + self.r) / 2.0
    }
    pub fn cy(&self) -> f32 {
        (self.t + self.b) / 2.0
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }
    pub fn inset(&self, dx: f32, dy: f32) -> Self {
        Self { l: self.l + dx, t: self.t + dy, r: self.r - dx, b: self.b - dy }
    }
    pub fn offset(&self, dx: f32, dy: f32) -> Self {
        Self { l: self.l + dx, t: self.t + dy, r: self.r + dx, b: self.b + dy }
    }
    pub fn intersects(&self, other: &Rect) -> bool {
        self.l < other.r && other.l < self.r && self.t < other.b && other.t < self.b
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Corners {
    pub tl: f32,
    pub tr: f32,
    pub br: f32,
    pub bl: f32,
}

impl Corners {
    /// Outer corners `outer` on the sides that are the group's edges, `inner` elsewhere.
    pub fn tile(first: bool, last: bool, outer: f32, inner: f32) -> Self {
        let top = if first { outer } else { inner };
        let bottom = if last { outer } else { inner };
        Self { tl: top, tr: top, br: bottom, bl: bottom }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextStyle {
    TitleLarge,
    TitleSmall,
    BodyLarge,
    BodyMedium,
    BodySmall,
    LabelLarge,
    LabelMedium,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Leading,
    Center,
    Trailing,
}
