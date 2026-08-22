//! Undo/redo log for the annotation canvas.
//!
//! The editor used to keep two plain `Vec<Annotation>` stacks, which can
//! only express "an annotation was appended". Moving and deleting existing
//! annotations need an operation log, so the stacks now hold `EditOp`s.
//!
//! Contract: every op is recorded *after* the caller has already applied it
//! to the annotation vector. `undo` inverts the op, `redo` re-applies it.

use serde::{Deserialize, Serialize};

use crate::annotation::Annotation;
use crate::edit;

/// One reversible change to the annotation list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EditOp {
    /// A new annotation was inserted at `index` (always the end, today).
    Add { index: usize, annotation: Annotation },
    /// The annotation at `index` was removed.
    Delete { index: usize, annotation: Annotation },
    /// The annotation at `index` was shifted by (dx, dy).
    Move { index: usize, dx: f32, dy: f32 },
    /// The whole canvas was cleared — Esc on a live selection. Carries the
    /// annotations so one Ctrl+Z brings the entire session back.
    Clear { annotations: Vec<Annotation> },
}

/// Undo/redo stacks over an externally-owned `Vec<Annotation>`.
#[derive(Debug, Default, Clone)]
pub struct History {
    undo: Vec<EditOp>,
    redo: Vec<EditOp>,
}

impl History {
    pub fn new() -> Self { Self::default() }

    /// Log an op the caller has already applied. Clears the redo stack —
    /// same convention as every other editor on the planet.
    pub fn record(&mut self, op: EditOp) {
        self.undo.push(op);
        self.redo.clear();
    }

    pub fn can_undo(&self) -> bool { !self.undo.is_empty() }
    pub fn can_redo(&self) -> bool { !self.redo.is_empty() }

    /// True when either stack still holds annotations the user could get
    /// back. Closing the editor while this is true throws work away, so the
    /// overlay asks for confirmation first.
    pub fn holds_recoverable_work(&self) -> bool {
        self.undo.iter().chain(self.redo.iter()).any(|op| match op {
            EditOp::Add { .. } | EditOp::Delete { .. } | EditOp::Move { .. } => true,
            EditOp::Clear { annotations } => !annotations.is_empty(),
        })
    }

    /// Invert the most recent op. Returns it so the caller can react (step
    /// numbering, clearing the current selection highlight).
    pub fn undo(&mut self, annotations: &mut Vec<Annotation>) -> Option<EditOp> {
        let op = self.undo.pop()?;
        match &op {
            EditOp::Add { index, .. } => {
                if *index < annotations.len() { annotations.remove(*index); }
            }
            EditOp::Delete { index, annotation } => {
                let at = (*index).min(annotations.len());
                annotations.insert(at, annotation.clone());
            }
            EditOp::Move { index, dx, dy } => {
                if let Some(a) = annotations.get_mut(*index) { edit::translate(a, -dx, -dy); }
            }
            EditOp::Clear { annotations: saved } => {
                annotations.clear();
                annotations.extend(saved.iter().cloned());
            }
        }
        self.redo.push(op.clone());
        Some(op)
    }

