import PDFKit
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers
import VisionKit

extension View {
    /// The web UI's bottom sheet (`attachment-sheet-layer`): a dimmed backdrop that fades in and a
    /// rounded card that rises 9 pt above the bottom edge, over everything on screen.
    func overlaySheet<Content: View>(
        isPresented: Binding<Bool>,
        dismissible: Bool = true,
        @ViewBuilder content: @escaping (_ close: @escaping () -> Void) -> Content
    ) -> some View {
        fullScreenCover(isPresented: isPresented) {
            SheetLayer(isPresented: isPresented, dismissible: dismissible, content: content)
                .presentationBackground(.clear)
        }
        // The cover's own slide is replaced by the sheet's fade-and-rise.
        .transaction(value: isPresented.wrappedValue) { $0.disablesAnimations = true }
    }
}

private struct SheetLayer<Content: View>: View {
    @Binding var isPresented: Bool
    let dismissible: Bool
    let content: (@escaping () -> Void) -> Content
    @State private var shown = false

    var body: some View {
        ZStack(alignment: .bottom) {
            Color.black.opacity(shown ? 0.2 : 0)
                .ignoresSafeArea()
                .onTapGesture { if dismissible { close() } }
                .accessibilityLabel("关闭")
            if shown {
                content(close)
                    .padding(.top, 8)
                    .padding(.horizontal, 10)
                    .padding(.bottom, 12)
                    .frame(maxWidth: 560)
                    .background {
                        ZStack {
                            RoundedRectangle(cornerRadius: 28, style: .continuous).fill(.ultraThinMaterial)
                            RoundedRectangle(cornerRadius: 28, style: .continuous).fill(Color.rgba(248, 248, 250, 0.94))
                        }
                    }
                    .hairline(Color.white.opacity(0.88), in: RoundedRectangle(cornerRadius: 28, style: .continuous))
                    .shadow(color: .black.opacity(0.14), radius: 21, y: -12)
                    .padding(9)
                    .transition(.offset(y: 28).combined(with: .scale(scale: 0.98)).combined(with: .opacity))
            }
        }
        .ignoresSafeArea(.container, edges: .bottom)
        .onAppear { withAnimation(.timingCurve(0.22, 0.74, 0.2, 1, duration: 0.28)) { shown = true } }
    }

    private func close() {
        withAnimation(.easeOut(duration: 0.2)) { shown = false }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
            var quiet = Transaction()
            quiet.disablesAnimations = true
            withTransaction(quiet) { isPresented = false }
        }
    }
}

/// The grabber at the top of a sheet.
struct SheetHandle: View {
    var body: some View {
        Capsule().fill(Color(hex: 0xC7C7CC)).frame(width: 34, height: 4).padding(.bottom, 13)
    }
}

/// `sheet-cancel`.
struct SheetCancel: View {
    var title = "取消"
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(title)
                .font(.px(14, .medium))
                .foregroundStyle(Palette.label)
                .frame(maxWidth: .infinity, minHeight: 46)
                .background(.white, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        }
        .buttonStyle(.plain)
    }
}

/// 照片 / 拍摄 / 文件 / 扫描: where an attachment comes from.
struct AttachmentSheet: View {
    let close: () -> Void
    let onPick: ([Attachment]) -> Void

    @State private var photos: [PhotosPickerItem] = []
    @State private var camera = false
    @State private var files = false
    @State private var scanner = false

    private static let documentTypes: [UTType] = {
        let extensions = ["pdf", "txt", "md", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "csv", "json"]
        return extensions.compactMap { UTType(filenameExtension: $0) }
    }()

    var body: some View {
        VStack(spacing: 0) {
            SheetHandle()
            HStack(alignment: .top, spacing: 8) {
                PhotosPicker(selection: $photos, maxSelectionCount: 10, matching: .images) {
                    tile("照片", .image)
                }
                .buttonStyle(.plain)
                option("拍摄", .camera) { camera = true }
                option("文件", .folder) { files = true }
                option("扫描", .file) { scanner = true }
            }
            SheetCancel(action: close).padding(.top, 14)
        }
        .onChange(of: photos) { _, items in
            guard !items.isEmpty else { return }
            Task {
                var picked: [Attachment] = []
                for (index, item) in items.enumerated() {
                    guard let data = try? await item.loadTransferable(type: Data.self) else { continue }
                    let type = item.supportedContentTypes.first
                    let ext = type?.preferredFilenameExtension ?? "jpg"
                    let mime = type?.preferredMIMEType ?? "image/jpeg"
                    picked.append(Attachment(name: "照片-\(Media.stamp())-\(index + 1).\(ext)", mime: mime, data: data))
                }
                finish(picked)
            }
        }
        .fullScreenCover(isPresented: $camera) {
            CameraPicker { image in
                camera = false
                if let image, let photo = Media.photo(image) { finish([photo]) }
            }
            .ignoresSafeArea()
        }
        .fullScreenCover(isPresented: $scanner) {
            DocumentScanner { pdf in
                scanner = false
                if let pdf { finish([Attachment(name: "扫描-\(Media.stamp()).pdf", mime: "application/pdf", data: pdf)]) }
            }
            .ignoresSafeArea()
        }
        .fileImporter(isPresented: $files, allowedContentTypes: Self.documentTypes, allowsMultipleSelection: true) { result in
            guard case .success(let urls) = result else { return }
            let picked: [Attachment] = urls.compactMap { url in
                let scoped = url.startAccessingSecurityScopedResource()
                defer { if scoped { url.stopAccessingSecurityScopedResource() } }
                guard let data = try? Data(contentsOf: url) else { return nil }
                return Attachment(name: url.lastPathComponent, mime: Media.mime(for: url), data: data)
            }
            finish(picked)
        }
    }

