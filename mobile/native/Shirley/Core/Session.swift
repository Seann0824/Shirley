import Foundation
import Observation
import Security

/// Why the access gate is up (`useAccessToken` in the web UI).
enum GateError {
    case invalid, unreachable, empty, address

    var message: String {
        switch self {
        case .invalid: "令牌无效，请重新输入"
        case .unreachable: "无法连接服务器，请检查网络后重试"
        case .empty: "请输入访问令牌"
        case .address: "请输入服务器地址"
        }
    }
}

/// Where the server is and the token that opens it. The web page gets its address from where it
/// was loaded; the app asks once, alongside the token, and remembers both.
@MainActor
@Observable
final class Session {
    static let shared = Session()

    private(set) var server: URL?
    private(set) var token: String?
    /// The gate is up: nothing can load until it is answered.
    var needed: Bool
    var busy = false
    var error: GateError?
    /// Bumped when a new address or token is taken: everything restarts, like the page reloading.
    private(set) var epoch = 0

    private static let serverKey = "shirley.server"

    private init() {
        server = UserDefaults.standard.string(forKey: Self.serverKey).flatMap(URL.init(string:))
        token = Keychain.read()
        needed = server == nil || token == nil
    }

    /// The gate may be dismissed only when the app already has somewhere to talk to.
    var configured: Bool { server != nil && token != nil }

    func url(_ path: String) -> URL? {
        guard let server else { return nil }
        return URL(string: path, relativeTo: server)?.absoluteURL
    }

    /// Any 401/403: the token is stale or mistyped, ask again.
    func reportUnauthorized() {
        needed = true
    }

    /// Reopen the gate to change the address or token.
    func reopen() {
        error = nil
        needed = true
    }

    func dismiss() {
        guard configured else { return }
        error = nil
        needed = false
    }

    /// `host:port`, `http://lan-ip:8787/` or a full https address: always a base URL with no path.
    static func normalize(_ raw: String) -> URL? {
        var text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return nil }
        if !text.contains("://") { text = "https://" + text }
        guard var parts = URLComponents(string: text), parts.host?.isEmpty == false else { return nil }
        parts.path = "/"
        parts.query = nil
        parts.fragment = nil
        return parts.url
    }

    /// Check the pair with the server; on success keep it and restart everything with it.
    func submit(address: String, token candidate: String) async {
        let token = candidate.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let base = Self.normalize(address) else {
            error = .address
            return
        }
        guard !token.isEmpty else {
            error = .empty
            return
        }
        busy = true
        error = nil
        let verdict = await Self.probe(base: base, token: token)
        busy = false
        switch verdict {
        case .ok:
            server = base
            self.token = token
            UserDefaults.standard.set(base.absoluteString, forKey: Self.serverKey)
            Keychain.save(token)
            needed = false
            epoch += 1
            EventHub.shared.restart()
        case .invalid:
            error = .invalid
        case .unreachable:
            error = .unreachable
        }
    }

    private enum Verdict { case ok, invalid, unreachable }

    /// Anything that needs the token, cheap and side-effect free: the bot list.
    private static func probe(base: URL, token: String) async -> Verdict {
        guard let url = URL(string: "/api/cmd/agent_list_bots", relativeTo: base) else { return .unreachable }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.httpBody = Data("{}".utf8)
        request.timeoutInterval = 15
        do {
            let (_, response) = try await URLSession.shared.data(for: request)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            if (200..<300).contains(status) { return .ok }
            return status == 401 || status == 403 ? .invalid : .unreachable
        } catch {
            return .unreachable
        }
    }
}

/// The token lives in the Keychain, not in preferences.
enum Keychain {
    private static let service = "dev.shirley.native"
    private static let account = "server-token"

    private static var query: [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: service,
         kSecAttrAccount as String: account]
    }

    static func read() -> String? {
        var request = query
        request[kSecReturnData as String] = true
        request[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: AnyObject?
        guard SecItemCopyMatching(request as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data,
              let text = String(data: data, encoding: .utf8),
              !text.isEmpty
        else { return nil }
        return text
    }

    static func save(_ token: String) {
        SecItemDelete(query as CFDictionary)
        var item = query
        item[kSecValueData as String] = Data(token.utf8)
        item[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
        SecItemAdd(item as CFDictionary, nil)
    }
}
