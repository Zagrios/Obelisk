import ImageIO
import SwiftUI

/// Decodes downloaded artwork at display size instead of retaining full originals.
enum ImageDecoding {
    nonisolated static func thumbnail(from data: Data, maxPixelSize: Int) -> CGImage? {
        guard let source = CGImageSourceCreateWithData(data as CFData, [kCGImageSourceShouldCache: false] as CFDictionary) else {
            return nil
        }
        return CGImageSourceCreateThumbnailAtIndex(source, 0, [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceThumbnailMaxPixelSize: maxPixelSize,
        ] as CFDictionary)
    }
}

enum StorefrontLibraryFilter: String, CaseIterable, Identifiable, Sendable {
    case all, installed, played

    var id: String { rawValue }

    var title: String {
        switch self {
        case .all: L10n.string("ui.filter.all")
        case .installed: L10n.string("ui.filter.installed")
        case .played: L10n.string("ui.filter.played")
        }
    }
}

/// The orders every storefront library offers, as its menu lists them.
enum StorefrontLibrarySort: String, CaseIterable, Identifiable, Sendable {
    case name, achievements, playtime, lastPlayed, dateAdded, size

    var id: String { rawValue }

    var title: String {
        switch self {
        case .name: L10n.string("model.library.sort.name")
        case .achievements: L10n.string("model.library.sort.achievements")
        case .playtime: L10n.string("model.library.sort.playtime")
        case .lastPlayed: L10n.string("model.library.sort.lastPlayed")
        case .dateAdded: L10n.string("model.library.sort.dateAdded")
        case .size: L10n.string("model.library.sort.size")
        }
    }

    var menuTitle: String {
        switch self {
        case .name: L10n.string("model.library.sortMenu.name")
        case .achievements: L10n.string("model.library.sortMenu.achievements")
        case .playtime: L10n.string("model.library.sortMenu.playtime")
        case .lastPlayed: L10n.string("model.library.sortMenu.lastPlayed")
        case .dateAdded: L10n.string("model.library.sortMenu.dateAdded")
        case .size: L10n.string("model.library.sortMenu.size")
        }
    }
}

/// How every storefront presents a game's history and size.
enum GameFormatting {
    static func playtime(_ playtime: Duration) -> String {
        let seconds = max(0, playtime.components.seconds)
        if seconds == 0 { return L10n.string("ui.game.never_played") }
        if seconds < 3600 { return L10n.string("ui.game.playtime_minutes", seconds / 60) }
        let hours = Double(seconds) / 3600
        return L10n.string("ui.game.playtime_hours", hours.formatted(.number.precision(.fractionLength(0...1)).locale(L10n.locale)))
    }

    static func lastPlayed(_ date: Date?) -> String {
        guard let date else { return L10n.string("ui.game.never_launched") }
        return date.formatted(.dateTime.day().month(.abbreviated).year().locale(L10n.locale))
    }

    static func bytes(_ value: UInt64) -> String {
        Int64(clamping: value).formatted(.byteCount(style: .file).locale(L10n.locale))
    }
}

/// Dismissable notice for a Keychain or receipt storage problem.
struct StorefrontWarningBanner: View {
    @Binding var message: String?

    var body: some View {
        if let message {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                Text(L10n.message(message)).font(.system(size: 12))
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 0)
                Button { self.message = nil } label: { Image(systemName: "xmark") }
                    .obeliskButtonStyle(.plain)
                    .accessibilityLabel(L10n.string("ui.common.dismiss_warning"))
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 13)
            .modifier(StorefrontPanelGlass(cornerRadius: 16, tint: .orange.opacity(0.12)))
        }
    }
}

/// A store whose search the library offers when no game it holds matches,
/// for a game the player does not own yet.
struct StoreSearch: Identifiable {
    let section: LibrarySection
    /// The store's search for a query, signed in as the account.
    let page: @MainActor (String) async -> WebPageStart

