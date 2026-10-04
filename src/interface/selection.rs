//! 鼠标选区：把"左键拖动"变成可复制的文本。
//!
//! 背景：终端原生选择需要用户再按一次复制键（⌘C / Ctrl+Shift+C），
//! 与"左键拖住选中即可"的诉求不符。因此这里由程序自己持有选区：
//! 左键按下记锚点、拖动延伸、松开即把选中文本写入系统剪贴板——
//! 全程只用鼠标，不引入任何新按键或"复制模式"。
//!
//! 选区用屏幕单元格坐标表示（与 ratatui `Buffer` 的全局坐标一致）。

use ratatui::{
    buffer::Buffer,
    layout::Position,
    style::Modifier,
};
use unicode_width::UnicodeWidthStr;

/// 一次鼠标拖动形成的选区（锚点 → 当前点，含端点）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// 按下左键时的起点。
    pub anchor: Position,
    /// 拖动到的当前点（可能位于锚点之前）。
    pub head: Position,
}

impl Selection {
    pub fn new(pos: Position) -> Self {
        Self {
            anchor: pos,
            head: pos,
        }
    }

    /// 把当前点延伸到 `pos`。
    pub fn extend(&mut self, pos: Position) {
        self.head = pos;
    }

    /// 是否只是原地单击（没有实际拖动）。
    pub fn is_click(&self) -> bool {
        self.anchor == self.head
    }

    /// 归一化后的行范围（上、下），含端点。
    fn rows(&self) -> (u16, u16) {
        (
            self.anchor.y.min(self.head.y),
            self.anchor.y.max(self.head.y),
        )
    }

    /// 第 `y` 行在选区内的列范围（左、右），按选区的"读序"形状裁剪：
    /// 首行从锚点列到行尾，末行从行首到当前列，中间整行。
    /// `y` 不在选区行范围内时返回 `None`。
    fn columns(&self, y: u16) -> Option<(u16, u16)> {
        let (top, bottom) = self.rows();
        if y < top || y > bottom {
            return None;
        }
        // 顶部端点的列、底部端点的列。
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

    /// 在缓冲区上高亮选区（反色）。坐标已裁剪到缓冲区范围内，越界部分忽略。
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

    /// 从缓冲区提取选中的文本。
    ///
    /// 宽字符（CJK 等）在缓冲区里占一个带符号的单元 + 一个被 `reset` 的续格。
    /// 续格的 `symbol()` 读出来是空格，逐格拼接会在 CJK 之间插入假空格，因此这里
    /// 按符号显示宽度前进：宽符号本身取一次，并跳过它占用的续格。每行末尾的补白
    /// 被裁掉，首尾的空行也一并去掉。
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
                    // 宽符号会占用后续续格，跳过它们避免重复计入（含被选中的情形）。
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

    /// 构造一个 5 行 x 10 列、内容为给定文本行的缓冲区。
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
        // 首行从第 3 列到行尾，中间整行，末行从行首到第 2 列。
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
        // 4 个宽字符占 8 列，选前两个。
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
