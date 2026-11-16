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
                .map(GameFormatting.bytes)
        ) {
            GameOperationStatus(installations: installations, game: game.libraryID)
            GameAboutSection(
                summary: game.description,
                facts: game.developer.isEmpty ? [] : [.init(label: L10n.string("ui.game.developer"), value: game.developer)]
            )
        }
    }
}
