import AppKit
import SwiftUI

struct CachedArtwork<Content: View, Placeholder: View>: View {
    let url: URL?
    let maxPixelSize: Int
    @ViewBuilder var content: (Image) -> Content
    @ViewBuilder var placeholder: () -> Placeholder
    @State private var loaded: (request: ArtworkCache.Request, image: NSImage)?

    private var request: ArtworkCache.Request? {
        url.map { ArtworkCache.Request(url: $0, maxPixelSize: maxPixelSize) }
    }

    private var image: NSImage? {
        guard let request else { return nil }
        if loaded?.request == request { return loaded?.image }
        // A recycled view can display an already decoded image on its first frame.
        return ArtworkCache.shared.cachedImage(for: request)
    }

    var body: some View {
        Group {
            if let image { content(Image(nsImage: image)) }
            else { placeholder() }
        }
        .task(id: request) {
            guard let request else { loaded = nil; return }
            let image = await ArtworkCache.shared.image(for: request)
            guard !Task.isCancelled else { return }
            loaded = image.map { (request, $0) }
        }
    }
}

/// Shares decoded public artwork across storefronts, keyed by URL and pixel size.
@MainActor
final class ArtworkCache {
    struct Request: Hashable, Sendable {
        let url: URL
        let maxPixelSize: Int

        var cacheKey: NSString { "\(maxPixelSize):\(url.absoluteString)" as NSString }
    }

    static let shared = ArtworkCache()
    private let images = NSCache<NSString, NSImage>()
    private var pending: [Request: Task<NSImage?, Never>] = [:]
    private let load: @Sendable (Request) async -> CGImage?

    init(load: @escaping @Sendable (Request) async -> CGImage? = ArtworkCache.download) {
        self.load = load
        images.totalCostLimit = 128 * 1024 * 1024
        images.countLimit = 256
    }

    func cachedImage(for request: Request) -> NSImage? {
        images.object(forKey: request.cacheKey)
    }

    func image(for request: Request) async -> NSImage? {
        if let image = cachedImage(for: request) { return image }
        if let task = pending[request] { return await task.value }
        // A disappearing card must not cancel a download needed by another
        // card, or by the same library when the user returns to it.
        let task = Task { @MainActor [load] in
            defer { pending[request] = nil }
            guard let bitmap = await load(request) else { return nil as NSImage? }
            let image = NSImage(cgImage: bitmap, size: .zero)
            images.setObject(image, forKey: request.cacheKey, cost: bitmap.bytesPerRow * bitmap.height)
            return image
        }
        pending[request] = task
        return await task.value
    }

    nonisolated static func download(_ request: Request) async -> CGImage? {
        do {
            let (data, response) = try await URLSession.shared.data(for: URLRequest(url: request.url, timeoutInterval: 20))
            guard let response = response as? HTTPURLResponse,
                  (200..<300).contains(response.statusCode), data.count <= 32 * 1024 * 1024
            else { return nil }
            // Decode at display resolution rather than retaining full-size
            // store originals for every small card. This runs off MainActor.
            return ImageDecoding.thumbnail(from: data, maxPixelSize: request.maxPixelSize)
        } catch { return nil }
    }
}
