import AVKit
import PDFKit
import SwiftUI
import UIKit
import WebKit

/// A web page the bot made. Its scripts run, but it has no bridge to the app and no base URL:
/// the native counterpart of the web UI's sandboxed `srcdoc` iframe. Links open in Safari.
struct HTMLPage: UIViewRepresentable {
    let html: String

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .nonPersistent()
        config.allowsInlineMediaPlayback = true
        let view = WKWebView(frame: .zero, configuration: config)
        view.navigationDelegate = context.coordinator
        view.uiDelegate = context.coordinator
        view.isOpaque = false
        view.backgroundColor = .white
        view.loadHTMLString(html, baseURL: nil)
        context.coordinator.loaded = html
        return view
    }

    func updateUIView(_ view: WKWebView, context: Context) {
        if context.coordinator.loaded != html {
            context.coordinator.loaded = html
            view.loadHTMLString(html, baseURL: nil)
        }
    }

    @MainActor
    final class Coordinator: NSObject, WKNavigationDelegate, WKUIDelegate {
        var loaded = ""

        func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction) async -> WKNavigationActionPolicy {
            if action.navigationType == .linkActivated, let url = action.request.url, url.scheme != "about" {
                await UIApplication.shared.open(url)
                return .cancel
            }
            return .allow
        }

        /// `window.open` / `target=_blank`: beside the app, in Safari.
        func webView(
            _ webView: WKWebView,
            createWebViewWith configuration: WKWebViewConfiguration,
            for action: WKNavigationAction,
            windowFeatures: WKWindowFeatures
        ) -> WKWebView? {
            if let url = action.request.url { UIApplication.shared.open(url) }
            return nil
        }
    }
}

/// The page's source, fetched as text.
private struct AssetText: View {
    let file: ViewedFile
    let content: (String) -> AnyView
    @State private var text: String?
    @State private var failed = false

    var body: some View {
        Group {
            if failed {
                ViewerNote(text: file.isHTML ? "打不开这个网页" : "打不开这个文件")
            } else if let text {
                content(text)
            } else {
                ViewerNote(text: file.isHTML ? "正在打开 \(file.name)…" : "正在打开…")
            }
        }
        .task(id: file.id) {
            do {
                text = try await API.assetText(file.bot, file.assetID)
            } catch {
                failed = true
            }
        }
    }
}

private struct ViewerNote: View {
    let text: String

    var body: some View {
        Text(text).font(.px(14)).foregroundStyle(Palette.secondary).frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// A page shown in a message or a task card; 放大 opens it over everything.
struct HTMLPreview: View {
    let file: ViewedFile
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(spacing: 0) {
            AssetText(file: file) { AnyView(HTMLPage(html: $0)) }
                .frame(height: min(520, UIScreen.main.bounds.height * 0.6))
                .background(.white)
            HStack(spacing: 8) {
                Text(file.name).font(.px(12)).foregroundStyle(Palette.secondary).lineLimit(1).truncationMode(.tail)
                Spacer(minLength: 0)
                PillButton(title: "放大", font: .px(12, .semibold), horizontal: 10, vertical: 4) { model.viewing = file }
                    .accessibilityLabel("放大查看 \(file.name)")
            }
            .padding(.vertical, 7)
            .padding(.horizontal, 10)
            .overlay(alignment: .top) { Rectangle().fill(Palette.bubbleBorder).frame(height: 0.5) }
        }
        .background(.white)
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        .hairline(Color.rgba(60, 60, 67, 0.18), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .padding(.top, 10)
    }
}

/// The viewer, over everything (`FileViewer.tsx`): whatever the file is, the best way to show it.
struct FileViewerView: View {
    let file: ViewedFile
    @Environment(\.dismiss) private var dismiss
    @State private var url: URL?

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Button { dismiss() } label: {
                    Icon(.chevron, size: 20)
                        .rotationEffect(.degrees(180))
                        .foregroundStyle(Palette.label)
                        .frame(width: 38, height: 38)
                        .background(Palette.fill, in: Circle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("关闭")
                Text(file.name)
                    .font(.px(15, .semibold))
                    .lineLimit(1)
                    .frame(maxWidth: .infinity)
                Color.clear.frame(width: 38, height: 38)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .overlay(alignment: .bottom) { Rectangle().fill(Color.rgba(60, 60, 67, 0.15)).frame(height: 0.5) }

            viewerBody
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .background(.white)
        .task(id: file.id) {
            if !file.isHTML && !file.isText { url = try? await API.assetURL(file.bot, file.assetID) }
        }
    }

    @ViewBuilder
    private var viewerBody: some View {
        let mime = file.baseMime
        if file.isHTML {
            AssetText(file: file) { AnyView(HTMLPage(html: $0).ignoresSafeArea(edges: .bottom)) }
        } else if file.isText {
            AssetText(file: file) { text in
                AnyView(ScrollView {
                    Text(text)
                        .font(.mono(13))
                        .lineSpacing(Markdown.spacing(13, 1.5))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(16)
                })
            }
        } else if let url {
            if mime.hasPrefix("image/") {
                ZoomableImage(url: url)
            } else if mime == "application/pdf" {
                PDFPage(url: url).ignoresSafeArea(edges: .bottom)
            } else if mime.hasPrefix("video/") || mime.hasPrefix("audio/") {
                MediaPlayer(url: url)
            } else {
                VStack(spacing: 12) {
                    Text("这种文件没法在这里预览。").font(.px(14)).foregroundStyle(Palette.secondary)
                    Link(destination: url) {
                        Text("下载 \(file.name)")
                            .font(.px(14, .semibold))
                            .foregroundStyle(.white)
                            .padding(.vertical, 8)
                            .padding(.horizontal, 16)
                            .background(Palette.label, in: Capsule())
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        } else {
            ViewerNote(text: "正在打开…")
        }
    }
}

/// A picture, fitted; pinch to look closer.
private struct ZoomableImage: View {
    let url: URL
    @State private var image: UIImage?
    @State private var scale: CGFloat = 1
    @GestureState private var pinch: CGFloat = 1

    var body: some View {
        Group {
            if let image {
                Image(uiImage: image)
                    .resizable()
                    .scaledToFit()
                    .scaleEffect(scale * pinch)
                    .gesture(
                        MagnifyGesture()
                            .updating($pinch) { value, state, _ in state = value.magnification }
                            .onEnded { value in scale = min(max(1, scale * value.magnification), 5) }
                    )
                    .onTapGesture(count: 2) { withAnimation(.ease(0.25)) { scale = scale > 1 ? 1 : 2.5 } }
            } else {
                ViewerNote(text: "正在打开…")
            }
        }
        .task(id: url) {
            if let fetched = try? await URLSession.shared.data(from: url) { image = UIImage(data: fetched.0) }
        }
    }
}

private struct PDFPage: UIViewRepresentable {
    let url: URL

    func makeUIView(context: Context) -> PDFView {
        let view = PDFView()
        view.autoScales = true
        view.backgroundColor = .white
        Task {
            if let fetched = try? await URLSession.shared.data(from: url) {
                view.document = PDFDocument(data: fetched.0)
            }
        }
        return view
    }

    func updateUIView(_ view: PDFView, context: Context) {}
}

private struct MediaPlayer: View {
    let url: URL
    @State private var player: AVPlayer?

    var body: some View {
        VideoPlayer(player: player)
            .onAppear {
                let next = AVPlayer(url: url)
                player = next
                next.play()
            }
            .onDisappear { player?.pause() }
    }
}
