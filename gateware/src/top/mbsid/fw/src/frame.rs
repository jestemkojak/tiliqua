//! Host-pure menu frame description + diff.
//!
//! The menu renders as a list of positioned text items (a `Frame`). Instead of
//! clearing the menu box and redrawing (a ~93k-pixel per-pixel fill through the
//! pixel_plot FIFO that races scanout and shows as a black wipe), `Painter`
//! (menu.rs) diffs the new frame against the last-painted one and emits
//! blitter-only ops: Erase = re-blit the OLD text at intensity 0 (glyph-exact;
//! blit REPLACE mode has no zero-color skip, so this really writes black over
//! exactly the pixels the old draw touched), Draw = blit the new text. No
//! rectangle fills in steady state, and everything stays in the single blitter
//! command FIFO (no pixel-plot/blit cross-queue ordering hazard).

use heapless::{String, Vec};

/// Max text items on screen at once. Worst case is the PatchEdit card:
/// title + Card row + 6 param rows + 2 scroll indicators + Save row = 11.
/// (Main gained a USB Mode row in M6a: title + 6 rows + detail + status = 9,
/// still under PatchEdit's 11, so 12 still suffices.)
pub const MAX_ITEMS: usize = 12;
/// Worst-case op count: every old item erased + every new item drawn.
pub const MAX_OPS: usize = 2 * MAX_ITEMS;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Item {
    pub x: i32,
    pub y: i32,
    /// Style selector: true = focused/bright (intensity 15), false = dim (9).
    pub bright: bool,
    pub text: String<48>,
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Frame {
    pub items: Vec<Item, MAX_ITEMS>,
}

impl Frame {
    /// Append a text item. Text longer than 48 bytes or more than MAX_ITEMS
    /// items are silently dropped — build_frame never produces either (same
    /// `let _ = write!` idiom as the old draw()).
    pub fn push(&mut self, x: i32, y: i32, bright: bool, text: &str) {
        let mut s: String<48> = String::new();
        let _ = s.push_str(text);
        let _ = self.items.push(Item { x, y, bright, text: s });
    }
}

/// One paint operation, borrowing by index: `Erase(i)` names `prev.items[i]`
/// (re-blit its text at intensity 0), `Draw(i)` names `next.items[i]`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaintOp {
    Erase(u8),
    Draw(u8),
}

/// Diff two frames into an op list. All Erase ops precede all Draw ops so an
/// erase can never punch glyph-shaped holes into freshly drawn text at the
/// same position.
///
/// Rules, per item, keyed by exact (x, y):
/// - old item with no same-position+same-text successor -> Erase
/// - new item with no same-position+same-text+same-style predecessor -> Draw
///   (same text, style change only => Draw with no Erase: identical glyph
///   pixels get overwritten in the new color)
pub fn diff(prev: &Frame, next: &Frame) -> Vec<PaintOp, MAX_OPS> {
    let mut ops: Vec<PaintOp, MAX_OPS> = Vec::new();
    for (i, o) in prev.items.iter().enumerate() {
        let survives = next.items.iter()
            .any(|n| n.x == o.x && n.y == o.y && n.text == o.text);
        if !survives {
            let _ = ops.push(PaintOp::Erase(i as u8));
        }
    }
    for (i, n) in next.items.iter().enumerate() {
        let unchanged = prev.items.iter()
            .any(|o| o.x == n.x && o.y == n.y && o.text == n.text
                     && o.bright == n.bright);
        if !unchanged {
            let _ = ops.push(PaintOp::Draw(i as u8));
        }
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(items: &[(i32, i32, bool, &str)]) -> Frame {
        let mut fr = Frame::default();
        for &(x, y, b, t) in items {
            fr.push(x, y, b, t);
        }
        fr
    }

    #[test]
    fn identical_frames_produce_no_ops() {
        let a = f(&[(60, 80, true, "MBSID  Patch"), (60, 104, false, "  Card     Main")]);
        let b = a.clone();
        assert!(diff(&a, &b).is_empty());
    }

    #[test]
    fn style_only_change_is_single_draw_no_erase() {
        let a = f(&[(60, 104, false, "  Card     Main")]);
        let b = f(&[(60, 104, true, "  Card     Main")]);
        let ops = diff(&a, &b);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0], PaintOp::Draw(0));
    }

    #[test]
    fn text_change_is_erase_then_draw() {
        let a = f(&[(60, 104, true, "> Card     Main")]);
        let b = f(&[(60, 104, true, "# Card     Main")]);
        let ops = diff(&a, &b);
        assert_eq!(&ops[..], &[PaintOp::Erase(0), PaintOp::Draw(0)]);
    }

    #[test]
    fn vanished_item_is_erase_only() {
        let a = f(&[(60, 80, true, "title"), (60, 248, true, "SAVED U000")]);
        let b = f(&[(60, 80, true, "title")]);
        let ops = diff(&a, &b);
        assert_eq!(&ops[..], &[PaintOp::Erase(1)]);
    }

    #[test]
    fn appeared_item_is_draw_only() {
        let a = f(&[(60, 80, true, "title")]);
        let b = f(&[(60, 80, true, "title"), (60, 248, true, "SAVED U000")]);
        let ops = diff(&a, &b);
        assert_eq!(&ops[..], &[PaintOp::Draw(1)]);
    }

    #[test]
    fn all_erases_precede_all_draws() {
        let a = f(&[(60, 104, true, "old A"), (60, 128, false, "old B")]);
        let b = f(&[(60, 104, true, "new A"), (60, 128, false, "new B")]);
        let ops = diff(&a, &b);
        let first_draw = ops.iter().position(|o| matches!(o, PaintOp::Draw(_))).unwrap();
        assert!(ops[..first_draw].iter().all(|o| matches!(o, PaintOp::Erase(_))));
        assert!(ops[first_draw..].iter().all(|o| matches!(o, PaintOp::Draw(_))));
    }

    #[test]
    fn moved_item_erased_at_old_position_drawn_at_new() {
        // Same text at a different y (e.g. PatchEdit Save row after scroll).
        let a = f(&[(60, 224, true, "> Save     Cancel")]);
        let b = f(&[(60, 248, true, "> Save     Cancel")]);
        let ops = diff(&a, &b);
        assert_eq!(&ops[..], &[PaintOp::Erase(0), PaintOp::Draw(0)]);
    }
}
