import ImageIO
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// Something the user is about to send (`components/attachments.ts`).
struct Attachment: Identifiable, Hashable {
    enum Kind { case image, pdf, file }

    let id = UUID()
    var name: String
    var mime: String
    var data: Data
    var kind: Kind
    /// The picture, for the tray and the optimistic bubble.
    var preview: UIImage?

    var size: String { Media.formatSize(data.count) }

    init(name: String, mime: String, data: Data) {
        self.name = name
        self.mime = mime
        self.data = data
        if mime.hasPrefix("image/") {
            kind = .image
            preview = UIImage(data: data)
        } else if mime == "application/pdf" || name.lowercased().hasSuffix(".pdf") {
            kind = .pdf
        } else {
            kind = .file
        }
    }

    static func == (lhs: Attachment, rhs: Attachment) -> Bool { lhs.id == rhs.id }
    func hash(into hasher: inout Hasher) { hasher.combine(id) }
}

enum Media {
    /// Longer side of an uploaded picture. Plenty for reading text in a screenshot.
    static let maxSide: CGFloat = 2560
    static let quality: CGFloat = 0.82

    static func formatSize(_ bytes: Int) -> String {
        bytes < 1024 * 1024
            ? "\(Int((Double(bytes) / 1024).rounded(.up))) KB"
            : String(format: "%.1f MB", Double(bytes) / 1024 / 1024)
    }

    static func mime(for url: URL) -> String {
        UTType(filenameExtension: url.pathExtension)?.preferredMIMEType ?? "application/octet-stream"
    }

    /// Whether this phone can write WebP (the web UI's target format); JPEG otherwise.
    private static let webp: Bool = {
        let types = (CGImageDestinationCopyTypeIdentifiers() as? [String]) ?? []
        return types.contains(UTType.webP.identifier)
    }()

    /// Shrink a picture before upload, scaled to `maxSide`. GIFs (maybe animated) and SVGs go up
    /// as they are, and so does anything the re-encode does not make smaller. HEIC always becomes
    /// JPEG/WebP: the models cannot read it.
    static func prepare(_ attachment: Attachment) -> (name: String, mime: String, data: Data) {
        let original = (attachment.name, attachment.mime, attachment.data)
        guard attachment.kind == .image,
              attachment.mime != "image/gif",
              attachment.mime != "image/svg+xml",
              let image = UIImage(data: attachment.data)
        else { return original }
        let scaled = fit(image)
        let type = webp ? UTType.webP : UTType.jpeg
        guard let cg = scaled.cgImage, let encoded = encode(cg, type: type) else { return original }
        let heic = attachment.mime == "image/heic" || attachment.mime == "image/heif"
        if encoded.count >= attachment.data.count && !heic { return original }
        let stem = (attachment.name as NSString).deletingPathExtension
        let ext = webp ? "webp" : "jpg"
        return ("\(stem.isEmpty ? "image" : stem).\(ext)", webp ? "image/webp" : "image/jpeg", encoded)
    }

    private static func fit(_ image: UIImage) -> UIImage {
        let size = image.size
        let longer = max(size.width, size.height)
        let scale = longer > maxSide ? maxSide / longer : 1
        let target = CGSize(width: max(1, (size.width * scale).rounded()), height: max(1, (size.height * scale).rounded()))
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        format.opaque = false
        // Drawing applies the EXIF orientation, so a portrait photo stays upright.
        return UIGraphicsImageRenderer(size: target, format: format).image { _ in
            image.draw(in: CGRect(origin: .zero, size: target))
        }
    }

    private static func encode(_ image: CGImage, type: UTType) -> Data? {
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data, type.identifier as CFString, 1, nil) else { return nil }
        CGImageDestinationAddImage(destination, image, [kCGImageDestinationLossyCompressionQuality: quality] as CFDictionary)
        return CGImageDestinationFinalize(destination) ? data as Data : nil
    }

    /// A picture taken with the camera, as an attachment.
    static func photo(_ image: UIImage) -> Attachment? {
        guard let data = image.jpegData(compressionQuality: 0.9) else { return nil }
        return Attachment(name: "照片-\(stamp()).jpg", mime: "image/jpeg", data: data)
    }

    static func stamp() -> String {
        let format = DateFormatter()
        format.dateFormat = "HHmmss"
        return format.string(from: Date())
    }
}

