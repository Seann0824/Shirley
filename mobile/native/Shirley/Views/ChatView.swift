import SwiftUI
import UIKit

/// The tools whose calls show as task cards and notes, not as activity.
private let taskTools: Set<String> = ["dispatch_task", "task_note"]

/// One line of the thread, flattened the way the web UI's DOM is: every bubble, picture, card
/// and note is its own row, and the gaps between them follow the stylesheet's margins.
private struct ThreadRow: Identifiable {
    enum Kind {
        case userMedia(RenderPart)
        case userText(String)
        case recall
        case note(role: String, text: String)
        case typing
        case botBlock(String, continued: Bool, caret: Bool)
        case botMedia(RenderPart)
        case tools([ToolCall], settled: Bool)
        case system(String)
        case task(ToolCall)
        case greeting
        case error(String)
        case run
    }

    let id: String
    let kind: Kind
    var recallable = false

    /// CSS margins on top of the thread's 15 pt gap.
    var marginTop: CGFloat {
        switch kind {
        case .botBlock(_, let continued, _): continued ? -6 : 0
        case .botMedia: -6
        case .tools: -9
        case .recall: -4
        case .system: 2
        default: 0
        }
    }

    var marginBottom: CGFloat {
        switch kind {
        case .tools: -9
        case .recall: 8
        case .system: 2
        default: 0
        }
    }

    var isUser: Bool {
        switch kind {
        case .userMedia, .userText: true
        default: false
        }
    }
}

/// Where the reader is in the conversation.
private struct ScrollState: Equatable {
    var offset: CGFloat = 0
    var content: CGFloat = 0
    var container: CGFloat = 0
    var insetTop: CGFloat = 0
    var insetBottom: CGFloat = 0

    var maxOffset: CGFloat { content + insetBottom - container }
    var fromBottom: CGFloat { maxOffset - offset }
    var fromTop: CGFloat { offset + insetTop }
}

/// One bot's conversation (`views/Chat.tsx`).
struct ChatView: View {
    let bot: Bot
    @Environment(AppModel.self) private var model
    @State private var conversation: ConversationStore
    @State private var position = ScrollPosition(edge: .bottom)
    /// Stays on the latest message while it grows, unless the user scrolled up to read.
    @State private var pinned = true
    /// Distance from the bottom, saved just before older items go on top, so the message the
    /// user was reading stays where it was.
    @State private var anchor: CGFloat?
    /// Opening: the cached copy shows first, then the fresh one replaces it, and the push
    /// transition moves things meanwhile. Until that settles (or the user takes the scroll),
    /// every change goes to the bottom and none of it counts as the user scrolling up.
    @State private var settling = true
    @State private var width: CGFloat = 360
    @State private var recallOpen = false
    @State private var draft: Draft?
    @State private var seeded: UUID?

    /// Within this many points of the top, the page before the oldest message is fetched.
    private static let olderThreshold: CGFloat = 240

    init(bot: Bot) {
        self.bot = bot
        _conversation = State(initialValue: ConversationStore(bot: bot.name))
    }

