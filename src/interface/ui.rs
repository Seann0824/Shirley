use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph, Wrap},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::app::{App, ChatMessage, Item, Role};
use super::markdown;

/// 输入框最多显示的行数，超出后内部纵向滚动，避免挤占消息区。
const INPUT_MAX_LINES: usize = 10;

pub(crate) struct MessageCache {
    width: usize,
    lines: Vec<Line<'static>>,
    row_offsets: Vec<usize>,
    /// 每个 UI 条目起始行在 `lines` 里的下标（源行号）。回溯编辑态据此定位被编辑条目。
    item_line_offsets: Vec<usize>,
    /// 视口起始屏幕行：编辑态下等于被编辑条目的屏幕行号（相当于把它当成"屏幕第一行"），
    /// 否则为 0。头部（编辑条目之前的内容）占用的屏幕行数即此值。
    start_row: usize,
}

impl MessageCache {
    fn new(app: &App, width: usize) -> Self {
        let mut lines = Vec::new();
        // 每个条目在 `lines` 里的起始行下标，稍后换算成屏幕行号。
        let mut item_line_offsets = Vec::with_capacity(app.items().len());
        // 一整轮 AI 回复只在块首出一次名字，之后才是 思考 / 工具调用 / 正文。
        // 连续的 Assistant 消息与工具调用同属一轮，遇到非 AI 条目才收束。
        let mut turn_open = false;
        for item in app.items() {
            item_line_offsets.push(lines.len());
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
        // 屏幕行数必须与 ratatui `Paragraph` 实际渲染一致：它按**词边界**换行，
        // 而非简单按字符数取整。早期用 `line.width().div_ceil(width)` 估算会少算
        // 行数（例如中英混排的长行），导致 `max_scroll` 偏小、最新消息被输入框挡住。
        // 这里直接复用 ratatui 自己的换行计数（`Paragraph::line_count`）。
        let mut row_offsets = Vec::with_capacity(lines.len() + 1);
        row_offsets.push(0);
        for line in &lines {
            let rows = Paragraph::new(line.clone())
                .wrap(Wrap { trim: false })
                .line_count(width as u16)
                .max(1);
            row_offsets.push(row_offsets.last().copied().unwrap() + rows);
        }
        // 回溯编辑态：视口从被编辑条目开始，头部行数 = 该条目的屏幕行号
        // （条目起始行 → 屏幕行号：row_offsets 按 lines 逐行累加，下标即行号）。
        let start_row = app
            .rewind_edit_item()
            .and_then(|item| item_line_offsets.get(item).copied())
            .map(|line| row_offsets[line])
            .unwrap_or(0);
        Self {
            width,
            lines,
            row_offsets,
            item_line_offsets,
            start_row,
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
    let prompt = "❯ ";
    let prompt_width = prompt.width();
    let area = frame.area();
    let inner_width = (area.width.saturating_sub(2) as usize).max(1);
    let (input_lines, cursor_line, cursor_col) =
        wrap_input(app.input(), app.input_cursor(), inner_width, prompt_width);
    let content_lines = input_lines.len().clamp(1, INPUT_MAX_LINES);
    let input_height = content_lines as u16 + 2;

    // 回溯编辑态：输入框就地替换被选中的那条消息，而不是钉在底部。
    let edit_item = app.rewind_edit_item();
    let [messages_area, input_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(if edit_item.is_some() { 0 } else { input_height }),
    ])
    .areas(area);

    let message_width = messages_area.width.max(1) as usize;
    if app
        .message_cache
        .as_ref()
        .is_none_or(|cache| cache.width != message_width)
    {
        app.message_cache = Some(MessageCache::new(app, message_width));
    }
    // 先取出缓存里的标量（`&App`），再对 `app` 做可变操作，避免借用冲突。
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

    let mut cursor: Option<(u16, u16)> = None;
    if let Some(item_index) = edit_item {
        // 编辑态：头部（该消息之前的对话）在输入框上方，尾部（该消息之后的对话）
        // 在下方；新消息出现只会把底部挤出视野，不会挤压编辑框。
        cursor = draw_rewind_edit(
            frame,
            app,
            messages_area,
            &input_lines,
            cursor_line,
            cursor_col,
            content_lines,
            prompt,
            prompt_width,
            item_index,
        );
    } else {
        // 普通态：沿用原有滚动。
        let scroll = if app.auto_scroll() {
            max_scroll
        } else {
            app.scroll().min(max_scroll)
        };
        let cache = app.message_cache.as_ref().unwrap();
        let (lines, inner_scroll) = cache.visible_lines(scroll, visible_height);
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((inner_scroll, 0)),
            messages_area,
        );

        let first = cursor_line.saturating_add(1).saturating_sub(content_lines);
        let visible = input_view_lines(&input_lines, first, content_lines, prompt);
        frame.render_widget(Paragraph::new(visible).block(input_block(app)), input_area);

        let cursor_row = cursor_line.saturating_sub(first);
        let cursor_x = input_area.x
            + 1
            + if cursor_line == 0 {
                prompt_width as u16
            } else {
                0
            }
            + cursor_col as u16;
        let cursor_y = input_area.y + 1 + cursor_row as u16;
        if cursor_y < input_area.y + input_area.height.saturating_sub(1) {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }

    if let Some((x, y)) = cursor
        && y < messages_area.y + messages_area.height
    {
        frame.set_cursor_position((x, y));
    }

    // 模糊指令候选浮层：在消息区底部贴一个列表，不挤压主布局。
    draw_command_suggestions(frame, app, messages_area);

    // 模型选择器：模态浮层，覆盖消息区中央。
    draw_model_picker(frame, app, messages_area);

    // 会话选择器：同款模态浮层。
    draw_session_picker(frame, app, messages_area);
}

/// 回溯编辑态渲染：头部 + 就地输入框 + 尾部。
///
/// 头部是该消息之前的对话（滚到末尾，正好接在输入框上方），输入框替换掉原消息，
/// 尾部是该消息之后的对话。三者按屏幕高度分配，头部优先占满剩余空间，尾部吃剩饭。
/// 返回输入框光标坐标（供 `draw` 设置）。
#[allow(clippy::too_many_arguments)]
fn draw_rewind_edit(
    frame: &mut Frame,
    app: &App,
    area: ratatui::layout::Rect,
    input_lines: &[String],
    cursor_line: usize,
    cursor_col: usize,
    content_lines: usize,
    prompt: &str,
    prompt_width: usize,
    item_index: usize,
) -> Option<(u16, u16)> {
    let cache = app.message_cache.as_ref().unwrap();
    let height = area.height as usize;
    let box_height = (content_lines + 2).min(height);
    let avail = height.saturating_sub(box_height);
    // 头部屏幕行数 = 被编辑条目的起始行号。
    let head_rows = cache.start_row;
    let head_show = head_rows.min(avail);
    let tail_budget = avail - head_show;

    let [head_area, box_area, tail_area] = Layout::vertical([
        Constraint::Length(head_show as u16),
        Constraint::Length(box_height as u16),
        Constraint::Min(0),
    ])
    .areas(area);

    // 头部：取其末尾 head_show 行（滚到紧邻输入框）。
    if head_show > 0 {
        let (lines, inner) = cache.visible_lines(head_rows - head_show, head_show);
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((inner, 0)),
            head_area,
        );
    }

    // 输入框：替换掉原消息的位置，用青色描边标出"正在编辑"。
    let first = cursor_line.saturating_add(1).saturating_sub(content_lines);
    let visible = input_view_lines(input_lines, first, content_lines, prompt);
    frame.render_widget(
        Paragraph::new(visible).block(edit_input_block(app)),
        box_area,
    );

    // 尾部：该消息之后的对话。
    if tail_budget > 0
        && let Some(&tail_line) = cache.item_line_offsets.get(item_index + 1)
    {
        let tail_row = cache.row_offsets[tail_line];
        let (lines, inner) = cache.visible_lines(tail_row, tail_budget);
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((inner, 0)),
            tail_area,
        );
    }

