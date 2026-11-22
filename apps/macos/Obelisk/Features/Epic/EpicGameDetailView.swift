import SwiftUI

struct EpicGameDetailView: View {
    @Bindable var model: EpicModel
    let game: EpicGame
    let context: GamePageContext<StorefrontArtwork, StorefrontArtwork>

    private var installations: EpicInstallations { model.installations }

    var body: some View {
        GameDetailPage(
            model: model,
            game: game,
            context: context,
            size: (installations.selectedReceipt?.totalBytes ?? model.selectedBuildBytes)
                .map(GameFormatting.bytes),
            headline: GameHeadline(facts: game.developer.isEmpty ? [] : [game.developer])
        ) {
            GameOperationStatus(installations: installations, game: game.libraryID)
            GameOverview(
                summary: game.description,
                facts: [GameFact("ui.game.developer", [game.developer])].compactMap(\.self)
            )
        }
    }
}