    var body: some View {
        VStack(spacing: 0) {
            header.padding(.horizontal, 17)
            ScrollViewReader { proxy in
                ScrollView {
                    thread
                        .padding(.vertical, 18)
                        .padding(.horizontal, 17)
                        .frame(maxWidth: model.wide ? 760 : .infinity)
                        .frame(maxWidth: .infinity)
                }
                .scrollPosition($position)
                .defaultScrollAnchor(.bottom)
                .scrollIndicators(.hidden)
                .scrollDismissesKeyboard(.interactively)
                .onScrollGeometryChange(for: ScrollState.self) { geometry in
                    ScrollState(
                        offset: geometry.contentOffset.y,
                        content: geometry.contentSize.height,
                        container: geometry.containerSize.height,
                        insetTop: geometry.contentInsets.top,
                        insetBottom: geometry.contentInsets.bottom
                    )
                } action: { old, new in
                    scrolled(old, new)
                }
                .onScrollPhaseChange { _, phase in
                    if phase == .interacting { settling = false }
                }
                .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = min($0, 760) - 34 }
                .safeAreaInset(edge: .bottom, spacing: 0) {
                    dock(proxy).frame(maxWidth: model.wide ? 760 : .infinity)
                }
            }
        }
        .background(Palette.page)
        .toolbar(.hidden, for: .navigationBar)
        .task {
            conversation.onTurnFinished = { [bots = model.bots] in Task { await bots.refresh() } }
            await conversation.run()
        }
        .onChange(of: conversation.loaded) { _, loaded in
            sendSeed()
            guard loaded, settling else { return }
            // Let the fresh items lay out (pictures, markdown) before scrolling is the user's again.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                guard settling else { return }
                settling = false
                pinned = true
                position.scrollTo(edge: .bottom)
            }
        }
        .onChange(of: model.seed) { _, _ in sendSeed() }
        .onChange(of: conversation.loadingOlder) { _, loading in
            // A page that failed to arrive leaves nothing to hold on to.
            if !loading { DispatchQueue.main.asyncAfter(deadline: .now() + 0.15) { anchor = nil } }
        }
        .onChange(of: lastUser) { _, _ in recallOpen = false }
    }

    // MARK: Header

    private var header: some View {
        let tone = BotTone(bot: bot.name)
        return HStack(spacing: 0) {
            GlassCircleButton(icon: .chevron, flipped: true, label: "返回") { model.back() }
                .opacity(model.wide && model.path.isEmpty ? 0 : 1)
                .disabled(model.wide && model.path.isEmpty)
            HStack(spacing: 8) {
                ZStack {
                    Circle().fill(tone.color)
                    BotMark(bot: bot.name, size: 27, eyes: NonoEyes(width: 3, height: 5, gap: 5)).foregroundStyle(.white)
                }
                .frame(width: 32, height: 32)
                .clipShape(Circle())
                HStack(spacing: 5) {
                    Text(bot.displayName).font(.px(14, .semibold)).foregroundStyle(Palette.label).lineLimit(1)
                    Circle().fill(Palette.online).frame(width: 6, height: 6)
                }
            }
            .frame(maxWidth: .infinity)
            GlassCircleButton(icon: .spark, iconSize: 19, label: "\(bot.displayName) 的技能") { model.go(.skills(bot.name)) }
        }
        .frame(height: 62)
    }

    // MARK: Thread

    /// The tool the running turn is in right now, read from the conversation itself, so it
    /// survives reopening the chat or a reload from the server.
    private var liveTool: String? {
        guard let last = conversation.items.last, last.kind == .tools, !last.settled else { return nil }
        return last.calls.last { $0.output == nil && $0.ok == nil }?.name
    }

    private var lastUser: Int {
        conversation.items.lastIndex { $0.kind == .message && $0.role == "user" } ?? -1
    }

    private var rows: [ThreadRow] {
        var rows: [ThreadRow] = []
        let items = conversation.items
        let cards = Conversation.cardsByTurnEnd(items)
        let base = conversation.start
        if conversation.loadingOlder { rows.append(ThreadRow(id: "older", kind: .system("加载更早的消息…"))) }
        if let error = conversation.error { rows.append(ThreadRow(id: "error", kind: .error(error))) }
        if !conversation.loaded && items.isEmpty && conversation.error == nil {
            rows.append(ThreadRow(id: "loading", kind: .system("正在加载…")))
        }
        if conversation.loaded && items.isEmpty { rows.append(ThreadRow(id: "greeting", kind: .greeting)) }
        for (index, item) in items.enumerated() {
            let key = "\(base + index)"
            switch item.kind {
            case .message where item.role == "user":
                // Every picture or file is its own message; the text is another.
                let recallable = index == lastUser
                for (part, media) in item.parts.filter(\.isMedia).enumerated() {
                    rows.append(ThreadRow(id: "\(key)u\(part)", kind: .userMedia(media), recallable: recallable))
                }
                if !item.content.isEmpty {
                    rows.append(ThreadRow(id: "\(key)u", kind: .userText(item.content), recallable: recallable))
                }
                if recallable && recallOpen { rows.append(ThreadRow(id: "\(key)r", kind: .recall)) }
            case .message where item.role == "notice" || item.role == "error":
                rows.append(ThreadRow(id: key, kind: .note(role: item.role, text: item.content)))
            case .message where item.streaming && item.content.isEmpty:
                rows.append(ThreadRow(id: key, kind: .typing))
            case .message:
                // One reply, several bubbles: each paragraph is its own message, like a person texting.
                let blocks = Markdown.splitBlocks(item.content)
                for (block, text) in blocks.enumerated() {
                    rows.append(ThreadRow(id: "\(key)b\(block)", kind: .botBlock(
                        text, continued: block > 0, caret: item.streaming && block == blocks.count - 1)))
                }
                for (part, media) in item.parts.filter(\.isMedia).enumerated() {
                    rows.append(ThreadRow(id: "\(key)m\(part)", kind: .botMedia(media)))
                }
            case .tools:
                let other = item.calls.filter { !taskTools.contains($0.name) }
                let activity = Conversation.toolActivity(other, settled: item.settled)
                if activity.running || !activity.parts.isEmpty {
                    rows.append(ThreadRow(id: "\(key)t", kind: .tools(other, settled: item.settled)))
                }
                // A call restored from the log carries no status; its JSON output is the proof it worked.
                for call in item.calls where call.name == "task_note" && call.ok != false
                    && ToolCall.field(call.output, "task_id") != nil {
                    let title = ToolCall.field(call.output, "title") ?? "任务"
                    rows.append(ThreadRow(id: "\(key)n\(call.id)", kind: .system("已补充到「\(title)」")))
                }
            case .compact:
                rows.append(ThreadRow(id: key, kind: .system("较早的对话已整理成摘要")))
            }
            for call in cards[index] ?? [] {
                if dispatchShows(call) { rows.append(ThreadRow(id: "card-\(call.id)", kind: .task(call))) }
            }
        }
        if conversation.running { rows.append(ThreadRow(id: "run-\(lastUser)", kind: .run)) }
        return rows
    }

    /// A dispatched task's card shows once the task list knows whether it still exists.
    private func dispatchShows(_ call: ToolCall) -> Bool {
        let id = ToolCall.field(call.output, "task_id")
        return id == nil || model.tasks.task(id ?? "") != nil || model.tasks.loaded
    }

    private var thread: some View {
        let all = self.rows
        return VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(all.enumerated()), id: \.element.id) { index, row in
                rowView(row)
                    .padding(.top, index == 0 ? 0 : gap(all[index - 1], row))
            }
        }
    }

    private func gap(_ previous: ThreadRow, _ row: ThreadRow) -> CGFloat {
        var gap = 15 + previous.marginBottom + row.marginTop
        // `.user-message + .user-message`
        if previous.isUser && row.isUser { gap -= 6 }
        return gap
    }

    private var bubbleWidth: CGFloat { width * 0.76 }

    @ViewBuilder
    private func rowView(_ row: ThreadRow) -> some View {
        switch row.kind {
        case .userMedia(let part):
            HStack {
                Spacer(minLength: 0)
                PartView(bot: bot.name, part: part, maxWidth: bubbleWidth).onTapGesture { toggleRecall(row) }
            }
        case .userText(let text):
            HStack {
                Spacer(minLength: 0)
                Text(text)
                    .font(.px(14))
                    .lineSpacing(Markdown.spacing(14, 1.45))
                    .foregroundStyle(.white)
                    .textSelection(.enabled)
                    .padding(.top, 10)
                    .padding(.horizontal, 12)
                    .padding(.bottom, 7)
                    .background(Color.black, in: Corners.user)
                    .frame(maxWidth: bubbleWidth, alignment: .trailing)
                    .onTapGesture { toggleRecall(row) }
            }
        case .recall:
            HStack {
                Spacer()
                PillButton(title: "撤回并编辑", font: .px(12, .semibold), horizontal: 12, vertical: 5) { takeBack() }
            }
        case .note(let role, let text):
            Text(text)
                .font(.px(role == "notice" ? 12 : 14))
                .lineSpacing(Markdown.spacing(14, 1.45))
                .foregroundStyle(role == "notice" ? Palette.secondary : Palette.red)
                .padding(.vertical, role == "notice" ? 2 : 10)
                .padding(.horizontal, 12)
                .background(role == "notice" ? Color.clear : Palette.red.opacity(0.08), in: Corners.bot)
                .hairline(Palette.bubbleBorder, in: Corners.bot)
                .frame(maxWidth: bubbleWidth, alignment: .leading)
        case .typing:
            botRow { TypingBubble() }
        case .botBlock(let text, let continued, let caret):
            botRow(avatarHidden: continued) {
                MarkdownView(text: text, caret: caret)
                    .padding(.top, 10)
                    .padding(.horizontal, 12)
                    .padding(.bottom, 7)
                    .background(Palette.bubble, in: Corners.bot)
                    .hairline(Palette.bubbleBorder, in: Corners.bot)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: bubbleWidth, alignment: .leading)
            }
        case .botMedia(let part):
            botRow(avatarHidden: true) { PartView(bot: bot.name, part: part, maxWidth: bubbleWidth) }
        case .tools(let calls, let settled):
            let activity = Conversation.toolActivity(calls, settled: settled)
            botRow(avatarHidden: true) {
                VStack(alignment: .leading, spacing: 6) {
                    if activity.running {
                        HStack(spacing: 3) {
                            PulseDot(delay: 0)
                            PulseDot(delay: 0.15)
                            PulseDot(delay: 0.3)
                        }
                        .accessibilityLabel("正在运行")
                    }
                    ForEach(Array(activity.parts.enumerated()), id: \.offset) { _, part in
                        PartView(bot: bot.name, part: part, maxWidth: 220, radius: 12)
                    }
                }
                .padding(.vertical, 2)
                .frame(minHeight: 12)
            }
        case .system(let text):
            Text(text)
                .font(.px(12))
                .foregroundStyle(Palette.tertiary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity)
        case .task(let call):
            DispatchedTask(bot: bot.name, call: call, width: width)
        case .greeting:
            botRow {
                Text("我在。直接告诉我需要完成什么。")
                    .font(.px(14))
                    .foregroundStyle(Palette.label)
                    .padding(.top, 10)
                    .padding(.horizontal, 12)
                    .padding(.bottom, 7)
                    .background(Palette.bubble, in: Corners.bot)
                    .hairline(Palette.bubbleBorder, in: Corners.bot)
            }
        case .error(let message):
            NoteView(text: message)
        case .run:
            RunStatusRow(bot: bot.name, name: bot.displayName, tool: liveTool)
        }
    }

    private func botRow<Content: View>(avatarHidden: Bool = false, @ViewBuilder _ content: () -> Content) -> some View {
        HStack(alignment: .bottom, spacing: 8) {
            MessageAvatar(bot: bot.name, hidden: avatarHidden)
            content()
            Spacer(minLength: 0)
        }
    }

    // MARK: Scrolling

    private func scrolled(_ old: ScrollState, _ new: ScrollState) {
        if settling {
            if new.fromBottom > 1 { position.scrollTo(edge: .bottom) }
            // A first page shorter than the screen still fetches more.
            if new.content <= new.container { loadOlder(new) }
            return
        }
        if new.content != old.content || new.container != old.container {
            if let anchor {
                position.scrollTo(y: max(0, new.maxOffset - anchor))
            } else if pinned {
                position.scrollTo(edge: .bottom)
            }
            // A first page shorter than the screen cannot be scrolled; fetch more until it can.
            if new.content <= new.container { loadOlder(new) }
            return
        }
        pinned = new.fromBottom < 80
        if new.fromTop <= Self.olderThreshold { loadOlder(new) }
    }

    private func loadOlder(_ state: ScrollState) {
        guard conversation.loaded, conversation.hasOlder, !conversation.loadingOlder, anchor == nil else { return }
        anchor = state.fromBottom
        Task { await conversation.loadOlder() }
    }

    // MARK: Sending

    /// A message from home or "new bot" goes out as soon as the conversation has loaded.
    private func sendSeed() {
        guard conversation.loaded, let seed = model.seed, seed.bot == bot.name, seeded != seed.nonce else { return }
        seeded = seed.nonce
        model.seed = nil
        send(seed.text, seed.attachments)
    }

    private func send(_ text: String, _ attachments: [Attachment]) {
        settling = false
        pinned = true
        position.scrollTo(edge: .bottom)
        Task {
            // Pictures and files go into the bot's library first; the message references them.
            var ids: [String] = []
            var failed: [String] = []
            var sent: [Attachment] = []
            for attachment in attachments {
                let prepared = await Task.detached { Media.prepare(attachment) }.value
                do {
                    let entry = try await API.upload(bot: bot.name, name: prepared.name, mime: prepared.mime, data: prepared.data)
                    ids.append(entry.id)
                    sent.append(attachment)
                } catch {
                    failed.append(attachment.name)
                }
            }
            if text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && ids.isEmpty {
                if !failed.isEmpty { conversation.report("附件上传失败：\(failed.joined(separator: "、"))") }
                return
            }
            // The bubble shows what was sent before the round trip.
            let parts: [RenderPart] = sent.map { attachment in
                if attachment.kind == .image, let preview = attachment.preview {
                    return .image(source: .url(ImageStore.shared.keepLocal(preview)), alt: attachment.name, mime: attachment.mime)
                }
                return .file(source: nil, name: attachment.name, mime: attachment.mime, bytes: attachment.data.count)
            }
            await conversation.send(text, images: ids, parts: parts)
            if !failed.isEmpty { conversation.report("没有发出去的附件：\(failed.joined(separator: "、"))") }
        }
    }

    private func toggleRecall(_ row: ThreadRow) {
        guard row.recallable else { return }
        withAnimation(.ease(0.2)) { recallOpen.toggle() }
    }

    /// Take the latest message back, stopping its reply, and put its text in the field.
    private func takeBack() {
        recallOpen = false
        Task {
            do {
                draft = Draft(text: try await conversation.recall())
            } catch {
                conversation.report(error.localizedDescription)
            }
        }
    }

    private func dock(_ proxy: ScrollViewProxy) -> some View {
        // Results waiting for the user's verdict, possibly scrolled out of sight.
        let waiting = model.tasks.tasks.filter { $0.bot == bot.name && $0.status == .inReview }
        let pill: AnyView? = waiting.isEmpty ? nil : AnyView(
            Button {
                guard let first = waiting.first else { return }
                withAnimation(.ease(0.4)) { proxy.scrollTo("task-\(first.id)", anchor: .center) }
            } label: {
                HStack(spacing: 6) {
                    Icon(.check, size: 13)
                    Text(waiting.count == 1 ? "「\(waiting[0].title)」做好了" : "\(waiting.count) 个结果等你看").lineLimit(1)
                }
                .font(.px(12, .semibold))
                .foregroundStyle(.white)
                .padding(.vertical, 6)
                .padding(.horizontal, 14)
                .background(Palette.label, in: Capsule())
            }
            .buttonStyle(.plain)
            .padding(.bottom, 2)
        )
        return ComposerDock(
            busy: conversation.running,
            onStop: { conversation.cancel() },
            draft: draft,
            above: pill
        ) { text, attachments in
            send(text, attachments)
        }
    }
}

