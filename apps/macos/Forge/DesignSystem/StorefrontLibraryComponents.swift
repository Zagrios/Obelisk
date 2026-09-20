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

/// Building blocks shared by every storefront library (Steam, Epic, later GOG).
enum StorefrontLibrary {
    static let gridColumns = [GridItem(.adaptive(minimum: 145, maximum: 210), spacing: 18, alignment: .top)]
}

enum StorefrontLibraryFilter: String, CaseIterable, Identifiable, Sendable {
    case all, installed, played, macOS

    var id: String { rawValue }

    var title: String {
        switch self {
        case .all: L10n.string("ui.filter.all")
        case .installed: L10n.string("ui.filter.installed")
        case .played: L10n.string("ui.filter.played")
        case .macOS: L10n.string("ui.filter.macos")
        }
    }
}

/// Dismissable notice for a Keychain or receipt storage problem.
struct StorefrontWarningBanner: View {
    @Binding var message: String?

    var body: some View {
        if let message {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: "exclamationmark.triangle").foregroundStyle(.orange)
                Text(L10n.message(message)).font(.system(size: 12))
                Spacer(minLength: 0)
                Button { self.message = nil } label: { Image(systemName: "xmark") }
                    .forgeButtonStyle(.plain)
                    .accessibilityLabel(L10n.string("ui.common.dismiss_warning"))
            }
            .padding(13)
            .background(.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
        }
    }
}

/// Shown when the library is empty or no game matches its search and filter.
struct StorefrontEmptyLibrary: View {
    @Binding var searchText: String
    @Binding var filter: StorefrontLibraryFilter
    let refresh: () -> Void

    init(searchText: Binding<String>, filter: Binding<StorefrontLibraryFilter> = .constant(.all), refresh: @escaping () -> Void) {
        self._searchText = searchText
        self._filter = filter
        self.refresh = refresh
    }

    private var hasCriteria: Bool {
        filter != .all || !searchText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: hasCriteria ? "magnifyingglass" : "gamecontroller")
                .font(.system(size: 36, weight: .light)).foregroundStyle(.secondary)
            Text(L10n.string(hasCriteria ? "ui.library.no_games_found" : "ui.library.empty"))
                .font(.system(size: 20, weight: .semibold))
            Button(L10n.string(filter != .all ? "ui.filter.show_all" : hasCriteria ? "ui.search.clear" : "ui.common.refresh")) {
                if hasCriteria {
                    searchText = ""
                    filter = .all
                } else {
                    refresh()
                }
            }
            .forgeButtonStyle(.bordered)
        }
        .frame(maxWidth: .infinity, minHeight: 260)
    }
}

/// Portrait artwork with a hover border, a title and an optional second line.
struct LibraryGridCard<Artwork: View>: View {
    let title: String
    var subtitle: String?
    var platform: GamePlatform?
    let open: () -> Void
    @ViewBuilder let artwork: Artwork
    @State private var hovered = false

    var body: some View {
        Button(action: open) {
            VStack(alignment: .leading, spacing: 9) {
                Color.clear
                    .aspectRatio(2.0 / 3.0, contentMode: .fit)
                    .overlay { artwork }
                    .overlay(alignment: .bottomTrailing) {
                        if hovered, let platform {
                            GamePlatformBadge(platform: platform)
                                .padding(10)
                        }
                    }
                    .clipShape(RoundedRectangle(cornerRadius: 9))
                    .overlay { RoundedRectangle(cornerRadius: 9).strokeBorder(hovered ? ForgeStyle.accent.opacity(0.7) : .primary.opacity(0.08), lineWidth: 1) }
                Text(title).font(.system(size: 13, weight: .semibold)).foregroundStyle(.primary).lineLimit(2)
                if let subtitle {
                    Text(subtitle).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            .contentShape(Rectangle())
        }
        .forgeButtonStyle(.plain)
        .accessibilityValue(platform?.title ?? "")
        .onHover { hovered = $0 }
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
