import SwiftUI
import UIKit

/// Text to put back in the field (a recalled message); a new `nonce` puts it again.
struct Draft: Equatable {
    let text: String
    let nonce = UUID()
}

/// The floating composer at the bottom of home, chat and "new bot" (`components/Composer.tsx`).
/// On a phone it is hold-to-talk: released in place it sends, slid up first it lands in the
/// field to be checked. The keyboard button switches to typing.
struct ComposerDock: View {
    var placeholder = "输入消息"
    /// The bot is replying: the input stays open, and with nothing typed the button stops it.
    var busy = false
    var onStop: (() -> Void)?
    var draft: Draft?
    /// Rendered inside the dock, above the controls (e.g. "results waiting").
    var above: AnyView?
    let onSend: (String, [Attachment]) -> Void

    @Environment(AppModel.self) private var model
    @State private var textMode: Bool?
    @State private var value = ""
    @State private var attachments: [Attachment] = []
    @State private var showSheet = false
    @State private var listening = false
    @State private var transcribing = false
    @State private var sliding = false
    @State private var liveText = ""
    @State private var voiceError: String?
    @State private var speech = SpeechInput()
    @State private var utterance = 0
    @State private var pressed = false
    @GestureState private var holding = false
    @FocusState private var focused: Bool
    /// Focus the field once it appears (it does not exist yet while the voice controls show).
    /// Only a switch to typing sets it, so coming back to the screen does not reopen the keyboard.
    @State private var focusOnAppear = false

    /// Past this many points upward, a release edits instead of sending.
    private static let slideToEdit: CGFloat = 60
    private static let permissionHint = "请在系统弹窗里允许麦克风和语音识别，以后就不会再问了"

    /// A keyboard is the natural input on a wide screen; hold-to-talk is the phone default.
    private var prefersText: Bool { model.wide }
    private var typing: Bool { textMode ?? prefersText }
    private var canStop: Bool { busy && onStop != nil }
    private var empty: Bool { value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && attachments.isEmpty }

    var body: some View {
        VStack(spacing: 4) {
            if let above { above }
            if !attachments.isEmpty {
                AttachmentTray(attachments: attachments) { id in attachments.removeAll { $0.id == id } }
            }
            if listening || transcribing { voiceLive }
            if let voiceError {
                Text(voiceError)
                    .font(.px(12))
                    .lineSpacing(Markdown.spacing(12, 1.5))
                    .foregroundStyle(Palette.secondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: .infinity)
                    .padding(.horizontal, 16)
                    .padding(.bottom, 6)
            }
            HStack(spacing: 11) {
                if typing { textComposer } else { voiceControls }
            }
            .frame(maxWidth: .infinity)
        }
        .padding(7)
        .frame(minHeight: 70)
        .glass(Color.white.opacity(0.82), in: RoundedRectangle(cornerRadius: 35, style: .continuous))
        .hairline(Color.white.opacity(0.9), in: RoundedRectangle(cornerRadius: 35, style: .continuous))
        .shadow(color: .black.opacity(0.12), radius: 16, y: 10)
        .padding(.horizontal, 17)
        .padding(.bottom, 19)
        .animation(.ease(0.24), value: attachments)
        .animation(.ease(0.24), value: listening || transcribing)
        .overlaySheet(isPresented: $showSheet) { close in
            AttachmentSheet(close: close) { picked in
                attachments += picked
                voiceError = nil
            }
        }
        .onChange(of: draft) { _, next in
            guard let next else { return }
            value = next.text
            openKeyboard()
        }
        .onChange(of: focused) { was, now in
            // The keyboard was put away (its own button, a swipe, another screen): with nothing
            // typed, the phone goes back to hold-to-talk instead of leaving an idle field.
            if was && !now && !prefersText && empty { textMode = nil }
        }
        .onChange(of: holding) { _, now in
            if now { press() } else { release() }
        }
        .onDisappear { speech.cancel() }
    }

    // MARK: Text

