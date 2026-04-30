//! Overlay state machine — mirrors the C# `OverlayForm.State` and `Edge` enums.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Idle,
    Selecting,
    Selected,
    Drawing,
    TextInput,
    Resizing,
    Moving,
}

impl Default for State {
    fn default() -> Self {
        State::Idle
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    None,
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Edge {
    /// 8-pixel hit-test radius around the selection rectangle. Matches the C#
    /// `EdgeThreshold` constant — keep in sync if either changes.
    pub const HIT_THRESHOLD: f32 = 8.0;

    pub fn cursor_size(self) -> CursorHint {
        match self {
            Edge::Left | Edge::Right                     => CursorHint::SizeWE,
            Edge::Top | Edge::Bottom                     => CursorHint::SizeNS,
            Edge::TopLeft | Edge::BottomRight            => CursorHint::SizeNWSE,
            Edge::TopRight | Edge::BottomLeft            => CursorHint::SizeNESW,
            Edge::None                                   => CursorHint::Default,
        }
    }

    pub fn is_some(self) -> bool {
        !matches!(self, Edge::None)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorHint {
    Default,
    Crosshair,
    SizeWE,
    SizeNS,
    SizeNWSE,
    SizeNESW,
}

/// Hit-test a point against the edges/corners of `selection` using the standard threshold.
pub fn hit_test_edge(selection: (f32, f32, f32, f32), p: (f32, f32)) -> Edge {
    let (x, y, w, h) = selection;
    if w <= 0.0 || h <= 0.0 {
        return Edge::None;
    }
    let (left, right) = (x, x + w);
    let (top,  bottom) = (y, y + h);
    let t = Edge::HIT_THRESHOLD;

    let near_left   = (p.0 - left).abs()   <= t;
    let near_right  = (p.0 - right).abs()  <= t;
    let near_top    = (p.1 - top).abs()    <= t;
    let near_bottom = (p.1 - bottom).abs() <= t;
    let in_x = p.0 >= left   - t && p.0 <= right  + t;
    let in_y = p.1 >= top    - t && p.1 <= bottom + t;

    if !in_x || !in_y { return Edge::None; }

    if near_left  && near_top    { return Edge::TopLeft; }
    if near_right && near_top    { return Edge::TopRight; }
    if near_left  && near_bottom { return Edge::BottomLeft; }
    if near_right && near_bottom { return Edge::BottomRight; }
    if near_left  { return Edge::Left;  }
    if near_right { return Edge::Right; }
    if near_top   { return Edge::Top;    }
    if near_bottom { return Edge::Bottom; }
    Edge::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_hit_test_picks_corners() {
        let sel = (100.0, 100.0, 200.0, 200.0);
        assert_eq!(hit_test_edge(sel, (100.0, 100.0)), Edge::TopLeft);
        assert_eq!(hit_test_edge(sel, (300.0, 100.0)), Edge::TopRight);
        assert_eq!(hit_test_edge(sel, (100.0, 300.0)), Edge::BottomLeft);
        assert_eq!(hit_test_edge(sel, (300.0, 300.0)), Edge::BottomRight);
    }

    #[test]
    fn edge_hit_test_picks_sides() {
        let sel = (0.0, 0.0, 100.0, 100.0);
        assert_eq!(hit_test_edge(sel, (0.0, 50.0)),   Edge::Left);
        assert_eq!(hit_test_edge(sel, (100.0, 50.0)), Edge::Right);
        assert_eq!(hit_test_edge(sel, (50.0, 0.0)),   Edge::Top);
        assert_eq!(hit_test_edge(sel, (50.0, 100.0)), Edge::Bottom);
    }

    #[test]
    fn edge_hit_test_inside_returns_none() {
        let sel = (0.0, 0.0, 200.0, 200.0);
        assert_eq!(hit_test_edge(sel, (100.0, 100.0)), Edge::None);
    }
}
