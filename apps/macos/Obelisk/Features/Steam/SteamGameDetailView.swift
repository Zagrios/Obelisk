import SwiftUI

struct SteamGameDetailView: View {
    @Bindable var model: SteamModel
    let game: SteamGame
    let context: GamePageContext<SteamArtwork, SteamArtwork>

    private var installations: SteamInstallations { model.installations }

    private var estimatedBytes: UInt64 {
        let platform = installations.selectedBuildChoice?.chosen ?? model.platform
        return platform == .mac ? model.details?.macosBytes ?? 0 : model.details?.windowsBytes ?? 0
    }

    private static var newsColumns: [GridItem] {
        [GridItem(.adaptive(minimum: 340), spacing: 16, alignment: .top)]
    }

    var body: some View {
        GameDetailPage(
            model: model,
            game: game,
            context: context,
            size: sizeText,
            showsUnplayed: true,
            headline: headline
        ) {
            if model.isLoadingDetails {
                ProgressView(L10n.string("ui.steam.finding_files")).controlSize(.small)
            } else if let error = model.detailsError {
                HStack {
                    Text(L10n.message(error)).font(.system(size: 12)).foregroundStyle(.secondary)
                    Button(L10n.string("ui.common.retry"), action: model.reloadDetails)
                        .obeliskButtonStyle(.glass)
                        .controlSize(.small)
                }
            }

            GameOperationStatus(installations: installations, game: game.appId)

            // What Wikipedia tells of the game, else its store's own words.
            GameOverview(
                summary: model.article?.summary ?? model.storePage?.summary ?? "",
                summarySource: model.article.flatMap { URL(string: $0.summaryUrl) },
                history: model.article?.history ?? [],
                historySource: model.article?.historyUrl.flatMap(URL.init(string:)),
                media: media,
                facts: facts
            )

            newsSection
        }
    }

    /// What the game's store page tells of it, in the player's language.
    private var facts: [GameFact] {
        guard let page = model.storePage else { return [] }
        return [
            GameFact("ui.game.developer", page.developers),
            GameFact("ui.game.publisher", page.publishers),
            GameFact("ui.game.release_date", page.releaseDate.map { [$0] } ?? []),
            GameFact("ui.game.genres", page.genres),
            page.metacritic.flatMap {
                GameFact("ui.game.rating", [L10n.string("ui.game.rating_value", Int64($0))])
            },
        ].compactMap(\.self)
    }

    /// The store page at a glance: its score, the developer, the genres, the
    /// year, then how the game plays.
    private var headline: GameHeadline {
        guard let page = model.storePage else { return GameHeadline() }
        let genres = page.genres.prefix(2).joined(separator: ", ")
        return GameHeadline(
            score: page.metacritic.map(Int.init),
            facts: [page.developers.first, genres.isEmpty ? nil : genres, GameHeadline.year(in: page.releaseDate)]
                .compactMap(\.self),
            features: page.features
        )
    }

    /// The store page's trailers, then its screenshots.
    private var media: [GameMedia] {
        guard let page = model.storePage else { return [] }
        let trailers = page.trailers.compactMap { trailer -> GameMedia? in
            guard let thumbnail = URL(string: trailer.thumbnail), let stream = URL(string: trailer.stream) else { return nil }
            return .trailer(name: trailer.name, thumbnail: thumbnail, stream: stream)
        }
        let screenshots = page.screenshots.compactMap { shot -> GameMedia? in
            guard let thumbnail = URL(string: shot.thumbnail), let full = URL(string: shot.full) else { return nil }
            return .screenshot(thumbnail: thumbnail, full: full)
        }
        return trailers + screenshots
    }

    // This view is only shown for `model.selectedGame`, so the receipts agree.
    private var sizeText: String? {
        let bytes = installations.selectedReceipt?.totalBytes ?? estimatedBytes
        return bytes > 0 ? GameFormatting.bytes(bytes) : nil
    }

    private var newsSection: some View {
        GamePageSection(title: L10n.string("ui.game.news")) {
            if model.isLoadingNews {
                ProgressView(L10n.string("ui.game.loading_news")).controlSize(.small)
            } else if let error = model.newsError {
                Text(L10n.message(error)).font(.system(size: 12)).foregroundStyle(.secondary)
            } else if model.news.isEmpty {
                Text(L10n.string("ui.game.no_recent_news")).font(.system(size: 12)).foregroundStyle(.secondary)
            } else {
                LazyVGrid(columns: Self.newsColumns, alignment: .leading, spacing: 16) {
                    ForEach(model.news, id: \.id) { article in
                        newsCard(article)
                    }
                }
            }
        }
    }

    private func newsCard(_ article: SteamNews) -> some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack(spacing: 8) {
                Text(article.source)
                Text("·")
                Text(Date(timeIntervalSince1970: TimeInterval(article.date)), format: .dateTime.day().month(.abbreviated).year().locale(L10n.locale))
            }
            .font(.system(size: 10, weight: .medium)).foregroundStyle(.secondary)
            Text(article.title).font(.system(size: 15, weight: .semibold))
            if !article.excerpt.isEmpty {
                Text(article.excerpt).font(.system(size: 12)).foregroundStyle(.secondary).lineLimit(5)
            }
            Spacer(minLength: 0)
            if let url = URL(string: article.url), url.scheme == "https", url.host != nil {
                Link(destination: url) { Label(L10n.string("ui.game.read_article"), systemImage: "arrow.up.right") }
                    .font(.system(size: 11, weight: .semibold))
                    .pointerCursor()
            }
        }
        .modifier(GamePageCard())
    }
}
