//! Markdown -> TUI rendering layer.
//!
//! Data flow: LLM raw markdown -> markdown parser (pulldown-cmark) -> UI blocks (this file's small AST)
//! → ratatui `Text`/`Line`/`Span`。
//!
//! Layering intent: parsing and layout are decoupled. `parse` only collapses the event stream into `Block`s; `render` only turns
//! `Block`s into styled lines. This way, swapping the parser (e.g. a future markdown variant for Responses) or
//! swapping the render target (e.g. exporting to HTML) requires replacing only one of the layers.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// A styled text fragment, equivalent to ratatui's `Span`, but independent of render types at the parse stage.
#[derive(Clone, Debug, PartialEq)]
pub struct StyledSpan {
    pub text: String,
    pub style: Style,
}

/// One line of inline content, made up of several `StyledSpan`s.
pub type StyledLine = Vec<StyledSpan>;

/// A list item: may contain multiple blocks (paragraphs, nested lists, code blocks, etc.) and an optional task checkbox state.
#[derive(Clone, Debug, PartialEq)]
pub struct ListItem {
    pub blocks: Vec<Block>,
    pub task: Option<bool>,
}

/// TUI block-level AST. The render layer recognizes only these blocks and no longer cares about markdown's raw syntax.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        lines: Vec<StyledLine>,
    },
    Paragraph(Vec<StyledLine>),
    Code {
        language: Option<String>,
        lines: Vec<String>,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<ListItem>,
    },
    Quote(Vec<Block>),
    Rule,
    Table {
        alignments: Vec<Alignment>,
        rows: Vec<Vec<StyledLine>>,
    },
    Html(String),
}

/// Parse markdown text into a block-level AST. `base` is the base for all styles, handy for wrapping a whole thinking block in a dark tone.
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

