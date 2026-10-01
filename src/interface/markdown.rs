//! Markdown → TUI 渲染层。
//!
//! 数据流：LLM raw markdown → markdown parser（pulldown-cmark）→ UI blocks（本文件的小 AST）
//! → ratatui `Text`/`Line`/`Span`。
//!
//! 分层意图：解析与排版解耦。`parse` 只负责把事件流收敛成 `Block`，`render` 只负责把
//! `Block` 变成带样式的行。这样换协议解析器（比如未来接 Responses 的 markdown 变体）或
//! 换渲染目标（比如导出成 HTML）时，只需要替换其中一层。

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// 一个带样式的文本片段，等价于 ratatui 的 `Span`，但在解析阶段不依赖渲染类型。
#[derive(Clone, Debug, PartialEq)]
pub struct StyledSpan {
    pub text: String,
    pub style: Style,
}

/// 一行内联内容，由若干 `StyledSpan` 组成。
pub type StyledLine = Vec<StyledSpan>;

/// 列表项：可能包含多段（段落、嵌套列表、代码块等），以及可选的 task 勾选状态。
#[derive(Clone, Debug, PartialEq)]
pub struct ListItem {
    pub blocks: Vec<Block>,
    pub task: Option<bool>,
}

/// TUI 块级 AST。渲染层只认这几种块，不再关心 markdown 的原始语法。
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading { level: u8, lines: Vec<StyledLine> },
    Paragraph(Vec<StyledLine>),
    Code { language: Option<String>, lines: Vec<String> },
    List { ordered: bool, start: u64, items: Vec<ListItem> },
    Quote(Vec<Block>),
    Rule,
    Table { alignments: Vec<Alignment>, rows: Vec<Vec<StyledLine>> },
    Html(String),
}

/// 解析 markdown 文本，产出块级 AST。`base` 作为所有样式的基底，方便思考块整体套暗色。
pub fn parse(content: &str, base: Style) -> Vec<Block> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(content, options);
    let mut builder = Builder {
        events: parser.peekable(),
        base,
    };
    builder.parse_blocks()
}

/// 把 markdown 文本渲染成 ratatui 的行。`width` 用于水平分割线等需要宽度感知的元素。
pub fn render(content: &str, base: Style, width: usize) -> Vec<Line<'static>> {
    let blocks = parse(content, base);
    let mut out = Vec::new();
    render_blocks(&blocks, base, 0, width, &mut out);
    out
}

struct Builder<'a, I>
where
    I: Iterator<Item = Event<'a>>,
{
    events: std::iter::Peekable<I>,
    base: Style,
}

