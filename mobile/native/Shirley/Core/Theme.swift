import SwiftUI

// The web UI's design tokens (`index.css` :root and the overrides in `app.css`), one to one.
// CSS px are iOS points, so every size below is the number the stylesheet uses.

extension Color {
    /// `#rrggbb`, optionally with an alpha.
    init(hex: UInt32, alpha: Double = 1) {
        self.init(
            .sRGB,
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255,
            opacity: alpha
        )
    }

    /// CSS `rgba(r, g, b, a)`.
    static func rgba(_ red: Double, _ green: Double, _ blue: Double, _ alpha: Double) -> Color {
        Color(.sRGB, red: red / 255, green: green / 255, blue: blue / 255, opacity: alpha)
    }
}

enum Palette {
    /// `--blue` (the design went monochrome: the accent is black).
    static let accent = Color(hex: 0x000000)
    /// `--green` (also monochrome: the "online" dot).
    static let online = Color(hex: 0x1C1C1E)
    static let red = Color(hex: 0xFF3B30)
    static let label = Color(hex: 0x1C1C1E)
    static let secondary = Color(hex: 0x6E6E73)
    static let tertiary = Color(hex: 0xAEAEB2)
    static let separator = Color.rgba(60, 60, 67, 0.16)
    static let glass = Color.white.opacity(0.62)
    static let glassStrong = Color.white.opacity(0.78)
    static let fill = Color.rgba(118, 118, 128, 0.12)
    /// The grey of a bot bubble, a task card and the composer field.
    static let bubble = Color(hex: 0xF2F2F7)
    static let bubbleBorder = Color.rgba(60, 60, 67, 0.12)
    static let gray = Color(hex: 0x8E8E93)
    static let page = Color.white
}

/// The three colour schemes a bot can have (`botTone` in the web UI).
enum BotTone {
    case nono, patch, scout, custom

    init(bot: String) {
        switch bot {
        case "nono", "default": self = .nono
        case "patch": self = .patch
        case "scout": self = .scout
        default: self = .custom
        }
    }

    /// Avatar background (the monochrome overrides at the end of `index.css`).
    var color: Color {
        switch self {
        case .nono, .custom: Color(hex: 0x1C1C1E)
        case .patch: Color(hex: 0x4A4A4C)
        case .scout: Color(hex: 0x717174)
        }
    }

    /// The home list's larger avatar uses a slightly different black for nono.
    var homeColor: Color {
        self == .nono || self == .custom ? Color(hex: 0x29292B) : color
    }
}

extension Font {
    /// The system font at a stylesheet size. 650 in CSS reads as semibold on iOS.
    static func px(_ size: CGFloat, _ weight: Font.Weight = .regular) -> Font {
        .system(size: size, weight: weight)
    }

    static func mono(_ size: CGFloat) -> Font {
        .system(size: size, design: .monospaced)
    }
}

/// CSS `cubic-bezier(0.22, 0.72, 0.2, 1)`: the curve the web UI uses for most motion.
extension Animation {
    static func ease(_ duration: Double) -> Animation {
        .timingCurve(0.22, 0.72, 0.2, 1, duration: duration)
    }
}

/// `border-radius: a b c d` (top-left, top-right, bottom-right, bottom-left).
struct Corners: Shape {
    var topLeft: CGFloat
    var topRight: CGFloat
    var bottomRight: CGFloat
    var bottomLeft: CGFloat

    func path(in rect: CGRect) -> Path {
        UnevenRoundedRectangle(
            topLeadingRadius: topLeft,
            bottomLeadingRadius: bottomLeft,
            bottomTrailingRadius: bottomRight,
            topTrailingRadius: topRight,
            style: .continuous
        ).path(in: rect)
    }

    /// A bot's bubble: the tail is the small bottom-left corner.
    static let bot = Corners(topLeft: 18, topRight: 18, bottomRight: 18, bottomLeft: 5)
    /// The user's bubble: the tail is bottom-right.
    static let user = Corners(topLeft: 18, topRight: 18, bottomRight: 5, bottomLeft: 18)
}

extension View {
    /// The frosted surfaces (`backdrop-filter: blur() saturate()` over a translucent white).
    func glass(_ tint: Color, in shape: some Shape) -> some View {
        background {
            ZStack {
                shape.fill(.ultraThinMaterial)
                shape.fill(tint)
            }
        }
    }

    /// `border: 0.5px solid color`.
    func hairline(_ color: Color, in shape: some Shape) -> some View {
        overlay { shape.strokeBorder(color, lineWidth: 0.5) }
    }
}

/// The spinning ring used for "working" (`install-spin`).
struct Spinner: View {
    var size: CGFloat = 12
    var line: CGFloat = 1.5
    var track: Color = Palette.tertiary
    var head: Color = Palette.label
    @State private var turning = false

    var body: some View {
        ZStack {
            Circle().stroke(track, lineWidth: line)
            Circle().trim(from: 0, to: 0.25).stroke(head, lineWidth: line).rotationEffect(.degrees(-45))
        }
        .frame(width: size, height: size)
        .rotationEffect(.degrees(turning ? 360 : 0))
        .animation(.linear(duration: 0.8).repeatForever(autoreverses: false), value: turning)
        .onAppear { turning = true }
    }
}

/// A dot that breathes (`tool-pulse`): 0.25 -> 1 opacity, 0.8 -> 1 scale.
struct PulseDot: View {
    var size: CGFloat = 5
    var color: Color = Palette.tertiary
    var delay: Double = 0
    var period: Double = 1.1
    @State private var on = false

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: size, height: size)
            .opacity(on ? 1 : 0.25)
            .scaleEffect(on ? 1 : 0.8)
            .animation(.easeInOut(duration: period / 2).repeatForever(autoreverses: true).delay(delay), value: on)
            .onAppear { on = true }
    }
}
