import Foundation
import Observation

/// How live events change a conversation. A line-for-line port of the web UI's
/// `state/conversation.ts`; the two must agree.
enum Conversation {
    static func fromSnapshot(_ items: [ConversationItem], running: Bool = false) -> [ConversationItem] {
        items.enumerated().map { index, item in
            var copy = item
            // Only the newest group of a turn that is still going may animate.
            if copy.kind == .tools { copy.settled = !(running && index == items.count - 1) }
            return copy
        }
    }

    /// Mark every tool group finished. A group can only still be running while it is the last
    /// thing in the conversation: once anything follows it, or the turn ends, its calls are over.
    private static func settleTools(_ items: [ConversationItem]) -> [ConversationItem] {
        items.map { item in
            guard item.kind == .tools, !item.settled else { return item }
            var copy = item
            copy.settled = true
            return copy
        }
    }

    /// Close any assistant message that is still receiving deltas, and any tool group before it.
    private static func sealStreaming(_ items: [ConversationItem]) -> [ConversationItem] {
        var sealed = items
        if let last = sealed.last, last.kind == .message, last.streaming {
            sealed[sealed.count - 1].streaming = false
        }
        return settleTools(sealed)
    }

    /// The user's own message. Sending mid-reply interrupts the reply, so whatever was streaming
    /// is finished as it stands.
    static func appendUser(_ items: [ConversationItem], content: String, parts: [RenderPart]) -> [ConversationItem] {
        sealStreaming(items) + [ConversationItem(kind: .message, role: "user", content: content, parts: parts)]
    }

    static func reduce(_ items: [ConversationItem], _ event: AgentEvent) -> [ConversationItem] {
        switch event.type {
        case "content_delta":
            let text = event.text ?? ""
            if let last = items.last, last.kind == .message, last.streaming {
                var next = items
                next[next.count - 1].content += text
                return next
            }
            return settleTools(items) + [ConversationItem(kind: .message, role: "assistant", content: text, streaming: true)]
        case "message_added":
            return event.role == "assistant" ? sealStreaming(items) : items
        case "tool_started":
            // Joining the live group keeps it open; anything else starts a new group.
            let live = items.last.map { $0.kind == .tools && !$0.settled } ?? false
            var sealed = live ? items : sealStreaming(items)
            let call = ToolCall(id: event.callID ?? UUID().uuidString, name: event.name ?? "", arguments: event.arguments ?? "")
            if let last = sealed.last, last.kind == .tools, !last.settled {
                sealed[sealed.count - 1].calls.append(call)
                return sealed
            }
            return sealed + [ConversationItem.tools([call])]
        case "tool_finished":
            return items.map { item in
                guard item.kind == .tools else { return item }
                var copy = item
                copy.calls = item.calls.map { call in
                    guard call.id == event.callID else { return call }
                    var done = call
                    done.output = event.output
                    done.ok = event.ok
                    done.parts = event.parts ?? []
                    return done
                }
                return copy
            }
        case "compact":
            return sealStreaming(items) + [ConversationItem(kind: .compact, content: event.content ?? "")]
        case "notice":
            return sealStreaming(items) + [ConversationItem(kind: .message, role: "notice", content: event.text ?? "")]
        case "retrying":
            return sealStreaming(items) + [ConversationItem(
                kind: .message, role: "notice",
                content: "第 \(event.attempt ?? 0) 次重试：\(event.reason ?? "")"
            )]
        case "error":
            return sealStreaming(items) + [ConversationItem(kind: .message, role: "error", content: event.message ?? "")]
        case "finished":
            return sealStreaming(items)
        default:
            return items
        }
    }

    /// Where each turn's task cards go: after the turn's last item. The bot hands work off and
    /// then says so; the card -- later the result -- reads after that, not above it.
    static func cardsByTurnEnd(_ items: [ConversationItem]) -> [Int: [ToolCall]] {
        var placed: [Int: [ToolCall]] = [:]
        var pending: [ToolCall] = []
        func settle(_ index: Int) {
            if !pending.isEmpty && index >= 0 { placed[index, default: []] += pending }
            pending = []
        }
        for (index, item) in items.enumerated() {
            if item.kind == .message && item.role == "user" { settle(index - 1) }
            if item.kind == .tools { pending += item.calls.filter { $0.name == "dispatch_task" } }
        }
        settle(items.count - 1)
        return placed
    }

