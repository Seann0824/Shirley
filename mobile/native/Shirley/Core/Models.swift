import Foundation

// Wire contract with the Shirley server. Mirrors the web UI's `api/types.ts` (which mirrors
// `interface/wire.rs`, `interface/runtime/mod.rs` and `pool/mod.rs`); keep all three in sync.

struct Bot: Decodable, Identifiable, Hashable {
    let name: String
    let displayName: String
    let tools: [String]
    let skills: [String]
    let persona: String?

    var id: String { name }

    enum CodingKeys: String, CodingKey {
        case name, tools, skills, persona
        case displayName = "display_name"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        displayName = try c.decodeIfPresent(String.self, forKey: .displayName) ?? name
        tools = try c.decodeIfPresent([String].self, forKey: .tools) ?? []
        skills = try c.decodeIfPresent([String].self, forKey: .skills) ?? []
        persona = try c.decodeIfPresent(String.self, forKey: .persona)
    }
}

enum TaskStatus: String, Decodable {
    case backlog, toDo = "to_do", inProgress = "in_progress", inReview = "in_review", blocked, done, cancelled

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = TaskStatus(rawValue: raw) ?? .backlog
    }

    var label: String {
        switch self {
        case .backlog: "已搁置"
        case .toDo: "排队中"
        case .inProgress: "进行中"
        case .inReview: "做好了"
        case .blocked: "没做成"
        case .done: "已完成"
        case .cancelled: "已取消"
        }
    }

    var working: Bool { self == .inProgress || self == .toDo }
}

struct DeliveryFile: Decodable, Hashable, Identifiable {
    let id: String
    let name: String
    let mime: String
    let bytes: Int

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? "文件"
        mime = try c.decodeIfPresent(String.self, forKey: .mime) ?? ""
        bytes = try c.decodeIfPresent(Int.self, forKey: .bytes) ?? 0
    }

    enum CodingKeys: String, CodingKey { case id, name, mime, bytes }
}

struct Delivery: Decodable, Hashable {
    let summary: String
    let files: [DeliveryFile]
    let error: String?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        summary = try c.decodeIfPresent(String.self, forKey: .summary) ?? ""
        files = try c.decodeIfPresent([DeliveryFile].self, forKey: .files) ?? []
        error = try c.decodeIfPresent(String.self, forKey: .error)
    }

    enum CodingKeys: String, CodingKey { case summary, files, error }
}

struct PoolTask: Decodable, Identifiable, Hashable {
    let id: String
    let bot: String
    let title: String
    let brief: String
    let status: TaskStatus
    let attempt: Int
    let createdMs: Double
    let startedMs: Double?
    let finishedMs: Double?
    let delivery: Delivery?
    let note: String?
    let pendingNotes: [String]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        bot = try c.decode(String.self, forKey: .bot)
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        brief = try c.decodeIfPresent(String.self, forKey: .brief) ?? ""
        status = try c.decode(TaskStatus.self, forKey: .status)
        attempt = try c.decodeIfPresent(Int.self, forKey: .attempt) ?? 1
        createdMs = try c.decodeIfPresent(Double.self, forKey: .createdMs) ?? 0
        startedMs = try c.decodeIfPresent(Double.self, forKey: .startedMs)
        finishedMs = try c.decodeIfPresent(Double.self, forKey: .finishedMs)
        delivery = try c.decodeIfPresent(Delivery.self, forKey: .delivery)
        note = try c.decodeIfPresent(String.self, forKey: .note)
        pendingNotes = try c.decodeIfPresent([String].self, forKey: .pendingNotes) ?? []
    }

    enum CodingKeys: String, CodingKey {
        case id, bot, title, brief, status, attempt, createdMs, startedMs, finishedMs, delivery, note, pendingNotes
    }
}

/// A media-library entry (`artifact::LibraryEntry`).
struct LibraryEntry: Decodable {
    let id: String
    let name: String
    let mime: String
    let bytes: Int
}

