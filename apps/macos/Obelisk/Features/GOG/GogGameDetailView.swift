import SwiftUI

struct GogGameDetailView: View {
    @Bindable var model: GogModel
    let game: GogGame
    let context: GamePageContext<StorefrontArtwork, StorefrontArtwork>

    private var installations: GogInstallations { model.installations }

    var body: some View {
        GameDetailPage(
            model: model,
            game: game,
            context: context,
            size: (installations.selectedReceipt?.totalBytes ?? model.selectedBuildBytes)
                .map(GameFormatting.bytes)
        ) {
            GameOperationStatus(installations: installations, game: game.productId)
            GameAboutSection(summary: game.summary, facts: facts)
        }
    }

    /// What gamesdb tells of the game, in the player's language where it can.
    private var facts: [GameAboutSection.Fact] {
        var facts: [GameAboutSection.Fact] = []
        func add(_ key: String, _ values: [String]) {
            let values = values.filter { !$0.isEmpty }
            guard !values.isEmpty else { return }
            facts.append(.init(label: L10n.string(key), value: values.formatted(.list(type: .and).locale(L10n.locale))))
        }
        add("ui.game.developer", game.developers)
        add("ui.game.publisher", game.publishers)
        if let released = game.releaseDate {
            var style = Date.FormatStyle(date: .long, time: .omitted, locale: L10n.locale)
            // gamesdb dates a release at midnight UTC.
            style.timeZone = .gmt
            add("ui.game.release_date", [Date(timeIntervalSince1970: TimeInterval(released)).formatted(style)])
        }
        add("ui.game.genres", game.genres)
        if let rating = game.rating {
            add("ui.game.rating", [L10n.string("ui.game.rating_value", Int64(rating))])
        }
        add("ui.game.languages", languages)
        return facts
    }

    /// The game's languages, named in the player's language, once each.
    private var languages: [String] {
        var seen = Set<String>()
        return game.languages.compactMap { code in
            let language = Locale(identifier: code).language.languageCode?.identifier ?? code
            guard seen.insert(language).inserted else { return nil }
            return L10n.locale.localizedString(forLanguageCode: language)
        }
    }
}
