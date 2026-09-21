import AppKit
import SwiftUI

struct EpicGameDetailView: View {
    @Bindable var model: EpicModel
    let game: EpicGame

    private var gameTransfer: EpicTransfer? {
        model.transferLibraryID == game.libraryID ? model.transfer : nil
    }

    var body: some View {
        @Bindable var achievements = model.achievements
        VStack(alignment: .leading, spacing: 23) {
            GameDetailHeader(
                title: game.title,
                libraryTitle: L10n.string("ui.epic.library"),
                playtime: EpicFormatting.playtime(model.selectedGameDetails?.playtimeSeconds),
                lastPlayed: "—",
                size: (model.selectedInstallation?.totalBytes ?? model.selectedGameDetails?.installationBytes).map(SteamFormatting.bytes) ?? "—",
                back: model.closeGame,
                additionalMetrics: { GameAchievementsButton(model: model.achievements) }
            ) {
                EpicArtwork(imageURL: game.headerImageUrl ?? game.imageUrl, maxPixelSize: 2048)
            } action: {
                HStack(spacing: 10) {
                    SteamGameActionButton(
                        title: model.primaryActionTitle,
                        symbol: model.primaryActionSymbol,
                        transfer: gameTransfer,
                        isTransferring: model.activeTransferLibraryID == game.libraryID,
                        isAvailable: model.primaryActionAvailable,
                        action: model.primaryAction,
                        cancel: model.cancelDownload,
                        isCancellable: model.isLaunching && model.launchLibraryID == game.libraryID
                    )
                    if let receipt = model.selectedInstallation {
                        GameManagementButton(gameName: game.title, isAvailable: model.canUninstallSelectedGame) {
                            model.uninstall(receipt)
                        }
                        .id(receipt.id + receipt.directory)
                    }
                }
            }

            if let transfer = gameTransfer, transfer.state == .failed || transfer.state == .cancelled {
                Text(transfer.message.isEmpty ? transfer.activityTitle : L10n.message(transfer.message))
                    .font(.system(size: 12)).foregroundStyle(.secondary)
            } else if let activeGame = model.activeTransferLibraryID ?? model.launchLibraryID, activeGame != game.libraryID {
                Text(L10n.string("ui.epic.other_game_operation"))
                    .font(.system(size: 12)).foregroundStyle(.secondary)
            } else if let receipt = model.selectedInstallation, !model.isDownloading && !model.isLaunching {
                Label(L10n.string(receipt.needsVerification ? "ui.epic.verification_required" : "ui.epic.installed"),
                      systemImage: receipt.needsVerification ? "checkmark.shield" : "checkmark.circle")
                    .font(.system(size: 12)).foregroundStyle(.secondary)
                Button(L10n.string("ui.epic.show_files"), systemImage: "folder") {
                    NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: receipt.directory)
                }
                .forgeButtonStyle(.borderless)
            }

            VStack(alignment: .leading, spacing: 16) {
                Text(L10n.string("ui.game.news")).font(.system(size: 20, weight: .semibold))
                Text(L10n.string("ui.game.no_news_available")).font(.system(size: 12)).foregroundStyle(.secondary)
            }
            .padding(.top, 5)

            if !game.description.isEmpty || !game.developer.isEmpty || !game.platforms.isEmpty {
                VStack(alignment: .leading, spacing: 16) {
                    Text(L10n.string("ui.game.about")).font(.system(size: 20, weight: .semibold))
                    if !game.developer.isEmpty {
                        Text(game.developer).font(.system(size: 13)).foregroundStyle(.secondary)
                    }
                    if !game.platforms.isEmpty {
                        Label(game.platforms, systemImage: "desktopcomputer")
                            .font(.system(size: 12)).foregroundStyle(.secondary)
                    }
                    if !game.description.isEmpty {
                        Text(game.description).font(.system(size: 14)).textSelection(.enabled)
                    }
                }
            }
        }
        .sheet(isPresented: $achievements.isPresented) {
            GameAchievementsView(model: model.achievements, gameName: game.title, retry: model.reloadAchievements)
        }
    }
}

enum EpicFormatting {
    static func playtime(_ seconds: UInt64?) -> String {
        guard let seconds else { return "—" }
        if seconds == 0 { return L10n.string("ui.game.never_played") }
        if seconds < 3600 { return L10n.string("ui.game.playtime_minutes", Int64(seconds / 60)) }
        let hours = Double(seconds) / 3600
        return L10n.string("ui.game.playtime_hours", hours.formatted(.number.precision(.fractionLength(0...1)).locale(L10n.locale)))
    }
}