    // 光标：输入框内第 1 列起。
    let cursor_row = cursor_line.saturating_sub(first);
    let cursor_x = box_area.x
        + 1
        + if cursor_line == 0 {
            prompt_width as u16
        } else {
            0
        }
        + cursor_col as u16;
    let cursor_y = box_area.y + 1 + cursor_row as u16;
    Some((cursor_x, cursor_y))
}

/// 把输入软换行后的可见文本转成带提示符的 `Line`（普通态与编辑态共用）。
fn input_view_lines(
    input_lines: &[String],
    first: usize,
    count: usize,
    prompt: &str,
) -> Vec<Line<'static>> {
    input_lines
        .iter()
        .enumerate()
        .skip(first)
        .take(count)
        .map(|(index, text)| {
            if index == 0 {
                Line::from(vec![
                    Span::styled(
                        prompt.to_owned(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(text.clone()),
                ])
            } else {
                Line::from(Span::raw(text.clone()))
            }
        })
        .collect()
}

/// 编辑态的输入框样式：青色描边 + 顶部操作提示，标出"正在就地编辑"。
fn edit_input_block(app: &App) -> Block<'static> {
    let hint = app
        .command_hint()
        .unwrap_or("Enter 重新发送 · Esc 取消本次修改")
        .to_owned();
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title_top(
            Line::from(hint)
                .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                .right_aligned(),
        )
}

