import Foundation

/// The one shared `GET /api/events` Server-Sent-Events stream: many listeners, reconnect with
/// backoff, and a `resync` to every listener after a gap (as `api/bridge.ts` does).
@MainActor
final class EventHub {
    static let shared = EventHub()

    private var listeners: [UUID: (AgentEvent) -> Void] = [:]
    private var connectionListeners: [UUID: (Bool) -> Void] = [:]
    private(set) var online = true
    private var loop: Task<Void, Never>?
    /// A stream has been open before: the next one follows a gap, so listeners resync.
    private var opened = false
    private var backgroundedAt: Date?
    private let decoder = JSONDecoder()

    /// Keep-alives arrive every 15 s; two missed in a row mean the connection is gone.
    private let session: URLSession = {
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 35
        config.timeoutIntervalForResource = 7 * 24 * 3600
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        return URLSession(configuration: config)
    }()

    func add(_ listener: @escaping (AgentEvent) -> Void) -> UUID {
        let id = UUID()
        listeners[id] = listener
        connect()
        return id
    }

    func remove(_ id: UUID) {
        listeners[id] = nil
    }

    /// Whether the stream is connected: a dropped one is the usual reason a turn looks dead.
    func watch(_ listener: @escaping (Bool) -> Void) -> UUID {
        let id = UUID()
        connectionListeners[id] = listener
        listener(online)
        return id
    }

    func unwatch(_ id: UUID) {
        connectionListeners[id] = nil
    }

    /// Start the stream if it is not running.
    func connect() {
        guard loop == nil else { return }
        loop = Task { await run() }
    }

    /// A new address or token: drop the old stream and start over.
    func restart() {
        loop?.cancel()
        loop = nil
        connect()
    }

    /// Going to the background: the system will suspend the app and usually cut the stream
    /// without telling it.
    func sleep() {
        backgroundedAt = Date()
    }

    /// Back in the foreground: a stream that may have died while suspended is replaced now,
    /// not after a timeout, and every listener resyncs for what it missed meanwhile.
    func wake() {
        let away = backgroundedAt.map { Date().timeIntervalSince($0) } ?? 0
        backgroundedAt = nil
        if !online || away > 3 { restart() }
    }

    private func setOnline(_ value: Bool) {
        guard online != value else { return }
        online = value
        connectionListeners.values.forEach { $0(value) }
    }

    private func emit(_ event: AgentEvent) {
        listeners.values.forEach { $0(event) }
    }

    private func frame(_ name: String, _ data: String) {
        switch name {
        case "agent_event":
            // A malformed frame is dropped; the next one is independent.
            if let event = try? decoder.decode(AgentEvent.self, from: Data(data.utf8)) { emit(event) }
        case "resync":
            // The server says this client fell behind; what it missed is only in a fresh snapshot.
            emit(.resync)
        default:
            break
        }
    }

    private func run() async {
        var delay: Double = 1
        while !Task.isCancelled {
            guard let url = Session.shared.url("/api/events"), let token = Session.shared.token else {
                // Nothing to connect to until the gate is answered (`restart` follows).
                loop = nil
                return
            }
            var request = URLRequest(url: url)
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
            request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
            do {
                let (bytes, response) = try await session.bytes(for: request)
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                if status == 401 || status == 403 {
                    Session.shared.reportUnauthorized()
                    setOnline(false)
                    loop = nil
                    return
                }
                guard status == 200 else { throw URLError(.badServerResponse) }
                setOnline(true)
                delay = 1
                // A reconnect means a gap in the stream.
                if opened { emit(.resync) }
                opened = true
                try await Self.pump(bytes) { [weak self] name, data in
                    await self?.frame(name, data)
                }
            } catch {
                if Task.isCancelled { return }
            }
            setOnline(false)
            try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            delay = min(delay * 2, 30)
        }
    }

    /// Read SSE frames off the main thread. Lines are split by hand: `AsyncBytes.lines` drops
    /// the blank line that ends a frame.
    nonisolated private static func pump(
        _ bytes: URLSession.AsyncBytes,
        _ deliver: (String, String) async -> Void
    ) async throws {
        var line: [UInt8] = []
        var name = "message"
        var data = ""
        for try await byte in bytes {
            guard byte == 0x0A else {
                line.append(byte)
                continue
            }
            var text = String(decoding: line, as: UTF8.self)
            line.removeAll(keepingCapacity: true)
            if text.hasSuffix("\r") { text.removeLast() }
            if text.isEmpty {
                if !data.isEmpty { await deliver(name, data) }
                name = "message"
                data = ""
            } else if text.hasPrefix(":") {
                continue
            } else if text.hasPrefix("event:") {
                name = text.dropFirst(6).trimmingCharacters(in: .whitespaces)
            } else if text.hasPrefix("data:") {
                var value = String(text.dropFirst(5))
                if value.hasPrefix(" ") { value.removeFirst() }
                data = data.isEmpty ? value : data + "\n" + value
            }
        }
    }
}