/// Render markdown text into ratatui lines. `width` is used by width-aware elements such as horizontal rules.
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
    /// Consume events until a block-container end tag is hit (List/Item/BlockQuote/Table, etc.);
    /// a top-level call naturally ends when events are exhausted. The end tags of paragraphs, headings, and code blocks are eaten by their own sub-parsers.
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
                // Container ended, hand back to the upper parser.
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
                Event::Start(Tag::Paragraph) => {
                    blocks.push(Block::Paragraph(self.parse_inline(None)));
                }
                Event::Start(Tag::List(start)) => blocks.push(self.parse_list(start)),
                Event::Start(Tag::CodeBlock(kind)) => blocks.push(self.parse_code_block(kind)),
                Event::Start(Tag::BlockQuote(_)) => blocks.push(Block::Quote(self.parse_blocks())),
                Event::End(TagEnd::Item) => break,
                // In a tight list the text hangs directly under Item, with no Paragraph wrapper.
                other if is_inline(&other) => {
                    blocks.push(Block::Paragraph(self.parse_inline(Some(other))));
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
                Some(Event::Start(Tag::TableHead | Tag::TableRow)) => {
                    let mut cells = Vec::new();
                    loop {
                        match self.events.next() {
                            Some(Event::Start(Tag::TableCell)) => {
                                // A table cell is a single line; hard breaks in the inline content are merged into one line.
                                cells.push(self.parse_inline(None).concat());
                            }
                            Some(Event::End(TagEnd::TableHead | TagEnd::TableRow)) | None => break,
                            Some(_) => {}
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

    /// Parse inline content, using the parser's default base style.
    fn parse_inline(&mut self, first: Option<Event<'a>>) -> Vec<StyledLine> {
        self.parse_inline_with_base(first, self.base)
    }

    /// Parse inline content. Maintains a style stack to handle nesting like Emphasis/Strong/Link,
    /// returning on a block-level end tag (the end tag is left for the caller to consume).
    fn parse_inline_with_base(&mut self, first: Option<Event<'a>>, base: Style) -> Vec<StyledLine> {
        let mut lines: Vec<StyledLine> = vec![Vec::new()];
        let mut style_stack = vec![base];
        let mut current = base;
        // (start span index, target URL, is_image)
        let mut link: Option<(usize, String, bool)> = None;

        let mut pending = first;
        loop {
            let event = match pending.take() {
                Some(event) => event,
                None => {
                    // The end tags of leaf blocks like paragraphs/headings are consumed by the inline parser;
                    // container end tags like Item/List/BlockQuote/Table are left to the caller.
                    match self.events.peek() {
                        None => break,
                        // Container end tags are left to the upper layer; a block-level start tag means the current paragraph ends.
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
                Event::End(TagEnd::Link | TagEnd::Image) => {
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
                Event::End(
                    TagEnd::Strong
                    | TagEnd::Emphasis
                    | TagEnd::Strikethrough
                    | TagEnd::Superscript
                    | TagEnd::Subscript,
                ) => {
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

/// The inline parser stops as soon as it hits this end tag, leaving it for the upper block parser.
/// Only containers (lists/quotes/tables/definition lists) belong to the upper layer; leaf blocks and inline tags are consumed here.
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

/// Block-level start tag: the inline parser must stop on it and hand back to the upper block parser.
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

/// Whether an event counts as inline content (a tight list hangs them directly under Item).
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
            | Event::Start(
                Tag::Strong
                    | Tag::Emphasis
                    | Tag::Strikethrough
                    | Tag::Link { .. }
                    | Tag::Image { .. }
            )
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
// Rendering: Block -> ratatui Line
// ---------------------------------------------------------------------------

fn render_blocks(
    blocks: &[Block],
    base: Style,
    indent: usize,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    for block in blocks {
        match block {
            Block::Heading { lines, .. } | Block::Paragraph(lines) => {
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
            Block::List {
                ordered,
                start,
                items,
            } => {
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
                    let mut spans = vec![Span::raw(" ".repeat(indent)), Span::styled("│ ", dim())];
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
    // Hanging indent: the marker occupies marker_width columns, narrowing the body content area;
    // wrapped continuation lines align to the body start with equal-width blanks, instead of returning to the line start.
    let content_width = width.saturating_sub(indent + marker_width);
    let mut first_line = true;
    // Render block by block: the first block shares the marker's line, and later blocks (including nested lists) also hang under the hanging indent.
    for block in &item.blocks {
        let mut produced = Vec::new();
        render_blocks(
            std::slice::from_ref(block),
            base,
            0,
            content_width,
            &mut produced,
        );
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
    // Even an empty list item needs a marker, so the whole item does not disappear.
    if first_line {
        out.push(Line::from(vec![
            Span::raw(pad),
            Span::styled(marker.to_owned(), marker_style(base)),
        ]));
    }
}

/// Split a line's spans into wrapped pieces by display width.
/// Prefer breaking at spaces; hard-break mid-word only when a single word itself is too wide;
/// the caller is responsible for prefixing each piece (indent / list marker), keeping continuation lines aligned with the first line's body.
fn wrap_line(spans: &[Span<'static>], max_width: usize) -> Vec<Vec<Span<'static>>> {
    if max_width == 0 {
        return vec![spans.to_vec()];
    }
    // Flatten to (char, style), making it easy to re-split lines by display width and word-break points.
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
            // Need to wrap: prefer breaking at the nearest space, avoiding splitting a word.
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

/// Merge adjacent same-style characters back into `Span`s, avoiding fragmentation after wrapping.
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
            let cell_width =
                cell.map_or(0, |spans| spans.iter().map(|span| span.text.width()).sum());
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

/// Render one line of inline content into a `Line`, adding an indent/prefix at the start when needed.
fn prefixed(line: &StyledLine, indent: usize, prefix: Option<Span<'static>>) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.len() + 2);
    if indent > 0 {
        spans.push(Span::raw(" ".repeat(indent)));
    }
    if let Some(prefix) = prefix {
        spans.push(prefix);
    }
    if line.is_empty() {
        // Keep empty lines empty, avoiding meaningless style spans.
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
        let blocks = parse("# Heading\n", Style::default());
        match &blocks[0] {
            Block::Heading { level, lines } => {
                assert_eq!(*level, 1);
                assert!(lines[0][0].style.add_modifier.contains(Modifier::BOLD));
                assert_eq!(lines[0][0].text, "Heading");
            }
            other => panic!("expected heading, got {other:?}"),
        }
    }

    #[test]
    fn bold_and_inline_code_are_styled() {
        let blocks = parse("This is **bold** and `code`\n", Style::default());
        let Block::Paragraph(lines) = &blocks[0] else {
            panic!("expected paragraph");
        };
        let bold = lines[0]
            .iter()
            .find(|span| span.text == "bold")
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
        let text = render_text("- Alpha\n- Beta\n");
        assert!(text[0].starts_with("• Alpha"), "{text:?}");
        assert!(text[1].starts_with("• Beta"), "{text:?}");
    }

    #[test]
    fn ordered_list_numbers_items() {
        let text = render_text("3. Gamma\n4. Delta\n");
        assert!(text[0].starts_with("3. Gamma"), "{text:?}");
        assert!(text[1].starts_with("4. Delta"), "{text:?}");
    }

    #[test]
    fn code_block_keeps_lines_and_language() {
        let text = render_text("```rust\nlet x = 1;\n```\n");
        assert!(text[0].contains("rust"), "{text:?}");
        assert!(
            text.iter().any(|line| line.contains("let x = 1;")),
            "{text:?}"
        );
    }

    #[test]
    fn link_shows_destination_when_different() {
        let blocks = parse("[docs](https://example.com)\n", Style::default());
        let Block::Paragraph(lines) = &blocks[0] else {
            panic!("expected paragraph");
        };
        let joined: String = lines[0].iter().map(|span| span.text.as_str()).collect();
        assert!(joined.contains("docs"), "{joined}");
        assert!(joined.contains("https://example.com"), "{joined}");
    }

    #[test]
    fn long_list_item_wraps_with_hanging_indent() {
        // Wrapped continuation lines must align to the body start after the marker, not return to the line start.
        let text = render_text(
            "- This is a very very long list item body used to test whether the indent alignment after wrapping is correct\n",
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
        let text = render_text(
            "1. An ordered list wrap alignment test with content long enough to trigger wrapping so the continuation aligns\n",
        );
        assert!(text[0].starts_with("1. "), "{text:?}");
        assert!(text.len() > 2, "should wrap: {text:?}");
        for line in text[1..].iter().filter(|line| !line.is_empty()) {
            assert!(
                line.starts_with("   "),
                "continuation should align after number: {text:?}"
            );
        }
    }

    #[test]
    fn nested_list_is_indented() {
        let text = render_text("- Alpha\n  - Alpha one\n");
        eprintln!("NESTED: {text:?}");
        assert!(text[0].starts_with("• Alpha"), "{text:?}");
        assert!(text[1].contains("Alpha one"), "{text:?}");
        assert!(text[1].starts_with("  "), "{text:?}");
    }

    #[test]
    fn blockquote_has_bar() {
        let text = render_text("> quoted content\n");
        assert!(
            text.iter()
                .any(|line| line.contains("│") && line.contains("quoted content")),
            "{text:?}"
        );
    }

    #[test]
    fn rule_renders_line() {
        let text = render_text("---\n");
        assert!(text.iter().any(|line| line.contains('─')), "{text:?}");
    }

    #[test]
    fn task_list_shows_checkbox() {
        let text = render_text("- [x] done\n- [ ] not done\n");
        assert!(text[0].contains("[x] done"), "{text:?}");
        assert!(text[1].contains("[ ] not done"), "{text:?}");
    }

    #[test]
    fn table_renders_header_separator() {
        let text = render_text("| A | B |\n|---|---|\n| 1 | 2 |\n");
        assert!(
            text.iter()
                .any(|line| line.contains('A') && line.contains('B'))
        );
        assert!(text.iter().any(|line| line.contains('┼')));
    }
}
