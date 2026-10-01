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
        // 一整轮 AI 回复只在块首出一次名字，之后才是 思考 / 工具调用 / 正文。
        // 连续的 Assistant 消息与工具调用同属一轮，遇到非 AI 条目才收束。
        let mut turn_open = false;
        for item in app.items() {
            match item {
                Item::Message(message) => {
                    if message.role == Role::Assistant {
                        if message.thinking && !app.show_thinking() {
                            continue;
                        }
                        if !turn_open {
                            lines.push(assistant_name_line());
                            turn_open = true;
                        }
                    } else {
                        turn_open = false;
                    }
                    append_message(&mut lines, message, width);
                }
                Item::Tools(group) => {
                    if !turn_open {
                        lines.push(assistant_name_line());
                        turn_open = true;
                    }
                    append_tools(&mut lines, group, app.show_tool_args(), width);
                }
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

/// 一轮 AI 回复的名字头，先于 思考 / 工具调用 / 正文 出现。
fn assistant_name_line() -> Line<'static> {
    Line::from(Span::styled(
        "夏莉：".to_owned(),
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    ))
}

fn append_message(lines: &mut Vec<Line>, message: &ChatMessage, width: usize) {
    let is_thinking = message.thinking;

    // Assistant 正文不再单独打名字（名字由块首统一给出），只保留 思考 的子标签。
    let (label, label_style, base_style): (Option<String>, Style, Style) = if is_thinking {
        (
            Some("🧠 思考".to_owned()),
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
                Some("你：".to_owned()),
                Style::default().fg(Color::Cyan),
                Style::default(),
            ),
            Role::Assistant => (None, Style::default(), Style::default()),
            Role::Summary => (
                Some("上下文摘要：".to_owned()),
                Style::default().fg(Color::Yellow),
                Style::default(),
            ),
            Role::Error => (
                Some("错误：".to_owned()),
                Style::default().fg(Color::Red),
                Style::default().fg(Color::Red),
            ),
        }
    };

    if let Some(label) = label {
        lines.push(Line::from(Span::styled(label, label_style)));
    }

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

fn append_tools(
    lines: &mut Vec<Line>,
    group: &super::app::ToolGroup,
    show_args: bool,
    width: usize,
) {
    let name_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);

    for call in &group.calls {
        // 默认一行：🔧 名称 + 关键参数摘要，超宽截断。
        let summary = summarize_arguments(&call.arguments);
        let mut line = vec![Span::styled(format!("🔧 {}", call.name), name_style)];
        if !summary.is_empty() {
            line.push(Span::styled(format!(" {summary}"), dim));
        }
        lines.push(truncate_line(Line::from(line), width));

        // 展开时，逐行给出格式化后的可读参数（不改动原始 JSON）。
        if show_args {
            for param in format_arguments(&call.arguments) {
                lines.push(param);
            }
        }
    }
    lines.push(Line::default());
}

/// 把 Line 截断到 width 显示宽度，超出部分用省略号收尾。
fn truncate_line(line: Line<'static>, width: usize) -> Line<'static> {
    if width == 0 || line.width() <= width {
        return line;
    }
    let mut spans = Vec::new();
    let mut used = 0usize;
    // 预留 1 列给省略号
    let budget = width.saturating_sub(1);
    'outer: for span in line.spans {
        let mut text = String::new();
        for ch in span.content.chars() {
            let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + w > budget {
                spans.push(Span::styled(text, span.style));
                break 'outer;
            }
            used += w;
            text.push(ch);
        }
        spans.push(Span::styled(text, span.style));
    }
    spans.push(Span::styled("…", Style::default().fg(Color::DarkGray)));
    Line::from(spans)
}

