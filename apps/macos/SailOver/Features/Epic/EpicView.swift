import SwiftUI

struct EpicView: View {
    @Bindable var model: EpicModel

    var body: some View {
        StorefrontPage { titleBar } content: {
            VStack(alignment: .leading, spacing: 25) {
                StorefrontWarningBanner(message: $model.storageWarning)
                if model.isConnected {
                    if model.cloud.failurePresented {
                        SteamCloudFailureView(
                            title: model.cloudTitle(for: model.cloud.promptGame),
                            message: model.cloud.promptMessage,
                            busy: model.cloud.promptWorking,
                            canLaunchAnyway: model.cloud.canLaunchAnyway,
                            onRetry: { model.cloud.respond(.retry) },
                            onCancel: { model.cloud.respond(.cancel) },
                            onLaunchAnyway: { model.cloud.respond(.launchAnyway) },
                            storefront: "epic"
                        )
                    }
                    if let game = model.selectedGame {
                        EpicGameDetailView(model: model, game: game)
                    } else { library }
                } else { EpicLoginView(model: model).padding(.top, 28) }
            }
            .padding(SailOverStyle.contentPadding)
            .padding(.top, 6)
            .frame(maxWidth: 1400, alignment: .leading)
        }
        .task { await model.start() }
        .onChange(of: model.gameSessions.isBusy) { _, isBusy in
            if !isBusy { model.refreshCloudIndicator() }
        }
        // The game's guard and launch sync are on disk once it runs.
        .onChange(of: model.gameSessions.active?.phase == .running) { _, running in
            if running { model.refreshCloudIndicator() }
        }
        .errorAlert(L10n.string("ui.store.epic"), message: $model.errorMessage)
    }

    private var titleBar: some View {
        StorefrontTitleBar(
            title: L10n.string("ui.store.epic"),
            accountName: model.accountName,
            searchText: $model.searchText,
            sortOptions: [StorefrontSortOption(id: "name", title: L10n.string("ui.sort.by_name"), isSelected: true, action: {})],
            filter: $model.filter,
            availableFilters: model.availableFilters,
            showsLibraryControls: model.isConnected && model.selectedGame == nil,
            isConnected: model.isConnected
        ) {
            Text(model.accountName ?? L10n.string("ui.epic.account"))
            Text(L10n.string("ui.library.account_game_count", Int64(model.games.count)))
            Divider()
            Button(model.friends?.menuTitle ?? L10n.string("ui.friends.menuLoading"), systemImage: "person.2", action: model.openFriends)
                .disabled(model.friends == nil)
                .accessibilityIdentifier("epic-friends-menu")
            Button(L10n.string("ui.library.refresh"), systemImage: "arrow.clockwise", action: model.refresh)
                .disabled(model.isLoadingGames)
            Toggle(L10n.string("ui.account.remember_me"), isOn: $model.rememberSession)
            Divider()
            Button(L10n.string("ui.account.sign_out"), systemImage: "rectangle.portrait.and.arrow.right", action: model.signOut)
        }
    }

    private var library: some View {
        VStack(alignment: .leading, spacing: 24) {
            if let transfer = model.installations.transfer, model.installations.isDownloading {
                SteamTransferView(transfer: transfer, isDownloading: true, cancel: model.installations.cancelDownload)
            }
            let filtered = model.filteredGames
            if model.isLoadingGames && model.games.isEmpty {
                ProgressView(L10n.string("ui.library.fetching")).frame(maxWidth: .infinity, minHeight: 260)
            } else if filtered.isEmpty {
                StorefrontEmptyLibrary(searchText: $model.searchText, filter: $model.filter, refresh: model.refresh)
            } else {
                LazyVGrid(columns: StorefrontLibrary.gridColumns, alignment: .leading, spacing: 24) {
                    ForEach(filtered, id: \.libraryID) { game in
                        LibraryGridCard(
                            title: game.title,
                            subtitle: game.developer.isEmpty ? nil : game.developer,
                            platform: .preferred(macos: game.macos, windows: game.windows),
                            open: { model.open(game) }
                        ) {
                            EpicArtwork(imageURL: game.imageUrl)
                        }
                        .help(game.title)
                        .accessibilityLabel(game.title)
                    }
                }
                if model.isLoadingGames { ProgressView().controlSize(.small) }
            }
        }
    }
}

struct EpicArtwork: View {
    let imageURL: String?
    var maxPixelSize = 768

    var body: some View {
        CachedArtwork(url: imageURL.flatMap(URL.init(string:)), maxPixelSize: maxPixelSize) { image in
            image.resizable().scaledToFill()
        } placeholder: {
            ArtworkPlaceholder()
        }
        .accessibilityHidden(true)
    }
}
