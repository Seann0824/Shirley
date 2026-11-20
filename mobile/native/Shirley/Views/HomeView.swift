import SwiftUI
import UIKit

private let toolIcons: [String: IconName] = [
    "bash": .code, "grep": .search, "read": .file, "media": .image, "web_search": .globe,
]

/// The bot list and the "ask anything" composer (`views/Home.tsx`).
struct HomeView: View {
    @Environment(AppModel.self) private var model
    @State private var deleting: Bot?
    @State private var showDelete = false
    private var session = Session.shared

    var body: some View {
        ScrollView {
            VStack(spacing: 0) {
                header
                if let error = model.bots.error, !session.needed {
                    NoteView(text: "无法连接服务器：\(error)")
                }
                if model.asking { RoutingNote(text: "正在找合适的 bot…") }
                botList.padding(.top, 18)
                if model.bots.visible.isEmpty && model.bots.error == nil && !session.needed {
                    NoteView(text: "没有可用的 Bot", error: false)
                }
            }
            .padding(.horizontal, 17)
            .padding(.bottom, 13)
        }
        .scrollIndicators(.hidden)
        .refreshable { await model.bots.refresh() }
        .background(model.wide ? Color(hex: 0xFAFAFA) : Palette.page)
        .safeAreaInset(edge: .bottom, spacing: 0) {
            ComposerDock(placeholder: "想做点什么？") { text, attachments in model.ask(text, attachments) }
        }
        .overlaySheet(isPresented: $showDelete) { close in
            if let bot = deleting {
                DeleteBotSheet(bot: bot, close: close) { try await model.bots.remove(bot.name) }
            }
        }
        .toolbar(.hidden, for: .navigationBar)
    }

    private var header: some View {
        HStack(spacing: 10) {
            MiniAvatar()
                // Long-press the mark to change the server address or token.
                .onLongPressGesture { session.reopen() }
            Text("Shirley").font(.px(15, .semibold)).foregroundStyle(Palette.label)
            Spacer()
            HStack(spacing: 7) {
                GlassCircleButton(icon: .plug, label: "连接") { model.go(.connections) }
                GlassCircleButton(icon: .plus, label: "创建 Bot") { model.go(.new) }
            }
        }
        .frame(height: 62)
    }

    private var botList: some View {
        // Working: answering in its conversation, or on a task it took on.
        let busy = Replying.shared.bots.union(model.tasks.tasks.filter { $0.status.working }.map(\.bot))
        // Work lives in each bot's conversation; the list only says which bot has something to look at.
        let review = Set(model.tasks.tasks.filter { $0.status == .inReview || $0.status == .blocked }.map(\.bot))
        let bots = model.bots.visible
        return VStack(spacing: 0) {
            ForEach(Array(bots.enumerated()), id: \.element.id) { index, bot in
                SwipeBotRow(
                    name: bot.displayName,
                    pinned: model.bots.pinned.contains(bot.name),
                    muted: model.bots.muted.contains(bot.name),
                    selected: model.wide && model.selectedBot?.name == bot.name,
                    onOpen: { model.openBot(bot.name) },
                    onTogglePin: { withAnimation(.ease(0.3)) { model.bots.togglePin(bot.name) } },
                    onToggleMute: { model.bots.toggleMute(bot.name) },
                    onDelete: {
                        deleting = bot
                        var quiet = Transaction()
                        quiet.disablesAnimations = true
                        withTransaction(quiet) { showDelete = true }
                    }
                ) {
                    BotRowContent(bot: bot, busy: busy.contains(bot.name), review: review.contains(bot.name),
                                  muted: model.bots.muted.contains(bot.name))
                }
                .overlay(alignment: .top) {
                    if index > 0 {
                        Rectangle().fill(Color.rgba(60, 60, 67, 0.09)).frame(height: 0.5).padding(.leading, 71)
                    }
                }
            }
        }
        .background(model.wide ? Color.clear : Color.white)
        .clipShape(RoundedRectangle(cornerRadius: 24, style: .continuous))
        .hairline(model.wide ? .clear : Color.white.opacity(0.8), in: RoundedRectangle(cornerRadius: 24, style: .continuous))
        .shadow(color: model.wide ? .clear : Color.rgba(47, 62, 80, 0.08), radius: 12.5, y: 8)
    }
}