enum AssetSource: Decodable, Hashable {
    case artifact(String)
    case url(String)

    enum CodingKeys: String, CodingKey { case kind, id, url }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        if try c.decode(String.self, forKey: .kind) == "artifact" {
            self = .artifact(try c.decode(String.self, forKey: .id))
        } else {
            self = .url(try c.decodeIfPresent(String.self, forKey: .url) ?? "")
        }
    }
}

enum RenderPart: Decodable, Hashable {
    case text(String)
    case image(source: AssetSource, alt: String, mime: String?)
    case file(source: AssetSource?, name: String, mime: String?, bytes: Int?)
    case link(url: String, title: String)
    /// `data` parts and anything newer than this app: not shown.
    case other

    enum CodingKeys: String, CodingKey { case kind, text, source, alt, mime, name, path, bytes, url, title }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "text":
            self = .text(try c.decodeIfPresent(String.self, forKey: .text) ?? "")
        case "image":
            self = .image(
                source: try c.decode(AssetSource.self, forKey: .source),
                alt: try c.decodeIfPresent(String.self, forKey: .alt) ?? "",
                mime: try c.decodeIfPresent(String.self, forKey: .mime)
            )
        case "file":
            self = .file(
                source: try c.decodeIfPresent(AssetSource.self, forKey: .source),
                name: try c.decodeIfPresent(String.self, forKey: .name) ?? "文件",
                mime: try c.decodeIfPresent(String.self, forKey: .mime),
                bytes: try c.decodeIfPresent(Int.self, forKey: .bytes)
            )
        case "link":
            self = .link(
                url: try c.decodeIfPresent(String.self, forKey: .url) ?? "",
                title: try c.decodeIfPresent(String.self, forKey: .title) ?? ""
            )
        default:
            self = .other
        }
    }

    var isText: Bool {
        if case .text = self { return true }
        return false
    }

    var isImage: Bool {
        if case .image = self { return true }
        return false
    }

    /// Shown at all: text is rendered from `content`, `data` is not for people.
    var isMedia: Bool {
        switch self {
        case .image, .file, .link: true
        default: false
        }
    }
}

struct ToolCall: Decodable, Hashable, Identifiable {
    let id: String
    let name: String
    let arguments: String
    var output: String?
    var ok: Bool?
    var parts: [RenderPart]

    init(id: String, name: String, arguments: String) {
        self.id = id
        self.name = name
        self.arguments = arguments
        output = nil
        ok = nil
        parts = []
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = try c.decode(String.self, forKey: .name)
        arguments = try c.decodeIfPresent(String.self, forKey: .arguments) ?? ""
        output = try c.decodeIfPresent(String.self, forKey: .output)
        ok = try c.decodeIfPresent(Bool.self, forKey: .ok)
        parts = try c.decodeIfPresent([RenderPart].self, forKey: .parts) ?? []
    }

    enum CodingKeys: String, CodingKey { case id, name, arguments, output, ok, parts }

    /// A string field of the call's JSON output or arguments, if it parses.
    static func field(_ json: String?, _ key: String) -> String? {
        guard let data = json?.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }
        return object[key] as? String
    }
}

/// One entry of a conversation as the server sends it (`ItemWire`), plus the client-only flags.
struct ConversationItem: Decodable, Hashable {
    enum Kind: String { case message, compact, tools }

    var kind: Kind
    var role = ""
    var content = ""
    var parts: [RenderPart] = []
    /// A reply still receiving deltas.
    var streaming = false
    var calls: [ToolCall] = []
    /// Restored from the log, or followed by something: its calls are over.
    var settled = false

    init(kind: Kind, role: String = "", content: String = "", parts: [RenderPart] = [], streaming: Bool = false) {
        self.kind = kind
        self.role = role
        self.content = content
        self.parts = parts
        self.streaming = streaming
    }

    static func tools(_ calls: [ToolCall]) -> ConversationItem {
        var item = ConversationItem(kind: .tools)
        item.calls = calls
        return item
    }

