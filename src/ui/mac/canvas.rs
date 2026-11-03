//! STUB (UI shell agent): Core Graphics and Core Text drawing in points, with the same drawing
//! API as the Direct2D canvas in src/ui/canvas.rs so window and icon code can be shared.

pub use super::geometry::{Align, Corners, Rect, TextStyle};
use super::palette::Color;

pub struct Canvas;

impl Canvas {
    pub fn new() -> Result<Self, String> {
        Ok(Self)
    }
    pub fn set_offset(&self, _dx: f32, _dy: f32) {}
    pub fn begin(&self) {}
    pub fn end(&self) -> bool {
        true
    }
    pub fn clear(&self, _color: Color) {}
    pub fn fill_rect(&self, _rect: Rect, _color: Color) {}
    pub fn fill_round(&self, _rect: Rect, _radius: f32, _color: Color) {}
    pub fn stroke_round(&self, _rect: Rect, _radius: f32, _color: Color, _width: f32) {}
    pub fn fill_shape(&self, _rect: Rect, _corners: Corners, _color: Color) {}
    pub fn fill_circle(&self, _cx: f32, _cy: f32, _radius: f32, _color: Color) {}
    pub fn fill_round_gradient(&self, _rect: Rect, _radius: f32, _colors: &[Color], _vertical: bool) {}
    pub fn fill_rounds_vertical(&self, _rects: &[Rect], _radius: f32, _top: f32, _bottom: f32, _colors: &[Color]) {}
    pub fn text(&self, _text: &str, _rect: Rect, _style: TextStyle, _color: Color, _align: Align) {}
    pub fn text_wrapped(&self, _text: &str, _rect: Rect, _style: TextStyle, _color: Color) {}
    pub fn measure(&self, _text: &str, _style: TextStyle) -> f32 {
        0.0
    }
    pub fn measure_height(&self, _text: &str, _style: TextStyle, _width: f32) -> f32 {
        0.0
    }
    pub fn glyph(&self, _glyph: char, _cx: f32, _cy: f32, _size: f32, _color: Color) {}
    pub fn push_clip(&self, _rect: Rect) {}
    pub fn pop_clip(&self) {}
}