impl<'a, I> Builder<'a, I>
where
    I: Iterator<Item = Event<'a>>,
{
    /// 消费事件直到遇到一个块级容器的结束标签（List/Item/BlockQuote/Table 等），
    /// 顶层调用则自然在事件耗尽时结束。段落、标题、代码块的结束标签在各自的子解析里被吃掉。
    fn parse_blocks(&mut self) -> Vec<Block> {
        let mut blocks = Vec::new();
        while let Some(event) = self.events.next() {
            match event {
                Event::Start(Tag::Paragraph) => {
                    blocks.push(Block::Paragraph(self.parse_inline(None)));
                }
                Event::Start(Tag::Heading { level, .. }) => {
                    let level = heading_level(level);
                    let style = heading_style(level, self.base);
                    blocks.push(Block::Heading {
                        level,
                        lines: self.parse_inline_with_base(None, style),
                    });
                }
                Event::Start(Tag::CodeBlock(kind)) => {
                    blocks.push(self.parse_code_block(kind));
                }
                Event::Start(Tag::List(start)) => {
                    blocks.push(self.parse_list(start));
                }
                Event::Start(Tag::BlockQuote(_)) => {
                    blocks.push(Block::Quote(self.parse_blocks()));
                }
                Event::Start(Tag::Table(alignments)) => {
                    blocks.push(self.parse_table(alignments));
                }
                Event::Start(Tag::HtmlBlock) => {
                    let mut html = String::new();
                    for event in self.events.by_ref() {
                        match event {
                            Event::Text(text) | Event::Html(text) => html.push_str(&text),
                            Event::End(TagEnd::HtmlBlock) => break,
                            _ => {}
                        }
                    }
                    blocks.push(Block::Html(html));
                }
                Event::Rule => blocks.push(Block::Rule),
                // 容器结束，交还给上层解析器。
                Event::End(_) => return blocks,
                _ => {}
            }
        }
        blocks
    }

    fn parse_list(&mut self, start: Option<u64>) -> Block {
        let ordered = start.is_some();
        let start = start.unwrap_or(1);
        let mut items = Vec::new();
        while let Some(event) = self.events.next() {
            match event {
                Event::Start(Tag::Item) => items.push(self.parse_item()),
                Event::End(TagEnd::List(_)) => break,
                _ => {}
            }
        }
        Block::List {
            ordered,
            start,
            items,
        }
    }

    fn parse_item(&mut self) -> ListItem {
        let mut blocks = Vec::new();
        let mut task = None;
        while let Some(event) = self.events.next() {
            match event {
                Event::TaskListMarker(checked) => task = Some(checked),
                Event::Start(Tag::Paragraph) => blocks.push(Block::Paragraph(self.parse_inline(None))),
                Event::Start(Tag::List(start)) => blocks.push(self.parse_list(start)),
                Event::Start(Tag::CodeBlock(kind)) => blocks.push(self.parse_code_block(kind)),
                Event::Start(Tag::BlockQuote(_)) => blocks.push(Block::Quote(self.parse_blocks())),
                Event::End(TagEnd::Item) => break,
                // tight list 里文本直接挂在 Item 下，没有 Paragraph 包裹。
                other if is_inline(&other) => {
                    blocks.push(Block::Paragraph(self.parse_inline(Some(other))))
                }
                _ => {}
            }
        }
        ListItem { blocks, task }
    }

    fn parse_code_block(&mut self, kind: CodeBlockKind) -> Block {
        let language = match kind {
            CodeBlockKind::Fenced(info) => {
                let info = info.trim();
                if info.is_empty() {
                    None
                } else {
                    Some(info.to_string())
                }
            }
            CodeBlockKind::Indented => None,
        };
        let mut text = String::new();
        for event in self.events.by_ref() {
            match event {
                Event::Text(chunk) => text.push_str(&chunk),
                Event::End(TagEnd::CodeBlock) => break,
                _ => {}
            }
        }
        let body = text.strip_suffix('\n').unwrap_or(&text);
        let lines = body.split('\n').map(str::to_owned).collect();
        Block::Code { language, lines }
    }

    fn parse_table(&mut self, alignments: Vec<Alignment>) -> Block {
        let mut rows = Vec::new();
        loop {
            match self.events.next() {
                Some(Event::Start(Tag::TableHead)) | Some(Event::Start(Tag::TableRow)) => {
                    let mut cells = Vec::new();
                    loop {
                        match self.events.next() {
                            Some(Event::Start(Tag::TableCell)) => {
                                // 表格单元格是单行，内联里的硬换行合并成一行。
                                cells.push(self.parse_inline(None).concat())
                            }
                            Some(Event::End(TagEnd::TableHead))
                            | Some(Event::End(TagEnd::TableRow)) => break,
                            Some(_) => {}
                            None => break,
                        }
                    }
                    rows.push(cells);
                }
                Some(Event::End(TagEnd::Table)) | None => break,
                Some(_) => {}
            }
        }
        Block::Table { alignments, rows }
    }

    /// 解析内联内容，使用解析器默认基底样式。
    fn parse_inline(&mut self, first: Option<Event<'a>>) -> Vec<StyledLine> {
        self.parse_inline_with_base(first, self.base)
    }

    /// 解析内联内容。维护一个样式栈来处理 Emphasis/Strong/Link 等嵌套，
    /// 遇到块级结束标签时返回（结束标签留给调用方消费）。
    fn parse_inline_with_base(
        &mut self,
        first: Option<Event<'a>>,
        base: Style,
    ) -> Vec<StyledLine> {
        let mut lines: Vec<StyledLine> = vec![Vec::new()];
        let mut style_stack = vec![base];
        let mut current = base;
        // (起始 span 下标, 目标 URL, 是否图片)
        let mut link: Option<(usize, String, bool)> = None;

        let mut pending = first;
        loop {
            let event = match pending.take() {
                Some(event) => event,
                None => {
                    // 段落/标题这类叶子块的结束标签归内联解析消费；
                    // Item/List/BlockQuote/Table 等容器结束标签留给调用方。
                    match self.events.peek() {
                        None => break,
                        // 容器结束标签留给上层；块级开始标签说明当前段落结束。
                        Some(Event::End(tag)) if is_container_end(*tag) => break,
                        Some(Event::Start(tag)) if is_block_start(tag) => break,
                        _ => self.events.next().unwrap(),
                    }
                }
            };
            match event {
                Event::Start(Tag::Strong) => {
                    current = current.add_modifier(Modifier::BOLD);
                    style_stack.push(current);
                }
                Event::Start(Tag::Emphasis) => {
                    current = current.add_modifier(Modifier::ITALIC);
                    style_stack.push(current);
                }
                Event::Start(Tag::Strikethrough) => {
                    current = current.add_modifier(Modifier::CROSSED_OUT);
                    style_stack.push(current);
                }
                Event::Start(Tag::Link { dest_url, .. }) => {
                    current = current.fg(Color::Blue).add_modifier(Modifier::UNDERLINED);
                    style_stack.push(current);
                    link = Some((lines.last().unwrap().len(), dest_url.to_string(), false));
                }
                Event::Start(Tag::Image { dest_url, .. }) => {
                    push_span(&mut lines, "🖼 ", current);
                    current = current.fg(Color::Blue);
                    style_stack.push(current);
                    link = Some((lines.last().unwrap().len(), dest_url.to_string(), true));
                }
                Event::End(TagEnd::Link) | Event::End(TagEnd::Image) => {
                    if let Some((start, dest, _)) = link.take() {
                        let text: String = lines
                            .last()
                            .unwrap()
                            .get(start..)
                            .unwrap_or_default()
                            .iter()
                            .map(|span| span.text.as_str())
                            .collect();
                        if !dest.is_empty() && dest != text {
                            push_span(&mut lines, &format!(" ({dest})"), dim());
                        }
                    }
                    style_stack.pop();
                    current = *style_stack.last().unwrap();
                }
                Event::Text(text) => push_span(&mut lines, &text, current),
                Event::Code(text) => {
                    push_span(&mut lines, &text, current.fg(Color::Yellow));
                }
                Event::Html(text) | Event::InlineHtml(text) => {
                    push_span(&mut lines, &text, current.fg(Color::DarkGray));
                }
                Event::SoftBreak => push_span(&mut lines, " ", current),
                Event::HardBreak => lines.push(Vec::new()),
                Event::End(TagEnd::Strong)
                | Event::End(TagEnd::Emphasis)
                | Event::End(TagEnd::Strikethrough)
                | Event::End(TagEnd::Superscript)
                | Event::End(TagEnd::Subscript) => {
                    style_stack.pop();
                    current = *style_stack.last().unwrap();
                }
                Event::End(_) => break,
                _ => {}
            }
        }
        lines
    }
}