/// The Shirley mark in the home header.
private struct MiniAvatar: View {
    var body: some View {
        ZStack {
            Circle().fill(Color.white.opacity(0.72))
                .overlay(Circle().strokeBorder(Color.white.opacity(0.9), lineWidth: 0.5))
                .shadow(color: Color.rgba(44, 62, 82, 0.08), radius: 5, y: 3)
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(Color(hex: 0x1C1C1E))
                .frame(width: 24, height: 19)
                .overlay(NonoEyes(width: 3, height: 4, gap: 5))
        }
        .frame(width: 38, height: 38)
        .overlay(alignment: .bottomTrailing) {
            Circle().fill(Palette.online).frame(width: 10, height: 10)
                .overlay(Circle().strokeBorder(.white, lineWidth: 2))
        }
        .accessibilityHidden(true)
    }
}

/// Avatar, name, status, and what the bot is for.
private struct BotRowContent: View {
    let bot: Bot
    let busy: Bool
    let review: Bool
    let muted: Bool

    var body: some View {
        HStack(spacing: 11) {
            avatar.opacity(muted ? 0.55 : 1)
            VStack(alignment: .leading, spacing: 0) {
                HStack(spacing: 7) {
                    Text(bot.displayName).font(.px(14, .semibold)).foregroundStyle(Palette.label).lineLimit(1)
                    StatusPill(busy: busy, review: review)
                }
                CapabilityStrip(bot: bot).padding(.top, 6)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .opacity(muted ? 0.55 : 1)
        }
        .padding(.vertical, 9)
        .padding(.horizontal, 14)
        .frame(maxWidth: .infinity, minHeight: 78, alignment: .leading)
    }

    private var avatar: some View {
        let tone = BotTone(bot: bot.name)
        return ZStack {
            if tone == .custom {
                // A preset picture, drawn a little larger than its 46 pt slot, as on the web.
                BotMark(bot: bot.name)
                    .frame(width: 54, height: 54)
                    .clipShape(RoundedRectangle(cornerRadius: 17, style: .continuous))
            } else {
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .fill(tone.homeColor)
                    .overlay(alignment: .top) {
                        RoundedRectangle(cornerRadius: 14, style: .continuous)
                            .strokeBorder(Color.white.opacity(0.2), lineWidth: 1)
                            .mask(LinearGradient(colors: [.white, .clear], startPoint: .top, endPoint: .center))
                    }
                if tone == .nono {
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .strokeBorder(Color.white.opacity(0.55), lineWidth: 1)
                        .frame(width: 27, height: 20)
                        .overlay(NonoEyes(width: 3, height: 5, gap: 6))
                } else {
                    BotMark(bot: bot.name).foregroundStyle(.white)
                }
            }
        }
        .frame(width: 46, height: 46)
        .overlay(alignment: .bottomTrailing) {
            Circle().fill(Palette.online).frame(width: 12, height: 12)
                .overlay(Circle().strokeBorder(.white, lineWidth: 2.5))
                .offset(x: tone == .custom ? 6 : 2, y: tone == .custom ? 6 : 2)
        }
    }
}

private struct StatusPill: View {
    let busy: Bool
    let review: Bool

    var body: some View {
        let (text, color, background, dot): (String, Color, Color, Color) =
            review ? ("有结果待看", Color(hex: 0x0A60C2), Color.rgba(0, 122, 255, 0.1), Palette.accent)
            : busy ? ("工作中", Color(hex: 0x248A3D), Color.rgba(52, 199, 89, 0.12), Palette.online)
            : ("空闲", Palette.secondary, Palette.fill, Palette.gray)
        return HStack(spacing: 4) {
            Circle().fill(dot).frame(width: 5, height: 5)
            Text(text).font(.px(8))
        }
        .foregroundStyle(color)
        .padding(.vertical, 2)
        .padding(.horizontal, 6)
        .background(background, in: RoundedRectangle(cornerRadius: 6, style: .continuous))
    }
}

/// What the bot is for, at a glance.
private struct CapabilityStrip: View {
    let bot: Bot

    var body: some View {
        switch bot.name {
        case "nono":
            HStack(spacing: 5) {
                ForEach(["web", "files", "create"], id: \.self) { item in
                    Image("capability-\(item)")
                        .resizable()
                        .scaledToFill()
                        .grayscale(1)
                        .frame(width: 27, height: 18)
                        .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
                        .hairline(Color.rgba(60, 60, 67, 0.1), in: RoundedRectangle(cornerRadius: 6, style: .continuous))
                }
            }
        case "patch":
            strip {
                Text("</>").font(.px(8, .bold))
                Capsule().fill(Palette.gray).frame(width: 20, height: 2)
                Capsule().fill(Palette.gray).frame(width: 20, height: 2)
            }
        case "scout":
            strip {
                Icon(.globe, size: 16)
                Rectangle().fill(Palette.tertiary).frame(height: 1)
                Icon(.search, size: 15)
            }
        default:
            strip(spacing: 5) {
                ForEach(Array(bot.tools.prefix(3)), id: \.self) { tool in
                    Icon(toolIcons[tool] ?? .tool, size: 14)
                }
            }
        }
    }

