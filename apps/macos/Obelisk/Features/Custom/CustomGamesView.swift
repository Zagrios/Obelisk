import SwiftUI

/// Custom games section: manages and launches non-store / standalone Windows executables.
struct CustomGamesView: View {
    @Bindable var model: CustomGamesModel
    @State private var bar = StorefrontBarState()
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.colorScheme) private var colorScheme

    private static var columns: [GridItem] {
        [GridItem(.adaptive(minimum: 160, maximum: 220), spacing: 16, alignment: .top)]
    }

    var body: some View {
        ZStack {
            if let selectedGame = model.selectedGame {
                CustomGameDetailView(model: model, game: selectedGame)
                    .transition(.asymmetric(insertion: .identity, removal: .opacity))
            } else {
                libraryContent
                    .transition(.asymmetric(insertion: .identity,
                                            removal: .opacity.combined(with: .scale(scale: reduceMotion ? 1 : 0.94))))
            }
        }
        .sheet(isPresented: $model.isAddingGame) {
            AddCustomGameSheet(model: model, editingGame: nil)
        }
        .sheet(item: $model.editingGame) { game in
            AddCustomGameSheet(model: model, editingGame: game)
        }
        .sheet(item: $model.settingsGame) { game in
            CustomLaunchSettingsSheet(model: model, game: game)
        }
        .confirmationDialog(
            model.gamePendingRemoval.map { L10n.string("ui.custom.remove_confirm_title", $0.title) } ?? "",
            isPresented: Binding(
                get: { model.gamePendingRemoval != nil },
                set: { if !$0 { model.gamePendingRemoval = nil } }
            ),
            titleVisibility: .visible
        ) {
            if let game = model.gamePendingRemoval {
                Button(L10n.string("ui.custom.remove_confirm_action"), role: .destructive) {
                    model.removeGame(game)
                    model.gamePendingRemoval = nil
                }
            }
            Button(L10n.string("ui.common.cancel"), role: .cancel) {
                model.gamePendingRemoval = nil
            }
        } message: {
            Text(L10n.string("ui.custom.remove_confirm_message"))
        }
    }

    @ViewBuilder
    private var libraryContent: some View {
        if model.games.isEmpty {
            emptyState
        } else {
            gamesGrid
        }
    }

    private var emptyState: some View {
        VStack(spacing: 20) {
            SymbolTile(name: "gamecontroller", colors: TileColors.violet, size: 56)
            VStack(spacing: 8) {
                Text(L10n.string("ui.custom.empty_title"))
                    .font(.system(size: 22, weight: .bold))
                Text(L10n.string("ui.custom.empty_description"))
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 440)
            }
            AccentCapsuleButton(
                title: L10n.string("ui.custom.add_game"),
                systemImage: "plus",
                size: .large,
                identifier: "custom-games-add-empty",
                action: { model.isAddingGame = true }
            )
            .padding(.top, 4)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(.horizontal, ObeliskStyle.contentPadding)
    }

    private var gamesGrid: some View {
        StorefrontScrollView {
            VStack(alignment: .leading, spacing: 20) {
                Color.clear.frame(height: ObeliskStyle.titleBarHeight + 8)

                if model.sortedGames.isEmpty {
                    VStack(spacing: 12) {
                        Image(systemName: "magnifyingglass")
                            .font(.system(size: 32, weight: .light))
                            .foregroundStyle(.secondary)
                        Text(L10n.string("ui.library.no_games_found"))
                            .font(.system(size: 18, weight: .semibold))
                        Button(L10n.string("ui.search.clear")) {
                            model.searchText = ""
                        }
                        .obeliskButtonStyle(.bordered)
                    }
                    .frame(maxWidth: .infinity, minHeight: 240)
                } else {
                    LazyVGrid(columns: Self.columns, alignment: .leading, spacing: 18) {
                        ForEach(model.sortedGames) { game in
                            gameCard(game)
                        }
                    }
                }
            }
            .padding(.horizontal, ObeliskStyle.contentPadding)
            .padding(.bottom, 36)
        }
        .onScrollGeometryChange(for: Bool.self, of: { $0.contentOffset.y + $0.contentInsets.top > 10 }) { _, under in
            bar.showsBackdrop = under
        }
        .overlay(alignment: .top) {
            topBar
        }
        .ignoresSafeArea(.container, edges: .top)
    }

    private var topBar: some View {
        LibraryGlassBar(
            searchText: $model.searchText,
            searchLabel: L10n.string("ui.search.placeholder"),
            showsBackdrop: bar.showsBackdrop
        ) {
            HStack(spacing: 10) {
                StorefrontGlassButton(
                    systemName: "plus",
                    size: 14,
                    label: L10n.string("ui.custom.add_game"),
                    identifier: "custom-games-add-bar",
                    action: { model.isAddingGame = true }
                )
                Text(L10n.string("model.section.custom.title"))
                    .font(.system(size: 14, weight: .semibold))
                Text(L10n.string("ui.library.game_count", Int64(model.games.count)))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
                sortMenu
            }
        }
    }

    private var sortMenu: some View {
        Menu {
            ForEach(CustomGameSortOption.allCases) { option in
                Button {
                    withAnimation(reduceMotion ? nil : ObeliskStyle.selectionAnimation) {
                        model.sortOption = option
                    }
                } label: {
                    HStack {
                        Text(option.title)
                        if model.sortOption == option {
                            Image(systemName: "checkmark")
                        }
                    }
                }
            }
        } label: {
            HStack(spacing: 6) {
                Image(systemName: "arrow.up.arrow.down")
                    .font(.system(size: 11, weight: .semibold))
                Text(model.sortOption.title)
                    .font(.system(size: 12, weight: .semibold))
            }
            .foregroundStyle(.primary)
            .padding(.horizontal, 12)
            .frame(height: 36)
            .modifier(StorefrontControlGlass())
            .contentShape(.capsule)
        }
        .menuStyle(.button)
        .obeliskButtonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel(L10n.string("ui.sort.library"))
        .help(L10n.string("ui.sort.library"))
    }

    private func gameCard(_ game: CustomGame) -> some View {
        LibraryCoverCard(
            title: game.title,
            accessibilityLabel: L10n.string("ui.library.game_accessibility", game.title,
                                            GameFormatting.lastPlayed(game.lastPlayed)),
            help: [game.title, game.formattedPlaytime].joined(separator: " · "),
            store: .custom,
            action: model.gameAction(for: game, place: .library),
            matchID: AnyHashable(game.id),
            open: {
                withAnimation(ObeliskStyle.pageAnimation(reduceMotion: reduceMotion)) {
                    model.selectedGame = game
                }
            }
        ) {
            CustomGameCoverView(game: game)
        }
        .contextMenu {
            Button {
                if model.isRunning(game) {
                    model.stop(game)
                } else {
                    model.launch(game)
                }
            } label: {
                Label(
                    model.isRunning(game) ? L10n.string("model.game.quit") : L10n.string("ui.game.play"),
                    systemImage: model.isRunning(game) ? "stop.fill" : "play.fill"
                )
            }
            Divider()
            Button {
                model.settingsGame = game
            } label: {
                Label(L10n.string("ui.custom.launch_settings_title"), systemImage: "slider.horizontal.3")
            }
            Button {
                model.editingGame = game
            } label: {
                Label(L10n.string("ui.custom.edit_game"), systemImage: "pencil")
            }
            Button {
                NSWorkspace.shared.selectFile(game.executablePath, inFileViewerRootedAtPath: "")
            } label: {
                Label(L10n.string("ui.custom.show_in_finder"), systemImage: "folder")
            }
            Divider()
            Button(role: .destructive) {
                model.gamePendingRemoval = game
            } label: {
                Label(L10n.string("ui.custom.remove_game"), systemImage: "trash")
            }
        }
    }
}
