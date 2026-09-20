import SwiftUI

struct SteamView: View {
    @Bindable var model: SteamModel

    var body: some View {
        StorefrontPage { titleBar } content: { content }
        .task { await model.start() }
        .sheet(isPresented: Binding(
            get: { model.cloud.promptPresented },
            set: { _ in }
        )) {
            SteamCloudPromptView(
                title: cloudPromptTitle,
                message: L10n.string(model.cloud.promptMessage),
                busy: model.cloud.promptWorking,
                onCancel: { model.cloud.respond(.cancel) },
                onChooseLocal: { model.cloud.respond(.local) },
                onChooseCloud: { model.cloud.respond(.cloud) }
            )
            .interactiveDismissDisabled()
        }
        .onChange(of: model.gameSessions.isBusy) { _, isBusy in
            if !isBusy { model.cloud.resume() }
        }
        .errorAlert(L10n.string("ui.store.steam"), message: $model.errorMessage)
    }

    private var cloudPromptTitle: String {
        let title = L10n.string("ui.steam.cloud.title")
        guard let appId = model.cloud.promptAppId else { return title }
        let name = model.games.first { $0.appId == appId }?.name
            ?? model.installations.first { $0.appId == appId }?.name
            ?? String(appId)
        return "\(title) · \(name)"
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 25) {
            StorefrontWarningBanner(message: $model.storageWarning)

            if model.isConnected {
                if model.cloud.failurePresented {
                    SteamCloudFailureView(
                        title: cloudPromptTitle,
                        message: model.cloud.promptMessage,
                        busy: model.cloud.promptWorking,
                        canLaunchAnyway: model.cloud.canLaunchAnyway,
                        onRetry: { model.cloud.respond(.retry) },
                        onCancel: { model.cloud.respond(.cancel) },
                        onLaunchAnyway: { model.cloud.respond(.launchAnyway) }
                    )
                }
                if let game = model.selectedGame {
                    SteamGameDetailView(model: model, game: game)
                } else {
                    library
                }
            } else {
                SteamLoginView(model: model)
                    .padding(.top, 28)
            }
        }
        .padding(ForgeStyle.contentPadding)
        .padding(.top, 6)
        .frame(maxWidth: 1400, alignment: .leading)
    }

    private var titleBar: some View {
        StorefrontTitleBar(
            title: L10n.string("ui.store.steam"),
            accountName: model.displayName,
            avatarURL: model.avatarURL,
            searchText: $model.searchText,
            sortTitle: model.sort.title,
            sortOptions: SteamLibrarySort.allCases.map { sort in
                StorefrontSortOption(
                    id: sort.id,
                    title: sort.menuTitle,
                    isSelected: model.sort == sort,
                    action: { model.sort = sort }
                )
            },
            showsLibraryControls: model.isConnected && model.selectedGame == nil,
            isConnected: model.isConnected
        ) {
            Text(model.displayName ?? L10n.string("ui.steam.account"))
            Text(L10n.string("ui.library.account_game_count", Int64(model.games.count)))
            Divider()
            Button(model.friends?.menuTitle ?? L10n.string("ui.friends.menuLoading"), systemImage: "person.2", action: model.openFriends)
                .accessibilityIdentifier("steam-friends-menu")
            Divider()
            Button(L10n.string("ui.library.refresh"), systemImage: "arrow.clockwise", action: model.refresh)
            Divider()
            Button(L10n.string("ui.account.sign_out"), systemImage: "rectangle.portrait.and.arrow.right", action: model.signOut)
        }
    }

    private var library: some View {
        VStack(alignment: .leading, spacing: 24) {
            if let transfer = model.transfer, model.isDownloading {
                SteamTransferView(transfer: transfer, isDownloading: model.isDownloading, cancel: model.cancelDownload)
            }

            let filtered = model.filteredGames
            if model.isLoadingGames && model.games.isEmpty {
                ProgressView(L10n.string("ui.library.fetching")).frame(maxWidth: .infinity, minHeight: 260)
            } else if filtered.isEmpty {
                StorefrontEmptyLibrary(searchText: $model.searchText, refresh: model.refresh)
            } else {
                LazyVGrid(columns: StorefrontLibrary.gridColumns, alignment: .leading, spacing: 24) {
                    ForEach(filtered, id: \.appId) { game in
                        let lastPlayed = SteamFormatting.lastPlayed(game.lastPlayed)
                        LibraryGridCard(title: game.name, subtitle: lastPlayed, open: { model.open(game) }) {
                            SteamArtwork(appId: game.appId, iconHash: game.iconHash)
                        }
                        .help(L10n.string("ui.steam.game_help", game.name, SteamFormatting.playtime(game.playtimeMinutes)))
                        .accessibilityLabel(L10n.string("ui.steam.game_accessibility", game.name, lastPlayed))
                    }
                }
                if model.isLoadingGames { ProgressView().controlSize(.small) }
            }
        }
    }
}

