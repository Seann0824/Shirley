//! Mouse selection: turning "left-button drag" into copyable text.
//!
//! Background: native terminal selection requires the user to press a copy key again (⌘C / Ctrl+Shift+C),
//! which does not match the desire for "hold left button to select and it just works". So the program holds the selection itself:
//! left-button press records an anchor, dragging extends, releasing writes the selected text to the system clipboard --
//! all with the mouse only, without introducing any new key or "copy mode".
//!
//! The selection is expressed in screen cell coordinates (matching ratatui `Buffer` global coordinates).

use ratatui::{
    buffer::Buffer,
    layout::Position,
    style::Modifier,
};
use unicode_width::UnicodeWidthStr;

/// A selection formed by one mouse drag (anchor -> current point, endpoints inclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// The starting point when the left button is pressed.
    pub anchor: Position,
    /// The current point dragged to (may be before the anchor).
    pub head: Position,
}

impl Selection {
    pub fn new(pos: Position) -> Self {
        Self {
            anchor: pos,
            head: pos,
        }
    }

    /// Extend the current point to `pos`.
    pub fn extend(&mut self, pos: Position) {
        self.head = pos;
    }

    /// Whether this is just a click in place (no actual drag).
    pub fn is_click(&self) -> bool {
        self.anchor == self.head
    }

    /// The normalized row range (top, bottom), endpoints inclusive.
    fn rows(&self) -> (u16, u16) {
        (
            self.anchor.y.min(self.head.y),
            self.anchor.y.max(self.head.y),
        )
    }

    /// The column range (left, right) of row `y` within the selection, clipped to the selection's "reading-order" shape:
    /// the first row from the anchor column to end of line, the last row from start of line to the current column, middle rows whole.
    /// Returns `None` when `y` is outside the selection's row range.
    fn columns(&self, y: u16) -> Option<(u16, u16)> {
        let (top, bottom) = self.rows();
        if y < top || y > bottom {
            return None;
        }
        // Column of the top endpoint and of the bottom endpoint.
        let (top_x, bottom_x) = if self.anchor.y <= self.head.y {
            (self.anchor.x, self.head.x)
        } else {
            (self.head.x, self.anchor.x)
        };
        if top == bottom {
            Some((top_x.min(bottom_x), top_x.max(bottom_x)))
        } else if y == top {
            Some((top_x, u16::MAX))
        } else if y == bottom {
            Some((0, bottom_x))
        } else {
            Some((0, u16::MAX))
        }
    }

    /// Highlight the selection on the buffer (reversed). Coordinates are clipped to the buffer area; out-of-bounds parts are ignored.
    pub fn highlight(&self, buffer: &mut Buffer) {
        let area = buffer.area;
        let (top, bottom) = self.rows();
        for y in top..=bottom {
            if y < area.top() || y >= area.bottom() {
                continue;
            }
            let Some((left, right)) = self.columns(y) else {
                continue;
            };
            let left = left.max(area.left());
            let right = right.min(area.right().saturating_sub(1));
            if left > right {
                continue;
            }
            for x in left..=right {
                if let Some(cell) = buffer.cell_mut(Position::new(x, y)) {
                    cell.modifier |= Modifier::REVERSED;
                }
            }
        }
    }

    /// Extract the selected text from the buffer.
    ///
    /// Wide characters (CJK, etc.) occupy one signed cell plus a continuation cell that is `reset`.
    /// The continuation cell's `symbol()` reads back as a space, so cell-by-cell concatenation would insert spurious spaces between CJK;
    /// therefore this advances by symbol display width: a wide symbol is taken once, skipping the continuation cell it occupies. Each row's
    /// trailing padding is trimmed, and leading/trailing blank lines are removed too.
    pub fn text(&self, buffer: &Buffer) -> String {
        let area = buffer.area;
        let (top, bottom) = self.rows();
        let mut lines: Vec<String> = Vec::new();
        for y in top..=bottom {
            if y < area.top() || y >= area.bottom() {
                continue;
            }
            let Some((left, right)) = self.columns(y) else {
                continue;
            };
            let left = left.max(area.left());
            let right = right.min(area.right().saturating_sub(1));
            if left > right {
                continue;
            }
            let mut line = String::new();
            let mut x = left;
            while x <= right {
                if let Some(cell) = buffer.cell(Position::new(x, y)) {
                    let symbol = cell.symbol();
                    line.push_str(symbol);
                    // A wide symbol occupies following continuation cells; skip them to avoid double counting (including when selected).
                    let width = UnicodeWidthStr::width(symbol).max(1) as u16;
                    x = x.saturating_add(width);
                } else {
                    x = x.saturating_add(1);
                }
            }
            lines.push(line.trim_end().to_owned());
        }
        while lines.first().is_some_and(|line| line.is_empty()) {
            lines.remove(0);
        }
        while lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    /// Build a 5-row x 10-column buffer whose content is the given text lines.
    fn buffer_with(lines: &[&str]) -> Buffer {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 5));
        for (y, text) in lines.iter().enumerate() {
            buffer.set_string(0, y as u16, text, ratatui::style::Style::default());
        }
        buffer
    }

    #[test]
    fn single_line_selection_extracts_substring() {
        let buffer = buffer_with(&["hello world"]);
        let mut sel = Selection::new(Position::new(0, 0));
        sel.extend(Position::new(4, 0));
        assert_eq!(sel.text(&buffer), "hello");
    }

    #[test]
    fn multi_line_selection_reads_in_reading_order() {
        let buffer = buffer_with(&["abcdef", "ghijkl", "mnopqr"]);
        let mut sel = Selection::new(Position::new(3, 0));
        sel.extend(Position::new(2, 2));
        // First row from column 3 to end of line, middle rows whole, last row from start of line to column 2.
        assert_eq!(sel.text(&buffer), "def\nghijkl\nmno");
    }

    #[test]
    fn reversed_drag_normalizes_to_same_text() {
        let buffer = buffer_with(&["abcdef", "ghijkl", "mnopqr"]);
        let mut forward = Selection::new(Position::new(3, 0));
        forward.extend(Position::new(2, 2));
        let mut backward = Selection::new(Position::new(2, 2));
        backward.extend(Position::new(3, 0));
        assert_eq!(forward.text(&buffer), backward.text(&buffer));
    }

    #[test]
    fn click_is_detected() {
        let sel = Selection::new(Position::new(2, 2));
        assert!(sel.is_click());
    }

    #[test]
    fn wide_chars_are_not_split_by_padding_cells() {
        let buffer = buffer_with(&["你好世界"]);
        let mut sel = Selection::new(Position::new(0, 0));
        // 4 wide characters occupy 8 columns; select the first two.
        sel.extend(Position::new(3, 0));
        assert_eq!(sel.text(&buffer), "你好");
    }

    #[test]
    fn trailing_padding_is_trimmed() {
        let buffer = buffer_with(&["hi"]);
        let mut sel = Selection::new(Position::new(0, 0));
        sel.extend(Position::new(9, 0));
        assert_eq!(sel.text(&buffer), "hi");
    }
}