/// Pictures shown in the conversation: artifacts load through a minted URL, the user's own
/// pictures show from memory before the upload round trip. Cached so scrolling does not refetch.
@MainActor
final class ImageStore {
    static let shared = ImageStore()

    private let cache = NSCache<NSString, UIImage>()
    /// Pictures the user just sent, keyed by the `local:` URL their optimistic bubble carries.
    private var local: [String: UIImage] = [:]

    func keepLocal(_ image: UIImage) -> String {
        let key = "local:\(UUID().uuidString)"
        local[key] = image
        return key
    }

    func cached(_ key: String) -> UIImage? {
        local[key] ?? cache.object(forKey: key as NSString)
    }

    /// How a picture is named in the cache and the size table.
    static func key(bot: String, source: AssetSource) -> String {
        switch source {
        case .artifact(let id): "\(bot)/\(id)"
        case .url(let url): url
        }
    }

    func cached(bot: String, source: AssetSource) -> UIImage? {
        cached(Self.key(bot: bot, source: source))
    }

    // MARK: Sizes

    /// Every picture's size once seen, kept across launches, so a picture not loaded yet still
    /// takes its real space and the thread does not jump when it arrives.
    private static let sizesKey = "imageSizes"
    private static let sizesLimit = 1000
    private lazy var sizes: [String: [Double]] =
        UserDefaults.standard.dictionary(forKey: Self.sizesKey) as? [String: [Double]] ?? [:]

    func size(bot: String, source: AssetSource) -> CGSize? {
        let key = Self.key(bot: bot, source: source)
        if let image = cached(key) { return image.size }
        guard let pair = sizes[key], pair.count == 2, pair[0] > 0, pair[1] > 0 else { return nil }
        return CGSize(width: pair[0], height: pair[1])
    }

    private func remember(_ image: UIImage, key: String) {
        guard !key.hasPrefix("local:"), sizes[key] == nil, image.size.width > 0, image.size.height > 0 else { return }
        if sizes.count >= Self.sizesLimit { sizes.removeAll() }
        sizes[key] = [Double(image.size.width), Double(image.size.height)]
        UserDefaults.standard.set(sizes, forKey: Self.sizesKey)
    }

    /// An image from the library (`bot` + artifact id) or a plain URL.
    func load(bot: String, source: AssetSource) async -> UIImage? {
        let key = Self.key(bot: bot, source: source)
        if let image = cached(key) {
            remember(image, key: key)
            return image
        }
        let url: URL?
        switch source {
        case .artifact(let id): url = try? await API.assetURL(bot, id)
        case .url(let text): url = URL(string: text)
        }
        guard let url, let fetched = try? await URLSession.shared.data(from: url), let image = UIImage(data: fetched.0) else {
            return nil
        }
        cache.setObject(image, forKey: key as NSString)
        remember(image, key: key)
        return image
    }
}

/// An image from the conversation, loaded once and kept. Until it arrives it holds the space
/// it will take (its remembered size, else 4:3), so nothing around it moves when it does.
struct RemoteImage<Placeholder: View>: View {
    let bot: String
    let source: AssetSource
    var mode: ContentMode = .fit
    let placeholder: () -> Placeholder
    @State private var image: UIImage?
    @State private var failed = false

    private static var fallbackRatio: CGFloat { 4.0 / 3.0 }

    init(bot: String, source: AssetSource, mode: ContentMode = .fit, @ViewBuilder placeholder: @escaping () -> Placeholder) {
        self.bot = bot
        self.source = source
        self.mode = mode
        self.placeholder = placeholder
        // Already in memory: show it in the first frame, at its real size.
        _image = State(initialValue: ImageStore.shared.cached(bot: bot, source: source))
    }

    var body: some View {
        Group {
            if let image {
                Image(uiImage: image).resizable().aspectRatio(contentMode: mode)
            } else if failed {
                placeholder()
            } else {
                Rectangle()
                    .fill(Palette.fill)
                    .aspectRatio(ratio, contentMode: .fit)
                    .overlay { placeholder() }
            }
        }
        .task(id: source) {
            if let hit = ImageStore.shared.cached(bot: bot, source: source) {
                image = hit
                return
            }
            failed = false
            image = await ImageStore.shared.load(bot: bot, source: source)
            failed = image == nil
        }
    }

    private var ratio: CGFloat {
        guard let size = ImageStore.shared.size(bot: bot, source: source) else { return Self.fallbackRatio }
        return size.width / size.height
    }
}
