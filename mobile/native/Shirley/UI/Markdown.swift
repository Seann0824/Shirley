import SwiftUI

/// Chat markdown (`components/Markdown.tsx` + the `.md` styles): GitHub-flavoured blocks --
/// paragraphs, headings, lists and task lists, quotes, fenced code, tables, rules -- with a
/// single newline as a line break, as people write in chat.
enum Markdown {
    /// Split a reply into the pieces shown as separate bubbles: paragraphs (blank-line
    /// separated), with a fenced code block kept whole.
    static func splitBlocks(_ text: String) -> [String] {
        var blocks: [String] = []
        var current: [String] = []
        var fenced = false
        func flush() {
            let block = current.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
            if !block.isEmpty { blocks.append(block) }
            current = []
        }
        for line in text.components(separatedBy: "\n") {
            if line.drop(while: { $0 == " " || $0 == "\t" }).hasPrefix("```") {
                if !fenced { flush() }
                current.append(line)
                fenced.toggle()
                if !fenced { flush() }
                continue
            }
            if !fenced && line.trimmingCharacters(in: .whitespaces).isEmpty {
                flush()
                continue
            }
            current.append(line)
        }
        flush()
        return blocks
    }

    indirect enum Block: Hashable {
        case paragraph(String)
        case heading(String)
        case code(String)
        case quote([Block])
        case list(ordered: Bool, start: Int, items: [ListItem])
        case table(header: [String], rows: [[String]])
        case rule
    }

    struct ListItem: Hashable {
        var text: String
        var depth: Int
        /// `[ ]` / `[x]` task-list marker.
        var checked: Bool?
    }

    private static let listMarker = #/^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/#
    private static let tableRule = #/^\s*\|?\s*:?-{2,}:?\s*(?:\|\s*:?-{2,}:?\s*)*\|?\s*$/#
    private static let headingMarker = #/^#{1,6}\s+(.*?)\s*#*\s*$/#
    private static let ruleMarker = #/^\s*(?:(?:-\s*){3,}|(?:\*\s*){3,}|(?:_\s*){3,})$/#

    static func parse(_ text: String) -> [Block] {
        let lines = text.components(separatedBy: "\n")
        var blocks: [Block] = []
        var index = 0
        var paragraph: [String] = []
        func flushParagraph() {
            if !paragraph.isEmpty { blocks.append(.paragraph(paragraph.joined(separator: "\n"))) }
            paragraph = []
        }
        while index < lines.count {
            let line = lines[index]
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("```") || trimmed.hasPrefix("~~~") {
                flushParagraph()
                let fence = String(trimmed.prefix(3))
                var code: [String] = []
                index += 1
                while index < lines.count, !lines[index].trimmingCharacters(in: .whitespaces).hasPrefix(fence) {
                    code.append(lines[index])
                    index += 1
                }
                blocks.append(.code(code.joined(separator: "\n")))
                index += 1
                continue
            }
            if trimmed.isEmpty {
                flushParagraph()
                index += 1
                continue
            }
            if let match = trimmed.wholeMatch(of: headingMarker) {
                flushParagraph()
                blocks.append(.heading(String(match.1)))
                index += 1
                continue
            }
            if trimmed.wholeMatch(of: ruleMarker) != nil {
                flushParagraph()
                blocks.append(.rule)
                index += 1
                continue
            }
            if trimmed.hasPrefix(">") {
                flushParagraph()
                var quoted: [String] = []
                while index < lines.count {
                    let current = lines[index].trimmingCharacters(in: .whitespaces)
                    guard current.hasPrefix(">") else { break }
                    var body = current.dropFirst()
                    if body.hasPrefix(" ") { body = body.dropFirst() }
                    quoted.append(String(body))
                    index += 1
                }
                blocks.append(.quote(parse(quoted.joined(separator: "\n"))))
                continue
            }
            if line.contains("|"), index + 1 < lines.count, lines[index + 1].wholeMatch(of: tableRule) != nil {
                flushParagraph()
                let header = cells(line)
                var rows: [[String]] = []
                index += 2
                while index < lines.count, lines[index].contains("|"), !lines[index].trimmingCharacters(in: .whitespaces).isEmpty {
                    rows.append(cells(lines[index]))
                    index += 1
                }
                blocks.append(.table(header: header, rows: rows))
                continue
            }
            if let first = line.wholeMatch(of: listMarker) {
                flushParagraph()
                let ordered = first.2.last == "." || first.2.last == ")"
                let start = ordered ? Int(first.2.dropLast()) ?? 1 : 1
                var items: [ListItem] = []
                while index < lines.count {
                    let current = lines[index]
                    if let match = current.wholeMatch(of: listMarker) {
                        var body = String(match.3)
                        var checked: Bool?
                        if body.hasPrefix("[ ] ") { checked = false; body.removeFirst(4) }
                        else if body.lowercased().hasPrefix("[x] ") { checked = true; body.removeFirst(4) }
                        items.append(ListItem(text: body, depth: match.1.count / 2, checked: checked))
                    } else if !current.trimmingCharacters(in: .whitespaces).isEmpty, !items.isEmpty,
                              current.first == " " || current.first == "\t" {
                        // An indented line continues the item above it.
                        items[items.count - 1].text += "\n" + current.trimmingCharacters(in: .whitespaces)
                    } else {
                        break
                    }
                    index += 1
                }
                blocks.append(.list(ordered: ordered, start: start, items: items))
                continue
            }
            paragraph.append(line)
            index += 1
        }
        flushParagraph()
        return blocks
    }