    private func strip<C: View>(spacing: CGFloat = 6, @ViewBuilder _ content: () -> C) -> some View {
        HStack(spacing: spacing) { content() }
            .foregroundStyle(Palette.secondary)
            .padding(.vertical, 4)
            .padding(.horizontal, 7)
            .frame(width: 96, height: 19, alignment: .leading)
            .background(Palette.fill, in: RoundedRectangle(cornerRadius: 7, style: .continuous))
    }
}

/// A bot card on the home list; swipe left for pin / mute / remove (`components/BotRow.tsx`).
struct SwipeBotRow<Content: View>: View {
    let name: String
    let pinned: Bool
    let muted: Bool
    let selected: Bool
    let onOpen: () -> Void
    let onTogglePin: () -> Void
    let onToggleMute: () -> Void
    let onDelete: () -> Void
    @ViewBuilder let content: () -> Content

    private static var actionWidth: CGFloat { 174 }
    @State private var open = false
    @State private var offset: CGFloat = 0

    var body: some View {
        ZStack(alignment: .trailing) {
            if open || offset != 0 {
                HStack(spacing: 0) {
                    action(pinned ? "取消" : "置顶", .pin, background: pinned ? .black : Color(hex: 0x1C1C1E)) { onTogglePin() }
                    action(muted ? "恢复" : "静音", .mute, background: muted ? .black : Color(hex: 0x636366)) { onToggleMute() }
                    action("删除", .trash, background: Palette.red) { onDelete() }
                }
                .frame(width: Self.actionWidth)
            }
            Button {
                if open { close() } else { onOpen() }
            } label: {
                content()
                    // White rows; the open conversation's row is grey in the two-pane layout.
                    .background(selected ? Color(hex: 0xECECEE) : Color.white)
                    .contentShape(Rectangle())
            }
            .buttonStyle(RowPressStyle())
            .offset(x: offset)
            .gesture(
                HorizontalPan { translation in
                    let base = open ? -Self.actionWidth : 0
                    offset = max(-Self.actionWidth, min(0, base + translation))
                } onEnd: { _ in
                    let shouldOpen = offset < -62
                    withAnimation(.ease(0.24)) {
                        open = shouldOpen
                        offset = shouldOpen ? -Self.actionWidth : 0
                    }
                }
            )
            .accessibilityLabel("\(name)，左滑显示操作")
            .accessibilityActions {
                Button(pinned ? "取消置顶" : "置顶", action: onTogglePin)
                Button(muted ? "恢复" : "静音", action: onToggleMute)
                Button("删除", action: onDelete)
            }
        }
        .clipped()
    }

    private func close() {
        withAnimation(.ease(0.24)) {
            open = false
            offset = 0
        }
    }

    private func action(_ title: String, _ icon: IconName, background: Color, run: @escaping () -> Void) -> some View {
        Button {
            run()
            close()
        } label: {
            VStack(spacing: 5) {
                Icon(icon, size: 18)
                Text(title).font(.px(8, .medium))
            }
            .foregroundStyle(.white)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(background)
        }
        .buttonStyle(.plain)
    }
}

/// `.home-bot-card:active`: a light grey while pressed.
private struct RowPressStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .overlay(Color.rgba(118, 118, 128, configuration.isPressed ? 0.1 : 0))
    }
}

/// A horizontal pan that only starts when the finger moves sideways, so the list still scrolls.
struct HorizontalPan: UIGestureRecognizerRepresentable {
    let onChange: (CGFloat) -> Void
    let onEnd: (CGFloat) -> Void

    func makeCoordinator(converter: CoordinateSpaceConverter) -> Coordinator { Coordinator() }

    func makeUIGestureRecognizer(context: Context) -> UIPanGestureRecognizer {
        let pan = UIPanGestureRecognizer()
        pan.delegate = context.coordinator
        return pan
    }

    func handleUIGestureRecognizerAction(_ recognizer: UIPanGestureRecognizer, context: Context) {
        let x = recognizer.translation(in: recognizer.view).x
        switch recognizer.state {
        case .changed: onChange(x)
        case .ended, .cancelled, .failed: onEnd(x)
        default: break
        }
    }

    final class Coordinator: NSObject, UIGestureRecognizerDelegate {
        func gestureRecognizerShouldBegin(_ recognizer: UIGestureRecognizer) -> Bool {
            guard let pan = recognizer as? UIPanGestureRecognizer else { return true }
            let velocity = pan.velocity(in: pan.view)
            return abs(velocity.x) > abs(velocity.y)
        }
    }
}