    var id: LibrarySection { section }
    var storeTitle: String { section.storeTitle }
}

/// Shown when the library is empty or no game matches its search and filter.
/// A search that finds nothing offers `stores`' own searches: the library's
/// looks only through the games the player owns.
struct StorefrontEmptyLibrary: View {
    @Binding var searchText: String
    @Binding var filter: StorefrontLibraryFilter
    let stores: [StoreSearch]
    let refresh: () -> Void
    @Environment(\.presentStore) private var presentStore

    init(searchText: Binding<String>, filter: Binding<StorefrontLibraryFilter> = .constant(.all),
         stores: [StoreSearch], refresh: @escaping () -> Void) {
        self._searchText = searchText
        self._filter = filter
        self.stores = stores
        self.refresh = refresh
    }

    private var query: String {
        searchText.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var hasCriteria: Bool {
        filter != .all || !query.isEmpty
    }

    /// The stores to search, once a search of the whole library found
    /// nothing: under a filter, the game may be owned.
    private var searchedStores: [StoreSearch] {
        query.isEmpty || filter != .all || presentStore == nil ? [] : stores
    }

    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: hasCriteria ? "magnifyingglass" : "gamecontroller")
                .font(.system(size: 36, weight: .light)).foregroundStyle(.secondary)
            Text(L10n.string(hasCriteria ? "ui.library.no_games_found" : "ui.library.empty"))
                .font(.system(size: 20, weight: .semibold))
            if !searchedStores.isEmpty {
                Text(L10n.string("ui.search.owned_only"))
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                HStack(spacing: 10) {
                    ForEach(searchedStores) { store in
                        Button(L10n.string("ui.search.store", store.storeTitle)) { search(store) }
                            .obeliskButtonStyle(.borderedProminent)
                            .accessibilityIdentifier("search-store-\(store.section.rawValue)")
                    }
                }
            }
            Button(L10n.string(filter != .all ? "ui.filter.show_all" : hasCriteria ? "ui.search.clear" : "ui.common.refresh")) {
                if hasCriteria {
                    searchText = ""
                    filter = .all
                } else {
                    refresh()
                }
            }
            .obeliskButtonStyle(.bordered)
        }
        .frame(maxWidth: .infinity, minHeight: 260)
    }

    /// Opens `store`'s search for the query over the whole window.
    private func search(_ store: StoreSearch) {
        let query = query
        presentStore?(StorePresentation(section: store.section) {
            await store.page(query)
        })
    }
}

/// A storefront's artwork at the location its catalog gives, filling its
/// frame, or neutral artwork without one.
struct StorefrontArtwork: View {
    let imageURL: String?
    var maxPixelSize = 768

    var body: some View {
        CachedArtwork(url: imageURL.flatMap(URL.init(string:)), maxPixelSize: maxPixelSize) { image in
            image.resizable().scaledToFill()
        } placeholder: {
            ArtworkPlaceholder()
        }
        .accessibilityHidden(true)
    }
}

/// Neutral artwork while an image loads or when a store publishes none.
struct ArtworkPlaceholder: View {
    var body: some View {
        ZStack {
            LinearGradient(colors: [Color.blue.opacity(0.18), Color.primary.opacity(0.04)], startPoint: .topLeading, endPoint: .bottomTrailing)
            Image(systemName: "gamecontroller")
                .font(.system(size: 34, weight: .light))
                .foregroundStyle(.secondary)
        }
    }
}

extension View {
    /// Presents `message` in an alert and clears it when dismissed.
    func errorAlert(_ title: String, message: Binding<String?>) -> some View {
        alert(
            title,
            isPresented: Binding(get: { message.wrappedValue != nil }, set: { if !$0 { message.wrappedValue = nil } })
        ) {
            Button(L10n.string("ui.common.ok"), role: .cancel) { message.wrappedValue = nil }
        } message: { Text(L10n.message(message.wrappedValue ?? "")) }
    }
}