/// A picture, file or link in the thread. A picture stands on its own, like a photo in a chat
/// app; a web page renders in place; a file or link gets a chip.
private struct PartView: View {
    let bot: String
    let part: RenderPart
    let maxWidth: CGFloat
    var radius: CGFloat = 18
    @Environment(AppModel.self) private var model

    var body: some View {
        switch part {
        case .image(let source, let alt, _):
            RemoteImage(bot: bot, source: source) {
                Text(alt).font(.px(12)).foregroundStyle(Palette.tertiary)
            }
            .frame(maxWidth: min(maxWidth, 240, UIScreen.main.bounds.width * 0.62), maxHeight: 320)
            .clipShape(RoundedRectangle(cornerRadius: radius, style: .continuous))
            .contentShape(Rectangle())
            .onTapGesture {
                if case .artifact(let id) = source {
                    model.viewing = ViewedFile(bot: bot, assetID: id, name: alt.isEmpty ? "图片" : alt, mime: "image/*")
                }
            }
        case .file(let source, let name, let mime, let bytes):
            let file: ViewedFile? = {
                if case .artifact(let id) = source { return ViewedFile(bot: bot, assetID: id, name: name, mime: mime ?? "") }
                return nil
            }()
            if let file, file.isHTML {
                HTMLPreview(file: file).frame(maxWidth: maxWidth)
            } else {
                HStack(spacing: 8) {
                    Icon(.file, size: 19)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(name).font(.px(9, .bold)).lineLimit(1).frame(maxWidth: 105, alignment: .leading)
                        if let bytes { Text("\(Int((Double(bytes) / 1024).rounded(.up))) KB").font(.px(7)).foregroundStyle(Palette.gray) }
                    }
                }
                .foregroundStyle(Palette.label)
                .padding(.vertical, 10)
                .padding(.horizontal, 14)
                .frame(minWidth: 145, alignment: .leading)
                .background(Palette.fill, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                .contentShape(Rectangle())
                .onTapGesture { if let file { model.viewing = file } }
            }
        case .link(let url, let title):
            if let target = URL(string: url) {
                Link(destination: target) {
                    Text(title.isEmpty ? url : title)
                        .font(.px(14))
                        .underline()
                        .foregroundStyle(Palette.label)
                        .padding(.vertical, 10)
                        .padding(.horizontal, 14)
                        .background(Palette.fill, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                }
            }
        default:
            EmptyView()
        }
    }
}

/// Three dots while the reply has not started.
private struct TypingBubble: View {
    @State private var up = false