struct SteamTransferView: View {
    let transfer: any StorefrontTransfer
    let isDownloading: Bool
    let cancel: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label(transfer.activityTitle, systemImage: systemImage)
                    .font(.system(size: 13, weight: .semibold))
                Spacer()
                if isDownloading {
                    Button(L10n.string("ui.common.cancel"), action: cancel)
                        .controlSize(.small)
                        .pointerCursor()
                }
            }
            if transfer.totalBytes > 0 {
                ProgressView(value: Double(min(transfer.completedBytes, transfer.totalBytes)), total: Double(transfer.totalBytes))
                Text(transfer.verifiedProgressText)
                    .font(.system(size: 11)).monospacedDigit().foregroundStyle(.secondary)
            } else if isDownloading { ProgressView().progressViewStyle(.linear) }
            if let downloadedGameDataText = transfer.downloadedGameDataText {
                Text(downloadedGameDataText)
                    .font(.system(size: 11)).monospacedDigit().foregroundStyle(.secondary)
            }
            if !transfer.currentFile.isEmpty {
                Text(L10n.message(transfer.currentFile)).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
            }
            if !transfer.message.isEmpty {
                Text(L10n.message(transfer.message)).font(.system(size: 12)).foregroundStyle(.secondary)
            }
        }
        .padding(16)
        .background(ForgeStyle.accent.opacity(0.055), in: RoundedRectangle(cornerRadius: 10))
    }

    private var systemImage: String {
        switch transfer.state {
        case .preparing: "arrow.clockwise"
        case .verifying: "checkmark.shield"
        case .downloading: "arrow.down.circle"
        case .complete: "checkmark.circle"
        case .cancelled: "pause.circle"
        case .failed: "exclamationmark.circle"
        }
    }
}

/// Both stores use the same progress text, accessibility and transfer controls.
protocol StorefrontTransfer {
    var state: TransferState { get }
    var completedBytes: UInt64 { get }
    var totalBytes: UInt64 { get }
    var contentBytes: UInt64 { get }
    var currentFile: String { get }
    var message: String { get }
}

extension SteamTransfer: StorefrontTransfer {}
extension EpicTransfer: StorefrontTransfer {}

extension StorefrontTransfer {
    var activityTitle: String {
        switch state {
        case .preparing: L10n.string("ui.transfer.checking_updates")
        case .verifying: L10n.string("ui.transfer.verifying_files")
        case .downloading: L10n.string("ui.transfer.downloading")
        case .complete: L10n.string("ui.transfer.up_to_date")
        case .cancelled: L10n.string("ui.transfer.interrupted")
        case .failed: L10n.string("ui.transfer.failed")
        }
    }

    var verifiedProgressText: String {
        L10n.string("ui.transfer.verified_files", SteamFormatting.bytes(completedBytes), SteamFormatting.bytes(totalBytes))
    }

    var downloadedGameDataText: String? {
        if contentBytes > 0 {
            L10n.string("ui.transfer.downloaded_data", SteamFormatting.bytes(contentBytes))
        } else if state == .verifying {
            L10n.string("ui.transfer.no_downloaded_data")
        } else {
            nil
        }
    }

    var buttonTitle: String {
        switch state {
        case .preparing: L10n.string("ui.transfer.preparing_short")
        case .verifying: L10n.string("ui.transfer.verifying_short")
        case .downloading: L10n.string("ui.transfer.downloading_short")
        case .complete: L10n.string("ui.transfer.finishing_short")
        case .cancelled: L10n.string("ui.transfer.cancelling_short")
        case .failed: L10n.string("ui.transfer.failed_short")
        }
    }

    var isInProgress: Bool {
        state == .preparing || state == .verifying || state == .downloading
    }

    var progressFraction: Double? {
        guard totalBytes > 0 else { return nil }
        return Double(min(completedBytes, totalBytes)) / Double(totalBytes)
    }

    var progressSummary: String {
        [
            activityTitle,
            totalBytes > 0 ? verifiedProgressText : nil,
            downloadedGameDataText,
            currentFile.isEmpty ? nil : L10n.message(currentFile),
            message.isEmpty ? nil : L10n.message(message),
        ]
        .compactMap { $0 }
        .joined(separator: "\n")
    }
}
