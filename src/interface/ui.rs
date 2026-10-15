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

/// Maximum number of lines shown for the input box; beyond that it scrolls vertically inside, to avoid squeezing the message area.
const INPUT_MAX_LINES: usize = 10;

pub(crate) struct MessageCache {
    width: usize,
    lines: Vec<Line<'static>>,
    row_offsets: Vec<usize>,
    /// Index into `lines` where each UI item starts (source line number). Rewind-edit mode uses this to locate the edited item.
    item_line_offsets: Vec<usize>,
    /// Viewport start screen row: in edit mode equals the screen row of the edited item (treating it as "screen line 0"),
    /// otherwise 0. The screen rows taken by the header (content before the edited item) equal this value.
    start_row: usize,
}

impl MessageCache {
    fn new(app: &App, width: usize) -> Self {
        let mut lines = Vec::new();
        // Start line index of each item in `lines`, later converted into screen rows.
        let mut item_line_offsets = Vec::with_capacity(app.items().len());
        // A whole AI reply prints its name once at the block start, and only then tool calls / body text.
        // Consecutive Assistant messages and tool calls belong to the same turn; the turn closes when a non-AI item appears.
        let mut turn_open = false;
        for item in app.items() {
            item_line_offsets.push(lines.len());
            match item {
                Item::Compact { content, expanded } => {
                    turn_open = false;
                    let arrow = if *expanded { "▾" } else { "▸" };
                    lines.push(Line::from(Span::styled(
                        format!("{arrow} context compacted"),
                        Style::default().fg(Color::Yellow),
                    )));
                    if *expanded {
                        lines.extend(markdown::render(content, Style::default(), width));
                    }
                    lines.push(Line::from(""));
                }
                Item::Message(message) => {
                    if message.role == Role::Assistant {
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
        // Screen row count must match how ratatui `Paragraph` actually renders: it wraps at **word boundaries**,
        // not by simply rounding character counts. An earlier estimate with `line.width().div_ceil(width)` undercounted
        // rows (e.g. long mixed CJK/English lines), making `max_scroll` too small and the newest message hidden behind the input box.
        // Here we reuse ratatui's own wrap counting (`Paragraph::line_count`) directly.
        let mut row_offsets = Vec::with_capacity(lines.len() + 1);
        row_offsets.push(0);
        for line in &lines {
            let rows = Paragraph::new(line.clone())
                .wrap(Wrap { trim: false })
                .line_count(width as u16)
                .max(1);
            row_offsets.push(row_offsets.last().copied().unwrap() + rows);
        }
        // Rewind-edit mode: the viewport starts at the edited item, so header rows = that item's screen row
        // (item start line -> screen row: row_offsets accumulates per line, and the index is the row number).
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
    app.compact_hit_regions.clear();
    let prompt = "❯ ";
    let prompt_width = prompt.width();
    let area = frame.area();
    let inner_width = (area.width.saturating_sub(2) as usize).max(1);
    let (input_lines, cursor_line, cursor_col) =
        wrap_input(app.input(), app.input_cursor(), inner_width, prompt_width);
    let content_lines = input_lines.len().clamp(1, INPUT_MAX_LINES);
    let input_height = content_lines as u16 + 2;

    // Rewind-edit mode: the input box replaces the selected message in place instead of being pinned to the bottom.
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
    // Read the scalars out of the cache first (`&App`), then mutate `app`, to avoid a borrow conflict.
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
        // Edit mode: the header (conversation before that message) sits above the input box, and the tail (conversation after it)
        // sits below; new messages only push the bottom out of view and never squeeze the edit box.
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
        // Normal mode: reuse the existing scrolling.
        let scroll = if app.auto_scroll() {
            max_scroll
        } else {
            app.scroll().min(max_scroll)
        };
        let cache = app.message_cache.as_ref().unwrap();
        let hit_regions = app
            .items()
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                if !matches!(item, Item::Compact { .. })
                    || app.is_picker_open()
                    || app.is_session_picker_open()
                    || !app.suggestions().is_empty()
                {
                    return None;
                }
                let row = cache.row_offsets[cache.item_line_offsets[index]];
                if row < scroll || row >= scroll + visible_height {
                    return None;
                }
                Some((
                    ratatui::layout::Rect::new(
                        messages_area.x,
                        messages_area.y + (row - scroll) as u16,
                        messages_area.width.min("▸ context compacted".width() as u16),
                        1,
                    ),
                    index,
                ))
            })
            .collect();
        let (lines, inner_scroll) = cache.visible_lines(scroll, visible_height);
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((inner_scroll, 0)),
            messages_area,
        );
        app.compact_hit_regions = hit_regions;

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

    // Fuzzy command-suggestion popup: stick a list at the bottom of the message area without squeezing the main layout.
    draw_command_suggestions(frame, app, messages_area);

    // Model picker: a modal overlay covering the center of the message area.
    draw_model_picker(frame, app, messages_area);

    // Session picker: same modal overlay.
    draw_session_picker(frame, app, messages_area);
}

/// Rewind-edit mode rendering: header + in-place input box + tail.
///
/// The header is the conversation before that message (scrolled to the end, so it sits right above the input box); the input box replaces the original message,
/// and the tail is the conversation after it. The three split the screen height; the header fills the remaining space first and the tail takes the leftovers.
/// Returns the input-box cursor position (for `draw` to set).
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
    // Header screen rows = the start line number of the edited item.
    let head_rows = cache.start_row;
    let head_show = head_rows.min(avail);
    let tail_budget = avail - head_show;

    let [head_area, box_area, tail_area] = Layout::vertical([
        Constraint::Length(head_show as u16),
        Constraint::Length(box_height as u16),
        Constraint::Min(0),
    ])
    .areas(area);

    // Header: take its last head_show lines (scrolled right up against the input box).
    if head_show > 0 {
        let (lines, inner) = cache.visible_lines(head_rows - head_show, head_show);
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((inner, 0)),
            head_area,
        );
    }

