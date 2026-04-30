//! Theme palette for dialogs (Settings, About). Mirrors C# `ThemeColors`.

use serde::{Deserialize, Serialize};

use crate::color::Rgba;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeName {
    Light,
    Dark,
}

impl Default for ThemeName {
    fn default() -> Self {
        ThemeName::Light
    }
}

impl ThemeName {
    pub fn parse(s: &str) -> ThemeName {
        if s.eq_ignore_ascii_case("dark") { ThemeName::Dark } else { ThemeName::Light }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ThemeName::Light => "Light",
            ThemeName::Dark  => "Dark",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ThemeColors {
    pub background:   Rgba,
    pub surface:      Rgba,
    pub surface_alt:  Rgba,
    pub border:       Rgba,
    pub text:         Rgba,
    pub text_muted:   Rgba,
    pub accent:       Rgba,
    pub button_bg:    Rgba,
    pub button_hover: Rgba,
}

impl ThemeColors {
    pub const LIGHT: ThemeColors = ThemeColors {
        background:   Rgba::new_opaque(245, 245, 247),
        surface:      Rgba::new_opaque(255, 255, 255),
        surface_alt:  Rgba::new_opaque(235, 235, 240),
        border:       Rgba::new_opaque(210, 210, 215),
        text:         Rgba::new_opaque(30,  30,  30),
        text_muted:   Rgba::new_opaque(110, 110, 120),
        accent:       Rgba::new_opaque(88,  86,  214),
        button_bg:    Rgba::new_opaque(228, 228, 232),
        button_hover: Rgba::new_opaque(215, 215, 220),
    };

    pub const DARK: ThemeColors = ThemeColors {
        background:   Rgba::new_opaque(32,  32,  36),
        surface:      Rgba::new_opaque(45,  45,  50),
        surface_alt:  Rgba::new_opaque(60,  60,  66),
        border:       Rgba::new_opaque(70,  70,  76),
        text:         Rgba::new_opaque(235, 235, 238),
        text_muted:   Rgba::new_opaque(160, 160, 168),
        accent:       Rgba::new_opaque(120, 118, 240),
        button_bg:    Rgba::new_opaque(70,  70,  76),
        button_hover: Rgba::new_opaque(90,  90,  96),
    };

    pub const fn for_theme(name: ThemeName) -> ThemeColors {
        match name {
            ThemeName::Light => Self::LIGHT,
            ThemeName::Dark  => Self::DARK,
        }
    }
}