    var body: some View {
        HStack(spacing: 4) {
            ForEach(0..<3, id: \.self) { index in
                Circle()
                    .fill(Palette.gray)
                    .frame(width: 5, height: 5)
                    .offset(y: up ? -2 : 2)
                    .opacity(up ? 1 : 0.4)
                    .animation(.easeInOut(duration: 0.8).repeatForever(autoreverses: true).delay(Double(index) * 0.15), value: up)
            }
        }
        .padding(.vertical, 8)
        .padding(.horizontal, 13)
        .frame(minWidth: 57, minHeight: 36)
        .background(Palette.bubble, in: Corners.bot)
        .hairline(Palette.bubbleBorder, in: Corners.bot)
        .onAppear { up = true }
        .accessibilityLabel("正在输入")
    }
}

/// Where the bot handed work off: the task's card, kept current, in place.
private struct DispatchedTask: View {
    let bot: String
    let call: ToolCall
    let width: CGFloat
    @Environment(AppModel.self) private var model

    var body: some View {
        let id = ToolCall.field(call.output, "task_id")
        let task = id.flatMap { model.tasks.task($0) }
        let title = ToolCall.field(call.arguments, "title") ?? "任务"
        HStack(alignment: .top, spacing: 8) {
            MessageAvatar(bot: bot)
            if let task {
                TaskCard(task: task).frame(maxWidth: min(620, width * 0.82), alignment: .leading)
            } else {
                Text(call.ok == false ? "没能派出「\(title)」" : id == nil ? "正在派出「\(title)」…" : "「\(title)」已从任务列表移除")
                    .font(.px(13))
                    .foregroundStyle(Palette.secondary)
                    .padding(.vertical, 8)
                    .padding(.horizontal, 12)
            }
            Spacer(minLength: 0)
        }
    }
}

/// While a turn runs: how long it has run, what it is doing, and how long since anything
/// happened -- so "working on a slow tool" and "stuck or disconnected" look different.
private struct RunStatusRow: View {
    let bot: String
    let name: String
    let tool: String?
    @State private var started = Date()
    @State private var lastEvent = Date()
    @State private var note: String?
    @State private var online = true

