use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    text::Line,
    widgets::{Block, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use super::app::App;

pub fn draw(frame: &mut Frame, app: &App) {
    let [messages_area, input_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).areas(frame.area());

    let mut lines = Vec::new();
    for (role, content) in app.messages() {
        lines.push(Line::from(format!("{role}：")));
        lines.extend(content.lines().map(|line| Line::from(line.to_owned())));
        lines.push(Line::default());
    }
    let message_width = messages_area.width.saturating_sub(2).max(1) as usize;
    let total_height: usize = lines
        .iter()
        .map(|line| {
            let width = line.width();
            width.max(1).div_ceil(message_width)
        })
        .sum();
    let visible_height = messages_area.height.saturating_sub(2) as usize;
    let scroll = total_height
        .saturating_sub(visible_height)
        .min(u16::MAX as usize) as u16;
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(" 消息 "))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        messages_area,
    );

    let visible_width = input_area.width.saturating_sub(2) as usize;
    let input = app.input();
    let mut start = input.len();
    let mut width = 0;
    for (index, ch) in input.char_indices().rev() {
        let char_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + char_width >= visible_width {
            break;
        }
        width += char_width;
        start = index;
    }
    let visible_input = &input[start..];
    frame.render_widget(
        Paragraph::new(visible_input).block(Block::bordered().title(if app.is_waiting() {
            " AI 回复中 · Esc 退出 "
        } else {
            " 输入框 · Enter 发送 · Esc 退出 "
        })),
        input_area,
    );

    if visible_width > 0 && !app.is_waiting() {
        let cursor_x = input_area.x + 1 + visible_input.width() as u16;
        frame.set_cursor_position((cursor_x, input_area.y + 1));
    }
}