    private var textComposer: some View {
        HStack(spacing: 4) {
            Button {
                textMode = false
                focused = false
            } label: {
                Icon(.mic, size: 20).foregroundStyle(Palette.label).frame(width: 42, height: 42)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("切换语音")

            TextField(attachments.isEmpty ? placeholder : "添加说明…", text: $value)
                .font(.px(16))
                .foregroundStyle(Palette.label)
                .focused($focused)
                .submitLabel(.send)
                .onSubmit { send(value) }
                .onAppear {
                    if focusOnAppear {
                        focusOnAppear = false
                        focused = true
                    }
                }

            if canStop && empty {
                roundButton(.stop, size: 16, background: Palette.label, foreground: .white, label: "停止") { onStop?() }
            } else {
                roundButton(.arrow, size: 18,
                            background: empty ? Color(hex: 0xD1D1D6) : .black,
                            foreground: empty ? Palette.gray : .white, label: "发送") { send(value) }
                    .disabled(empty)
            }
        }
        .padding(5)
        .frame(height: 54)
        .background(Palette.bubble, in: Capsule())
    }

    private func roundButton(_ icon: IconName, size: CGFloat, background: Color, foreground: Color, label: String,
                             action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Icon(icon, size: size)
                .foregroundStyle(foreground)
                .frame(width: 42, height: 42)
                .background(background, in: Circle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
    }

    // MARK: Voice

    private var voiceControls: some View {
        Group {
            if canStop {
                sideButton(.stop, size: 16, dark: true, label: "停止") { onStop?() }
            }
            sideButton(.plus, size: 20, label: "添加附件") {
                var quiet = Transaction()
                quiet.disablesAnimations = true
                withTransaction(quiet) { showSheet = true }
            }
            voiceButton
            sideButton(.keyboard, size: 20, label: "文字输入") { openKeyboard() }
        }
    }

    private func sideButton(_ icon: IconName, size: CGFloat, dark: Bool = false, label: String,
                            action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Icon(icon, size: size)
                .foregroundStyle(dark ? .white : Palette.label)
                .frame(width: 43, height: 43)
                .background(dark ? Palette.label : Palette.bubble, in: Circle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
    }

    private var voiceButton: some View {
        HStack(spacing: 9) {
            ButtonWave(active: listening)
                .frame(width: 44, height: 44)
                .background(Color.white.opacity(0.12), in: Circle())
            Text(listening ? (sliding ? "松开编辑" : "松开发送 · 上滑编辑") : transcribing ? "整理中…" : "按住说话")
                .font(.px(12, .semibold))
                .lineLimit(1)
        }
        .padding(.leading, 5)
        .padding(.trailing, 18)
        .padding(.vertical, 4)
        .frame(maxWidth: .infinity, minHeight: 54, maxHeight: 54)
        .foregroundStyle(.white)
        .background(Palette.accent, in: Capsule())
        .shadow(color: .black.opacity(0.15), radius: 9, y: 7)
        .scaleEffect(listening ? 1.025 : 1)
        .animation(.easeOut(duration: 0.18), value: listening)
        .contentShape(Capsule())
        .gesture(
            DragGesture(minimumDistance: 0)
                .updating($holding) { _, state, _ in state = true }
                .onChanged { drag in
                    guard listening else { return }
                    let up = -drag.translation.height > Self.slideToEdit
                    if up != sliding {
                        sliding = up
                        UISelectionFeedbackGenerator().selectionChanged()
                    }
                }
        )
        .accessibilityLabel("按住说话")
    }

    private var voiceLive: some View {
        // `max-height: 7.5em` with the newest words in view: the oldest lines give way.
        (Text(liveText.isEmpty ? (transcribing ? "正在整理…" : "正在听…") : liveText)
            .foregroundStyle(liveText.isEmpty ? Palette.tertiary : Palette.label)
         + Text(transcribing && !liveText.isEmpty ? " · 整理中" : "")
            .font(.px(12))
            .foregroundStyle(Palette.tertiary))
            .font(.px(15))
            .lineSpacing(Markdown.spacing(15, 1.45))
            .lineLimit(5)
            .truncationMode(.head)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 10)
            .padding(.horizontal, 14)
            .background(Palette.fill, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
            .padding(.horizontal, 6)
            .padding(.top, 2)
            .padding(.bottom, 4)
    }

    /// Finger down on the voice button: open the microphone and start listening.
    private func press() {
        // One recogniser: the last utterance finishes (at most a moment) before the next starts.
        guard !listening, !transcribing else { return }
        utterance += 1
        let id = utterance
        pressed = true
        sliding = false
        voiceError = nil
        liveText = ""
        listening = true
        UIImpactFeedbackGenerator(style: .medium).impactOccurred()
        Task {
            if SpeechInput.willAsk { liveText = Self.permissionHint }
            do {
                try await speech.begin { text in
                    if utterance == id { liveText = text }
                }
            } catch {
                listening = false
                liveText = ""
                let failure = error as? SpeechInput.Failure
                voiceError = error.localizedDescription
                if failure?.fatal == true { textMode = true }
                return
            }
            // Released while the microphone was still opening (or a permission prompt was answered).
            if !pressed || utterance != id {
                speech.cancel()
                listening = false
                liveText = ""
                return
            }
            if liveText == Self.permissionHint { liveText = "" }
        }
    }

    /// Finger up (or the gesture was taken away): send what was heard, or put it in the field.
    private func release() {
        pressed = false
        guard listening, speech.running else { return }
        let edit = sliding
        sliding = false
        listening = false
        let total = speech.elapsed
        if total < 0.3 {
            speech.cancel()
            liveText = ""
            voiceError = "按住按钮，说完再松开"
            return
        }
        transcribing = true
        let id = utterance
        Task {
            let heard = await speech.finish()
            transcribing = false
            if utterance == id { liveText = "" }
            if !place(heard, edit: edit) { voiceError = "没有识别到内容，请再说一次" }
        }
    }

    /// Released in place, heard text is sent; slid up first, it goes into the field to be checked.
    private func place(_ heard: String, edit: Bool) -> Bool {
        let text = heard.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return false }
        let typed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        let combined = typed.isEmpty ? text : "\(typed) \(text)"
        if edit {
            value = combined
            openKeyboard()
        } else {
            send(combined)
        }
        return true
    }

    /// Switch to typing with the keyboard up; if the field is not on screen yet, focus it when it appears.
    private func openKeyboard() {
        if typing {
            focused = true
        } else {
            focusOnAppear = true
            textMode = true
        }
    }

    private func send(_ text: String) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty || !attachments.isEmpty else { return }
        // The first message is the natural moment to ask: "tell me when the reply is ready".
        Notifier.askPermission()
        onSend(trimmed, attachments)
        value = ""
        attachments = []
        textMode = nil
        if !prefersText { focused = false }
    }
}

/// The five bars on the voice button; they pulse while listening.
private struct ButtonWave: View {
    let active: Bool
    @State private var up = false
    private let heights: [CGFloat] = [10, 19, 28, 19, 10]

