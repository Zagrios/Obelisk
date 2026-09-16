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
    static let gridColumns = [GridItem(.adaptive(minimum: 145, maximum: 210), spacing: 18)]
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
                    .buttonStyle(.plain).accessibilityLabel(L10n.string("ui.common.dismiss_warning"))
            }
            .padding(13)
            .background(.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
        }
    }
}

/// Shown when the connected library has no game, or none matches the search.
struct StorefrontEmptyLibrary: View {
    @Binding var searchText: String
    let refresh: () -> Void

    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: searchText.isEmpty ? "gamecontroller" : "magnifyingglass")
                .font(.system(size: 36, weight: .light)).foregroundStyle(.secondary)
            Text(searchText.isEmpty ? L10n.string("ui.library.empty") : L10n.string("ui.library.no_games_found"))
                .font(.system(size: 20, weight: .semibold))
            Button(searchText.isEmpty ? L10n.string("ui.common.refresh") : L10n.string("ui.search.clear")) {
                if searchText.isEmpty { refresh() } else { searchText = "" }
            }
            .buttonStyle(.bordered)
        }
        .frame(maxWidth: .infinity, minHeight: 260)
    }
}

/// Portrait artwork with a hover border, a title and an optional second line.
struct LibraryGridCard<Artwork: View>: View {
    let title: String
    var subtitle: String?
    let open: () -> Void
    @ViewBuilder let artwork: Artwork
    @State private var hovered = false

    var body: some View {
        Button(action: open) {
            VStack(alignment: .leading, spacing: 9) {
                Color.clear
                    .aspectRatio(2.0 / 3.0, contentMode: .fit)
                    .overlay { artwork }
                    .clipShape(RoundedRectangle(cornerRadius: 9))
                    .overlay { RoundedRectangle(cornerRadius: 9).strokeBorder(hovered ? ForgeStyle.accent.opacity(0.7) : .primary.opacity(0.08), lineWidth: 1) }
                Text(title).font(.system(size: 13, weight: .semibold)).foregroundStyle(.primary).lineLimit(2)
                if let subtitle {
                    Text(subtitle).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
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
