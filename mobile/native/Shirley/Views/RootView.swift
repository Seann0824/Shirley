import SwiftUI

/// The app (`App.tsx`): home and a stack of screens on a phone; on a wide screen the bot list
/// beside whatever is open, with "home" meaning the selected conversation.
struct RootView: View {
    @State private var model = AppModel()
    @Environment(\.scenePhase) private var phase
    private var session = Session.shared

    var body: some View {
        GeometryReader { geometry in
            let wide = geometry.size.width >= 900
            Group {
                if wide { twoPane } else { phone }
            }
            // A new address or token starts everything over, like the page reloading.
            .id(session.epoch)
            .onAppear { model.wide = wide }
            .onChange(of: wide) { _, now in model.wide = now }
        }
        .environment(model)
        .overlay {
            if session.needed {
                TokenGateView().transition(.opacity)
            }
        }
        .animation(.easeOut(duration: 0.2), value: session.needed)
        .fullScreenCover(item: $model.viewing) { file in
            FileViewerView(file: file)
        }
        .task(id: session.epoch) {
            model.path = []
            model.startWatching()
            EventHub.shared.connect()
            await model.bots.refresh()
        }
        .task(id: session.epoch) { await model.tasks.poll() }
        .onChange(of: phase) { _, now in
            if now == .active {
                model.becameActive()
                Task {
                    await model.tasks.refresh()
                    await model.bots.refresh()
                }
            } else {
                model.active = false
            }
        }
        // Back on the list: ask the server where every bot stands, in case an event went missing.
        .onChange(of: model.path.isEmpty) { _, home in
            if home { Task { await model.bots.refresh() } }
        }
        .preferredColorScheme(.light)
    }

    private var phone: some View {
        NavigationStack(path: $model.path) {
            HomeView()
                .navigationDestination(for: Route.self) { destination($0) }
        }
    }

    private var twoPane: some View {
        HStack(spacing: 0) {
            NavigationStack {
                HomeView()
            }
            .frame(width: 372)
            .overlay(alignment: .trailing) { Rectangle().fill(Palette.separator).frame(width: 1) }
            NavigationStack(path: $model.path) {
                Group {
                    if let bot = model.selectedBot {
                        ChatView(bot: bot).id(bot.name)
                    } else {
                        Palette.page
                    }
                }
                .navigationDestination(for: Route.self) { destination($0) }
            }
        }
    }

    @ViewBuilder
    private func destination(_ route: Route) -> some View {
        switch route {
        case .chat(let name):
            if let bot = model.bot(name) {
                ChatView(bot: bot).id(name)
            } else {
                VStack(spacing: 0) {
                    BackHeader(title: "") { model.back() }.padding(.horizontal, 17)
                    LoadingRows().padding(.horizontal, 17)
                    Spacer()
                }
                .toolbar(.hidden, for: .navigationBar)
                .task { await model.bots.refresh() }
            }
        case .new:
            NewBotView()
        case .skills(let name):
            if let bot = model.bot(name) { SkillsView(bot: bot) }
        case .connections:
            ConnectionsView()
        }
    }
}

/// The access gate: no address or token, nothing loads, so this asks for them
/// (`components/TokenGate.tsx`). It cannot be dismissed until the app has a working pair.
struct TokenGateView: View {
    private var session = Session.shared
    @State private var address = Session.shared.server?.absoluteString ?? ""
    @State private var token = ""
    @State private var appeared = false
    @FocusState private var field: Field?

    private enum Field { case address, token }

    var body: some View {
        ZStack {
            Color.black.opacity(0.22)
                .background(.ultraThinMaterial.opacity(0.4))
                .ignoresSafeArea()
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Text("访问令牌").font(.px(17, .semibold)).foregroundStyle(Palette.label)
                    Spacer()
                    if session.configured {
                        Button("取消") { session.dismiss() }
                            .font(.px(15))
                            .foregroundStyle(Palette.secondary)
                    }
                }
                Text(hint)
                    .font(.px(13))
                    .lineSpacing(Markdown.spacing(13, 1.5))
                    .foregroundStyle(Palette.secondary)
                input("服务器地址，如 https://shirley.example.com", text: $address, secure: false, invalid: session.error == .address)
                    .focused($field, equals: .address)
                    .keyboardType(.URL)
                    .submitLabel(.next)
                    .onSubmit { field = .token }
                input("粘贴访问令牌", text: $token, secure: true,
                      invalid: session.error == .invalid || session.error == .empty)
                    .focused($field, equals: .token)
                    .submitLabel(.go)
                    .onSubmit(submit)
                if let error = session.error {
                    Text(error.message).font(.px(13)).foregroundStyle(Palette.red).padding(.top, -4)
                }
                Button(action: submit) {
                    Text(session.busy ? "连接中…" : "连接")
                        .font(.px(15, .semibold))
                        .foregroundStyle(.white)
                        .frame(maxWidth: .infinity, minHeight: 46)
                        .background(Palette.label, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
                }
                .buttonStyle(.plain)
                .disabled(session.busy)
                .opacity(session.busy ? 0.55 : 1)
            }
            .padding(20)
            .frame(maxWidth: 360)
            .glass(Color.white.opacity(0.9), in: RoundedRectangle(cornerRadius: 22, style: .continuous))
            .hairline(Color.white.opacity(0.92), in: RoundedRectangle(cornerRadius: 22, style: .continuous))
            .shadow(color: .black.opacity(0.18), radius: 24, y: 18)
            .padding(20)
            .scaleEffect(appeared ? 1 : 0.98)
            .offset(y: appeared ? 0 : 8)
            .opacity(appeared ? 1 : 0)
        }
        .onAppear {
            withAnimation(.timingCurve(0.22, 0.74, 0.2, 1, duration: 0.24)) { appeared = true }
            field = address.isEmpty ? .address : .token
        }
    }

    private var hint: AttributedString {
        var text = AttributedString("填一次就会记住。令牌在服务器 ")
        text += code("/etc/shirley/cloud.env")
        text += AttributedString(" 的 ")
        text += code("SHIRLEY_SERVER_TOKEN")
        text += AttributedString(" 里。")
        return text
    }

    private func code(_ text: String) -> AttributedString {
        var part = AttributedString(text)
        part.font = .mono(12)
        part.foregroundColor = Palette.label
        part.backgroundColor = Palette.fill
        return part
    }

    private func input(_ prompt: String, text: Binding<String>, secure: Bool, invalid: Bool) -> some View {
        Group {
            if secure {
                SecureField(prompt, text: text)
            } else {
                TextField(prompt, text: text)
            }
        }
        .font(.px(16))
        .textInputAutocapitalization(.never)
        .autocorrectionDisabled()
        .padding(.horizontal, 12)
        .frame(height: 44)
        .background(.white, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(invalid ? Palette.red : Color.rgba(60, 60, 67, 0.2), lineWidth: invalid ? 1 : 0.5)
        )
    }

    private func submit() {
        guard !session.busy else { return }
        Task { await session.submit(address: address, token: token) }
    }
}