    var body: some View {
        HStack(spacing: 3) {
            ForEach(0..<5, id: \.self) { index in
                Capsule()
                    .fill(.white)
                    .frame(width: 3, height: heights[index])
                    .scaleEffect(y: active ? (up != (index % 2 == 1) ? 1 : 0.55) : 1)
                    .opacity(active ? (up != (index % 2 == 1) ? 1 : 0.55) : 1)
            }
        }
        .onChange(of: active) { _, now in
            if now {
                withAnimation(.easeInOut(duration: 0.55).repeatForever(autoreverses: true)) { up = true }
            } else {
                withAnimation(.easeOut(duration: 0.15)) { up = false }
            }
        }
    }
}

/// What is about to be sent, above the controls.
private struct AttachmentTray: View {
    let attachments: [Attachment]
    let onRemove: (UUID) -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 7) {
                ForEach(attachments) { attachment in
                    HStack(spacing: 7) {
                        thumb(attachment)
                        VStack(alignment: .leading, spacing: 3) {
                            Text(attachment.name).font(.px(9, .semibold)).lineLimit(1)
                            Text(attachment.size).font(.px(7)).foregroundStyle(Palette.gray)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        Button { onRemove(attachment.id) } label: {
                            Text("×")
                                .font(.px(14))
                                .foregroundStyle(.white)
                                .frame(width: 19, height: 19)
                                .background(Palette.gray, in: Circle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("移除 \(attachment.name)")
                    }
                    .padding(5)
                    .frame(width: 142, height: 52)
                    .background(Palette.bubble, in: RoundedRectangle(cornerRadius: 13, style: .continuous))
                    .hairline(Palette.bubbleBorder, in: RoundedRectangle(cornerRadius: 13, style: .continuous))
                }
            }
            .padding(.horizontal, 1)
            .padding(.top, 2)
            .padding(.bottom, 4)
        }
    }

    @ViewBuilder
    private func thumb(_ attachment: Attachment) -> some View {
        if attachment.kind == .image, let preview = attachment.preview {
            Image(uiImage: preview)
                .resizable()
                .scaledToFill()
                .frame(width: 42, height: 42)
                .clipShape(RoundedRectangle(cornerRadius: 9, style: .continuous))
        } else {
            VStack(spacing: 2) {
                Icon(attachment.kind == .pdf ? .file : .paperclip, size: 18)
                Text(attachment.kind == .pdf ? "PDF" : "FILE").font(.px(6, .bold)).kerning(0.24)
            }
            .foregroundStyle(.white)
            .frame(width: 42, height: 42)
            .background(attachment.kind == .pdf ? Palette.red : Palette.label,
                        in: RoundedRectangle(cornerRadius: 9, style: .continuous))
        }
    }
}