    /// What a tool group shows: whether it still works, and the files it produced.
    static func toolActivity(_ calls: [ToolCall], settled: Bool) -> (running: Bool, parts: [RenderPart]) {
        let parts = calls.flatMap { $0.parts.filter(\.isMedia) }
        let running = !settled && calls.contains { $0.ok == nil }
        return (running, parts)
    }
}

/// One bot's conversation (`state/useConversation.ts`): the newest page on open, live events
/// layered on top, older pages on demand.
@MainActor
@Observable
final class ConversationStore {
    static let pageSize = 40
    private static let mainSession = "main"

    let bot: String
    private(set) var items: [ConversationItem] = []
    /// Turns asked for and not yet finished. Sending mid-reply interrupts it: the old turn's
    /// `finished` arrives after the new one was asked for, and must not clear the new one.
    private(set) var outstanding = 0
    private(set) var loaded = false
    private(set) var error: String?
    /// Index of `items[0]` in the whole conversation: above 0, older items wait on the server.
    private(set) var start = 0
    private(set) var loadingOlder = false

    var running: Bool { outstanding > 0 }
    var hasOlder: Bool { start > 0 }

    @ObservationIgnored private var cursor: Int?
    @ObservationIgnored private var buffered: [AgentEvent] = []
    @ObservationIgnored private var generation = 0
    @ObservationIgnored var onTurnFinished: (() -> Void)?

    init(bot: String) {
        self.bot = bot
    }

    /// Listen and load; runs until the view goes away (the calling task is cancelled).
    func run() async {
        let listener = EventHub.shared.add { [weak self] event in self?.receive(event) }
        await load()
        while !Task.isCancelled {
            try? await Task.sleep(nanoseconds: 3_600_000_000_000)
        }
        EventHub.shared.remove(listener)
    }

    private func apply(_ event: AgentEvent) {
        items = Conversation.reduce(items, event)
        if event.type == "finished" || event.type == "error" {
            outstanding = max(0, outstanding - 1)
            onTurnFinished?()
        }
    }

    private func receive(_ event: AgentEvent) {
        if event.type == "resync" {
            cursor = nil
            Task { await load() }
            return
        }
        guard event.bot == bot, (event.session ?? Self.mainSession) == Self.mainSession else { return }
        guard let cursor else {
            buffered.append(event)
            return
        }
        if let sequence = event.sequence, sequence <= cursor { return }
        apply(event)
    }

    func load() async {
        generation += 1
        loadingOlder = false
        do {
            let snapshot = try await API.subscribe(bot, limit: Self.pageSize)
            items = Conversation.fromSnapshot(snapshot.items, running: snapshot.running)
            start = snapshot.start
            outstanding = snapshot.running ? 1 : 0
            cursor = snapshot.sequence
            loaded = true
            error = nil
            let pending = buffered
            buffered = []
            for event in pending where (event.sequence ?? Int.max) > snapshot.sequence {
                apply(event)
            }
        } catch {
            self.error = error.localizedDescription
            loaded = true
        }
    }

    func send(_ text: String, images: [String], parts: [RenderPart]) async {
        items = Conversation.appendUser(items, content: text, parts: parts)
        outstanding += 1
        do {
            // The bubble matches what was typed; the wire still needs something to answer.
            let wire = text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !images.isEmpty ? "请处理这些附件" : text
            try await API.send(bot, text: wire, images: images)
        } catch {
            outstanding = max(0, outstanding - 1)
            report(error.localizedDescription)
        }
    }

    /// Fetch the page before the oldest item shown and put it on top.
    func loadOlder() async {
        guard start > 0, !loadingOlder else { return }
        loadingOlder = true
        let asked = generation
        defer { if asked == generation { loadingOlder = false } }
        guard let page = try? await API.history(bot, before: start, limit: Self.pageSize), asked == generation else { return }
        items = Conversation.fromSnapshot(page.items) + items
        start = page.start
    }

    func cancel() {
        Task { try? await API.cancel(bot) }
    }

    /// Take back the latest message (stopping its reply); its text comes back to edit.
    func recall() async throws -> String {
        let text = try await API.recall(bot)
        cursor = nil
        await load()
        return text
    }

    /// A client-side problem (e.g. a failed upload), shown inline like a server error.
    func report(_ message: String) {
        items.append(ConversationItem(kind: .message, role: "error", content: message))
    }
}