/// 普通态输入框的卡片样式（圆角 + 标题 + 提示 + 底部信息）。
fn input_block(app: &App) -> Block<'static> {
    let border_style = if app.is_compressing() {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .title_top(input_label(app))
        .title_top(Line::from(input_hint(app)).right_aligned())
        .title_bottom(footer_line(app))
}


/// 渲染 `/model` 的模型选择器。
///
/// 居中覆盖在消息区之上：标题标出当前模型，列表高亮当前项（`●`），
/// 底部给一行操作提示。列表来自应用层的模型目录，这里只管画。
fn draw_model_picker(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let Some(picker) = app.picker() else {
        return;
    };
    if area.height < 4 || area.width < 8 {
        return;
    }
    // 面板宽度取可用宽度的一部分，并容纳最长条目。
    let longest = picker
        .entries
        .iter()
        .map(|e| e.label.width() + e.value.width() + 6)
        .max()
        .unwrap_or(0);
    let width = (longest as u16 + 4)
        .clamp(20, area.width.saturating_sub(2).max(20))
        .min(area.width);
    let height = (picker.entries.len() as u16 + 3).min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup_area = ratatui::layout::Rect { x, y, width, height };

    // 清除底下内容，做出模态感。
    frame.render_widget(ratatui::widgets::Clear, popup_area);

    let current = picker.current.as_deref().unwrap_or("");
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title_top(
            Line::from("选择模型 (↑↓ 切换 · Enter 确认 · Esc 取消)".to_owned())
                .style(Style::default().fg(Color::Cyan)),
        );

    let lines: Vec<Line> = picker
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let highlighted = i == picker.selected;
            let is_current = entry.value == current;
            let cursor = if highlighted { "▶ " } else { "  " };
            let mark = if is_current { "● " } else { "  " };
            // 标签为主、值为辅：label 常与 value 相同，相同则只显示一次。
            let text = if entry.label == entry.value {
                entry.label.clone()
            } else {
                format!("{}  ({})", entry.label, entry.value)
            };
            let style = if highlighted {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if is_current {
                Style::default().fg(Color::Green)
            } else {
                Style::default()
            };
            Line::from(Span::styled(
                format!("{cursor}{mark}{text}  [{}]", entry.provider),
                style,
            ))
        })
        .collect();

    frame.render_widget(Paragraph::new(lines).block(block), popup_area);
}

/// 渲染 `/session` 的会话选择器（与模型选择器同款模态浮层）。
///
/// 首项是「＋ 新建会话」哨兵，其余为真实会话；每项第二行显示首条用户消息预览，
/// 让用户认得出会话（只显示时间戳名字无法分辨）。当前会话打绿标。
fn draw_session_picker(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let Some(picker) = app.session_picker() else {
        return;
    };
    if area.height < 4 || area.width < 12 {
        return;
    }
    // 每项占两行（名字 + 预览），哨兵项只占一行。
    let content_rows: u16 = picker
        .entries
        .iter()
        .map(|e| if e.is_empty() { 1 } else { 2 })
        .sum();
    let height = (content_rows + 3).min(area.height);
    let width = (area.width.saturating_sub(2)).clamp(24, 72).min(area.width);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup_area = ratatui::layout::Rect { x, y, width, height };

    frame.render_widget(ratatui::widgets::Clear, popup_area);

    let current = picker.current.as_deref();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title_top(
            Line::from("选择会话 (↑↓ 切换 · Enter 确认 · Esc 取消)".to_owned())
                .style(Style::default().fg(Color::Cyan)),
        );

    let mut lines: Vec<Line> = Vec::new();
    for (i, entry) in picker.entries.iter().enumerate() {
        let highlighted = i == picker.selected;
        let is_current = current == Some(entry.name.as_str());
        let cursor = if highlighted { "▶ " } else { "  " };
        let mark = if is_current { "● " } else { "  " };
        let style = if highlighted {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if is_current {
            Style::default().fg(Color::Green)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format!("{cursor}{mark}{}", entry.label),
            style,
        )));
        // 预览行：缩进对齐，弱化样式（跟随高亮底色，保证可读）。
        if !entry.is_empty() {
            let preview_style = if highlighted {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            lines.push(Line::from(Span::styled(
                format!("      {}", entry.preview),
                preview_style,
            )));
        }
    }

    frame.render_widget(Paragraph::new(lines).block(block), popup_area);
}

