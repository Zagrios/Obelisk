import SwiftUI

struct SteamGameDetailView: View {
    @Bindable var model: SteamModel
    let game: SteamGame
    @Environment(\.colorScheme) private var colorScheme

    private var installations: SteamInstallations { model.installations }

    private var estimatedBytes: UInt64 {
        model.platform == "macos" ? model.details?.macosBytes ?? 0 : model.details?.windowsBytes ?? 0
    }

    var body: some View {
        GameDetailPage(
            model: model,
            game: game.appId,
            name: game.name,
            libraryTitle: L10n.string("ui.steam.library"),
            size: sizeText,
            lastPlayed: SteamFormatting.lastPlayed(game.lastPlayed),
            playtime: SteamFormatting.playtime(game.playtimeMinutes),
            back: model.backToLibrary
        ) {
            SteamArtwork(appId: game.appId, portrait: false, iconHash: game.iconHash)
        } content: {
            if model.isLoadingDetails {
                ProgressView(L10n.string("ui.steam.finding_files")).controlSize(.small)
            } else if let error = model.detailsError {
                HStack {
                    Text(L10n.message(error)).font(.system(size: 12)).foregroundStyle(.secondary)
                    Button(L10n.string("ui.common.retry"), action: model.reloadDetails)
                        .sailOverButtonStyle(.borderless)
                }
            }

            GameOperationStatus(installations: installations, game: game.appId)

            newsSection
        }
    }

    // This view is only shown for `model.selectedGame`, so the receipts agree.
    private var sizeText: String {
        let bytes = installations.selectedReceipt?.totalBytes ?? estimatedBytes
        return bytes > 0 ? SteamFormatting.bytes(bytes) : "—"
    }

    private var newsSection: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(L10n.string("ui.game.news")).font(.system(size: 20, weight: .semibold))
            if model.isLoadingNews {
                ProgressView(L10n.string("ui.game.loading_news")).controlSize(.small)
            } else if let error = model.newsError {
                Text(L10n.message(error)).font(.system(size: 12)).foregroundStyle(.secondary)
            } else if model.news.isEmpty {
                Text(L10n.string("ui.game.no_recent_news")).font(.system(size: 12)).foregroundStyle(.secondary)
            } else {
                ForEach(model.news, id: \.id) { article in
                    VStack(alignment: .leading, spacing: 9) {
                        HStack(spacing: 8) {
                            Text(article.source)
                            Text("·")
                            Text(Date(timeIntervalSince1970: TimeInterval(article.date)), format: .dateTime.day().month(.abbreviated).year().locale(L10n.locale))
                        }
                        .font(.system(size: 10)).foregroundStyle(.secondary)
                        Text(article.title).font(.system(size: 15, weight: .semibold))
                        if !article.excerpt.isEmpty {
                            Text(article.excerpt).font(.system(size: 12)).foregroundStyle(.secondary).lineLimit(5)
                        }
                        if let url = URL(string: article.url), url.scheme == "https", url.host != nil {
                            Link(destination: url) { Label(L10n.string("ui.game.read_article"), systemImage: "arrow.up.right") }
                                .font(.system(size: 11, weight: .medium))
                                .pointerCursor()
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(17)
                    .background(SailOverPalette(colorScheme: colorScheme).surface, in: RoundedRectangle(cornerRadius: 10))
                }
            }
        }
        .padding(.top, 5)
    }

}
