//! Tools the user can pick on the overlay toolbar.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tool {
    Pen,
    Line,
    Arrow,
    Rectangle,
    Ellipse,
    Marker,
    Text,
    Step,
    Pixelate,
}

impl Default for Tool {
    fn default() -> Self {
        Tool::Pen
    }
}

impl Tool {
    /// Single-letter keyboard shortcut, matching the C# OverlayForm.OnKeyDown switch.
    pub fn shortcut(self) -> char {
        match self {
            Tool::Pen       => 'p',
            Tool::Line      => 'l',
            Tool::Arrow     => 'a',
            Tool::Rectangle => 'r',
            Tool::Ellipse   => 'e',
            Tool::Marker    => 'm',
            Tool::Text      => 't',
            Tool::Step      => 'n',
            Tool::Pixelate  => 'b',
        }
    }

    /// Tooltip + accelerator text, for UI rendering.
    pub fn label(self) -> &'static str {
        match self {
            Tool::Pen       => "Pen (P)",
            Tool::Line      => "Line (L)",
            Tool::Arrow     => "Arrow (A)",
            Tool::Rectangle => "Rectangle (R)",
            Tool::Ellipse   => "Ellipse (E)",
            Tool::Marker    => "Marker (M)",
            Tool::Text      => "Text (T)",
            Tool::Step      => "Numbered step (N)",
            Tool::Pixelate  => "Pixelate / blur (B)",
        }
    }

    /// All tools in toolbar order.
    pub const ALL: [Tool; 9] = [
        Tool::Pen,
        Tool::Line,
        Tool::Arrow,
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Marker,
        Tool::Text,
        Tool::Step,
        Tool::Pixelate,
    ];

    pub fn from_key(c: char) -> Option<Tool> {
        let c = c.to_ascii_lowercase();
        Tool::ALL.iter().copied().find(|t| t.shortcut() == c)
    }
}