fn push_span(lines: &mut [StyledLine], text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    lines.last_mut().unwrap().push(StyledSpan {
        text: text.to_owned(),
        style,
    });
}

/// 内联解析遇到该结束标签就收手，把它留给上层块解析器。
/// 只有容器（列表/引用/表格/定义列表）属于上层；叶子块与内联标签都在这里消费。
fn is_container_end(tag: TagEnd) -> bool {
    matches!(
        tag,
        TagEnd::Item
            | TagEnd::List(_)
            | TagEnd::BlockQuote(_)
            | TagEnd::Table
            | TagEnd::TableHead
            | TagEnd::TableRow
            | TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
    )
}

/// 块级开始标签：内联解析遇到它要收手，交回上层块解析器。
fn is_block_start(tag: &Tag) -> bool {
    matches!(
        tag,
        Tag::Paragraph
            | Tag::Heading { .. }
            | Tag::CodeBlock(_)
            | Tag::List(_)
            | Tag::Item
            | Tag::BlockQuote(_)
            | Tag::Table(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::HtmlBlock
            | Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
    )
}

/// 判断一个事件是否属于内联内容（tight list 会直接把它们挂在 Item 下）。
fn is_inline(event: &Event) -> bool {
    matches!(
        event,
        Event::Text(_)
            | Event::Code(_)
            | Event::InlineMath(_)
            | Event::Html(_)
            | Event::InlineHtml(_)
            | Event::SoftBreak
            | Event::HardBreak
            | Event::TaskListMarker(_)
            | Event::Start(Tag::Strong)
            | Event::Start(Tag::Emphasis)
            | Event::Start(Tag::Strikethrough)
            | Event::Start(Tag::Link { .. })
            | Event::Start(Tag::Image { .. })
    )
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

// ---------------------------------------------------------------------------
// 渲染：Block → ratatui Line
// ---------------------------------------------------------------------------

fn render_blocks(blocks: &[Block], base: Style, indent: usize, width: usize, out: &mut Vec<Line<'static>>) {
    for block in blocks {
        match block {
            Block::Heading { lines, .. } => {
                for line in lines {
                    out.push(prefixed(line, indent, None));
                }
                out.push(Line::default());
            }
            Block::Paragraph(lines) => {
                for line in lines {
                    out.push(prefixed(line, indent, None));
                }
                out.push(Line::default());
            }
            Block::Code { language, lines } => {
                if let Some(language) = language {
                    out.push(Line::from(vec![
                        Span::raw(" ".repeat(indent)),
                        Span::styled(format!("╭ {language}"), dim()),
                    ]));
                }
                for line in lines {
                    out.push(Line::from(vec![
                        Span::raw(" ".repeat(indent)),
                        Span::styled("│ ", dim()),
                        Span::styled(line.clone(), code_style(base)),
                    ]));
                }
                out.push(Line::default());
            }
            Block::List { ordered, start, items } => {
                for (index, item) in items.iter().enumerate() {
                    let marker = list_marker(item.task, *ordered, *start + index as u64);
                    render_list_item(item, &marker, indent, width, base, out);
                }
                out.push(Line::default());
            }
            Block::Quote(inner) => {
                let mut inner_lines = Vec::new();
                render_blocks(inner, base, 0, width, &mut inner_lines);
                for line in inner_lines {
                    let mut spans = vec![
                        Span::raw(" ".repeat(indent)),
                        Span::styled("│ ", dim()),
                    ];
                    spans.extend(line.spans.into_iter().map(|mut span| {
                        span.style = span.style.add_modifier(Modifier::ITALIC);
                        span
                    }));
                    out.push(Line::from(spans));
                }
                out.push(Line::default());
            }
            Block::Rule => {
                let rule_width = if width == 0 { 40 } else { width.min(80) };
                out.push(Line::from(Span::styled("─".repeat(rule_width), dim())));
                out.push(Line::default());
            }
            Block::Table { alignments, rows } => {
                render_table(alignments, rows, indent, out);
                out.push(Line::default());
            }
            Block::Html(html) => {
                for line in html.lines() {
                    out.push(Line::from(Span::styled(line.to_owned(), dim())));
                }
                out.push(Line::default());
            }
        }
    }
}

fn render_list_item(
    item: &ListItem,
    marker: &str,
    indent: usize,
    width: usize,
    base: Style,
    out: &mut Vec<Line<'static>>,
) {
    let marker_width = marker.width();
    let pad = " ".repeat(indent);
    // 悬挂缩进：标记占 marker_width 列，正文内容区相应收窄；
    // 换行后的续行用等宽空白对齐到正文起点，而不是回到行首。
    let content_width = width.saturating_sub(indent + marker_width);
    let mut first_line = true;
    // 逐块渲染：首块与标记同行，后续块（含嵌套列表）同样挂在悬挂缩进之下。
    for block in &item.blocks {
        let mut produced = Vec::new();
        render_blocks(std::slice::from_ref(block), base, 0, content_width, &mut produced);
        while produced.last().is_some_and(|line| line.spans.is_empty()) {
            produced.pop();
        }
        for line in produced {
            for piece in wrap_line(&line.spans, content_width) {
                let mut spans = vec![Span::raw(pad.clone())];
                if first_line {
                    spans.push(Span::styled(marker.to_owned(), marker_style(base)));
                    first_line = false;
                } else {
                    spans.push(Span::raw(" ".repeat(marker_width)));
                }
                spans.extend(piece);
                out.push(Line::from(spans));
            }
        }
    }
    // 空列表项也要有标记，避免整项消失。
    if first_line {
        out.push(Line::from(vec![
            Span::raw(pad),
            Span::styled(marker.to_owned(), marker_style(base)),
        ]));
    }
}

/// 按显示宽度把一行的 span 拆成多个换行片段。
/// 尽量在空格处断行，只有单个词本身超宽时才从中间硬断；
/// 调用方负责给每个片段补上前缀（缩进 / 列表标记），从而保证续行与首行正文对齐。
fn wrap_line(spans: &[Span<'static>], max_width: usize) -> Vec<Vec<Span<'static>>> {
    if max_width == 0 {
        return vec![spans.to_vec()];
    }
    // 展平成 (字符, 样式)，方便按显示宽度与断词点重新切行。
    let mut chars: Vec<(char, Style)> = Vec::new();
    for span in spans {
        for ch in span.content.chars() {
            chars.push((ch, span.style));
        }
    }

    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    let mut current: Vec<(char, Style)> = Vec::new();
    let mut width = 0usize;
    let mut last_space: Option<usize> = None;

    for (ch, style) in chars {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if !current.is_empty() && width + ch_width > max_width {
            // 需要换行：优先在最近的空格处断，避免把词切开。
            let (mut head, tail) = match last_space {
                Some(index) => {
                    let tail = current.split_off(index + 1);
                    (std::mem::take(&mut current), tail)
                }
                None => (std::mem::take(&mut current), Vec::new()),
            };
            while head.last().is_some_and(|(c, _)| *c == ' ') {
                head.pop();
            }
            lines.push(coalesce(head));
            width = 0;
            last_space = None;
            for (index, (c, _)) in tail.iter().enumerate() {
                if *c == ' ' {
                    last_space = Some(index);
                }
                width += UnicodeWidthChar::width(*c).unwrap_or(0);
            }
            current = tail;
        }
        if ch == ' ' {
            last_space = Some(current.len());
        }
        width += ch_width;
        current.push((ch, style));
    }
    lines.push(coalesce(current));
    lines
}

/// 把相邻同样式的字符合并回 `Span`，避免换行后碎片化。
fn coalesce(chars: Vec<(char, Style)>) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (ch, style) in chars {
        match spans.last_mut() {
            Some(span) if span.style == style => span.content.to_mut().push(ch),
            _ => spans.push(Span::styled(ch.to_string(), style)),
        }
    }
    spans
}

fn render_table(
    alignments: &[Alignment],
    rows: &[Vec<StyledLine>],
    indent: usize,
    out: &mut Vec<Line<'static>>,
) {
    if rows.is_empty() {
        return;
    }
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; columns];
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            let cell_width: usize = cell.iter().map(|span| span.text.width()).sum();
            widths[index] = widths[index].max(cell_width);
        }
    }

    for (row_index, row) in rows.iter().enumerate() {
        let mut spans = vec![Span::raw(" ".repeat(indent))];
        for (column, &column_width) in widths.iter().enumerate() {
            if column > 0 {
                spans.push(Span::styled(" │ ", dim()));
            }
            let cell = row.get(column);
            let cell_width = cell
                .map(|spans| spans.iter().map(|span| span.text.width()).sum())
                .unwrap_or(0);
            let pad = column_width.saturating_sub(cell_width);
            let align = alignments.get(column).copied().unwrap_or(Alignment::None);
            let (left_pad, right_pad) = match align {
                Alignment::Right => (pad, 0),
                Alignment::Center => (pad / 2, pad - pad / 2),
                _ => (0, pad),
            };
            if left_pad > 0 {
                spans.push(Span::raw(" ".repeat(left_pad)));
            }
            if let Some(cell) = cell {
                for span in cell {
                    let mut style = span.style;
                    if row_index == 0 {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    spans.push(Span::styled(span.text.clone(), style));
                }
            }
            if right_pad > 0 {
                spans.push(Span::raw(" ".repeat(right_pad)));
            }
        }
        out.push(Line::from(spans));

        if row_index == 0 {
            let mut separator = vec![Span::raw(" ".repeat(indent))];
            for (column, &column_width) in widths.iter().enumerate() {
                if column > 0 {
                    separator.push(Span::styled("─┼─", dim()));
                }
                separator.push(Span::styled("─".repeat(column_width), dim()));
            }
            out.push(Line::from(separator));
        }
    }
}

/// 把一行内联内容渲染成 `Line`，必要时在行首加缩进/前缀。
fn prefixed(line: &StyledLine, indent: usize, prefix: Option<Span<'static>>) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.len() + 2);
    if indent > 0 {
        spans.push(Span::raw(" ".repeat(indent)));
    }
    if let Some(prefix) = prefix {
        spans.push(prefix);
    }
    if line.is_empty() {
        // 保持空行是空的，避免触发无意义的样式 span。
        return Line::from(spans);
    }
    spans.extend(
        line.iter()
            .map(|span| Span::styled(span.text.clone(), span.style)),
    );
    Line::from(spans)
}

fn list_marker(task: Option<bool>, ordered: bool, number: u64) -> String {
    match task {
        Some(true) => "[x] ".to_owned(),
        Some(false) => "[ ] ".to_owned(),
        None if ordered => format!("{number}. "),
        None => "• ".to_owned(),
    }
}

fn heading_style(level: u8, base: Style) -> Style {
    let color = match level {
        1 => Color::Cyan,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Magenta,
        _ => Color::LightBlue,
    };
    let mut style = base.fg(color).add_modifier(Modifier::BOLD);
    if level <= 2 {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

fn code_style(base: Style) -> Style {
    base.fg(Color::LightGreen)
}

fn marker_style(base: Style) -> Style {
    base.fg(Color::Cyan)
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    fn render_text(markdown: &str) -> Vec<String> {
        plain(&render(markdown, Style::default(), 40))
    }

    #[test]
    fn heading_gets_bold_style() {
        let blocks = parse("# 标题\n", Style::default());
        match &blocks[0] {
            Block::Heading { level, lines } => {
                assert_eq!(*level, 1);
                assert!(lines[0][0].style.add_modifier.contains(Modifier::BOLD));
                assert_eq!(lines[0][0].text, "标题");
            }
            other => panic!("expected heading, got {other:?}"),
        }
    }

    #[test]
    fn bold_and_inline_code_are_styled() {
        let blocks = parse("这是 **粗体** 和 `code`\n", Style::default());
        let Block::Paragraph(lines) = &blocks[0] else {
            panic!("expected paragraph");
        };
        let bold = lines[0]
            .iter()
            .find(|span| span.text == "粗体")
            .expect("bold span");
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        let code = lines[0]
            .iter()
            .find(|span| span.text == "code")
            .expect("code span");
        assert_eq!(code.style.fg, Some(Color::Yellow));
    }

    #[test]
    fn unordered_list_uses_bullet() {
        let text = render_text("- 甲\n- 乙\n");
        assert!(text[0].starts_with("• 甲"), "{text:?}");
        assert!(text[1].starts_with("• 乙"), "{text:?}");
    }

    #[test]
    fn ordered_list_numbers_items() {
        let text = render_text("3. 丙\n4. 丁\n");
        assert!(text[0].starts_with("3. 丙"), "{text:?}");
        assert!(text[1].starts_with("4. 丁"), "{text:?}");
    }

    #[test]
    fn code_block_keeps_lines_and_language() {
        let text = render_text("```rust\nlet x = 1;\n```\n");
        assert!(text[0].contains("rust"), "{text:?}");
        assert!(text.iter().any(|line| line.contains("let x = 1;")), "{text:?}");
    }

    #[test]
    fn link_shows_destination_when_different() {
        let blocks = parse("[文档](https://example.com)\n", Style::default());
        let Block::Paragraph(lines) = &blocks[0] else {
            panic!("expected paragraph");
        };
        let joined: String = lines[0].iter().map(|span| span.text.as_str()).collect();
        assert!(joined.contains("文档"), "{joined}");
        assert!(joined.contains("https://example.com"), "{joined}");
    }

    #[test]
    fn long_list_item_wraps_with_hanging_indent() {
        // 换行后的续行必须对齐到标记之后的正文起点，而不是回到行首。
        let text = render_text(
            "- 这是一条非常非常长的列表项内容，用来测试换行之后的缩进对齐是否正常\n",
        );
        assert!(text[0].starts_with("• "), "{text:?}");
        assert!(text.len() > 2, "should wrap into multiple lines: {text:?}");
        for line in text[1..].iter().filter(|line| !line.is_empty()) {
            assert!(
                line.starts_with("  ") && !line.starts_with("• "),
                "continuation should hang-indent: {text:?}",
            );
        }
    }

    #[test]
    fn ordered_list_wrap_aligns_after_number() {
        let text = render_text("1. 有序列表的换行对齐测试，内容足够长以触发换行，续行应该对齐\n");
        assert!(text[0].starts_with("1. "), "{text:?}");
        assert!(text.len() > 2, "should wrap: {text:?}");
        for line in text[1..].iter().filter(|line| !line.is_empty()) {
            assert!(line.starts_with("   "), "continuation should align after number: {text:?}");
        }
    }

    #[test]
    fn nested_list_is_indented() {
        let text = render_text("- 甲\n  - 甲一\n");
        eprintln!("NESTED: {text:?}");
        assert!(text[0].starts_with("• 甲"), "{text:?}");
        assert!(text[1].contains("甲一"), "{text:?}");
        assert!(text[1].starts_with("  "), "{text:?}");
    }

    #[test]
    fn blockquote_has_bar() {
        let text = render_text("> 引用内容\n");
        assert!(text.iter().any(|line| line.contains("│") && line.contains("引用内容")), "{text:?}");
    }

    #[test]
    fn rule_renders_line() {
        let text = render_text("---\n");
        assert!(text.iter().any(|line| line.contains('─')), "{text:?}");
    }

    #[test]
    fn task_list_shows_checkbox() {
        let text = render_text("- [x] 完成\n- [ ] 未完成\n");
        assert!(text[0].contains("[x] 完成"), "{text:?}");
        assert!(text[1].contains("[ ] 未完成"), "{text:?}");
    }

    #[test]
    fn table_renders_header_separator() {
        let text = render_text("| A | B |\n|---|---|\n| 1 | 2 |\n");
        assert!(text.iter().any(|line| line.contains("A") && line.contains("B")));
        assert!(text.iter().any(|line| line.contains('┼')));
    }
}
