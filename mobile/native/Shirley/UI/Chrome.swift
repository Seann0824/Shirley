import SwiftUI

/// The web UI's line icons (`components/Icon.tsx`), drawn from the same SVG paths: a 24-unit box,
/// 1.8 stroke, round caps, in the current foreground colour.
enum IconName: String {
    case home, bot, box, brain, globe, folder, image, code, plus, arrow, paperclip, mic, check, clock
    case spark, chevron, shield, bell, file, search, keyboard, pin, mute, trash, chat, camera, stop, tool, plug
}

struct Icon: View {
    let name: IconName
    var size: CGFloat = 22

    init(_ name: IconName, size: CGFloat = 22) {
        self.name = name
        self.size = size
    }

    var body: some View {
        Image("icon-\(name.rawValue)")
            .renderingMode(.template)
            .resizable()
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}

private let presetAvatars = ["nono-classic", "nono-focus", "nono-scout", "nono-maker", "nono-calm", "nono-spark"]

/// `hash(name) % 6` exactly as `BotAvatar.tsx` computes it, so a bot keeps its face across apps.
private func presetIndex(_ name: String) -> Int {
    var value: UInt32 = 0
    // `for (const char of text)` walks code points and reads each one's first UTF-16 unit.
    for scalar in name.unicodeScalars {
        value = value &* 31 &+ UInt32(String(scalar).utf16.first ?? 0)
    }
    return Int(value % UInt32(presetAvatars.count))
}

/// Two little eyes: nono's face.
struct NonoEyes: View {
    var width: CGFloat = 3
    var height: CGFloat = 5
    var gap: CGFloat = 5

    var body: some View {
        HStack(spacing: gap) {
            Capsule().fill(.white).frame(width: width, height: height)
            Capsule().fill(.white).frame(width: width, height: height)
        }
    }
}

/// The mark inside a bot's avatar circle. Platform bots have their own; others get a preset image.
struct BotMark: View {
    let bot: String
    /// Icon size for patch / scout (`BotAvatar size`).
    var size: CGFloat = 25
    /// nono's eyes for this context.
    var eyes = NonoEyes()

    var body: some View {
        switch BotTone(bot: bot) {
        case .nono: eyes
        case .patch: Icon(.code, size: size)
        case .scout: Icon(.globe, size: size)
        case .custom: Image("avatar-\(presetAvatars[presetIndex(bot)])").resizable().scaledToFill()
        }
    }
}

/// The small round avatar beside a bot's messages (29 pt, `message-avatar`).
struct MessageAvatar: View {
    let bot: String
    var hidden = false

    var body: some View {
        ZStack {
            Circle().fill(BotTone(bot: bot).color)
            BotMark(bot: bot, size: 15, eyes: NonoEyes(width: 2, height: 4, gap: 3))
                .foregroundStyle(.white)
        }
        .frame(width: 29, height: 29)
        .clipShape(Circle())
        .opacity(hidden ? 0 : 1)
    }
}

/// A round header button (`header-actions button`, `back-header > button`).
struct GlassCircleButton: View {
    let icon: IconName
    var iconSize: CGFloat = 20
    var flipped = false
    let label: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Icon(icon, size: iconSize)
                .rotationEffect(.degrees(flipped ? 180 : 0))
                .foregroundStyle(Color(hex: 0x252527))
                .frame(width: 38, height: 38)
                .glass(Color.white.opacity(0.52), in: Circle())
                .hairline(Color.white.opacity(0.82), in: Circle())
                .shadow(color: Color.rgba(42, 55, 72, 0.08), radius: 6.5, y: 4)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
    }
}

/// Header of a secondary screen: back, title, nothing on the right.
struct BackHeader: View {
    let title: String
    let onBack: () -> Void

    var body: some View {
        HStack {
            GlassCircleButton(icon: .chevron, flipped: true, label: "返回", action: onBack)
            Spacer()
            Text(title).font(.px(15, .semibold)).foregroundStyle(Palette.label).lineLimit(1)
            Spacer()
            Color.clear.frame(width: 38, height: 38)
        }
        .frame(height: 62)
    }
}

/// Skeleton cards while a page loads.
struct LoadingRows: View {
    var cards = 2
    @State private var sweep = false

    var body: some View {
        VStack(spacing: 10) {
            ForEach(0..<cards, id: \.self) { _ in
                HStack(spacing: 12) {
                    shimmer(RoundedRectangle(cornerRadius: 15, style: .continuous), Color(hex: 0xE1E1E6))
                        .frame(width: 48, height: 48)
                    VStack(alignment: .leading, spacing: 8) {
                        GeometryReader { geo in
                            VStack(alignment: .leading, spacing: 8) {
                                shimmer(RoundedRectangle(cornerRadius: 6), Color(hex: 0xDEDEE3)).frame(width: geo.size.width * 0.58, height: 8)
                                shimmer(RoundedRectangle(cornerRadius: 6), Color(hex: 0xDEDEE3)).frame(width: geo.size.width * 0.82, height: 6)
                            }
                        }
                        .frame(height: 22)
                    }
                }
                .padding(13)
                .frame(minHeight: 82)
                .background(Palette.bubble, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
            }
            HStack(spacing: 6) {
                Spinner(size: 7, line: 1.5, track: .clear, head: Palette.gray)
                Text("正在同步").font(.px(9)).foregroundStyle(Palette.gray)
            }
            .padding(.top, 5)
        }
        .padding(.top, 14)
        .onAppear { sweep = true }
        .accessibilityLabel("正在加载")
    }

    private func shimmer(_ shape: some Shape, _ color: Color) -> some View {
        shape.fill(color)
            .overlay {
                GeometryReader { geo in
                    LinearGradient(colors: [.clear, .white.opacity(0.75), .clear], startPoint: .leading, endPoint: .trailing)
                        .offset(x: sweep ? geo.size.width : -geo.size.width)
                        .animation(.easeInOut(duration: 1.25).repeatForever(autoreverses: false), value: sweep)
                }
                .clipShape(shape)
            }
    }
}

/// `error-note` / `empty-note`.
struct NoteView: View {
    let text: String
    var error = true

    var body: some View {
        Text(text)
            .font(.px(13))
            .lineSpacing(13 * 0.5 - 4)
            .foregroundStyle(error ? Palette.red : Palette.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 10)
            .padding(.horizontal, 14)
            .background(Palette.fill, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            .padding(.vertical, 12)
            .padding(.horizontal, 20)
    }
}

/// "Working on it" with a spinner (`routing-note`).
struct RoutingNote: View {
    let text: String

    var body: some View {
        HStack(spacing: 8) {
            Spinner(size: 12)
            Text(text).font(.px(13)).foregroundStyle(Palette.secondary)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 8)
    }
}

/// A rounded pill button (task actions, skill install, connection actions).
struct PillButton: View {
    let title: String
    var primary = false
    var font: Font = .px(13, .semibold)
    var horizontal: CGFloat = 14
    var vertical: CGFloat = 6
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(title)
                .font(font)
                .foregroundStyle(primary ? .white : Palette.label)
                .padding(.horizontal, horizontal)
                .padding(.vertical, vertical)
                .background(primary ? Palette.label : Palette.fill, in: Capsule())
        }
        .buttonStyle(.plain)
    }
}
