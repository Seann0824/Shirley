import SwiftUI
import UIKit

/// Describe a bot and the platform makes it (`views/NewBot.tsx`); its conversation opens when
/// it is ready.
struct NewBotView: View {
    @Environment(AppModel.self) private var model
    @State private var brief: String?
    @State private var error: String?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                BackHeader(title: "新 Bot") { model.back() }
                VStack(alignment: .leading, spacing: 16) {
                    HStack(alignment: .bottom, spacing: 8) {
                        Icon(.bot, size: 17)
                            .foregroundStyle(.white)
                            .frame(width: 29, height: 29)
                            .background(Palette.label, in: Circle())
                        Text("告诉我你需要一个怎样的 Bot。我会替你定好它的名字、性格，并装上它需要的技能。")
                            .font(.px(14))
                            .lineSpacing(Markdown.spacing(14, 1.45))
                            .foregroundStyle(Palette.label)
                            .padding(.vertical, 11)
                            .padding(.horizontal, 13)
                            .background(Palette.bubble, in: Corners.bot)
                            .containerRelativeFrame(.horizontal, alignment: .leading) { length, _ in length * 0.78 }
                        Spacer(minLength: 0)
                    }
                    if let brief {
                        HStack {
                            Spacer(minLength: 0)
                            Text(brief)
                                .font(.px(14))
                                .lineSpacing(Markdown.spacing(14, 1.45))
                                .foregroundStyle(.white)
                                .padding(.vertical, 11)
                                .padding(.horizontal, 13)
                                .background(Color.black, in: Corners.user)
                        }
                        RoutingNote(text: "正在创建…")
                    }
                    if let error { NoteView(text: error) }
                }
                .padding(.top, 17)
                .padding(.horizontal, 2)
            }
            .padding(.horizontal, 17)
        }
        .scrollIndicators(.hidden)
        .background(Palette.page)
        .safeAreaInset(edge: .bottom, spacing: 0) {
            // The same composer as home and chat: talk or type the description.
            ComposerDock(placeholder: "例如：帮我跟踪竞品并整理周报") { text, _ in create(text) }
        }
        .toolbar(.hidden, for: .navigationBar)
    }

    private func create(_ text: String) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, brief == nil else { return }
        brief = trimmed
        error = nil
        Task {
            do {
                let made = try await API.designBot(trimmed)
                await model.bots.refresh()
                model.openBot(made)
            } catch {
                self.error = error.localizedDescription
                brief = nil
            }
        }
    }
}