    /// Re-apply the most recently undone op.
    pub fn redo(&mut self, annotations: &mut Vec<Annotation>) -> Option<EditOp> {
        let op = self.redo.pop()?;
        match &op {
            EditOp::Add { index, annotation } => {
                let at = (*index).min(annotations.len());
                annotations.insert(at, annotation.clone());
            }
            EditOp::Delete { index, .. } => {
                if *index < annotations.len() { annotations.remove(*index); }
            }
            EditOp::Move { index, dx, dy } => {
                if let Some(a) = annotations.get_mut(*index) { edit::translate(a, *dx, *dy); }
            }
            EditOp::Clear { .. } => annotations.clear(),
        }
        self.undo.push(op.clone());
        Some(op)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::{Point2, Stroke};
    use crate::color::Rgba;

    fn p(x: f32, y: f32) -> Point2 { Point2::new(x, y) }

    fn add(h: &mut History, anns: &mut Vec<Annotation>, a: Annotation) {
        anns.push(a.clone());
        h.record(EditOp::Add { index: anns.len() - 1, annotation: a });
    }

    #[test]
    fn add_undo_redo_round_trip() {
        let (mut h, mut anns) = (History::new(), Vec::new());
        add(&mut h, &mut anns, Annotation::pen(Stroke::default(), p(1.0, 1.0)));
        add(&mut h, &mut anns, Annotation::step(Rgba::RED, p(5.0, 5.0), 1));
        assert_eq!(anns.len(), 2);
        h.undo(&mut anns);
        assert_eq!(anns.len(), 1);
        h.undo(&mut anns);
        assert!(anns.is_empty());
        assert!(h.undo(&mut anns).is_none(), "undo past the bottom is a no-op");
        h.redo(&mut anns);
        h.redo(&mut anns);
        assert_eq!(anns.len(), 2);
        assert!(h.redo(&mut anns).is_none());
    }

    #[test]
    fn delete_is_undoable_at_its_original_index() {
        let (mut h, mut anns) = (History::new(), Vec::new());
        add(&mut h, &mut anns, Annotation::text(Rgba::RED, p(0.0, 0.0), "first"));
        add(&mut h, &mut anns, Annotation::text(Rgba::RED, p(0.0, 0.0), "second"));
        add(&mut h, &mut anns, Annotation::text(Rgba::RED, p(0.0, 0.0), "third"));
        let removed = anns.remove(1);
        h.record(EditOp::Delete { index: 1, annotation: removed.clone() });
        assert_eq!(anns.len(), 2);
        h.undo(&mut anns);
        assert_eq!(anns.len(), 3);
        assert_eq!(anns[1], removed, "the annotation comes back where it was");
        h.redo(&mut anns);
        assert_eq!(anns.len(), 2);
        assert_eq!(anns[1], Annotation::text(Rgba::RED, p(0.0, 0.0), "third"));
    }

    #[test]
    fn move_is_undoable_and_redoable() {
        let (mut h, mut anns) = (History::new(), Vec::new());
        add(&mut h, &mut anns, Annotation::step(Rgba::RED, p(10.0, 10.0), 1));
        let original = anns[0].clone();
        edit::translate(&mut anns[0], 30.0, -12.0);
        h.record(EditOp::Move { index: 0, dx: 30.0, dy: -12.0 });
        let moved = anns[0].clone();
        h.undo(&mut anns);
        assert_eq!(anns[0], original, "undo puts the annotation back");
        h.redo(&mut anns);
        assert_eq!(anns[0], moved, "redo moves it again");
    }

    #[test]
    fn clear_restores_the_whole_canvas_in_one_undo() {
        let (mut h, mut anns) = (History::new(), Vec::new());
        add(&mut h, &mut anns, Annotation::pen(Stroke::default(), p(1.0, 1.0)));
        add(&mut h, &mut anns, Annotation::pen(Stroke::default(), p(2.0, 2.0)));
        let saved = std::mem::take(&mut anns);
        h.record(EditOp::Clear { annotations: saved.clone() });
        assert!(anns.is_empty());
        h.undo(&mut anns);
        assert_eq!(anns, saved, "one Ctrl+Z brings back everything Esc cleared");
        h.redo(&mut anns);
        assert!(anns.is_empty());
    }

    #[test]
    fn recording_clears_the_redo_stack() {
        let (mut h, mut anns) = (History::new(), Vec::new());
        add(&mut h, &mut anns, Annotation::pen(Stroke::default(), p(1.0, 1.0)));
        h.undo(&mut anns);
        assert!(h.can_redo());
        add(&mut h, &mut anns, Annotation::pen(Stroke::default(), p(9.0, 9.0)));
        assert!(!h.can_redo(), "a new edit forks the timeline");
        assert_eq!(anns.len(), 1);
    }

    #[test]
    fn recoverable_work_tracks_both_stacks() {
        let (mut h, mut anns) = (History::new(), Vec::new());
        assert!(!h.holds_recoverable_work(), "a fresh session has nothing to lose");
        add(&mut h, &mut anns, Annotation::pen(Stroke::default(), p(1.0, 1.0)));
        assert!(h.holds_recoverable_work());
        h.undo(&mut anns);
        assert!(anns.is_empty());
        assert!(h.holds_recoverable_work(), "undone work is still recoverable with Ctrl+Y");
    }

    #[test]
    fn stale_indices_never_panic() {
        // Defensive: an op whose index no longer exists must be dropped, not
        // panic the editor mid-session.
        let mut h = History::new();
        let mut anns: Vec<Annotation> = Vec::new();
        h.record(EditOp::Add { index: 7, annotation: Annotation::pen(Stroke::default(), p(0.0, 0.0)) });
        h.undo(&mut anns);
        assert!(anns.is_empty());
        h.record(EditOp::Move { index: 42, dx: 1.0, dy: 1.0 });
        h.undo(&mut anns);
        assert!(anns.is_empty());
    }
}
