import Foundation

/// A failed request, with the status kept: 401/403 means "ask for a token", not "server is down".
struct APIError: LocalizedError {
    let message: String
    let status: Int

    var errorDescription: String? { message }
}

/// Transport to the Shirley server: `POST /api/cmd/{name}` for commands, plus upload. Same
/// endpoints and arguments as the web UI's `api/bridge.ts`.
@MainActor
enum API {
    private static let decoder = JSONDecoder()

    private static func makeRequest(_ path: String) throws -> URLRequest {
        guard let url = Session.shared.url(path) else {
            throw APIError(message: "还没有设置服务器地址", status: 0)
        }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        if let token = Session.shared.token {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        return request
    }

    private static func check(_ data: Data, _ response: URLResponse, fallback: String = "") throws {
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard !(200..<300).contains(status) else { return }
        if status == 401 || status == 403 { Session.shared.reportUnauthorized() }
        let body = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        throw APIError(message: body.isEmpty ? (fallback.isEmpty ? "HTTP \(status)" : fallback) : body, status: status)
    }

    /// Run a command and return its raw body.
    private static func raw(_ name: String, _ args: [String: Any?]) async throws -> Data {
        var request = try makeRequest("/api/cmd/\(name)")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: args.compactMapValues { $0 })
        let (data, response) = try await URLSession.shared.data(for: request)
        try check(data, response)
        return data
    }

    static func cmd<T: Decodable>(_ name: String, _ args: [String: Any?] = [:], as type: T.Type = T.self) async throws -> T {
        try decoder.decode(T.self, from: try await raw(name, args))
    }

    static func run(_ name: String, _ args: [String: Any?] = [:]) async throws {
        _ = try await raw(name, args)
    }

    // MARK: Bots

    static func listBots() async throws -> [Bot] { try await cmd("agent_list_bots") }

    /// Which bot should hear `text`.
    static func route(_ text: String) async throws -> String {
        struct Answer: Decodable { let bot: String }
        return try await cmd("agent_route", ["text": text], as: Answer.self).bot
    }

    static func deleteBot(_ bot: String) async throws { try await run("agent_delete_bot", ["bot": bot]) }

    /// Describe a bot; the platform designs, saves and equips it.
    static func designBot(_ brief: String) async throws -> String {
        struct Made: Decodable { let bot: String }
        return try await cmd("agent_design_bot", ["brief": brief], as: Made.self).bot
    }

    // MARK: Connectors

    static func connectors() async throws -> [Connector] { try await cmd("agent_connectors") }

    static func connect(kind: String, bot: String, settings: [String: String]) async throws -> String {
        struct Linked: Decodable { let account: String? }
        return try await cmd("agent_connect", ["name": kind, "bot": bot, "settings": settings], as: Linked.self).account ?? ""
    }

    static func disconnect(kind: String) async throws { try await run("agent_disconnect", ["name": kind]) }

    // MARK: Skills

    static func skillMarket(_ bot: String) async throws -> SkillMarket { try await cmd("agent_skill_market", ["bot": bot]) }
    static func installSkill(_ bot: String, _ id: String) async throws { try await run("agent_skill_install", ["bot": bot, "id": id]) }
    static func uninstallSkill(_ bot: String, _ id: String) async throws { try await run("agent_skill_uninstall", ["bot": bot, "id": id]) }

    // MARK: Conversation

    /// The conversation's newest `limit` items plus the event cursor.
    static func subscribe(_ bot: String, limit: Int) async throws -> SessionSnapshot {
        try await cmd("agent_subscribe", ["bot": bot, "limit": limit])
    }

    /// Up to `limit` items before index `before`, for scrolling up.
    static func history(_ bot: String, before: Int, limit: Int) async throws -> HistoryPage {
        try await cmd("agent_history", ["bot": bot, "before": before, "limit": limit])
    }

    /// Carries the phone's clock and timezone, so the bot knows when (in the user's time) it was said.
    static func send(_ bot: String, text: String, images: [String]) async throws {
        try await run("agent_send", [
            "bot": bot,
            "text": text,
            "images": images,
            "sentAt": Int64(Date().timeIntervalSince1970 * 1000),
            "tzOffset": TimeZone.current.secondsFromGMT() / 60,
        ])
    }

    /// Take back the latest message; returns its text to edit.
    static func recall(_ bot: String) async throws -> String {
        struct Recalled: Decodable { let text: String }
        return try await cmd("agent_recall", ["bot": bot], as: Recalled.self).text
    }

    static func cancel(_ bot: String) async throws { try await run("agent_cancel", ["bot": bot]) }

    // MARK: Task pool (every mutation returns the whole list)

    static func poolList() async throws -> [PoolTask] { try await cmd("agent_pool_list") }
    static func poolAccept(_ id: String) async throws -> [PoolTask] { try await cmd("agent_pool_accept", ["id": id]) }
    static func poolNote(_ id: String, _ note: String) async throws -> [PoolTask] { try await cmd("agent_pool_note", ["id": id, "note": note]) }
    static func poolRetry(_ id: String) async throws -> [PoolTask] { try await cmd("agent_pool_retry", ["id": id]) }
    static func poolCancel(_ id: String) async throws -> [PoolTask] { try await cmd("agent_pool_cancel", ["id": id]) }
    static func poolRemove(_ id: String) async throws -> [PoolTask] { try await cmd("agent_pool_remove", ["id": id]) }

    // MARK: Media library

    /// A text document from the library (an HTML page to render).
    static func assetText(_ bot: String, _ id: String) async throws -> String {
        struct Text: Decodable { let text: String }
        return try await cmd("agent_asset_text", ["bot": bot, "id": id], as: Text.self).text
    }

    /// A URL that loads an artifact without a header (minted per request).
    static func assetURL(_ bot: String, _ id: String) async throws -> URL {
        let data = try await raw("agent_asset_url", ["bot": bot, "id": id])
        guard let path = try JSONSerialization.jsonObject(with: data, options: .fragmentsAllowed) as? String,
              let url = Session.shared.url(path)
        else { throw APIError(message: "文件地址无效", status: 0) }
        return url
    }

    /// Put a file into the media library; the entry's id is what a message references.
    static func upload(bot: String, name: String, mime: String, data: Data) async throws -> LibraryEntry {
        var parts = URLComponents()
        parts.path = "/api/upload"
        parts.queryItems = [
            URLQueryItem(name: "bot", value: bot),
            URLQueryItem(name: "name", value: name),
            URLQueryItem(name: "mime", value: mime),
        ]
        var request = try makeRequest(parts.string ?? "/api/upload")
        request.httpBody = data
        let (body, response) = try await URLSession.shared.data(for: request)
        try check(body, response)
        return try decoder.decode(LibraryEntry.self, from: body)
    }
}
