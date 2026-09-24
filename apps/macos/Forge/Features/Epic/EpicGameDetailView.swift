import SwiftUI

struct EpicGameDetailView: View {
    @Bindable var model: EpicModel
    let game: EpicGame

    private var installations: EpicInstallations { model.installations }

    private var gameTransfer: GameTransfer? {
        installations.transferGame == game.libraryID ? installations.transfer : nil
    }

    var body: some View {
        @Bindable var achievements = model.achievements
        VStack(alignment: .leading, spacing: 23) {
            GameDetailHeader(
                title: game.title,
                libraryTitle: L10n.string("ui.epic.library"),
                playtime: model.selectedGameDetails?.playtimeSeconds.map(EpicFormatting.playtime),
                size: (installations.selectedReceipt?.totalBytes ?? model.selectedGameDetails?.installationBytes).map(SteamFormatting.bytes),
                back: model.closeGame,
                additionalMetrics: {
                    GameAchievementsButton(model: achievements)
                    if let cloud = installations.cloudIndicator {
                        CloudIndicatorButton(presentation: cloud, canSync: installations.canSyncCloud, sync: model.syncCloud,
                                             storefront: "epic")
                    }
                }
            ) {
                EpicArtwork(imageURL: game.headerImageUrl ?? game.imageUrl, maxPixelSize: 2048)
            } action: {
                HStack(spacing: 10) {
                    let cancelsSynchronization = model.cloud.canCancelSynchronization(for: game.libraryID)
                    GameActionButton(
                        title: installations.primaryActionTitle,
                        symbol: installations.primaryActionSymbol,
                        transfer: gameTransfer,
                        isTransferring: installations.activeTransferGame == game.libraryID,
                        isAvailable: installations.primaryActionAvailable,
                        action: installations.primaryAction,
                        cancel: {
                            if cancelsSynchronization { model.cloud.respond(.cancel) }
                            else { installations.cancelDownload() }
                        },
                        isCancellable: cancelsSynchronization
                            || (installations.isLaunching && installations.launchGame == game.libraryID),
                        isIndeterminate: installations.isSynchronizingSaves
                    )
                    if let receipt = installations.selectedReceipt {
                        GameManagementButton(
                            gameName: game.title,
                            isAvailable: installations.canUninstallSelectedGame,
                            uninstall: { installations.uninstall(receipt) },
                            directory: receipt.directory
                        )
                        .id(receipt.id + receipt.directory)
                    }
                }
            }

            if let message = model.gameSessionMessage {
                Text(message).font(.system(size: 12)).foregroundStyle(.secondary)
            }


            if let transfer = gameTransfer, transfer.state == .failed || transfer.state == .cancelled {
                Text(transfer.message.isEmpty ? transfer.activityTitle : L10n.message(transfer.message))
                    .font(.system(size: 12)).foregroundStyle(.secondary)
            } else if let activeGame = installations.activeTransferGame ?? installations.launchGame,
                      activeGame != game.libraryID {
                Text(L10n.string("ui.epic.other_game_operation"))
                    .font(.system(size: 12)).foregroundStyle(.secondary)
            } else if installations.selectedReceipt?.needsVerification == true,
                      !installations.isDownloading, !installations.isLaunching {
                Label(L10n.string("ui.epic.verification_required"), systemImage: "checkmark.shield")
                    .font(.system(size: 12)).foregroundStyle(.secondary)
            }
        }
        .sheet(isPresented: $achievements.isPresented) {
            GameAchievementsView(model: achievements, gameName: game.title, retry: model.reloadAchievements)
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