/// One bot's skills: what it has, and the market to add more from (`views/Skills.tsx`).
struct SkillsView: View {
    let bot: Bot
    @Environment(AppModel.self) private var model
    @State private var data: SkillMarket?
    @State private var error: String?
    /// Skill id -> what is happening to it right now.
    @State private var busy: [String: Bool] = [:]

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                BackHeader(title: "\(bot.displayName) 的技能") { model.back() }
                if let error { NoteView(text: error) }
                if data == nil && error == nil { LoadingRows() }
                if let data { content(data).transition(.opacity.combined(with: .offset(y: 5))) }
            }
            .padding(.horizontal, 17)
            .animation(.easeOut(duration: 0.28), value: data == nil)
        }
        .scrollIndicators(.hidden)
        .background(Palette.page)
        .toolbar(.hidden, for: .navigationBar)
        .task { await load() }
    }

    private func content(_ data: SkillMarket) -> some View {
        let installed = Set(data.installed.map(\.id))
        let titles = Dictionary(data.market.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        let offered = data.market.filter { !installed.contains($0.id) }
        var categories: [String] = []
        for skill in offered where !categories.contains(skill.category.isEmpty ? "其他" : skill.category) {
            categories.append(skill.category.isEmpty ? "其他" : skill.category)
        }
        return VStack(alignment: .leading, spacing: 0) {
            heading("已安装")
            if data.installed.isEmpty { NoteView(text: "还没有技能。从下面的市场里装一个。", error: false) }
            ForEach(data.installed) { skill in
                let market = skill.market.flatMap { titles[$0] }
                SkillRow(icon: .spark, title: market?.title ?? skill.id, summary: market?.summary ?? skill.description,
                         note: skill.market == nil ? "Bot 自己写的" : nil,
                         action: busy[skill.id] == true ? "移除中…" : "移除", quiet: true, busy: busy[skill.id] == true) {
                    Task { await act(skill.id, install: false) }
                }
            }
            ForEach(categories, id: \.self) { category in
                heading(category)
                ForEach(offered.filter { ($0.category.isEmpty ? "其他" : $0.category) == category }) { skill in
                    SkillRow(icon: .box, title: skill.title, summary: skill.summary, note: nil,
                             action: busy[skill.id] == true ? "安装中…" : "安装", quiet: false, busy: busy[skill.id] == true) {
                        Task { await act(skill.id, install: true) }
                    }
                }
            }
            Text("技能来自公开仓库（如 anthropics/skills），装到这个 Bot 里，只有它能用。")
                .font(.px(12))
                .foregroundStyle(Palette.tertiary)
                .padding(.top, 16)
                .padding(.horizontal, 4)
        }
        .padding(.horizontal, 4)
        .padding(.bottom, 24)
    }

    private func heading(_ text: String) -> some View {
        Text(text)
            .font(.px(13, .semibold))
            .foregroundStyle(Palette.secondary)
            .padding(.top, 18)
            .padding(.bottom, 8)
            .padding(.horizontal, 4)
    }

    private func load() async {
        do {
            data = try await API.skillMarket(bot.name)
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func act(_ id: String, install: Bool) async {
        busy[id] = true
        error = nil
        do {
            if install { try await API.installSkill(bot.name, id) } else { try await API.uninstallSkill(bot.name, id) }
            await load()
        } catch {
            self.error = error.localizedDescription
        }
        busy[id] = nil
    }
}

private struct SkillRow: View {
    let icon: IconName
    let title: String
    let summary: String
    let note: String?
    let action: String
    let quiet: Bool
    let busy: Bool
    let run: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            Icon(icon, size: 17)
                .foregroundStyle(Palette.label)
                .frame(width: 36, height: 36)
                .background(Palette.fill, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.px(15)).foregroundStyle(Palette.label)
                Text(summary)
                    .font(.px(13))
                    .lineSpacing(Markdown.spacing(13, 1.4))
                    .foregroundStyle(Palette.secondary)
                    .lineLimit(2)
                if let note { Text(note).font(.px(11)).foregroundStyle(Palette.tertiary) }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            PillButton(title: action, primary: !quiet, action: run)
                .disabled(busy)
                .opacity(busy ? 0.55 : 1)
        }
        .padding(.vertical, 12)
        .padding(.horizontal, 14)
        .background(Palette.glassStrong, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
        .padding(.bottom, 8)
    }
}

/// What each connection brings, in one line.
private let connectorSummary: [String: String] = [
    "github": "提到你、分配给你、请你审查的 issue 和 PR，交给 bot 处理，并可直接评论回复",
    "feishu": "私聊机器人或在群里 @ 它的消息，交给 bot 处理并在飞书里回复",
    "slack": "@ 提及和私信，交给 bot 处理并在线程里回复",
    "webhook": "任何能发 HTTP 请求的系统（CI、监控告警、脚本）都能把事情交给 bot",
]

private let connectorIcons: [String: IconName] = ["github": .code, "feishu": .chat, "slack": .chat, "webhook": .plug]

/// Connected services: where bots get work from besides this chat (`views/Connections.tsx`).
struct ConnectionsView: View {
    @Environment(AppModel.self) private var model
    @State private var list: [Connector]?
    @State private var error: String?
    @State private var open: String?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                BackHeader(title: "连接") { model.back() }
                if let error { NoteView(text: error) }
                if list == nil && error == nil { LoadingRows() }
                if let list {
                    VStack(alignment: .leading, spacing: 0) {
                        Text("连上之后，那里发生的事会交给你选的 bot，按它的角色在后台处理，不占用你们的对话。")
                            .font(.px(13))
                            .lineSpacing(Markdown.spacing(13, 1.5))
                            .foregroundStyle(Palette.secondary)
                            .padding(.top, 6)
                            .padding(.bottom, 14)
                            .padding(.horizontal, 4)
                        ForEach(list) { connector in
                            ConnectionRow(connector: connector, open: open == connector.kind) {
                                withAnimation(.ease(0.24)) { open = open == connector.kind ? nil : connector.kind }
                            } reload: {
                                Task { await load() }
                            } closed: {
                                open = nil
                                Task { await load() }
                            }
                        }
                    }
                    .padding(.horizontal, 4)
                    .padding(.bottom, 32)
                }
            }
            .padding(.horizontal, 17)
        }
        .scrollIndicators(.hidden)
        .scrollDismissesKeyboard(.interactively)
        .background(Palette.page)
        .toolbar(.hidden, for: .navigationBar)
        .task {
            // Status changes on its own (connecting -> connected, events counted).
            while !Task.isCancelled {
                await load()
                try? await Task.sleep(nanoseconds: 5_000_000_000)
            }
        }
    }

    private func load() async {
        do {
            list = try await API.connectors()
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }
}

