import SwiftUI

struct LibraryView: View {
    @Bindable var model: AppModel
    @Environment(\.colorScheme) private var colorScheme

    private let columns = [GridItem(.adaptive(minimum: 220, maximum: 340), spacing: 18, alignment: .top)]

    private var isStore: Bool { model.selection.isStorefront }

    private var hasSearch: Bool {
        !model.searchText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        if isStore {
            StorefrontPage {
                StorefrontTitleBar(
                    title: model.selection.title,
                    accountName: nil,
                    searchText: $model.searchText,
                    showsLibraryControls: false,
                    isConnected: false
                ) {
                    EmptyView()
                }
            } content: {
                storeEmptyState
                    .padding(SailOverStyle.contentPadding)
                    .frame(maxWidth: 1300, alignment: .leading)
            }
        } else {
            localLibrary
        }
    }

    private var localLibrary: some View {
        let filtered = model.filteredGames
        return ScrollView {
            VStack(alignment: .leading, spacing: 26) {
                header
                searchBar(count: filtered.count)

                if model.isLoading {
                    ProgressView(L10n.string("ui.library.loading"))
                        .frame(maxWidth: .infinity, minHeight: 270)
                } else if hasSearch && filtered.isEmpty {
                    noResults
                } else if filtered.isEmpty {
                    localEmptyState
                } else {
                    LazyVGrid(columns: columns, alignment: .leading, spacing: 20) {
                        ForEach(filtered) { game in

                            GameCard(
                                game: game,
                                launch: { model.launchGame(game) },
                                remove: { model.removeGame(game) }
                            )
                        }
                    }
                }

                if model.selection == .recent && !hasSearch {
                    storeCards
                }
            }
            .padding(SailOverStyle.contentPadding)
            .padding(.top, 6)
            .frame(maxWidth: 1300, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 20) {
            VStack(alignment: .leading, spacing: 7) {
                Text(model.selection.title)
                    .font(.system(size: 34, weight: .bold))
                    .tracking(-1)
                    .accessibilityAddTraits(.isHeader)
                Text(model.selection.subtitle)
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
            }

            Spacer(minLength: 12)

            Button(action: model.importGame) {
                Label(model.isImporting ? L10n.string("ui.library.adding_game") : L10n.string("ui.library.add_game"), systemImage: "plus")
            }
            .buttonStyle(SailOverPrimaryButtonStyle())
            .disabled(model.isImporting)
            .fixedSize()
            .help(L10n.string("ui.library.add_game_help"))
        }
    }

    private func searchBar(count: Int) -> some View {
        HStack(spacing: 16) {

            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 13))
                    .foregroundStyle(.tertiary)
                TextField(L10n.string("ui.library.search"), text: $model.searchText)
                    .textFieldStyle(.plain)
                    .font(.system(size: 13))
                    .accessibilityLabel(L10n.string("ui.library.search_accessibility"))

                if !model.searchText.isEmpty {
                    Button {
                        model.searchText = ""
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundStyle(.tertiary)
                    }
                    .sailOverButtonStyle(.plain)
                    .accessibilityLabel(L10n.string("ui.search.clear"))
                }
            }
            .padding(.horizontal, 11)
            .padding(.vertical, 9)
            .background(
                SailOverPalette(colorScheme: colorScheme).surface,
                in: RoundedRectangle(cornerRadius: 8)
            )
            .overlay {
                RoundedRectangle(cornerRadius: 8)
                    .strokeBorder(SailOverPalette(colorScheme: colorScheme).border)
            }
            .frame(maxWidth: 300)

            Spacer(minLength: 0)

