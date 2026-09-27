use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use super::app::{App, ChatMessage, Item, Role};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [messages_area, status_area, input_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(3),
    ])
    .areas(frame.area());

    let show_thinking = app.show_thinking();

    let mut lines: Vec<Line> = Vec::new();
    for item in app.items() {
        match item {
            Item::Message(message) => {
                append_message(&mut lines, message, show_thinking);
            }
            Item::Tools(group) => {
                append_tools(&mut lines, group);
            }
        }
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
    let max_scroll = total_height
        .saturating_sub(visible_height)
        .min(u16::MAX as usize) as u16;
    app.set_max_scroll(max_scroll);
    let scroll = if app.auto_scroll() {
        max_scroll
    } else {
        app.scroll().min(max_scroll)
    };

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

    frame.render_widget(Paragraph::new(status_line(app)), status_area);

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
            " 输入框 · Enter 发送 · Ctrl+T 思考 · Esc 退出 "
        })),
        input_area,
    );

    if visible_width > 0 && !app.is_waiting() {
        let cursor_x = input_area.x + 1 + visible_input.width() as u16;
        frame.set_cursor_position((cursor_x, input_area.y + 1));
    }
}

fn append_message(lines: &mut Vec<Line>, message: &ChatMessage, show_thinking: bool) {
    let is_thinking = message.thinking;
    if is_thinking && !show_thinking {
        return;
    }

    let (label, label_style) = if is_thinking {
        (
            "🧠 夏莉（思考）：".to_owned(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )
    } else {
        match message.role {
            Role::User => ("你：".to_owned(), Style::default().fg(Color::Cyan)),
            Role::Assistant => ("夏莉：".to_owned(), Style::default().fg(Color::Magenta)),
            Role::Error => ("错误：".to_owned(), Style::default().fg(Color::Red)),
        }
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

fn append_tools(lines: &mut Vec<Line>, group: &super::app::ToolGroup) {
    let style = Style::default().fg(Color::Yellow);
    for call in &group.calls {
        lines.push(Line::from(Span::styled(format!("🔧 {}", call.name), style)));
    }
    lines.push(Line::default());
}

fn status_line(app: &App) -> Line<'static> {
    let Some(last) = app.last_usage() else {
        return Line::from(Span::styled(
            " 缓存：等待首次模型调用…".to_owned(),
            Style::default().fg(Color::DarkGray),
        ));
    };

    let total = app.total_usage();

    let hit_style = |rate: Option<f64>| match rate {
        // 未上报，不能标红误导成失效
        None => Style::default().fg(Color::DarkGray),
        Some(rate) if rate >= 0.8 => Style::default().fg(Color::Green),
        Some(rate) if rate >= 0.5 => Style::default().fg(Color::Yellow),
        Some(_) => Style::default().fg(Color::Red),
    };

    let format_rate = |rate: Option<f64>| match rate {
        Some(rate) => format!("{:.1}%", rate * 100.0),
        None => "n/a".to_owned(),
    };

    let last_rate = last.cache_hit_rate();
    let total_rate = total.cache_hit_rate();
    let coverage = total.cache_reported_input_tokens.unwrap_or(0);
    let coverage_text = if coverage < total.input_tokens {
        format!(" (覆盖 {coverage}/{})", total.input_tokens)
    } else {
        String::new()
    };
    let last_cached = last
        .cached_input_tokens
        .map(|tokens| tokens.to_string())
        .unwrap_or_else(|| "n/a".to_owned());

    Line::from(vec![
        Span::styled(" 缓存 ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("最近调用 {} ", format_rate(last_rate)),
            hit_style(last_rate),
        ),
        Span::styled(
            format!("累计 {}{} ", format_rate(total_rate), coverage_text),
            hit_style(total_rate),
        ),
        Span::styled(
            format!(
                "| 输入 {} (命中 {}) 输出 {}",
                last.input_tokens, last_cached, last.output_tokens
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}