/// 渲染 `/` 触发的模糊指令候选浮层。
///
/// 直接覆盖在消息区底部若干行之上，做成一个带描边的小面板。
/// 最多展示 5 个候选，超出截断（当前指令极少，足够）。
fn draw_command_suggestions(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let suggestions = app.suggestions();
    if suggestions.is_empty() {
        return;
    }
    let shown = suggestions.iter().take(5);
    let count = shown.clone().count();
    let width = area.width.clamp(1, 40);
    let height = (count as u16) + 2; // 上下描边各 1 行
    if height > area.height {
        return;
    }
    // 贴底：从消息区底部向上对齐。
    let y = area.y + area.height.saturating_sub(height);
    let popup_area = ratatui::layout::Rect {
        x: area.x,
        y,
        width,
        height,
    };
    let title_style = Style::default().fg(Color::Cyan);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title_top(Line::from("指令建议 (↑↓←→ 选择 · Tab/Enter 补全)".to_owned()).style(title_style));
    let selected = app.suggestion_index();
    let lines: Vec<Line> = suggestions
        .iter()
        .take(5)
        .enumerate()
        .map(|(i, name)| {
            let is_selected = i == selected;
            let tag = if is_selected { "▶ " } else { "  " };
            let style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Green)
            };
            Line::from(Span::styled(format!("{tag}/{name}"), style))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(block),
        popup_area,
    );
}