    // Input box: replaces the original message's position, outlined in cyan to mark "being edited".
    let first = cursor_line.saturating_add(1).saturating_sub(content_lines);
    let visible = input_view_lines(input_lines, first, content_lines, prompt);
    frame.render_widget(
        Paragraph::new(visible).block(edit_input_block(app)),
        box_area,
    );

    // Tail: the conversation after that message.
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

    // Cursor: starts at column 1 inside the input box.
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

/// Turn the soft-wrapped visible input text into `Line`s with the prompt prefix (shared by normal and edit modes).
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

/// Edit-mode input box style: cyan outline + top action hint, marking "editing in place".
fn edit_input_block(app: &App) -> Block<'static> {
    let hint = app
        .command_hint()
        .unwrap_or("Enter resend · Esc cancel this edit")
        .to_owned();
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title_top(
            Line::from(hint)
                .style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .right_aligned(),
        )
}

/// Normal-mode input box card style (rounded corners + title + hint + bottom info).
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

/// Render the `/model` model picker.
///
/// Centered over the message area: the title shows the current model, the list highlights the current entry (`●`),
/// and the bottom gives a line of action hints. The list comes from the application-layer model catalog; here we only draw it.
fn draw_model_picker(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let Some(picker) = app.picker() else {
        return;
    };
    if area.height < 4 || area.width < 8 {
        return;
    }
    // Panel width is a fraction of the available width, and must fit the longest entry.
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
    let popup_area = ratatui::layout::Rect {
        x,
        y,
        width,
        height,
    };

    // Clear the content underneath for a modal feel.
    frame.render_widget(ratatui::widgets::Clear, popup_area);

    let current = picker.current.as_deref().unwrap_or("");
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title_top(
            Line::from("Select model (↑↓ switch · Enter confirm · Esc cancel)".to_owned())
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
            // Label primary, value secondary: label often equals value, so show it once when they match.
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

/// Render the `/session` session picker (same modal overlay as the model picker).
///
/// The first item is the "+ New session" sentinel, the rest are real sessions; each item's second line shows the first user-message preview,
/// so the user can recognize the session (a timestamp-only name is indistinguishable). The current session is marked green.
fn draw_session_picker(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let Some(picker) = app.session_picker() else {
        return;
    };
    if area.height < 4 || area.width < 12 {
        return;
    }
    // Each item takes two lines (name + preview); the sentinel item takes only one.
    let content_rows: u16 = picker
        .entries
        .iter()
        .map(|e| if e.is_empty() { 1 } else { 2 })
        .sum();
    let height = (content_rows + 3).min(area.height);
    let width = (area.width.saturating_sub(2)).clamp(24, 72).min(area.width);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup_area = ratatui::layout::Rect {
        x,
        y,
        width,
        height,
    };

    frame.render_widget(ratatui::widgets::Clear, popup_area);

    let current = picker.current.as_deref();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title_top(
            Line::from("Select session (↑↓ switch · Enter confirm · Esc cancel)".to_owned())
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
        // Preview line: indented to align, dimmed style (follows the highlight background so it stays readable).
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

/// Render the fuzzy command-suggestion popup triggered by `/`.
///
/// Overlays the bottom few lines of the message area as a small outlined panel.
/// Shows at most 5 suggestions, truncating beyond that (there are very few commands, so this is plenty).
fn draw_command_suggestions(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let suggestions = app.suggestions();
    if suggestions.is_empty() {
        return;
    }
    let shown = suggestions.iter().take(5);
    let count = shown.clone().count();
    let width = area.width.clamp(1, 40);
    let height = (count as u16) + 2; // one line of border top and bottom each
    if height > area.height {
        return;
    }
    // Bottom-anchored: align upward from the bottom of the message area.
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
        .title_top(
            Line::from("Command suggestions (↑↓←→ select · Tab/Enter complete)".to_owned()).style(title_style),
        );
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
    frame.render_widget(Paragraph::new(lines).block(block), popup_area);
}

/// Soft-wrap the input by display width while preserving hard newlines (pasted `\n`).
/// Returns each line's visible text, the cursor's line index, and the cursor's display column within that line.
/// The first line has less usable width than subsequent lines by `prompt_width`, because of the prompt prefix.
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
            // When the cursor sits before a newline, it stays at the end of the current line.
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
        // Cursor before this char: if the char was pushed to the next line by soft wrap, the cursor lands at the start of the new line too.
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

/// Name header for one AI reply, printed before tool calls / body text.
fn assistant_name_line() -> Line<'static> {
    Line::from(Span::styled(
        "Shirley: ".to_owned(),
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    ))
}

fn append_message(lines: &mut Vec<Line>, message: &ChatMessage, width: usize) {
    // The Assistant body no longer prints its own name (the name is given once at the block start).
    let (label, label_style, base_style): (Option<String>, Style, Style) = match message.role {
        Role::User => (
            Some("You: ".to_owned()),
            Style::default().fg(Color::Cyan),
            Style::default(),
        ),
        Role::Assistant => (None, Style::default(), Style::default()),
        Role::System => (
            Some("Command: ".to_owned()),
            Style::default().fg(Color::Green),
            Style::default().fg(Color::Green),
        ),
        Role::Error => (
            Some("Error: ".to_owned()),
            Style::default().fg(Color::Red),
            Style::default().fg(Color::Red),
        ),
    };

    if let Some(label) = label {
        lines.push(Line::from(Span::styled(label, label_style)));
    }

    // Message body goes through Markdown -> Block -> Line/Span rendering; error messages stay as-is, so backticks/stars
    // in an error are not swallowed as syntax.
    if message.role == Role::Error {
        for raw in message.content.lines() {
            lines.push(Line::from(Span::styled(raw.to_owned(), base_style)));
        }
    } else {
        let mut rendered = markdown::render(&message.content, base_style, width);
        // render inserts blank lines between blocks; drop the trailing blank lines here and let the code below close uniformly.
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
        // Default one line: 🔧 name + key-argument summary, truncated if too wide.
        let summary = summarize_arguments(&call.arguments);
        let mut line = vec![Span::styled(format!("🔧 {}", call.name), name_style)];
        if !summary.is_empty() {
            line.push(Span::styled(format!(" {summary}"), dim));
        }
        lines.push(truncate_line(Line::from(line), width));

        // When expanded, show formatted readable arguments line by line (without touching the raw JSON).
        if show_args {
            for param in format_arguments(&call.arguments) {
                lines.push(param);
            }
        }
    }
    lines.push(Line::default());
}

/// Truncate a Line to the given display width, ending with an ellipsis if it overflows.
fn truncate_line(line: Line<'static>, width: usize) -> Line<'static> {
    if width == 0 || line.width() <= width {
        return line;
    }
    let mut spans = Vec::new();
    let mut used = 0usize;
    // Reserve 1 column for the ellipsis
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

/// Compress the raw JSON arguments into a one-line KV summary, surfacing the most important fields first.
fn summarize_arguments(arguments: &str) -> String {
    let arguments = arguments.trim();
    if arguments.is_empty() {
        return String::new();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        // The model occasionally emits invalid JSON; showing it as-is beats hiding it
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

/// Compact readable representation of a single value: strings unquoted, scalars shown directly, compound types compressed to JSON.
fn compact_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => "null".to_owned(),
        other => other.to_string(),
    }
}

/// Readable argument view when expanded: one line `key: value` per top-level field,
/// strings unquoted, nested structures shown indented, without touching the raw arguments.
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
                // Multi-line string: first line follows the key, the rest are indented to align
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
                // Compound type: key on its own line, with formatted JSON indented below
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
        (" compacting context ".to_owned(), Color::Yellow)
    } else if let Some(seconds) = app.waiting_seconds() {
        (format!(" Shirley replying {seconds}s "), Color::Cyan)
    } else {
        (" input ".to_owned(), Color::Cyan)
    };
    Line::from(Span::styled(
        text,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ))
}

fn input_hint(app: &App) -> String {
    // A command-opened edit mode (model picker / rewind edit) shows its dedicated hint first.
    if let Some(hint) = app.command_hint() {
        return hint.to_owned();
    }
    if app.is_compressing() {
        " Esc interrupt ".to_owned()
    } else if app.is_waiting() {
        " Keep typing · Esc interrupt ".to_owned()
    } else {
        " Enter send · ↑↓ history · Ctrl+O args · Ctrl+S compact · Esc quit ".to_owned()
    }
}

/// Truncate by display width (CJK counts as 2 columns), adding an ellipsis if over -- the status line is tight, and a long goal must not blow it up.
fn truncate(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_owned();
    }
    let budget = max_width.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Bottom info is right-aligned throughout, keeping only KV-form numbers, enough for a glance.
fn footer_line(app: &App) -> Line<'static> {
    let dim = Style::default().fg(Color::DarkGray);
    let sep = || Span::styled(" · ", dim);
    let mut spans: Vec<Span> = Vec::new();

    // Current session name (so the user knows which session they are in after multi-session switching).
    if let Some(name) = app.current_session() {
        spans.push(Span::styled("session ", dim));
        spans.push(Span::styled(name.to_owned(), dim));
        spans.push(sep());
    }

    // Task progress (todo ledger): make the most direct "the task is advancing" signal visible to the user.
    // Shows "goal · 3/7", with the in-progress step anchored in cyan. An empty ledger takes no space.
    if let Some(task) = app.task_view() {
        if let Some(goal) = task.goal.as_deref() {
            spans.push(Span::styled("task ", dim));
            spans.push(Span::styled(truncate(goal, 24), dim));
            spans.push(sep());
        }
        if task.total > 0 {
            let progress = format!("{}/{}", task.completed, task.total);
            let color = if task.completed == task.total {
                Color::Green
            } else {
                Color::Cyan
            };
            spans.push(Span::styled("progress ", dim));
            spans.push(Span::styled(progress, Style::default().fg(color)));
            spans.push(sep());
        }
    }

    // Context usage
    spans.push(Span::styled("context ", dim));
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

    // Cache hit rate: recent / cumulative
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

    spans.push(Span::styled("cache ", dim));
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
        app.add_message(shirley_agent_sdk::Message::User {
            content: "hi".into(),
        });
        app.add_message(shirley_agent_sdk::Message::User {
            content: "bye".into(),
        });

        // Non-edit mode: the viewport starts at row 0.
        assert_eq!(MessageCache::new(&app, 40).start_row, 0);

        // Edit mode: the viewport is pinned to the edited item; header rows equal that item's screen row.
        app.add_message(shirley_agent_sdk::Message::User {
            content: "edit me".into(),
        });
        app.set_rewind_edit_for_test(2);
        let cache = MessageCache::new(&app, 40);
        assert_eq!(cache.item_line_offsets.len(), 3);
        assert_eq!(
            cache.start_row,
            cache.row_offsets[cache.item_line_offsets[2]]
        );
        assert!(cache.start_row > 0, "the third item does not start at row 0");
    }

    #[test]
    fn cache_row_count_matches_paragraph_word_wrap() {
        // Regression: `row_offsets` must match ratatui `Paragraph`'s wrapping (by word boundary,
        // not by rounding character count), otherwise `max_scroll` is too small and the newest message is hidden behind the input box.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::widgets::{Paragraph, Wrap};
        use shirley_agent_sdk::{Agent, Message, ModelConfig, ModelProtocol};
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
                    "This is a very long reply number {i}, used to test wrapping and occlusion, abcdefghijklmnopqrstuvwxyz0123456789"
                )),
                reasoning_content: None,
                tool_calls: vec![],
            });
        }
        let width = 60u16;
        let cache = MessageCache::new(&app, width as usize);
        let computed = *cache.row_offsets.last().unwrap();

        // Hand the same batch of lines to Paragraph to actually render, and measure the real rows occupied.
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
            "row_offsets({computed}) is fewer than the actually rendered rows ({actual}), which would hide the newest message behind the input box"
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
        // First line usable width = 10 - 2 (prompt) = 8
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
        // First line capacity 8: the 9th character wraps to the second line, and a cursor before the 9th char should land at the start of the second line.
        let (lines, cursor_line, cursor_col) = wrap_input("abcdefghi", 8, 10, 2);
        assert_eq!(lines, vec!["abcdefgh", "i"]);
        assert_eq!((cursor_line, cursor_col), (1, 0));
    }
}
