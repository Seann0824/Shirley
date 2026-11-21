import Foundation
import Observation
import UIKit

/// A screen above home (`Tab` in the web UI's `App.tsx`).
enum Route: Hashable {
    case chat(String)
    case new
    case skills(String)
    case connections
}

/// A message to send as soon as a conversation opens (home screen, "new bot").
struct Seed: Equatable {
    let bot: String
    let text: String
    let attachments: [Attachment]
    let nonce = UUID()
}

/// A file in a bot's library, to look at.
struct ViewedFile: Identifiable, Hashable {
    let bot: String
    let assetID: String
    let name: String
    let mime: String
    var id: String { "\(bot)/\(assetID)" }

    var baseMime: String {
        mime.split(separator: ";").first.map { $0.trimmingCharacters(in: .whitespaces).lowercased() } ?? ""
    }

    var isHTML: Bool { baseMime == "text/html" }
    var isText: Bool { baseMime.hasPrefix("text/") || baseMime == "application/json" }
}

/// App-wide state: the stores, where the user is, and what is open over everything.
@MainActor
@Observable
final class AppModel {
    /// The generalist: open first, and where unroutable messages go.
    static let defaultBot = "nono"

    let bots = BotsStore()
    let tasks = TasksStore()

    var path: [Route] = []
    /// The conversation beside the list (two-pane layout).
    var selected = AppModel.defaultBot
    var wide = false
    var seed: Seed?
    var asking = false
    /// The file viewer, over everything.
    var viewing: ViewedFile?
    /// Whether the app is in front of the user.
    var active = true

    @ObservationIgnored private var watcher = ReplyWatcher()
    @ObservationIgnored private var listener: UUID?
    @ObservationIgnored private var unread = 0

    func bot(_ name: String) -> Bot? {
        bots.bots.first { $0.name == name }
    }

    var selectedBot: Bot? {
        bot(selected) ?? bots.bots.first
    }

    /// The conversation on screen, if any.
    var currentChat: String? {
        if wide {
            return path.isEmpty ? selectedBot?.name : nil
        }
        if case .chat(let name) = path.last { return name }
        return nil
    }

    func go(_ route: Route) {
        if path.last == route { return }
        path.append(route)
    }

    func back() {
        if !path.isEmpty { path.removeLast() }
    }

    /// Open a bot's conversation. Coming from "new bot", the conversation replaces that screen,
    /// so back does not return to the form.
    func openBot(_ name: String) {
        if wide {
            selected = name
            path = []
            return
        }
        selected = name
        if path.last == .new {
            path[path.count - 1] = .chat(name)
        } else {
            path = [.chat(name)]
        }
    }

    /// The home composer: the platform picks the bot, then the message goes into its conversation.
    func ask(_ text: String, _ attachments: [Attachment]) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty || !attachments.isEmpty else { return }
        asking = true
        Task {
            let bot = (try? await API.route(trimmed.isEmpty ? "处理这些附件" : trimmed)) ?? Self.defaultBot
            asking = false
            seed = Seed(bot: bot, text: text, attachments: attachments)
            openBot(bot)
        }
    }

    // MARK: Reply notifications

    func startWatching() {
        guard listener == nil else { return }
        listener = EventHub.shared.add { [weak self] event in self?.watch(event) }
        AppDelegate.openBot = { [weak self] bot in self?.openBot(bot) }
        if let pending = AppDelegate.pendingBot {
            AppDelegate.pendingBot = nil
            openBot(pending)
        }
    }

    private func watch(_ event: AgentEvent) {
        if event.type == "resync" {
            Task { await bots.refresh() }
        } else {
            Replying.shared.note(event)
        }
        let looking: (String) -> Bool = { [weak self] bot in
            guard let self else { return false }
            return self.active && self.currentChat == bot
        }
        guard let reply = watcher.take(event, looking: looking) else { return }
        if bots.muted.contains(reply.bot) { return }
        if !active { unread += 1 }
        Notifier.show(bot: reply.bot, title: bot(reply.bot)?.displayName ?? reply.bot, body: reply.body, badge: unread)
    }

    func becameActive() {
        active = true
        unread = 0
        Notifier.clearBadge()
        EventHub.shared.wake()
    }

    func becameInactive() {
        active = false
        EventHub.shared.sleep()
    }
}
