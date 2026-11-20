import SwiftUI

func relativeTime(_ ms: Double?) -> String {
    guard let ms, ms > 0 else { return "" }
    let minutes = Int(((Date().timeIntervalSince1970 * 1000 - ms) / 60_000).rounded())
    if minutes < 1 { return "刚刚" }
    if minutes < 60 { return "\(minutes) 分钟前" }
    let hours = Int((Double(minutes) / 60).rounded())
    if hours < 24 { return "\(hours) 小时前" }
    return "\(Int((Double(hours) / 24).rounded())) 天前"
}

private func fileIcon(_ mime: String) -> IconName {
    if mime.hasPrefix("image/") { return .image }
    if mime.contains("html") || mime.contains("javascript") || mime.contains("json") { return .code }
    return .file
}

/// One piece of work a bot took on (`components/TaskCard.tsx`): where it stands, what came back,
/// and the user's verdict.
struct TaskCard: View {
    let task: PoolTask
    @Environment(AppModel.self) private var model
    @State private var noting = false
    @State private var note = ""
    @FocusState private var noteFocused: Bool

    private var tasks: TasksStore { model.tasks }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 5) {
                    if task.status.working {
                        Spinner(size: 10)
                    } else {
                        Icon(task.status == .blocked ? .stop : .check, size: 13)
                    }
                    Text(stateLine)
                }
                .font(.px(11))
                .foregroundStyle(stateColor)
                Text(task.title)
                    .font(.px(15, .semibold))
                    .lineSpacing(Markdown.spacing(15, 1.35))
                    .foregroundStyle(Palette.label)
                    .fixedSize(horizontal: false, vertical: true)
            }

            if let error = task.delivery?.error {
                Text(error).font(.px(13)).foregroundStyle(Palette.red).padding(.top, 8)
            } else if let summary = task.delivery?.summary, !summary.isEmpty {
                TaskSummary(text: summary).padding(.top, 8)
            }
            if task.status.working, let redo = task.note, !redo.isEmpty {
                Text("按你的补充重做：\(redo)").font(.px(13)).foregroundStyle(Palette.secondary).padding(.top, 8)
            }
            if !task.pendingNotes.isEmpty {
                Text((task.status == .blocked ? "重试时会加上：" : "做完这一版后会加上：") + task.pendingNotes.joined(separator: "；"))
                    .font(.px(13)).foregroundStyle(Palette.secondary).padding(.top, 8)
            }

            if let files = task.delivery?.files, !files.isEmpty {
                Deliverables(bot: task.bot, files: files)
            }

            if noting {
                noteForm.padding(.top, 10)
            } else {
                actions.padding(.top, 10)
            }
        }
        .padding(.vertical, 12)
        .padding(.horizontal, 14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.bubble, in: Corners.bot)
        .hairline(Palette.bubbleBorder, in: Corners.bot)
        .id("task-\(task.id)")
    }

    private var stateLine: String {
        var line = task.status.label
        let time = relativeTime(task.finishedMs ?? task.startedMs ?? task.createdMs)
        if !time.isEmpty { line += " · \(time)" }
        if task.attempt > 1 { line += " · 第 \(task.attempt) 次" }
        return line
    }

    private var stateColor: Color {
        switch task.status {
        case .inReview, .done: Palette.label
        case .blocked: Palette.red
        default: Palette.secondary
        }
    }

    private var actions: some View {
        HStack(spacing: 8) {
            switch task.status {
            case .inReview:
                PillButton(title: "可以", primary: true) { tasks.accept(task.id) }
                PillButton(title: "再改改") { startNote() }
            case .inProgress, .toDo:
                PillButton(title: "补充") { startNote() }
                PillButton(title: "取消") { tasks.cancel(task.id) }
            case .blocked:
                PillButton(title: "重试", primary: true) { tasks.retry(task.id) }
                PillButton(title: "补充后重做") { startNote() }
            case .done:
                PillButton(title: "再改改") { startNote() }
            case .cancelled:
                PillButton(title: "移除") { tasks.remove(task.id) }
            case .backlog:
                EmptyView()
            }
        }
    }

    private var noteForm: some View {
        HStack(spacing: 6) {
            TextField(task.status == .inReview || task.status == .done ? "哪里要改？" : "要补充什么？", text: $note)
                .font(.px(16))
                .focused($noteFocused)
                .submitLabel(.send)
                .onSubmit(submitNote)
                .padding(.vertical, 7)
                .padding(.horizontal, 12)
                .background(.white, in: Capsule())
                .overlay(Capsule().strokeBorder(Palette.separator, lineWidth: 1))
            PillButton(title: "发送", primary: true, horizontal: 12) { submitNote() }
                .disabled(note.trimmingCharacters(in: .whitespaces).isEmpty)
                .opacity(note.trimmingCharacters(in: .whitespaces).isEmpty ? 0.4 : 1)
            PillButton(title: "取消", horizontal: 12) { noting = false }
        }
    }

    private func startNote() {
        noting = true
        noteFocused = true
    }

    private func submitNote() {
        let text = note.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        tasks.note(task.id, text)
        note = ""
        noting = false
    }
}

