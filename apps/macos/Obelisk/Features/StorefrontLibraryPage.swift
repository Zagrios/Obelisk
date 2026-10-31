import SwiftUI

/// What a storefront model gives its library and its game page. It carries
/// what `StorefrontAccount` tells of the account in the window, which the
/// extensions of `StorefrontAccount.swift` write for every storefront.
@MainActor protocol LibraryStorefront: StorefrontAccount, Observable {
    associatedtype Store: InstallationStorefront
    associatedtype Game: LibraryGame where Game.ID == Store.GameID
    var installations: GameInstallations<Store> { get }
    var accountName: String? { get }
    var avatarURL: URL? { get }
    var isRestoring: Bool { get }
    var isConnecting: Bool { get }
    var gameSessions: GameSessions<Store> { get }
    var rosetta: RosettaModel<GameSessions<Store>> { get }
    /// The account's games, which every library view reads.
    var library: AccountLibrary<Store, Game> { get }
    var achievements: GameAchievementsModel { get }
    var gameSessionMessage: String? { get }
    func syncCloud()
    func reloadAchievements()
    var isLoadingGames: Bool { get }
    var cloud: CloudModel<Store> { get }
    /// The game whose page shows instead of the library.
    var selectedGame: Game? { get }
    func open(_ game: Game)
    /// Returns from the game's page to the library.
    func closeGame()
    /// What the library installs of `game` without opening its page, when
    /// the storefront has a build of it.
    func libraryOffer(of game: Game) -> GameOffer?
    /// The messages the account has not read, which its menu tells.
    var unreadCount: Int { get }
}

extension LibraryStorefront {
    func syncCloud() { installations.syncCloud() }
    var unreadCount: Int { 0 }

    /// Names the game a Cloud question is about.
    func cloudTitle(for game: Store.GameID?) -> String {
        guard let game else { return Store.cloudTitle }
        let name = library.game(game)?.title
            ?? installations.receipts.first { $0.gameID == game }?.name
            ?? Store.name(game)
        return "\(Store.cloudTitle) · \(name)"
    }

    /// A game of the library, as its Game action reads it: its native build
    /// first, and what the library installs without opening its page.
    func librarySelection(_ game: Game) -> InstallationSelection<Store> {
        InstallationSelection(game: game.id, platform: .mac, offer: libraryOffer(of: game))
    }
}

/// Where a library was scrolled, kept while a game's page replaces it. Nothing
/// observes it, so scrolling redraws nothing.
@MainActor
final class LibraryScrollMemory {
    var offset: CGFloat = 0
}

/// A storefront's signed-in library: the featured game as its hero, then every
/// game of the account under the browser's floating glass bar, and the
/// storefront's Transfer floating at the bottom.
struct StorefrontLibraryPage<Model: LibraryStorefront, HeroArtwork: View, HeroLogo: View, Artwork: View>: View {
    @Bindable var model: Model
    let storeTitle: String
    let libraryTitle: String
    let scrollMemory: LibraryScrollMemory
    /// The stores its empty search results offer.
    let stores: [StoreSearch]
    /// Opens a game's page, from its cover or from the hero.
    let open: (Model.Game, GameOpeningSource) -> Void
    /// The featured game's wide artwork, reaching under the bar.
    @ViewBuilder let heroArtwork: (Model.Game) -> HeroArtwork
    @ViewBuilder let heroLogo: (Model.Game) -> HeroLogo
    /// A game's cover.
    @ViewBuilder let artwork: (Model.Game) -> Artwork
    @State private var position: ScrollPosition
    @Environment(\.storefrontBar) private var bar

    private static var columns: [GridItem] {
        [GridItem(.adaptive(minimum: 160, maximum: 220), spacing: 16, alignment: .top)]
    }

    init(
        model: Model,
        storeTitle: String,
        libraryTitle: String,
        scrollMemory: LibraryScrollMemory,
        stores: [StoreSearch],
        open: @escaping (Model.Game, GameOpeningSource) -> Void,
        @ViewBuilder heroArtwork: @escaping (Model.Game) -> HeroArtwork,
        @ViewBuilder heroLogo: @escaping (Model.Game) -> HeroLogo,
        @ViewBuilder artwork: @escaping (Model.Game) -> Artwork
    ) {
        self.model = model
        self.storeTitle = storeTitle
        self.libraryTitle = libraryTitle
        self.scrollMemory = scrollMemory
        self.stores = stores
        self.open = open
        self.heroArtwork = heroArtwork
        self.heroLogo = heroLogo
        self.artwork = artwork
        // A library opened at its top stays free to grow above, as its hero
        // does once the games arrive.
        let offset = scrollMemory.offset
        _position = State(initialValue: offset > 0 ? ScrollPosition(y: offset) : ScrollPosition())
    }

