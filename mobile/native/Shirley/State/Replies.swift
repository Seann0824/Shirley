import Foundation
import UIKit
import UserNotifications

/// Reply notifications, like a messaging app (`state/notify.ts`): when a bot finishes answering
/// and you are not looking at that conversation, a notification shows its first line; tapping it
/// opens the conversation.
@MainActor
final class ReplyWatcher {
    private static let previewChars = 120
    private var replies: [String: String] = [:]

    /// Feed one event; returns the notification to show, if any.
    func take(_ event: AgentEvent, looking: (String) -> Bool) -> (bot: String, body: String)? {
        guard let bot = event.bot, (event.session ?? "main") == "main" else { return nil }
        switch event.type {
        case "content_delta":
            replies[bot, default: ""] += event.text ?? ""
            return nil
        case "error":
            replies[bot] = nil
            return nil
        case "finished":
            let reply = replies[bot] ?? ""
            replies[bot] = nil
            // Stopped or interrupted by the user: they know, and there is nothing new to read.
            if event.stopReason == "cancelled" || looking(bot) { return nil }
            let body = Self.preview(reply)
            return (bot, body.isEmpty ? "回复好了" : body)
        default:
            return nil
        }
    }

    /// The plain text of a reply: no Markdown marks, one line.
    static func preview(_ markdown: String) -> String {
        var text = markdown
        let rules: [(String, String)] = [
            ("```[\\s\\S]*?```", " "),
            ("!\\[[^\\]]*\\]\\([^)]*\\)", " "),
            ("\\[([^\\]]*)\\]\\([^)]*\\)", "$1"),
            ("[#>*_`~|-]+", " "),
            ("\\s+", " "),
        ]
        for (pattern, template) in rules {
            text = text.replacingOccurrences(of: pattern, with: template, options: .regularExpression)
        }
        text = text.trimmingCharacters(in: .whitespaces)
        return text.count > previewChars ? String(text.prefix(previewChars - 1)) + "…" : text
    }
}

enum Notifier {
    /// Ask once, at the natural moment: the first message sent.
    static func askPermission() {
        let center = UNUserNotificationCenter.current()
        center.getNotificationSettings { settings in
            guard settings.authorizationStatus == .notDetermined else { return }
            center.requestAuthorization(options: [.alert, .sound, .badge]) { _, _ in }
        }
    }

    /// One bot's notification; a newer one from the same bot replaces it.
    static func show(bot: String, title: String, body: String, badge: Int) {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        content.threadIdentifier = "reply:\(bot)"
        content.userInfo = ["bot": bot]
        if badge > 0 { content.badge = NSNumber(value: badge) }
        let request = UNNotificationRequest(identifier: "reply:\(bot)", content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request)
    }

    static func clearBadge() {
        UNUserNotificationCenter.current().setBadgeCount(0)
    }
}