/// 把原始 JSON 参数压成一行 KV 摘要，优先露出最关键字段。
fn summarize_arguments(arguments: &str) -> String {
    let arguments = arguments.trim();
    if arguments.is_empty() {
        return String::new();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        // 模型偶尔会吐出非法 JSON，原样展示好过藏起来
        return arguments.to_owned();
    };
    let Some(map) = value.as_object() else {
        return compact_value(&value);
    };
    if map.is_empty() {
        return String::new();
    }
    map.iter()
        .map(|(key, value)| format!("{key}={}", compact_value(value)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 单个值的紧凑可读表示：字符串去引号，标量直接显示，复合类型压成 JSON。
fn compact_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => "null".to_owned(),
        other => other.to_string(),
    }
}

/// 展开时的可读参数视图：每个顶层字段一行 `key: value`，
/// 字符串去引号，嵌套结构缩进展示，不改动原始 arguments。
fn format_arguments(arguments: &str) -> Vec<Line<'static>> {
    let indent = "  ";
    let key_style = Style::default().fg(Color::Cyan);
    let value_style = Style::default().fg(Color::Gray);

    let arguments = arguments.trim();
    if arguments.is_empty() {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return vec![Line::from(Span::styled(
            format!("{indent}{arguments}"),
            value_style,
        ))];
    };

    let Some(map) = value.as_object() else {
        return vec![Line::from(Span::styled(
            format!("{indent}{}", compact_value(&value)),
            value_style,
        ))];
    };

    let mut lines = Vec::new();
    for (key, value) in map {
        match value {
            serde_json::Value::String(text) if !text.contains('\n') => {
                lines.push(Line::from(vec![
                    Span::styled(format!("{indent}{key}: "), key_style),
                    Span::styled(text.clone(), value_style),
                ]));
            }
            serde_json::Value::String(text) => {
                // 多行字符串：首行跟 key，其余行缩进对齐
                let mut parts = text.lines();
                let first = parts.next().unwrap_or("");
                lines.push(Line::from(vec![
                    Span::styled(format!("{indent}{key}: "), key_style),
                    Span::styled(first.to_owned(), value_style),
                ]));
                for rest in parts {
                    lines.push(Line::from(Span::styled(
                        format!("{indent}{indent}{rest}"),
                        value_style,
                    )));
                }
            }
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                // 复合类型：key 单独一行，下面缩进给出格式化 JSON
                lines.push(Line::from(Span::styled(format!("{indent}{key}:"), key_style)));
                let pretty = serde_json::to_string_pretty(value)
                    .unwrap_or_else(|_| value.to_string());
                for raw in pretty.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("{indent}{indent}{raw}"),
                        value_style,
                    )));
                }
            }
            scalar => {
                lines.push(Line::from(vec![
                    Span::styled(format!("{indent}{key}: "), key_style),
                    Span::styled(compact_value(scalar), value_style),
                ]));
            }
        }
    }
    lines
}

fn input_label(app: &App) -> Line<'static> {
    let (text, color) = if app.is_compressing() {
        (" 正在压缩上下文 ".to_owned(), Color::Yellow)
    } else if let Some(seconds) = app.waiting_seconds() {
        (format!(" 夏莉回复中 {seconds}s "), Color::Cyan)
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
        " Enter 发送 · ↑↓ 历史 · Ctrl+T 思考 · Ctrl+O 参数 · Esc 退出 ".to_owned()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn summarize_picks_all_kv_on_one_line() {
        let text = summarize_arguments(r#"{"path":"a/b.rs","line":12}"#);
        assert_eq!(text, "line=12 path=a/b.rs");
    }

    #[test]
    fn summarize_falls_back_to_raw_on_invalid_json() {
        assert_eq!(summarize_arguments("not json"), "not json");
        assert_eq!(summarize_arguments(""), "");
    }

    #[test]
    fn format_arguments_is_readable_and_unquoted() {
        let lines = format_arguments(r#"{"path":"a/b.rs","count":3}"#);
        let text: Vec<String> = lines.iter().map(plain).collect();
        assert_eq!(text, vec!["  count: 3", "  path: a/b.rs"]);
    }

    #[test]
    fn format_arguments_pretty_prints_nested() {
        let lines = format_arguments(r#"{"opts":{"deep":true}}"#);
        let text: Vec<String> = lines.iter().map(plain).collect();
        assert_eq!(text[0], "  opts:");
        assert!(text.iter().any(|l| l.contains("\"deep\": true")));
    }

    #[test]
    fn truncate_line_respects_width() {
        let line = Line::from("abcdef");
        let out = plain(&truncate_line(line, 4));
        assert_eq!(out, "abc…");
        assert_eq!(out.width(), 4);
    }

    #[test]
    fn truncate_line_keeps_short_line() {
        let line = Line::from("ab");
        assert_eq!(plain(&truncate_line(line, 10)), "ab");
    }
}
