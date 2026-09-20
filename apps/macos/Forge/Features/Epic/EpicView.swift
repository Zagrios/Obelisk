import AppKit
import ImageIO
import SwiftUI

struct EpicView: View {
    @Bindable var model: EpicModel

    var body: some View {
        StorefrontPage { titleBar } content: {
            VStack(alignment: .leading, spacing: 25) {
                StorefrontWarningBanner(message: $model.storageWarning)
                if model.isConnected {
                    if let game = model.selectedGame {
                        EpicGameDetailView(model: model, game: game)
                    } else { library }
                } else { EpicLoginView(model: model).padding(.top, 28) }
            }
            .padding(ForgeStyle.contentPadding)
            .padding(.top, 6)
            .frame(maxWidth: 1400, alignment: .leading)
        }
        .task { await model.start() }
        .errorAlert(L10n.string("ui.store.epic"), message: $model.errorMessage)
    }

    private var titleBar: some View {
        StorefrontTitleBar(
            title: L10n.string("ui.store.epic"),
            accountName: model.accountName,
            searchText: $model.searchText,
            sortOptions: [StorefrontSortOption(id: "name", title: L10n.string("ui.sort.by_name"), isSelected: true, action: {})],
            filter: $model.filter,
            availableFilters: model.availableFilters,
            showsLibraryControls: model.isConnected && model.selectedGame == nil,
            isConnected: model.isConnected
        ) {
            Text(model.accountName ?? L10n.string("ui.epic.account"))
            Text(L10n.string("ui.library.account_game_count", Int64(model.games.count)))
            Divider()
            Button(model.friends?.menuTitle ?? L10n.string("ui.friends.menuLoading"), systemImage: "person.2", action: model.openFriends)
                .disabled(model.friends == nil)
                .accessibilityIdentifier("epic-friends-menu")
            Button(L10n.string("ui.library.refresh"), systemImage: "arrow.clockwise", action: model.refresh)
                .disabled(model.isLoadingGames)
            Toggle(L10n.string("ui.account.remember_me"), isOn: $model.rememberSession)
            Divider()
            Button(L10n.string("ui.account.sign_out"), systemImage: "rectangle.portrait.and.arrow.right", action: model.signOut)
        }
    }

    private var library: some View {
        VStack(alignment: .leading, spacing: 24) {
            if let transfer = model.transfer, model.isDownloading {
                SteamTransferView(transfer: transfer, isDownloading: model.isDownloading, cancel: model.cancelDownload)
            }
            let filtered = model.filteredGames
            if model.isLoadingGames && model.games.isEmpty {
                ProgressView(L10n.string("ui.library.fetching")).frame(maxWidth: .infinity, minHeight: 260)
            } else if filtered.isEmpty {
                StorefrontEmptyLibrary(searchText: $model.searchText, filter: $model.filter, refresh: model.refresh)
            } else {
                LazyVGrid(columns: StorefrontLibrary.gridColumns, alignment: .leading, spacing: 24) {
                    ForEach(filtered, id: \.libraryID) { game in
                        LibraryGridCard(
                            title: game.title,
                            subtitle: game.developer.isEmpty ? nil : game.developer,
                            platform: .preferred(macos: game.macos, windows: game.windows),
                            open: { model.open(game) }
                        ) {
                            EpicArtwork(imageURL: game.imageUrl)
                        }
                        .help(game.title)
                        .accessibilityLabel(game.title)
                    }
                }
                if model.isLoadingGames { ProgressView().controlSize(.small) }
            }
        }
    }
}

struct EpicArtwork: View {
    let imageURL: String?
    var maxPixelSize = 768
    @State private var loaded: (request: EpicArtworkCache.Request, image: NSImage)?

    private var request: EpicArtworkCache.Request? {
        imageURL.flatMap(URL.init(string:)).map { EpicArtworkCache.Request(url: $0, maxPixelSize: maxPixelSize) }
    }

    private var image: NSImage? {
        guard let request else { return nil }
        if loaded?.request == request { return loaded?.image }
        // Read synchronously: restoring the cache inside .task would still
        // show a placeholder for the grid's first frame after going back.
        return EpicArtworkCache.shared.cachedImage(for: request)
    }

    var body: some View {
        Group {
            if let image { Image(nsImage: image).resizable().scaledToFill() }
            else { ArtworkPlaceholder() }
        }
        .accessibilityHidden(true)
        .task(id: request) {
            guard let request else { loaded = nil; return }
            let image = await EpicArtworkCache.shared.image(for: request)
            guard !Task.isCancelled else { return }
            loaded = image.map { (request, $0) }
        }
    }
}

/// Keeps decoded public artwork available when the library grid is recreated.
@MainActor
final class EpicArtworkCache {
    struct Request: Hashable, Sendable {
        let url: URL
        let maxPixelSize: Int

        var cacheKey: NSString { "\(maxPixelSize):\(url.absoluteString)" as NSString }
    }

    static let shared = EpicArtworkCache()
    private let images = NSCache<NSString, NSImage>()
    private var pending: [Request: Task<NSImage?, Never>] = [:]
    private let load: @Sendable (Request) async -> CGImage?

    init(load: @escaping @Sendable (Request) async -> CGImage? = EpicArtworkCache.download) {
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