            if !model.isLoading && !model.games.isEmpty {
                Text(L10n.string("ui.library.game_count", Int64(count)))
                    .font(.system(size: 12))
                    .foregroundStyle(.tertiary)
            }
        }
    }

    private var localEmptyState: some View {
        VStack(spacing: 17) {
            Image(systemName: "gamecontroller")
                .font(.system(size: 35, weight: .light))
                .foregroundStyle(SailOverStyle.accent)
                .frame(width: 76, height: 76)
                .background(SailOverStyle.accent.opacity(0.09), in: RoundedRectangle(cornerRadius: 22))

            VStack(spacing: 8) {
                Text(model.selection == .recent ? L10n.string("ui.library.recent_empty_title") : L10n.string("ui.library.local_empty_title"))
                    .font(.system(size: 23, weight: .semibold))
                    .tracking(-0.4)
                Text(L10n.string("ui.library.local_empty_description", AppIdentity.name))
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 350)
            }

            Button(action: model.importGame) {
                Label(L10n.string("ui.library.add_game"), systemImage: "plus")
            }
            .buttonStyle(SailOverPrimaryButtonStyle())
            .disabled(model.isImporting)
            .padding(.top, 3)

            Text(L10n.string("ui.library.supported_files"))
                .font(.system(size: 11))
                .foregroundStyle(.tertiary)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 35)
        .padding(.horizontal, 24)
        .background {
            RoundedRectangle(cornerRadius: 14)
                .fill(SailOverPalette(colorScheme: colorScheme).surface)
                .overlay(alignment: .topTrailing) {
                    RadialGradient(
                        colors: [SailOverStyle.accent.opacity(0.055), .clear],
                        center: .topTrailing,
                        startRadius: 0,
                        endRadius: 350
                    )
                    .clipShape(RoundedRectangle(cornerRadius: 14))
                }
        }
        .overlay {
            RoundedRectangle(cornerRadius: 14)
                .strokeBorder(SailOverPalette(colorScheme: colorScheme).border)
        }
    }

    private var storeEmptyState: some View {
        VStack(spacing: 18) {
            StoreMark(section: model.selection, size: 76)
            VStack(spacing: 9) {
                Text(L10n.string("ui.library.store_title", model.selection.title))
                    .font(.system(size: 23, weight: .semibold))
                    .tracking(-0.4)
                Text(L10n.string("ui.library.store_unavailable", model.selection.title))
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            ComingSoonBadge()
        }
        .padding(32)
        .frame(maxWidth: .infinity, minHeight: 320)
        .background(
            SailOverPalette(colorScheme: colorScheme).surface,
            in: RoundedRectangle(cornerRadius: 14)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 14)
                .strokeBorder(SailOverPalette(colorScheme: colorScheme).border)
        }
    }

    private var noResults: some View {
        VStack(spacing: 15) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 34, weight: .light))
                .foregroundStyle(.tertiary)
            Text(L10n.string("ui.search.no_results"))
                .font(.system(size: 21, weight: .semibold))
            Text(L10n.string("ui.search.no_matching_games", model.searchText))
                .font(.system(size: 13))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            Button(L10n.string("ui.search.clear")) { model.searchText = "" }
                .sailOverButtonStyle(.bordered)
        }
        .padding(32)
        .frame(maxWidth: .infinity, minHeight: 300)
    }

    private var storeCards: some View {
        VStack(alignment: .leading, spacing: 13) {
            HStack {
                Text(L10n.string("ui.library.your_libraries"))
                    .font(.system(size: 17, weight: .semibold))
                    .accessibilityAddTraits(.isHeader)
                Spacer()
            }

            LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 12), count: 3), spacing: 12) {
                ForEach(LibrarySection.storefronts) { section in
                    Button {
                        model.selection = section
                    } label: {

                        VStack(alignment: .leading, spacing: 21) {
                            HStack(spacing: 6) {
                                StoreMark(section: section, size: 36)
                                Spacer(minLength: 0)
                                if section == .gog {
                                    ComingSoonBadge()
                                }
                            }
                            HStack {
                                Text(section.title)
                                    .font(.system(size: 15, weight: .semibold))
                                    .foregroundStyle(.primary)
                                Spacer(minLength: 0)
                                Image(systemName: "chevron.right")
                                    .font(.system(size: 10, weight: .semibold))
                                    .foregroundStyle(.tertiary)
                            }
                        }
                        .padding(17)
                        .background(
                            SailOverPalette(colorScheme: colorScheme).surface,
                            in: RoundedRectangle(cornerRadius: 11)
                        )
                        .overlay {
                            RoundedRectangle(cornerRadius: 11)
                                .strokeBorder(SailOverPalette(colorScheme: colorScheme).border)
                        }
                        .contentShape(RoundedRectangle(cornerRadius: 11))
                    }
                    .sailOverButtonStyle(.plain)
                    .accessibilityLabel(section == .gog ? L10n.string("ui.library.gog_coming_soon") : L10n.string("ui.library.store_accessibility", section.title))
                }
            }
        }
        .padding(.top, 2)
    }
}
