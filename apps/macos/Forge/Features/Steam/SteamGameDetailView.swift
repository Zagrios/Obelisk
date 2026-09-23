import SwiftUI

struct SteamGameDetailView: View {
    @Bindable var model: SteamModel
    let game: SteamGame
    @Environment(\.colorScheme) private var colorScheme

    // This view is only shown for `model.selectedGame`, so the receipts agree.
    private var receipt: SteamInstallReceipt? { model.selectedInstallation }

    private var estimatedBytes: UInt64 {
        model.platform == "macos" ? model.details?.macosBytes ?? 0 : model.details?.windowsBytes ?? 0
    }

    private var gameTransfer: SteamTransfer? {
        model.transfer.flatMap { $0.appId == game.appId ? $0 : nil }
    }

    var body: some View {
        @Bindable var achievements = model.achievements
        VStack(alignment: .leading, spacing: 23) {
            GameDetailHeader(
                title: game.name,
                libraryTitle: L10n.string("ui.steam.library"),
                playtime: SteamFormatting.playtime(game.playtimeMinutes),
                lastPlayed: SteamFormatting.lastPlayed(game.lastPlayed),
                size: sizeText,
                back: model.backToLibrary,
                additionalMetrics: {
                    GameAchievementsButton(model: achievements)
                    if let cloud = model.cloudIndicator {
                        CloudIndicatorButton(presentation: cloud, canSync: model.canSyncCloud, sync: model.syncCloud)
                    }
                }
            ) {
                SteamArtwork(appId: game.appId, portrait: false, iconHash: game.iconHash)
            } action: {
                HStack(spacing: 10) {
                    let cancelsSynchronization = model.cloud.canCancelSynchronization(for: game.appId)
                    GameActionButton(
                        title: model.primaryActionTitle,
                        symbol: model.primaryActionSymbol,
                        transfer: gameTransfer,
                        isTransferring: model.activeTransferAppId == game.appId,
                        isAvailable: model.primaryActionAvailable,
                        action: model.primaryAction,
                        cancel: {
                            if cancelsSynchronization { model.cloud.respond(.cancel) }
                            else { model.cancelDownload() }
                        },
                        isCancellable: cancelsSynchronization,
                        isIndeterminate: model.isSynchronizingSaves
                    )
                    if let receipt {
                        GameManagementButton(
                            gameName: game.name,
                            isAvailable: model.canUninstallSelectedGame,
                            uninstall: { model.uninstall(receipt) },
                            directory: receipt.directory
                        )
                        .id(receipt.id + receipt.directory)
                    }
                }
            }

            if let message = model.gameSessionMessage {
                Text(message).font(.system(size: 12)).foregroundStyle(.secondary)
            }


            if model.isLoadingDetails {
                ProgressView(L10n.string("ui.steam.finding_files")).controlSize(.small)
            } else if let error = model.detailsError {
                HStack {
                    Text(L10n.message(error)).font(.system(size: 12)).foregroundStyle(.secondary)
                    Button(L10n.string("ui.common.retry"), action: model.reloadDetails)
                        .forgeButtonStyle(.borderless)
                }
            }

            if let transfer = gameTransfer, transfer.state == .failed || transfer.state == .cancelled {
                Text(transfer.message.isEmpty ? transfer.activityTitle : L10n.message(transfer.message))
                    .font(.system(size: 12)).foregroundStyle(.secondary)
            } else if let activeAppId = model.activeTransferAppId, activeAppId != game.appId {
                Text(L10n.string("ui.steam.other_game_operation"))
                    .font(.system(size: 12)).foregroundStyle(.secondary)
            }

            newsSection
        }
        .sheet(isPresented: $achievements.isPresented) {
            GameAchievementsView(model: achievements, gameName: game.name, retry: model.reloadAchievements)
        }
    }

    private var sizeText: String {
        let bytes = receipt?.totalBytes ?? estimatedBytes
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
                    .background(ForgePalette(colorScheme: colorScheme).surface, in: RoundedRectangle(cornerRadius: 10))
                }
            }
        }
        .padding(.top, 5)
    }

}