    enum CodingKeys: String, CodingKey { case kind, role, content, parts, calls }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        kind = Kind(rawValue: try c.decode(String.self, forKey: .kind)) ?? .message
        role = try c.decodeIfPresent(String.self, forKey: .role) ?? ""
        content = try c.decodeIfPresent(String.self, forKey: .content) ?? ""
        parts = try c.decodeIfPresent([RenderPart].self, forKey: .parts) ?? []
        calls = try c.decodeIfPresent([ToolCall].self, forKey: .calls) ?? []
    }
}

struct SessionSnapshot: Decodable {
    let items: [ConversationItem]
    let start: Int
    let running: Bool
    let sequence: Int
}

struct HistoryPage: Decodable {
    let items: [ConversationItem]
    let start: Int
}

/// One frame of `/api/events`. Flat on purpose: every event type reads the fields it has.
struct AgentEvent: Decodable {
    var type: String
    var bot: String?
    var session: String?
    var sequence: Int?
    var text: String?
    var role: String?
    var content: String?
    var callID: String?
    var name: String?
    var arguments: String?
    var ok: Bool?
    var output: String?
    var parts: [RenderPart]?
    var attempt: Int?
    var reason: String?
    var stopReason: String?
    var message: String?

    enum CodingKeys: String, CodingKey {
        case type, bot, session, sequence, text, role, content, name, arguments, ok, output, parts, attempt, reason, message
        case callID = "call_id"
        case stopReason = "stop_reason"
    }

    init(type: String) {
        self.type = type
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        type = try c.decode(String.self, forKey: .type)
        bot = try? c.decodeIfPresent(String.self, forKey: .bot)
        session = try? c.decodeIfPresent(String.self, forKey: .session)
        sequence = try? c.decodeIfPresent(Int.self, forKey: .sequence)
        text = try? c.decodeIfPresent(String.self, forKey: .text)
        role = try? c.decodeIfPresent(String.self, forKey: .role)
        content = try? c.decodeIfPresent(String.self, forKey: .content)
        callID = try? c.decodeIfPresent(String.self, forKey: .callID)
        name = try? c.decodeIfPresent(String.self, forKey: .name)
        arguments = try? c.decodeIfPresent(String.self, forKey: .arguments)
        ok = try? c.decodeIfPresent(Bool.self, forKey: .ok)
        output = try? c.decodeIfPresent(String.self, forKey: .output)
        parts = try? c.decodeIfPresent([RenderPart].self, forKey: .parts)
        attempt = try? c.decodeIfPresent(Int.self, forKey: .attempt)
        reason = try? c.decodeIfPresent(String.self, forKey: .reason)
        stopReason = try? c.decodeIfPresent(String.self, forKey: .stopReason)
        message = try? c.decodeIfPresent(String.self, forKey: .message)
    }

    /// Client-side: events may have been missed (lagged or reconnected stream); reload.
    static let resync = AgentEvent(type: "resync")
}

struct MarketSkill: Decodable, Identifiable {
    let id: String
    let title: String
    let summary: String
    let category: String
}

struct InstalledSkill: Decodable, Identifiable {
    let id: String
    let description: String
    let market: String?
}

struct SkillMarket: Decodable {
    let market: [MarketSkill]
    let installed: [InstalledSkill]
}

struct ConnectorField: Decodable, Identifiable {
    let key: String
    let label: String
    let help: String
    let secret: Bool
    var id: String { key }
}

struct ConnectorStatus: Decodable {
    let state: String
    let detail: String?
    let events: Int
}

struct Connector: Decodable, Identifiable {
    let kind: String
    let label: String
    let fields: [ConnectorField]
    let connected: Bool
    let bot: String?
    let values: [String: String]?
    let webhookPath: String?
    let status: ConnectorStatus

    var id: String { kind }

    enum CodingKeys: String, CodingKey {
        case kind, label, fields, connected, bot, values, status
        case webhookPath = "webhook_path"
    }
}