    private func finish(_ picked: [Attachment]) {
        if !picked.isEmpty { onPick(picked) }
        close()
    }

    private func option(_ label: String, _ icon: IconName, action: @escaping () -> Void) -> some View {
        Button(action: action) { tile(label, icon) }.buttonStyle(.plain)
    }

    private func tile(_ label: String, _ icon: IconName) -> some View {
        VStack(spacing: 7) {
            Icon(icon, size: 22)
                .foregroundStyle(Palette.label)
                .frame(width: 54, height: 54)
                .background(.white, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                .hairline(Palette.bubbleBorder, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                .shadow(color: .black.opacity(0.05), radius: 5, y: 3)
            Text(label).font(.px(10, .medium)).foregroundStyle(Palette.label)
        }
        .frame(maxWidth: .infinity)
        .contentShape(Rectangle())
    }
}

/// The camera, for one picture.
struct CameraPicker: UIViewControllerRepresentable {
    let done: (UIImage?) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(done: done) }

    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = UIImagePickerController.isSourceTypeAvailable(.camera) ? .camera : .photoLibrary
        picker.delegate = context.coordinator
        return picker
    }

    func updateUIViewController(_ controller: UIImagePickerController, context: Context) {}

    final class Coordinator: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate {
        let done: (UIImage?) -> Void

        init(done: @escaping (UIImage?) -> Void) {
            self.done = done
        }

        func imagePickerController(_ picker: UIImagePickerController, didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
            done(info[.originalImage] as? UIImage)
        }

        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) {
            done(nil)
        }
    }
}

/// The system document scanner; the pages come back as one PDF.
struct DocumentScanner: UIViewControllerRepresentable {
    let done: (Data?) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(done: done) }

    func makeUIViewController(context: Context) -> VNDocumentCameraViewController {
        let scanner = VNDocumentCameraViewController()
        scanner.delegate = context.coordinator
        return scanner
    }

    func updateUIViewController(_ controller: VNDocumentCameraViewController, context: Context) {}

    final class Coordinator: NSObject, VNDocumentCameraViewControllerDelegate {
        let done: (Data?) -> Void

        init(done: @escaping (Data?) -> Void) {
            self.done = done
        }

        func documentCameraViewController(_ controller: VNDocumentCameraViewController, didFinishWith scan: VNDocumentCameraScan) {
            let document = PDFDocument()
            for index in 0..<scan.pageCount {
                if let page = PDFPage(image: scan.imageOfPage(at: index)) {
                    document.insert(page, at: document.pageCount)
                }
            }
            done(document.pageCount > 0 ? document.dataRepresentation() : nil)
        }

        func documentCameraViewControllerDidCancel(_ controller: VNDocumentCameraViewController) {
            done(nil)
        }

        func documentCameraViewController(_ controller: VNDocumentCameraViewController, didFailWithError error: Error) {
            done(nil)
        }
    }
}

/// Deleting a bot is for real (the server keeps a copy in its trash folder), so it is confirmed.
struct DeleteBotSheet: View {
    let bot: Bot
    let close: () -> Void
    let onDelete: () async throws -> Void
    @State private var working = false
    @State private var error: String?

    var body: some View {
        VStack(spacing: 0) {
            SheetHandle()
            Text("删除「\(bot.displayName)」？")
                .font(.px(16, .semibold))
                .multilineTextAlignment(.center)
                .padding(.horizontal, 8)
            Text("它的对话、记忆、工作区和技能会一起删除，绑定它的连接器会断开，未完成的任务会取消。")
                .font(.px(13))
                .lineSpacing(Markdown.spacing(13, 1.45))
                .foregroundStyle(Palette.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 8)
                .padding(.top, 8)
            if let error { NoteView(text: error) }
            Button {
                Task { await confirm() }
            } label: {
                Text(working ? "正在删除…" : "删除")
                    .font(.px(14, .semibold))
                    .foregroundStyle(.white)
                    .frame(maxWidth: .infinity, minHeight: 46)
                    .background(Palette.red, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
            }
            .buttonStyle(.plain)
            .disabled(working)
            .opacity(working ? 0.6 : 1)
            .padding(.top, 16)
            SheetCancel(action: close).disabled(working).padding(.top, 8)
        }
    }

    private func confirm() async {
        working = true
        error = nil
        do {
            try await onDelete()
            close()
        } catch {
            self.error = error.localizedDescription
            working = false
        }
    }
}
