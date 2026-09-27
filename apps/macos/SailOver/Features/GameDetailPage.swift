import SwiftUI

/// What a storefront model gives its game page.
@MainActor protocol GameDetailModel: AnyObject {
    associatedtype Store: InstallationStorefront
    var installations: GameInstallations<Store> { get }
    var achievements: GameAchievementsModel { get }
    var gameSessionMessage: String? { get }
    func syncCloud()
    func reloadAchievements()
}

extension SteamModel: GameDetailModel {}
extension EpicModel: GameDetailModel {}

/// A storefront's game page: the header and its launch bar, the Game session's
/// message, then what only the storefront shows.
struct GameDetailPage<Model: GameDetailModel, Artwork: View, Content: View>: View {
    let model: Model
    let game: Model.Store.GameID
    let name: String
    let libraryTitle: String
    let size: String?
    let lastPlayed: String?
    let playtime: String?
    let back: () -> Void
    private let artwork: Artwork
    private let content: Content

    init(
        model: Model,
        game: Model.Store.GameID,
        name: String,
        libraryTitle: String,
        size: String?,
        lastPlayed: String? = nil,
        playtime: String?,
        back: @escaping () -> Void,
        @ViewBuilder artwork: () -> Artwork,
        @ViewBuilder content: () -> Content
    ) {
        self.model = model
        self.game = game
        self.name = name
        self.libraryTitle = libraryTitle
        self.size = size
        self.lastPlayed = lastPlayed
        self.playtime = playtime
        self.back = back
        self.artwork = artwork()
        self.content = content()
    }

    private var installations: GameInstallations<Model.Store> { model.installations }

    var body: some View {
        @Bindable var achievements = model.achievements
        VStack(alignment: .leading, spacing: 23) {
            GameDetailHeader(
                title: name,
                libraryTitle: libraryTitle,
                metrics: GameMetrics(
                    isInstalled: installations.selectedReceipt != nil,
                    size: size,
                    lastPlayed: lastPlayed,
                    playtime: playtime,
                    cloud: installations.cloudIndicator.map {
                        CloudIndicatorButton(presentation: $0, canSync: installations.canSyncCloud, sync: model.syncCloud)
                    },
                    achievements: achievements
                ),
                back: back
            ) {
                artwork
            } action: {
                GamePrimaryAction(installations: installations, game: game)
            } tools: {
                if let receipt = installations.selectedReceipt {
                    GameManagementButton(
                        gameName: name,
                        isAvailable: installations.canUninstallSelectedGame,
                        uninstall: { installations.uninstall(receipt) },
                        directory: receipt.directory
                    )
                    .id(receipt.id + receipt.directory)
                }
            }

            if let message = model.gameSessionMessage {
                Text(message).font(.system(size: 12)).foregroundStyle(.secondary)
            }

            content
        }
        .sheet(isPresented: $achievements.isPresented) {
            GameAchievementsView(model: achievements, gameName: name, retry: model.reloadAchievements)
        }
    }
}

/// The game's primary action. It reads the Transfer progress in its own body,
/// so a progress tick redraws only the button.
private struct GamePrimaryAction<Store: InstallationStorefront>: View {
    let installations: GameInstallations<Store>
    let game: Store.GameID

    var body: some View {
        let cancelsSynchronization = installations.cloud.canCancelSynchronization(for: game)
        GameActionButton(
            title: installations.primaryActionTitle,
            launches: installations.primaryActionLaunches,
            transfer: installations.transfer(for: game),
            isTransferring: installations.activeTransferGame == game,
            isAvailable: installations.primaryActionAvailable,
            action: installations.primaryAction,
            cancel: {
                if cancelsSynchronization { installations.cloud.cancelSynchronization(for: game) }
                else { installations.cancelDownload() }
            },
            isCancellable: cancelsSynchronization
                || (installations.isLaunching && installations.launchGame == game),
            isStep: installations.isSynchronizingSaves || installations.runtimeInstallFraction != nil,
            stepFraction: installations.runtimeInstallFraction
        )
    }
}

/// Reports a game's failed or cancelled Transfer, or another game's operation
/// holding the storefront.
struct GameOperationStatus<Store: InstallationStorefront>: View {
    let installations: GameInstallations<Store>
    let game: Store.GameID

    var body: some View {
        if let message {
            Text(message).font(.system(size: 12)).foregroundStyle(.secondary)
        }
    }

    private var message: String? {
        if let transfer = installations.transfer(for: game), transfer.state == .failed || transfer.state == .cancelled {
            return transfer.message.isEmpty ? transfer.activityTitle : L10n.message(transfer.message)
        }
        if let activeGame = installations.activeTransferGame ?? installations.launchGame, activeGame != game {
            return L10n.string(Store.otherGameOperation)
        }
        return nil
    }
}
