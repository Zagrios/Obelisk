import AppKit
import SwiftUI

/// The first of `urls` that loads, decoded at `maxPixelSize` and shared
/// through `ArtworkCache`.
struct CachedArtwork<Content: View, Placeholder: View, Failure: View>: View {
    /// Tried in order: a storefront does not publish every image of every game.
    let urls: [URL]
    let maxPixelSize: Int
    @ViewBuilder var content: (Image) -> Content
    @ViewBuilder var placeholder: () -> Placeholder
    /// Shown once every URL failed.
    @ViewBuilder var failure: () -> Failure
    @State private var loaded: (requests: [ArtworkCache.Request], image: NSImage?)?

    init(
        urls: [URL],
        maxPixelSize: Int,
        @ViewBuilder content: @escaping (Image) -> Content,
        @ViewBuilder placeholder: @escaping () -> Placeholder,
        @ViewBuilder failure: @escaping () -> Failure
    ) {
        self.urls = urls
        self.maxPixelSize = maxPixelSize
        self.content = content
        self.placeholder = placeholder
        self.failure = failure
    }

    private var requests: [ArtworkCache.Request] {
        urls.map { ArtworkCache.Request(url: $0, maxPixelSize: maxPixelSize) }
    }

    var body: some View {
        let requests = requests
        Group {
            if let loaded, loaded.requests == requests {
                if let image = loaded.image { content(Image(nsImage: image)) }
                else { failure() }
            } else if let image = requests.lazy.compactMap(ArtworkCache.shared.cachedImage(for:)).first {
                // A recycled view can display an already decoded image on its first frame.
                content(Image(nsImage: image))
            } else {
                placeholder()
            }
        }
        .task(id: requests) {
            for request in requests {
                let image = await ArtworkCache.shared.image(for: request)
                guard !Task.isCancelled else { return }
                if let image { return loaded = (requests, image) }
            }
            loaded = (requests, nil)
        }
    }
}

extension CachedArtwork where Failure == Placeholder {
    /// The placeholder also stands for artwork that failed to load.
    init(
        url: URL?,
        maxPixelSize: Int,
        @ViewBuilder content: @escaping (Image) -> Content,
        @ViewBuilder placeholder: @escaping () -> Placeholder
    ) {
        self.init(urls: url.map { [$0] } ?? [], maxPixelSize: maxPixelSize, content: content,
                  placeholder: placeholder, failure: placeholder)
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

    /// `URLSession.shared`'s disk cache holds a few dozen covers, so a large
    /// library would download them again at each launch. Storefronts mark
    /// their artwork cacheable. Public artwork only, no account data; decoded
    /// images stay in `images`, so nothing is kept in memory here.
    private nonisolated static let session: URLSession = {
        let configuration = URLSessionConfiguration.default
        let directory = URL.cachesDirectory.appending(path: "\(AppIdentity.storageIdentifier)/Artwork",
                                                      directoryHint: .isDirectory)
        configuration.urlCache = URLCache(memoryCapacity: 0, diskCapacity: 512 * 1024 * 1024, directory: directory)
        return URLSession(configuration: configuration)
    }()

    nonisolated static func download(_ request: Request) async -> CGImage? {
        do {
            let (data, response) = try await session.data(for: URLRequest(url: request.url, timeoutInterval: 20))
            guard let response = response as? HTTPURLResponse,
                  (200..<300).contains(response.statusCode), data.count <= 32 * 1024 * 1024
            else { return nil }
            // Decode at display resolution rather than retaining full-size
            // store originals for every small card. This runs off MainActor.
            return ImageDecoding.thumbnail(from: data, maxPixelSize: request.maxPixelSize)
        } catch { return nil }
    }
}