    var body: some View {
        // The hero belongs to the whole library, never to a search or a filter.
        let heroGame = model.library.hero
        let actions = GameActions(model.installations, in: .library)
        let backdropOffset = heroGame == nil ? 0 : ObeliskStyle.libraryHeroHeight
        return StorefrontScrollView {
            VStack(alignment: .leading, spacing: 0) {
                if let heroGame {
                    hero(heroGame, action: actions(model.librarySelection(heroGame)))
                }
                VStack(alignment: .leading, spacing: 22) {
                    StorefrontWarningBanner(message: $model.storageWarning)
                    library(actions)
                }
                .padding(.horizontal, ObeliskStyle.contentPadding)
                .padding(.top, heroGame == nil ? 12 : 30)
                .padding(.bottom, 36)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .scrollPosition($position)
        .onScrollGeometryChange(for: CGFloat.self, of: { $0.contentOffset.y + $0.contentInsets.top }) { _, offset in
            scrollMemory.offset = offset
        }
        // Whether games or the library's header are under the bar, rather
        // than the hero's artwork or nothing at all.
        .onScrollGeometryChange(for: Bool.self, of: { $0.contentOffset.y + $0.contentInsets.top > backdropOffset }) { _, under in
            bar?.showsBackdrop = under
            bar?.showsGameTitle = false
        }
        .ignoresSafeArea(.container, edges: .top)
    }

    private func hero(_ game: Model.Game, action: GameAction) -> some View {
        LibraryHero(
            title: game.title,
            details: [
                model.library.lastPlayed(of: game).map { .init(symbol: "calendar", text: GameFormatting.lastPlayed($0)) },
                model.library.playtime(of: game).map { .init(symbol: "clock", text: GameFormatting.playtime($0)) },
            ].compactMap { $0 },
            action: action,
            open: { open(game, .hero) }
        ) {
            heroArtwork(game)
        } logo: {
            heroLogo(game)
        }
    }

    private func library(_ actions: GameActions<Model.Store>) -> some View {
        let library = model.library
        let filtered = library.shown
        return VStack(alignment: .leading, spacing: 22) {
            header(count: filtered.count)

            if let missing = library.missingMetadata {
                HStack(spacing: 12) {
                    Text(missing)
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                    Spacer(minLength: 0)
                    Button(L10n.string("ui.common.retry"), action: model.refresh)
                        .obeliskButtonStyle(.glass)
                        .controlSize(.small)
                        .disabled(model.isLoadingGames || library.pendingMetadata != nil)
                }
            }

            if model.isLoadingGames && library.games.isEmpty {
                ProgressView(L10n.string("ui.library.fetching")).frame(maxWidth: .infinity, minHeight: 260)
            } else if filtered.isEmpty, let pending = library.pendingMetadata {
                ProgressView(pending).frame(maxWidth: .infinity, minHeight: 260)
            } else if filtered.isEmpty {
                StorefrontEmptyLibrary(searchText: Bindable(library).searchText, filter: Bindable(library).filter,
                                       stores: stores, refresh: model.refresh)
            } else {
                grid(filtered, actions: actions)
                if model.isLoadingGames || library.pendingMetadata != nil {
                    ProgressView().controlSize(.small).frame(maxWidth: .infinity)
                }
            }
        }
    }

    private func header(count: Int) -> some View {
        let title = VStack(alignment: .leading, spacing: 3) {
            Text(libraryTitle)
                .font(.system(size: 22, weight: .bold))
                .accessibilityAddTraits(.isHeader)
            if !model.library.games.isEmpty {
                Text(L10n.string("ui.library.game_count", Int64(count)))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            }
        }
        .fixedSize()
        let filters = LibraryFilterPicker(storeTitle: storeTitle, filters: StorefrontLibraryFilter.allCases,
                                          selection: Bindable(model.library).filter)
        return ViewThatFits(in: .horizontal) {
            HStack(spacing: 14) {
                title
                Spacer(minLength: 16)
                filters
                sortButton
            }
            VStack(alignment: .leading, spacing: 16) {
                HStack(spacing: 14) {
                    title
                    Spacer(minLength: 16)
                    sortButton
                }
                filters
            }
        }
    }

    private var sortButton: some View {
        let library = model.library
        return LibrarySortButton(
            storeTitle: storeTitle,
            sortTitle: library.sort.title,
            options: StorefrontLibrarySort.allCases.map { sort in
                StorefrontSortOption(id: sort.id, title: sort.menuTitle, isSelected: library.sort == sort) {
                    library.sort = sort
                }
            }
        )
    }

    private func grid(_ games: [Model.Game], actions: GameActions<Model.Store>) -> some View {
        LazyVGrid(columns: Self.columns, alignment: .leading, spacing: 18) {
            ForEach(games) { game in
                let title = game.title
                LibraryCoverCard(
                    title: title,
                    accessibilityLabel: L10n.string("ui.library.game_accessibility", title,
                                                    GameFormatting.lastPlayed(model.library.lastPlayed(of: game))),
                    help: [title, model.library.playtime(of: game).map(GameFormatting.playtime)].compactMap { $0 }
                        .joined(separator: " · "),
                    action: actions(model.librarySelection(game)),
                    matchID: game.id,
                    open: { open(game, .cover) }
                ) {
                    artwork(game)
                }
            }
        }
    }
}

extension SteamModel: LibraryStorefront {}
extension WebAccountModel: LibraryStorefront {}
