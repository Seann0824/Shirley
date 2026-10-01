use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use super::app::{App, ChatMessage, Item, Role};
use super::markdown;

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
                Item::Message(message) => {
                    append_message(&mut lines, message, app.show_thinking(), width)
                }
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
    let [messages_area, input_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(3),
    ])
    .areas(frame.area());

    let message_width = messages_area.width.max(1) as usize;
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
    let visible_height = messages_area.height as usize;
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

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((inner_scroll, 0)),
        messages_area,
    );

    // 输入框用圆角描边 + 顶部标签/提示 + 底部右侧缓存信息，做出「卡片」的层次感。
    let prompt = "❯ ";
    let prompt_width = prompt.width() as u16;
    let visible_width = input_area.width.saturating_sub(2 + prompt_width) as usize;
    let input = app.input();
    let cursor = app.input_cursor();
    // 水平滚动窗口：从光标往回留出可见宽度（预留 1 列给光标），
    // 再向后铺满可见宽度，保证光标始终在框内可见。
    let mut start = cursor;
    let mut back_width = 0;
    for (index, ch) in input[..cursor].char_indices().rev() {
        let char_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if back_width + char_width > visible_width.saturating_sub(1) {
            break;
        }
        back_width += char_width;
        start = index;
    }
    let mut end = input.len();
    let mut forward_width = back_width;
    for (offset, ch) in input[cursor..].char_indices() {
        let char_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if forward_width + char_width > visible_width {
            end = cursor + offset;
            break;
        }
        forward_width += char_width;
    }
    let visible_input = &input[start..end];
    let cursor_offset = input[start..cursor].width() as u16;

    let border_style = if app.is_compressing() {
        Style::default().fg(Color::Yellow)
    } else if app.is_waiting() {
        Style::default().fg(Color::Magenta)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .title_top(input_label(app))
        .title_top(Line::from(input_hint(app)).right_aligned())
        .title_bottom(footer_line(app));
    let content = Line::from(vec![
        Span::styled(
            prompt,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(visible_input.to_owned()),
    ]);
    frame.render_widget(Paragraph::new(content).block(block), input_area);

    if visible_width > 0 {
        let cursor_x = input_area.x + 1 + prompt_width + cursor_offset;
        frame.set_cursor_position((cursor_x, input_area.y + 1));
    }
}

fn append_message(
    lines: &mut Vec<Line>,
    message: &ChatMessage,
    show_thinking: bool,
    width: usize,
) {
    let is_thinking = message.thinking;
    if is_thinking && !show_thinking {
        return;
    }

    let (label, label_style, base_style) = if is_thinking {
        (
            "🧠 夏莉（思考）：".to_owned(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )
    } else {
        match message.role {
            Role::User => (
                "你：".to_owned(),
                Style::default().fg(Color::Cyan),
                Style::default(),
            ),
            Role::Assistant => (
                "夏莉：".to_owned(),
                Style::default().fg(Color::Magenta),
                Style::default(),
            ),
            Role::Summary => (
                "上下文摘要：".to_owned(),
                Style::default().fg(Color::Yellow),
                Style::default(),
            ),
            Role::Error => (
                "错误：".to_owned(),
                Style::default().fg(Color::Red),
                Style::default().fg(Color::Red),
            ),
        }
    };

    lines.push(Line::from(Span::styled(label, label_style)));

    // 消息正文走 Markdown → Block → Line/Span 渲染；错误消息保持原样，避免把报错里的
    // 反引号/星号当成语法吃掉。
    if message.role == Role::Error && !is_thinking {
        for raw in message.content.lines() {
            lines.push(Line::from(Span::styled(raw.to_owned(), base_style)));
        }
    } else {
        let mut rendered = markdown::render(&message.content, base_style, width);
        // render 会在块之间补空行，这里去掉尾部空行，由下面统一收尾。
        while rendered.last().is_some_and(|line| line.spans.is_empty()) {
            rendered.pop();
        }
        lines.extend(rendered);
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

fn input_label(app: &App) -> Line<'static> {
    let (text, color) = if app.is_compressing() {
        (" 正在压缩上下文 ".to_owned(), Color::Yellow)
    } else if let Some(seconds) = app.waiting_seconds() {
        (format!(" 夏莉回复中 {seconds}s "), Color::Magenta)
    } else {
        (" 输入 ".to_owned(), Color::Cyan)
    };
    Line::from(Span::styled(
        text,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ))
}

fn input_hint(app: &App) -> String {
    if app.is_compressing() {
        " Esc 退出 ".to_owned()
    } else if app.is_waiting() {
        " 可继续输入 · Esc 退出 ".to_owned()
    } else {
        " Enter 发送 · ↑↓ 历史 · Ctrl+T 思考 · Esc 退出 ".to_owned()
    }
}

/// 底部信息统一右对齐，只保留 KV 形式的数字，够扫一眼即可。
fn footer_line(app: &App) -> Line<'static> {
    let dim = Style::default().fg(Color::DarkGray);
    let sep = || Span::styled(" · ", dim);
    let mut spans: Vec<Span> = Vec::new();

    // 思考：显 / 隐
    if !app.is_compressing() {
        spans.push(Span::styled("思考 ", dim));
        spans.push(Span::styled(
            if app.show_thinking() { "显" } else { "隐" },
            dim,
        ));
        spans.push(sep());
    }

    // 上下文占用率
    spans.push(Span::styled("上下文 ", dim));
    if app.is_compressing() {
        spans.push(Span::styled("…", Style::default().fg(Color::Yellow)));
    } else if let Some((used, limit)) = app.context_usage() {
        let percent = used as f64 / limit as f64 * 100.0;
        let color = if percent >= 80.0 {
            Color::Red
        } else if percent >= 60.0 {
            Color::Yellow
        } else {
            Color::Green
        };
        spans.push(Span::styled(
            format!("{percent:.0}%"),
            Style::default().fg(color),
        ));
    } else {
        spans.push(Span::styled("-", dim));
    }
    spans.push(sep());

    // 缓存命中率：最近 / 累计
    let hit_style = |rate: Option<f64>| match rate {
        None => dim,
        Some(rate) if rate >= 0.8 => Style::default().fg(Color::Green),
        Some(rate) if rate >= 0.5 => Style::default().fg(Color::Yellow),
        Some(_) => Style::default().fg(Color::Red),
    };
    let format_rate = |rate: Option<f64>| match rate {
        Some(rate) => format!("{:.0}%", rate * 100.0),
        None => "-".to_owned(),
    };

    spans.push(Span::styled("缓存 ", dim));
    match app.last_usage() {
        Some(last) => {
            let last_rate = last.cache_hit_rate();
            let total_rate = app.total_usage().cache_hit_rate();
            spans.push(Span::styled(format_rate(last_rate), hit_style(last_rate)));
            spans.push(Span::styled("/", dim));
            spans.push(Span::styled(format_rate(total_rate), hit_style(total_rate)));
        }
        None => spans.push(Span::styled("-", dim)),
    }

    Line::from(spans).right_aligned()
}
