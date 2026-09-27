use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use super::app::{App, ChatMessage, Item, Role};

pub(crate) struct MessageCache {
    width: usize,
    lines: Vec<Line<'static>>,
    row_offsets: Vec<usize>,
}

impl MessageCache {
    fn new(app: &App, width: usize) -> Self {
        let mut lines = Vec::new();
        for item in app.items() {
            match item {
                Item::Message(message) => append_message(&mut lines, message, app.show_thinking()),
                Item::Tools(group) => append_tools(&mut lines, group),
            }
        }
        let mut row_offsets = Vec::with_capacity(lines.len() + 1);
        row_offsets.push(0);
        for line in &lines {
            row_offsets
                .push(row_offsets.last().copied().unwrap() + line.width().max(1).div_ceil(width));
        }
        Self {
            width,
            lines,
            row_offsets,
        }
    }

    fn visible_lines(&self, scroll: usize, height: usize) -> (Vec<Line<'static>>, u16) {
        if self.lines.is_empty() || height == 0 {
            return (Vec::new(), 0);
        }
        let first = self
            .row_offsets
            .partition_point(|&offset| offset <= scroll)
            .saturating_sub(1);
        let end = self
            .row_offsets
            .partition_point(|&offset| offset < scroll.saturating_add(height))
            .min(self.lines.len());
        let inner_scroll = (scroll - self.row_offsets[first]) as u16;
        (self.lines[first..end.max(first + 1)].to_vec(), inner_scroll)
    }
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [messages_area, status_area, input_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(3),
    ])
    .areas(frame.area());

    let message_width = messages_area.width.saturating_sub(2).max(1) as usize;
    if app
        .message_cache
        .as_ref()
        .is_none_or(|cache| cache.width != message_width)
    {
        app.message_cache = Some(MessageCache::new(app, message_width));
    }
    let total_height = *app
        .message_cache
        .as_ref()
        .unwrap()
        .row_offsets
        .last()
        .unwrap();
    let visible_height = messages_area.height.saturating_sub(2) as usize;
    let max_scroll = total_height.saturating_sub(visible_height);
    app.set_max_scroll(max_scroll);
    let scroll = if app.auto_scroll() {
        max_scroll
    } else {
        app.scroll().min(max_scroll)
    };
    let (lines, inner_scroll) = app
        .message_cache
        .as_ref()
        .unwrap()
        .visible_lines(scroll, visible_height);

    let title = if app.show_thinking() {
        " 消息 · 思考已显示（Ctrl+T 隐藏） "
    } else {
        " 消息 · 思考已隐藏（Ctrl+T 显示） "
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(title))
            .wrap(Wrap { trim: false })
            .scroll((inner_scroll, 0)),
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
    let input_title = if let Some(seconds) = app.waiting_seconds() {
        format!(" AI 回复中 {seconds}s · Esc 退出 ")
    } else {
        " 输入框 · Enter 发送 · Ctrl+T 思考 · Esc 退出 ".to_owned()
    };
    frame.render_widget(
        Paragraph::new(visible_input).block(Block::bordered().title(input_title)),
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
    let name_style = Style::default().fg(Color::Yellow);
    let arg_style = Style::default().fg(Color::DarkGray);
    for call in &group.calls {
        lines.push(Line::from(vec![
            Span::styled(format!("🔧 {}", call.name), name_style),
            Span::styled(summarize_arguments(&call.arguments), arg_style),
        ]));
    }
    lines.push(Line::default());
}

// 把工具参数压成一行摘要，重点是把"读了哪个文件"这种关键信息露出来
fn summarize_arguments(arguments: &str) -> String {
    let arguments = arguments.trim();
    if arguments.is_empty() {
        return String::new();
    }

    let value: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(value) => value,
        // 模型偶尔会吐出非法 JSON，原样展示好过藏起来
        Err(_) => return format!(" {arguments}"),
    };

    // 优先挑各工具最关键的字段
    for key in ["path", "command", "pattern", "query"] {
        if let Some(text) = value.get(key).and_then(|field| field.as_str()) {
            return format!(" {text}");
        }
    }

    // 退而求其次，把参数对象压成紧凑 JSON
    match serde_json::to_string(&value) {
        Ok(text) => format!(" {text}"),
        Err(_) => String::new(),
    }
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