private struct ConnectionRow: View {
    let connector: Connector
    let open: Bool
    let toggle: () -> Void
    let reload: () -> Void
    let closed: () -> Void
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 12) {
                Icon(connectorIcons[connector.kind] ?? .plug, size: 18)
                    .foregroundStyle(connector.connected ? .white : Palette.label)
                    .frame(width: 38, height: 38)
                    .background(connector.connected ? Palette.label : Palette.fill,
                                in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                VStack(alignment: .leading, spacing: 0) {
                    HStack(spacing: 8) {
                        Text(connector.label).font(.px(15, .bold)).foregroundStyle(Palette.label)
                        if connector.connected { stateBadge }
                    }
                    if let summary = connectorSummary[connector.kind] {
                        Text(summary)
                            .font(.px(13))
                            .lineSpacing(Markdown.spacing(13, 1.45))
                            .foregroundStyle(Palette.secondary)
                            .padding(.top, 3)
                    }
                    if connector.connected, let bot = connector.bot {
                        Text("由 \(model.bot(bot)?.displayName ?? bot) 处理").font(.px(12)).foregroundStyle(Palette.tertiary).padding(.top, 4)
                    }
                    if connector.status.state == "error", let detail = connector.status.detail {
                        Text(detail).font(.px(12)).foregroundStyle(Palette.red).padding(.top, 4)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                PillButton(title: open ? "收起" : connector.connected ? "管理" : "连接",
                           primary: !connector.connected, horizontal: 16, vertical: 7, action: toggle)
            }
            if open {
                ConnectForm(connector: connector, onDone: reload, onClosed: closed)
                    .transition(.opacity)
            }
        }
        .padding(14)
        .background(Palette.glassStrong, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
        .padding(.bottom, 10)
    }

    private var stateBadge: some View {
        let state = connector.status.state
        let text: String = {
            switch state {
            case "connected": connector.status.events > 0 ? "已连接 · 处理了 \(connector.status.events) 件" : "已连接"
            case "connecting": "连接中…"
            case "error": "连接出错，自动重试中"
            default: "已保存"
            }
        }()
        let colors: (Color, Color) = state == "connected" ? (Color(hex: 0x248A3D), Color.rgba(52, 199, 89, 0.12))
            : state == "error" ? (Palette.red, Palette.red.opacity(0.1)) : (Palette.secondary, Palette.fill)
        return Text(text)
            .font(.px(11, .semibold))
            .foregroundStyle(colors.0)
            .padding(.vertical, 1)
            .padding(.horizontal, 8)
            .background(colors.1, in: Capsule())
    }
}

private struct ConnectForm: View {
    let connector: Connector
    let onDone: () -> Void
    let onClosed: () -> Void
    @Environment(AppModel.self) private var model
    @State private var values: [String: String] = [:]
    @State private var bot = ""
    @State private var busy = false
    @State private var message: (ok: Bool, text: String)?
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(connector.fields) { field in
                VStack(alignment: .leading, spacing: 5) {
                    Text(field.label).font(.px(13, .semibold)).foregroundStyle(Palette.label)
                    Group {
                        if field.secret {
                            SecureField(connector.connected ? "已保存，留空则不变" : "", text: binding(field.key))
                        } else {
                            TextField("", text: binding(field.key))
                        }
                    }
                    .font(.px(16))
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .padding(.horizontal, 12)
                    .frame(height: 40)
                    .background(.white, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                    .hairline(Color.rgba(60, 60, 67, 0.2), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                    Text(field.help).font(.px(12)).lineSpacing(Markdown.spacing(12, 1.4)).foregroundStyle(Palette.tertiary)
                }
            }
            VStack(alignment: .leading, spacing: 5) {
                Text("交给哪个 bot 处理").font(.px(13, .semibold)).foregroundStyle(Palette.label)
                Menu {
                    Picker("交给哪个 bot 处理", selection: $bot) {
                        ForEach(model.bots.bots) { Text($0.displayName).tag($0.name) }
                    }
                } label: {
                    HStack {
                        Text(model.bot(bot)?.displayName ?? bot).font(.px(14)).foregroundStyle(Palette.label)
                        Spacer()
                        Image(systemName: "chevron.up.chevron.down").font(.px(12)).foregroundStyle(Palette.secondary)
                    }
                    .padding(.horizontal, 12)
                    .frame(height: 40)
                    .background(.white, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                    .hairline(Color.rgba(60, 60, 67, 0.2), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                }
            }
            if let path = connector.webhookPath, let url = Session.shared.url(path)?.absoluteString {
                VStack(alignment: .leading, spacing: 6) {
                    Text("把事件 POST 到这个地址（地址本身就是密钥，别公开）：")
                    Text(url)
                        .font(.mono(12))
                        .foregroundStyle(Palette.label)
                        .textSelection(.enabled)
                        .padding(.vertical, 8)
                        .padding(.horizontal, 10)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Palette.fill, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                    PillButton(title: copied ? "已复制" : "复制", font: .px(12, .semibold), horizontal: 12, vertical: 5) {
                        UIPasteboard.general.string = url
                        copied = true
                    }
                }
                .font(.px(12))
                .foregroundStyle(Palette.secondary)
            }
            if let message {
                Text(message.text).font(.px(13)).foregroundStyle(message.ok ? Color(hex: 0x248A3D) : Palette.red)
            }
            HStack(spacing: 8) {
                PillButton(title: busy ? "检查中…" : connector.connected ? "保存" : "连接", primary: true,
                           horizontal: 16, vertical: 7) { Task { await connect() } }
                    .disabled(busy)
                    .opacity(busy ? 0.55 : 1)
                if connector.connected {
                    PillButton(title: "断开", horizontal: 16, vertical: 7) { Task { await disconnect() } }
                        .disabled(busy)
                        .opacity(busy ? 0.55 : 1)
                }
            }
        }
        .padding(.top, 14)
        .overlay(alignment: .top) { Rectangle().fill(Color.rgba(60, 60, 67, 0.15)).frame(height: 0.5) }
        .padding(.top, 14)
        .onAppear {
            values = connector.values ?? [:]
            bot = connector.bot ?? model.bots.bots.first?.name ?? AppModel.defaultBot
        }
    }

    private func binding(_ key: String) -> Binding<String> {
        Binding(get: { values[key] ?? "" }, set: { values[key] = $0 })
    }

    private func connect() async {
        busy = true
        message = nil
        do {
            let account = try await API.connect(kind: connector.kind, bot: bot, settings: values)
            message = (true, account.isEmpty ? "已连接" : "已连接：\(account)")
            onDone()
        } catch {
            message = (false, error.localizedDescription)
        }
        busy = false
    }

    private func disconnect() async {
        busy = true
        do {
            try await API.disconnect(kind: connector.kind)
            onClosed()
        } catch {
            message = (false, error.localizedDescription)
            busy = false
        }
    }
}