    /// Silence longer than this is mentioned, plainly: a model thinking hard or a slow tool is
    /// normal. Whether the turn still runs is not guessed -- the conversation asks the server
    /// after 15 quiet seconds, and a turn that is over disappears.
    private static let quietSeconds = 60

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { context in
            let elapsed = max(0, Int(context.date.timeIntervalSince(started)))
            let quiet = max(0, Int(context.date.timeIntervalSince(lastEvent)))
            let line = status(quiet: quiet)
            let text = line.text
            let warn = line.warn
            HStack(alignment: .center, spacing: 8) {
                MessageAvatar(bot: bot)
                HStack(spacing: 8) {
                    PulseDot(size: 7, color: warn ? Color(hex: 0xFF9500) : Color(hex: 0x34C759))
                    Text(text).frame(maxWidth: .infinity, alignment: .leading)
                    Text(Self.clock(elapsed)).monospacedDigit()
                }
                .font(.px(12))
                .foregroundStyle(warn ? Color(hex: 0xC93400) : Palette.secondary)
            }
        }
        .task(id: bot) { await watch() }
    }

    private func status(quiet: Int) -> (text: String, warn: Bool) {
        let base = note ?? (tool.map { "正在使用 \($0)" } ?? "\(name) 正在思考")
        if !online { return ("与服务器的连接断开了，正在重连…", true) }
        if quiet >= Self.quietSeconds { return ("\(base) · 已 \(Self.clock(quiet)) 没有新输出", false) }
        return (base, false)
    }

    private static func clock(_ seconds: Int) -> String {
        String(format: "%d:%02d", seconds / 60, seconds % 60)
    }

    private func watch() async {
        let listener = EventHub.shared.add { event in
            if let from = event.bot, from != bot { return }
            lastEvent = Date()
            switch event.type {
            case "retrying": note = "出错了，正在重试（第 \(event.attempt ?? 0) 次）"
            case "compaction_started": note = "正在整理上下文"
            case "compaction_finished", "content_delta": note = nil
            default: break
            }
        }
        let watcher = EventHub.shared.watch { online = $0 }
        while !Task.isCancelled {
            try? await Task.sleep(nanoseconds: 3_600_000_000_000)
        }
        EventHub.shared.remove(listener)
        EventHub.shared.unwatch(watcher)
    }
}
