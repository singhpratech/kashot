//! 32-bit ARGB color, framework-agnostic. Same memory layout as System.Drawing.Color
//! so values round-trip when sharing settings JSON with the C# version.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const RED:   Rgba = Rgba::new_opaque(220,  38,  38);
    pub const WHITE: Rgba = Rgba::new_opaque(255, 255, 255);
    pub const BLACK: Rgba = Rgba::new_opaque(0,   0,   0);

    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn new_opaque(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Pack to a Win32 / .NET-compatible ARGB i32 (0xAARRGGBB).
    pub fn to_argb(self) -> i32 {
        i32::from_be_bytes([self.a, self.r, self.g, self.b])
    }

    pub fn from_argb(v: i32) -> Self {
        let [a, r, g, b] = v.to_be_bytes();
        Self { r, g, b, a }
    }

    pub fn with_alpha(self, alpha: u8) -> Self {
        Self { a: alpha, ..self }
    }

    /// Linear-mix `self` toward `other` by `t` in `[0, 1]`.
    pub fn lerp(self, other: Rgba, t: f32) -> Rgba {
        let t = t.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| ((a as f32) * (1.0 - t) + (b as f32) * t).round() as u8;
        Rgba {
            r: mix(self.r, other.r),
            g: mix(self.g, other.g),
            b: mix(self.b, other.b),
            a: mix(self.a, other.a),
        }
    }

    pub fn to_rgba_f32(self) -> [f32; 4] {
        [
            self.r as f32 / 255.0,
            self.g as f32 / 255.0,
            self.b as f32 / 255.0,
            self.a as f32 / 255.0,
        ]
    }
}

impl Default for Rgba {
    fn default() -> Self {
        Rgba::RED
    }
}
