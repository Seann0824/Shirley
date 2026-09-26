use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use super::app::App;

pub fn draw(frame: &mut Frame, app: &App) {
    let [messages_area, input_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).areas(frame.area());

    let mut lines: Vec<Line> = Vec::new();
    for message in app.messages() {
        let is_thinking = message.thinking;
        if is_thinking && !app.show_thinking() {
            continue;
        }

        let (label, label_style) = if is_thinking {
            (
                "🧠 夏莉（思考）：".to_owned(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )
        } else if message.role == "你" {
            ("你：".to_owned(), Style::default().fg(Color::Cyan))
        } else if message.role == "错误" {
            ("错误：".to_owned(), Style::default().fg(Color::Red))
        } else {
            ("夏莉：".to_owned(), Style::default().fg(Color::Magenta))
        };

        lines.push(Line::from(Span::styled(label, label_style)));
        for raw in message.content.lines() {
            let line = if is_thinking {
                Line::from(Span::styled(
                    raw.to_owned(),
                    Style::default().fg(Color::DarkGray),
                ))
            } else {
                Line::from(raw.to_owned())
            };
            lines.push(line);
        }
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

    let title = if app.show_thinking() {
        " 消息 · 思考已显示（Ctrl+T 隐藏） "
    } else {
        " 消息 · 思考已隐藏（Ctrl+T 显示） "
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(title))
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
            " 输入框 · Enter 发送 · Ctrl+T 切换思考 · Esc 退出 "
        })),
        input_area,
    );

    if visible_width > 0 && !app.is_waiting() {
        let cursor_x = input_area.x + 1 + visible_input.width() as u16;
        frame.set_cursor_position((cursor_x, input_area.y + 1));
    }
}