/// 把输入按显示宽度软换行，并保留硬换行（粘贴进来的 `\n`）。
/// 返回每行的可见文本、光标所在行号与光标在该行内的显示列。
/// 首行因为前面有提示符，可用宽度比后续行少 `prompt_width`。
fn wrap_input(
    input: &str,
    cursor: usize,
    inner_width: usize,
    prompt_width: usize,
) -> (Vec<String>, usize, usize) {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    let mut cursor_line = 0usize;
    let mut cursor_col = 0usize;
    let mut cursor_set = false;
    let capacity = |line_index: usize| {
        if line_index == 0 {
            inner_width.saturating_sub(prompt_width).max(1)
        } else {
            inner_width.max(1)
        }
    };
    for (offset, ch) in input.char_indices() {
        if ch == '\n' {
            // 光标位于换行符之前时，停在当前行末尾。
            if !cursor_set && offset == cursor {
                cursor_line = lines.len();
                cursor_col = current_width;
                cursor_set = true;
            }
            lines.push(std::mem::take(&mut current));
            current_width = 0;
            continue;
        }
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width > 0 && current_width + width > capacity(lines.len()) {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        // 光标在该字符之前：若字符被软换行挤到下一行，光标也随之落到新行行首。
        if !cursor_set && offset == cursor {
            cursor_line = lines.len();
            cursor_col = current_width;
            cursor_set = true;
        }
        current.push(ch);
        current_width += width;
    }
    if !cursor_set {
        cursor_line = lines.len();
        cursor_col = current_width;
    }
    lines.push(current);
    (lines, cursor_line, cursor_col)
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
            Role::System => (
                Some("指令：".to_owned()),
                Style::default().fg(Color::Green),
                Style::default().fg(Color::Green),
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
                lines.push(Line::from(Span::styled(
                    format!("{indent}{key}:"),
                    key_style,
                )));
                let pretty =
                    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
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
    // 指令打开的编辑态（模型选择 / 回溯编辑）优先展示其专属提示。
    if let Some(hint) = app.command_hint() {
        return hint.to_owned();
    }
    if app.is_compressing() {
        " Esc 打断 ".to_owned()
    } else if app.is_waiting() {
        " 可继续输入 · Esc 打断 ".to_owned()
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

    // 当前会话名（多会话切换后让用户知道自己在哪份会话里）。
    if let Some(name) = app.current_session() {
        spans.push(Span::styled("会话 ", dim));
        spans.push(Span::styled(name.to_owned(), dim));
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

    #[test]
    fn edit_mode_start_row_matches_item_position() {
        use shirley_agent_sdk::{Agent, ModelConfig, ModelProtocol};
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::ChatCompletions)
            .base_url("http://localhost")
            .model("test")
            .build();
        let mut app = App::new(Agent::builder().model_config(config).build().unwrap());
        app.add_message(shirley_agent_sdk::Message::User { content: "hi".into() });
        app.add_message(shirley_agent_sdk::Message::User { content: "bye".into() });

        // 非编辑态：视口从第 0 行开始。
        assert_eq!(MessageCache::new(&app, 40).start_row, 0);

        // 编辑态：视口顶到被编辑条目，头部行数即该条目的屏幕行号。
        app.add_message(shirley_agent_sdk::Message::User { content: "edit me".into() });
        app.set_rewind_edit_for_test(2);
        let cache = MessageCache::new(&app, 40);
        assert_eq!(cache.item_line_offsets.len(), 3);
        assert_eq!(cache.start_row, cache.row_offsets[cache.item_line_offsets[2]]);
        assert!(cache.start_row > 0, "第三个条目不从第 0 行开始");
    }

    #[test]
    fn cache_row_count_matches_paragraph_word_wrap() {
        // 回归：`row_offsets` 必须与 ratatui `Paragraph` 的换行一致（按词边界，
        // 而非按字符数取整），否则 `max_scroll` 偏小，最新消息会被输入框遮挡。
        use shirley_agent_sdk::{Agent, Message, ModelConfig, ModelProtocol};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::widgets::{Paragraph, Wrap};
        let config = ModelConfig::builder()
            .protocol(ModelProtocol::ChatCompletions)
            .base_url("http://localhost")
            .model("test")
            .build();
        let mut app = App::new(Agent::builder().model_config(config).build().unwrap());
        for i in 0..4 {
            app.add_message(Message::User {
                content: format!("user turn {i}"),
            });
            app.add_message(Message::Assistant {
                content: Some(format!(
                    "这是一个很长的回复第 {i} 条，用来测试换行与遮挡问题，abcdefghijklmnopqrstuvwxyz0123456789"
                )),
                reasoning_content: None,
                thinking_signature: None,
                tool_calls: vec![],
            });
        }
        let width = 60u16;
        let cache = MessageCache::new(&app, width as usize);
        let computed = *cache.row_offsets.last().unwrap();

        // 把同一批 lines 交给 Paragraph 真正渲染，量出实际占用的行数。
        let backend = TestBackend::new(width, 400);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                frame.render_widget(
                    Paragraph::new(cache.lines.clone()).wrap(Wrap { trim: false }),
                    area,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut actual = 0;
        for y in 0..400u16 {
            let occupied = (0..width).any(|x| buffer[(x, y)].symbol() != " ");
            if occupied {
                actual = y as usize + 1;
            }
        }
        assert!(
            computed >= actual,
            "row_offsets({computed}) 少于实际渲染行数({actual})，会导致最新消息被输入框遮挡"
        );
    }

    #[test]
    fn wrap_input_keeps_hard_newlines() {
        let (lines, cursor_line, cursor_col) = wrap_input("ab\ncd", 2, 20, 2);
        assert_eq!(lines, vec!["ab", "cd"]);
        assert_eq!((cursor_line, cursor_col), (0, 2));
    }

    #[test]
    fn wrap_input_wraps_long_line() {
        // 首行可用宽度 = 10 - 2(提示符) = 8
        let (lines, cursor_line, cursor_col) = wrap_input("abcdefghij", 10, 10, 2);
        assert_eq!(lines, vec!["abcdefgh", "ij"]);
        assert_eq!((cursor_line, cursor_col), (1, 2));
    }

    #[test]
    fn wrap_input_cursor_at_line_boundary() {
        let (_, cursor_line, cursor_col) = wrap_input("ab\n", 3, 20, 2);
        assert_eq!((cursor_line, cursor_col), (1, 0));
    }

    #[test]
    fn wrap_input_cursor_before_soft_wrapped_char() {
        // 首行容量 8：第 9 个字符会换到第二行，光标在第 9 个字符之前应落在第二行行首。
        let (lines, cursor_line, cursor_col) = wrap_input("abcdefghi", 8, 10, 2);
        assert_eq!(lines, vec!["abcdefgh", "i"]);
        assert_eq!((cursor_line, cursor_col), (1, 0));
    }
}
