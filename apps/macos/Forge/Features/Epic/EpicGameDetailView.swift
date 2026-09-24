import SwiftUI

struct EpicGameDetailView: View {
    @Bindable var model: EpicModel
    let game: EpicGame

    private var installations: EpicInstallations { model.installations }

    var body: some View {
        GameDetailPage(
            model: model,
            game: game.libraryID,
            name: game.title,
            libraryTitle: L10n.string("ui.epic.library"),
            size: (installations.selectedReceipt?.totalBytes ?? model.selectedGameDetails?.installationBytes)
                .map(SteamFormatting.bytes),
            playtime: model.selectedGameDetails?.playtimeSeconds.map(EpicFormatting.playtime),
            back: model.closeGame
        ) {
            EpicArtwork(imageURL: game.headerImageUrl ?? game.imageUrl, maxPixelSize: 2048)
        } content: {
            GameOperationStatus(installations: installations, game: game.libraryID)
        }
    }
}

enum EpicFormatting {
    static func playtime(_ seconds: UInt64) -> String {
        if seconds == 0 { return L10n.string("ui.game.never_played") }
        if seconds < 3600 { return L10n.string("ui.game.playtime_minutes", Int64(seconds / 60)) }
        let hours = Double(seconds) / 3600
        return L10n.string("ui.game.playtime_hours", hours.formatted(.number.precision(.fractionLength(0...1)).locale(L10n.locale)))
    }
}