    private static func cells(_ line: String) -> [String] {
        var row = line.trimmingCharacters(in: .whitespaces)
        if row.hasPrefix("|") { row.removeFirst() }
        if row.hasSuffix("|") { row.removeLast() }
        return row.components(separatedBy: "|").map { $0.trimmingCharacters(in: .whitespaces) }
    }

    /// Inline markdown -- bold, italic, `code`, ~~strike~~, links -- styled as the stylesheet does.
    static func inline(_ text: String, size: CGFloat, color: Color) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        var result = (try? AttributedString(markdown: text, options: options)) ?? AttributedString(text)
        result.foregroundColor = color
        for run in result.runs {
            if let intent = run.inlinePresentationIntent, intent.contains(.code) {
                result[run.range].font = .mono(size * 0.9)
                result[run.range].backgroundColor = Color.black.opacity(0.06)
            }
            if run.link != nil {
                result[run.range].underlineStyle = Text.LineStyle.single
                result[run.range].foregroundColor = Palette.label
            }
        }
        return result
    }

    /// Extra space between lines for a CSS `line-height` (the system font's own is about 1.2).
    static func spacing(_ size: CGFloat, _ lineHeight: CGFloat) -> CGFloat {
        max(0, size * (lineHeight - 1.2))
    }
}

/// One block of chat markdown, rendered (`MarkdownBlock` in the web UI).
struct MarkdownView: View {
    let text: String
    var size: CGFloat = 14
    var lineHeight: CGFloat = 1.5
    var color: Color = Palette.label
    /// Streaming: a faint caret after the last line.
    var caret = false

    var body: some View {
        let blocks = Markdown.parse(text)
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { index, block in
                BlockView(block: block, size: size, lineHeight: lineHeight, color: color,
                          caret: caret && index == blocks.count - 1)
            }
        }
    }
}

private struct BlockView: View {
    let block: Markdown.Block
    let size: CGFloat
    let lineHeight: CGFloat
    let color: Color
    let caret: Bool

    var body: some View {
        switch block {
        case .paragraph(let text):
            paragraph(text, weight: .regular)
        case .heading(let text):
            paragraph(text, weight: .semibold).padding(.vertical, 4)
        case .code(let code):
            ScrollView(.horizontal, showsIndicators: false) {
                Text(code)
                    .font(.mono(12))
                    .lineSpacing(Markdown.spacing(12, 1.5))
                    .foregroundStyle(color)
                    .fixedSize()
                    .textSelection(.enabled)
            }
            .padding(.vertical, 8)
            .padding(.horizontal, 10)
            .background(Color.black.opacity(0.06), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        case .quote(let inner):
            HStack(spacing: 0) {
                Rectangle().fill(Color.rgba(60, 60, 67, 0.25)).frame(width: 3)
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(Array(inner.enumerated()), id: \.offset) { _, child in
                        BlockView(block: child, size: size, lineHeight: lineHeight, color: Palette.secondary, caret: false)
                    }
                }
                .padding(.leading, 10)
            }
            .fixedSize(horizontal: false, vertical: true)
        case .list(let ordered, let start, let items):
            VStack(alignment: .leading, spacing: 2) {
                ForEach(Array(items.enumerated()), id: \.offset) { index, item in
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        marker(ordered: ordered, number: start + index, item: item)
                        paragraph(item.text, weight: .regular, caret: caret && index == items.count - 1)
                    }
                    .padding(.leading, size * 0.3 + CGFloat(item.depth) * size * 1.3)
                }
            }
        case .table(let header, let rows):
            ScrollView(.horizontal, showsIndicators: false) {
                Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
                    GridRow {
                        ForEach(Array(header.enumerated()), id: \.offset) { _, cell in
                            tableCell(cell, header: true)
                        }
                    }
                    ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                        GridRow {
                            ForEach(0..<header.count, id: \.self) { column in
                                tableCell(column < row.count ? row[column] : "", header: false)
                            }
                        }
                    }
                }
            }
        case .rule:
            Rectangle().fill(Color.rgba(60, 60, 67, 0.2)).frame(height: 0.5).padding(.vertical, 8)
        }
    }

    private func paragraph(_ text: String, weight: Font.Weight, caret: Bool? = nil) -> some View {
        var content = Markdown.inline(text, size: size, color: color)
        if caret ?? self.caret {
            var mark = AttributedString("▍")
            mark.foregroundColor = color.opacity(0.5)
            content += mark
        }
        return Text(content)
            .font(.px(size, weight))
            .lineSpacing(Markdown.spacing(size, lineHeight))
            .fixedSize(horizontal: false, vertical: true)
            .textSelection(.enabled)
    }

    @ViewBuilder
    private func marker(ordered: Bool, number: Int, item: Markdown.ListItem) -> some View {
        if let checked = item.checked {
            Image(systemName: checked ? "checkmark.square" : "square").font(.px(size - 2)).foregroundStyle(color)
        } else if ordered {
            Text("\(number).").font(.px(size)).foregroundStyle(color).monospacedDigit()
        } else {
            Text(item.depth == 0 ? "•" : "◦").font(.px(size)).foregroundStyle(color)
        }
    }

    private func tableCell(_ text: String, header: Bool) -> some View {
        Text(Markdown.inline(text, size: 13, color: color))
            .font(.px(13, header ? .semibold : .regular))
            .lineLimit(1)
            .fixedSize()
            .padding(.vertical, 4)
            .padding(.horizontal, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(header ? Color.black.opacity(0.04) : .clear)
            .overlay(Rectangle().stroke(Color.rgba(60, 60, 67, 0.2), lineWidth: 0.5))
    }
}