/// A collapsed delivery note: the first paragraph, the rest on demand.
private struct TaskSummary: View {
    let text: String
    @State private var open = false

    var body: some View {
        let blocks = Markdown.splitBlocks(text)
        // Collapsed, show enough to know what came back: a lone heading says nothing.
        var preview = 0
        var length = 0
        while preview < blocks.count && length < 40 {
            length += blocks[preview].replacingOccurrences(of: "[*`#\\-\\s]", with: "", options: .regularExpression).count
            preview += 1
        }
        let shown = open ? blocks : Array(blocks.prefix(max(1, preview)))
        return VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(shown.enumerated()), id: \.offset) { _, block in
                MarkdownView(text: block, size: 13, lineHeight: 1.55)
            }
            if blocks.count > shown.count || open {
                Button(open ? "收起" : "展开全部") { withAnimation(.ease(0.24)) { open.toggle() } }
                    .font(.px(12, .semibold))
                    .foregroundStyle(Palette.secondary)
                    .buttonStyle(.plain)
                    .padding(.top, 4)
            }
        }
    }
}

/// What came back: pages and pictures shown, everything else as a file to open.
private struct Deliverables: View {
    let bot: String
    let files: [DeliveryFile]
    @Environment(AppModel.self) private var model

    var body: some View {
        let viewed = files.map { ViewedFile(bot: bot, assetID: $0.id, name: $0.name, mime: $0.mime) }
        let pages = viewed.filter(\.isHTML)
        let pictures = viewed.filter { $0.mime.hasPrefix("image/") }
        let others = viewed.filter { !$0.isHTML && !$0.mime.hasPrefix("image/") }
        VStack(alignment: .leading, spacing: 0) {
            ForEach(pages) { HTMLPreview(file: $0) }
            ForEach(pictures) { file in
                RemoteImage(bot: bot, source: .artifact(file.assetID)) { EmptyView() }
                    .frame(maxWidth: .infinity, maxHeight: 360, alignment: .leading)
                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                    .padding(.top, 10)
                    .onTapGesture { model.viewing = file }
            }
            if !others.isEmpty {
                FlowLayout(spacing: 6) {
                    ForEach(others) { file in
                        Button { model.viewing = file } label: {
                            HStack(spacing: 6) {
                                Icon(fileIcon(file.mime), size: 16)
                                Text(file.name).lineLimit(1).truncationMode(.tail)
                            }
                            .font(.px(12))
                            .foregroundStyle(Palette.label)
                            .padding(.vertical, 6)
                            .padding(.horizontal, 10)
                            .background(Palette.fill, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.top, 10)
            }
        }
    }
}

/// Wrapping row (`flex-wrap: wrap`).
struct FlowLayout: Layout {
    var spacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? .infinity
        var x: CGFloat = 0
        var y: CGFloat = 0
        var line: CGFloat = 0
        var widest: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(ProposedViewSize(width: width, height: nil))
            if x > 0 && x + size.width > width {
                y += line + spacing
                x = 0
                line = 0
            }
            x += size.width + spacing
            widest = max(widest, x - spacing)
            line = max(line, size.height)
        }
        return CGSize(width: min(widest, width), height: y + line)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x = bounds.minX
        var y = bounds.minY
        var line: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(ProposedViewSize(width: bounds.width, height: nil))
            if x > bounds.minX && x + size.width > bounds.maxX {
                y += line + spacing
                x = bounds.minX
                line = 0
            }
            view.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
            x += size.width + spacing
            line = max(line, size.height)
        }
    }
}
