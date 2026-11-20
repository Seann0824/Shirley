import Foundation
import Observation

/// The bot list plus this device's pin / mute choices (`state/useBots.ts`). Pin and mute are not
/// server state on purpose.
@MainActor
@Observable
final class BotsStore {
    private(set) var bots: [Bot] = []
    private(set) var error: String?
    private(set) var pinned: [String]
    private(set) var muted: [String]

    private static let prefsKey = "shirley.botPrefs"

    private struct Prefs: Codable {
        var pinned: [String]
        var muted: [String]
    }

    init() {
        let saved = UserDefaults.standard.data(forKey: Self.prefsKey).flatMap { try? JSONDecoder().decode(Prefs.self, from: $0) }
        pinned = saved?.pinned ?? []
        muted = saved?.muted ?? []
    }

    /// Pinned first; otherwise the server's order.
    var visible: [Bot] {
        bots.filter { pinned.contains($0.name) } + bots.filter { !pinned.contains($0.name) }
    }

    func refresh() async {
        do {
            bots = try await API.listBots()
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    func togglePin(_ name: String) {
        pinned = Self.toggle(pinned, name)
        save()
    }

    func toggleMute(_ name: String) {
        muted = Self.toggle(muted, name)
        save()
    }

    /// Delete the bot on the server, then forget it here too.
    func remove(_ name: String) async throws {
        try await API.deleteBot(name)
        pinned.removeAll { $0 == name }
        muted.removeAll { $0 == name }
        save()
        await refresh()
    }

    private static func toggle(_ list: [String], _ name: String) -> [String] {
        list.contains(name) ? list.filter { $0 != name } : list + [name]
    }

    private func save() {
        if let data = try? JSONEncoder().encode(Prefs(pinned: pinned, muted: muted)) {
            UserDefaults.standard.set(data, forKey: Self.prefsKey)
        }
    }
}

/// The platform task pool (`state/useTasks.ts`): polled every 5 s while the app is open; every
/// mutation returns the whole list.
@MainActor
@Observable
final class TasksStore {
    private(set) var tasks: [PoolTask] = []
    private(set) var loaded = false
    private(set) var error: String?

    func refresh() async {
        await settle { try await API.poolList() }
    }

    /// Poll until the calling task is cancelled.
    func poll() async {
        while !Task.isCancelled {
            await refresh()
            try? await Task.sleep(nanoseconds: 5_000_000_000)
        }
    }

    private func settle(_ call: () async throws -> [PoolTask]) async {
        do {
            tasks = try await call()
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
        loaded = true
    }

    func accept(_ id: String) { Task { await settle { try await API.poolAccept(id) } } }
    func note(_ id: String, _ text: String) { Task { await settle { try await API.poolNote(id, text) } } }
    func retry(_ id: String) { Task { await settle { try await API.poolRetry(id) } } }
    func cancel(_ id: String) { Task { await settle { try await API.poolCancel(id) } } }
    func remove(_ id: String) { Task { await settle { try await API.poolRemove(id) } } }

    func task(_ id: String) -> PoolTask? { tasks.first { $0.id == id } }
}
