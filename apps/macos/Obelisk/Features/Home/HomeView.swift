import SwiftUI

/// Home: every library at once. The last game played anywhere is its hero,
/// then come the games played before it, the games on the Mac and the ones
/// Obelisk was set up to run, each a shelf of covers, then a tile for each
/// library. Its search looks through every library, and offers Steam's and
/// Epic's stores when it finds nothing. A game opens its page over Home, its
/// cover flying in as from the game's own library. Signed in nowhere, Home is
/// a sign-in page leading to each storefront's.
struct HomeView: View {
    @Bindable var model: AppModel
    @Namespace private var namespace
    @State private var bar = StorefrontBarState()
    /// Where the open game came from, once a game opened with motion.
    @State private var source: GameOpeningSource?
    /// The card the open game's cover flew from, and flies back to.
    @State private var card: AnyHashable?
    @State private var scrollMemory = LibraryScrollMemory()
    @State private var shelfMemory = ShelfScrollMemory()
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    /// A storefront's game whose page Home opened.
    private enum OpenGame {
        case steam(SteamGame), epic(EpicGame), gog(GogGame)

        var title: String {
            switch self {
            case let .steam(game): game.name
            case let .epic(game): game.title
            case let .gog(game): game.title
            }
        }

        var section: LibrarySection {
            switch self {
            case .steam: .steam
            case .epic: .epic
            case .gog: .gog
            }
        }
    }

    /// The game Home opened, while its storefront still shows its page.
    private var openGame: OpenGame? {
        guard let id = model.homeGame else { return nil }
        func opened<Game: Identifiable>(_ game: Game?) -> Game? {
            game.flatMap { "\($0.id)" == id.game ? $0 : nil }
        }
        switch id.section {
        case .steam: return opened(model.steam.selectedGame).map(OpenGame.steam)
        case .epic: return opened(model.epic.selectedGame).map(OpenGame.epic)
        case .gog: return opened(model.gog.selectedGame).map(OpenGame.gog)
        default: return nil
        }
    }

    var body: some View {
        Group {
            switch model.homeContent {
            case .libraries:
                libraries
            case .signIn:
                HomeSignIn(model: model)
            case .restoring:
                // Saved sessions are restoring: their libraries are on their way.
                Color.clear
            }
        }
        .errorAlert(L10n.string("ui.store.steam"), message: errorMessage(of: .steam))
        .errorAlert(L10n.string("ui.store.epic"), message: errorMessage(of: .epic))
        .errorAlert(L10n.string("ui.store.gog"), message: errorMessage(of: .gog))
    }

    /// What a storefront's account failed with, which its alert dismisses.
    private func errorMessage(of section: LibrarySection) -> Binding<String?> {
        Binding(get: { model.account(section)?.errorMessage }, set: { model.account(section)?.errorMessage = $0 })
    }

    private var libraries: some View {
        let openGame = openGame
        return ZStack {
            if let openGame {
                gamePage(openGame)
                    // Above Home, which falls back beneath it.
                    .zIndex(1)
                    .transition(.asymmetric(insertion: .identity, removal: .opacity))
            } else {
                HomePage(model: model, scrollMemory: scrollMemory, shelfMemory: shelfMemory, open: open)
                    .transition(.asymmetric(insertion: .identity,
                                            removal: .opacity.combined(with: .scale(scale: reduceMotion ? 1 : 0.94))))
            }
        }
        // An overlay rather than a safe area bar, as a library's.
        .overlay(alignment: .top) {
            HomeBar(model: model, bar: bar, game: openGame.map { ($0.title, $0.section) }, back: back,
                    openLibrary: model.show)
        }
        .environment(\.storefrontBar, bar)
        .environment(\.gameTransition, GameTransition(namespace: namespace, source: source, card: card))
        .ignoresSafeArea(.container, edges: .top)
    }

    @ViewBuilder private func gamePage(_ game: OpenGame) -> some View {
        switch game {
        case let .steam(game):
            SteamGameDetailView(model: model.steam, game: game, context: GamePageContext(
                heroArtwork: SteamView.heroArtwork(game), cover: SteamView.cover(game)
            ))
            .id(game.id)
        case let .epic(game):
            EpicGameDetailView(model: model.epic, game: game, context: GamePageContext(
                heroArtwork: EpicView.heroArtwork(game), cover: EpicView.cover(game)
            ))
            .id(game.id)
        case let .gog(game):
            GogGameDetailView(model: model.gog, game: game, context: GamePageContext(
                heroArtwork: GogView.heroArtwork(game), cover: GogView.cover(game)
            ))
            .id(game.id)
        }
    }

    private func open(_ game: HomeGame, from source: GameOpeningSource, card: AnyHashable?) {
        self.source = source
        self.card = card
        withAnimation(ObeliskStyle.pageAnimation(reduceMotion: reduceMotion)) {
            model.homeGame = game.id
            game.open()
        }
    }

    private func back() {
        withAnimation(ObeliskStyle.pageAnimation(reduceMotion: reduceMotion)) { model.closeHomeGame() }
    }
}

/// Home's bar: the search through every library, or, on a game's page, the
/// way back and the game's storefront. It reads the scroll state in its own
/// body, so scrolling redraws the bar alone.
private struct HomeBar: View {
    @Bindable var model: AppModel
    let bar: StorefrontBarState
    /// The title and storefront of the game whose page shows.
    let game: (title: String, section: LibrarySection)?
    let back: () -> Void
    let openLibrary: (LibrarySection) -> Void

    var body: some View {
        LibraryGlassBar(
            searchText: $model.searchText,
            searchLabel: L10n.string("ui.home.search_label"),
            showsBackdrop: bar.showsBackdrop,
            game: game.map { game in
                LibraryGlassBar.GameBar(title: game.title, libraryTitle: LibrarySection.home.title,
                                        showsTitle: bar.showsGameTitle, back: back)
            }
        ) {
            if let game {
                storeButton(game.section)
            }
        }
    }

    /// The game's storefront, opening its library.
    private func storeButton(_ section: LibrarySection) -> some View {
        let name = section.storeTitle
        let label = L10n.string("ui.library.store_accessibility", name)
        return Button { openLibrary(section) } label: {
            HStack(spacing: 8) {
                if let logo = section.logoAssetName {
                    Image(logo)
                        .renderingMode(.template)
                        .resizable()
                        .scaledToFit()
                        .frame(width: 16, height: 16)
                }
                Text(name)
                    .font(.system(size: 13, weight: .semibold))
            }
            .foregroundStyle(.primary)
            .padding(.horizontal, 14)
            .frame(height: 36)
            .modifier(StorefrontControlGlass())
            .contentShape(.capsule)
        }
        .obeliskButtonStyle(.plain)
        .fixedSize()
        .accessibilityLabel(label)
        .accessibilityIdentifier("home-store")
        .help(label)
    }
}

/// Home's scrolling content: its hero, shelves and libraries, or what every
/// library finds for the search.
private struct HomePage: View {
    @Bindable var model: AppModel
    let scrollMemory: LibraryScrollMemory
    let shelfMemory: ShelfScrollMemory
    /// Opens a game's page, from one of its cards or from the hero.
    let open: (HomeGame, GameOpeningSource, AnyHashable?) -> Void
    @State private var position: ScrollPosition
    @State private var tilesInOneRow = true
    @Environment(\.storefrontBar) private var bar

    /// A game's card on one shelf: a game may show on more than one.
    private struct Card: Hashable {
        let shelf: String
        let game: HomeGame.ID
    }

    private static var columns: [GridItem] {
        [GridItem(.adaptive(minimum: 160, maximum: 220), spacing: 16, alignment: .top)]
    }

    /// Whether the library tiles fit in one row.
    private static func tilesInOneRow(width: CGFloat) -> Bool {
        width >= 3 * 280 + 2 * 16
    }

    init(model: AppModel, scrollMemory: LibraryScrollMemory, shelfMemory: ShelfScrollMemory,
         open: @escaping (HomeGame, GameOpeningSource, AnyHashable?) -> Void) {
        self.model = model
        self.scrollMemory = scrollMemory
        self.shelfMemory = shelfMemory
        self.open = open
        let offset = scrollMemory.offset
        _position = State(initialValue: offset > 0 ? ScrollPosition(y: offset) : ScrollPosition())
    }

    /// Every library as Home shows it, in Home's order.
    private struct Libraries {
        let steam: HomeLibrary
        let epic: HomeLibrary
        let gog: HomeLibrary

        var all: [HomeLibrary] { [steam, epic, gog] }

        /// The library of a storefront's section; Home and Settings have none.
        subscript(section: LibrarySection) -> HomeLibrary? {
            switch section {
            case .steam: steam
            case .epic: epic
            case .gog: gog
            case .home, .settings: nil
            }
        }
    }

    private var homeLibraries: Libraries {
        Libraries(
            steam: model.steam.homeLibrary(in: .steam, cover: SteamView.cover, heroArtwork: SteamView.heroArtwork,
                                           heroLogo: SteamView.heroLogo),
            epic: model.epic.homeLibrary(in: .epic, cover: EpicView.cover, heroArtwork: EpicView.heroArtwork,
                                         heroLogo: EpicView.heroLogo),
            gog: model.gog.homeLibrary(in: .gog, cover: GogView.cover, heroArtwork: GogView.heroArtwork,
                                       heroLogo: GogView.heroLogo)
        )
    }

    /// What every library finds for `query`, by name.
    private func homeResults(matching query: String) -> [HomeGame] {
        LibraryOrder.merge([
            model.steam.homeResults(matching: query, in: .steam, cover: SteamView.cover,
                                    heroArtwork: SteamView.heroArtwork, heroLogo: SteamView.heroLogo),
            model.epic.homeResults(matching: query, in: .epic, cover: EpicView.cover,
                                   heroArtwork: EpicView.heroArtwork, heroLogo: EpicView.heroLogo),
            model.gog.homeResults(matching: query, in: .gog, cover: GogView.cover,
                                  heroArtwork: GogView.heroArtwork, heroLogo: GogView.heroLogo),
        ], title: \.title, rank: { _ in nil })
    }

    var body: some View {
        let query = model.searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        let libraries = query.isEmpty ? homeLibraries : nil
        let shelves = libraries.map { HomeShelves($0.all) }
        let heroGame = shelves?.hero
        let backdropOffset = heroGame == nil ? 0 : ObeliskStyle.libraryHeroHeight
        return StorefrontScrollView {
            VStack(alignment: .leading, spacing: 0) {
                if let heroGame {
                    hero(heroGame)
                }
                VStack(alignment: .leading, spacing: 30) {
                    notices
                    if let shelves, let libraries {
                        if !shelves.recent.isEmpty {
                            CoverShelf(title: L10n.string("ui.home.recent"), items: shelves.recent, id: "recent",
                                       memory: shelfMemory) { card($0, on: "recent") }
                        }
                        if !shelves.installed.isEmpty {
                            CoverShelf(title: L10n.string("ui.home.installed"), items: shelves.installed, id: "installed",
                                       memory: shelfMemory) { card($0, on: "installed") }
                        }
                        if !shelves.verified.isEmpty {
                            CoverShelf(title: L10n.string("ui.home.verified", AppIdentity.name),
                                       items: shelves.verified, id: "verified",
                                       memory: shelfMemory) { card($0, on: "verified") }
                        }
                        self.libraries(libraries)
                    } else {
                        results(homeResults(matching: query))
                    }
                }
                .padding(.top, heroGame == nil ? 12 : 24)
                .padding(.bottom, 36)
            }
        }
        .scrollPosition($position)
        // The results, and Home back from them, show from their top.
        .onChange(of: query.isEmpty) { position.scrollTo(edge: .top) }
        .onScrollGeometryChange(for: CGFloat.self, of: { $0.contentOffset.y + $0.contentInsets.top }) { _, offset in
            scrollMemory.offset = offset
        }
        // Whether shelves are under the bar, rather than the hero's artwork
        // or nothing at all.
        .onScrollGeometryChange(for: Bool.self, of: { $0.contentOffset.y + $0.contentInsets.top > backdropOffset }) { _, under in
            bar?.showsBackdrop = under
            bar?.showsGameTitle = false
        }
        .ignoresSafeArea(.container, edges: .top)
    }

    private var hasNotices: Bool {
        model.accounts.contains { $0.storageWarning != nil }
    }

    @ViewBuilder private var notices: some View {
        if hasNotices {
            VStack(alignment: .leading, spacing: 22) {
                ForEach(LibrarySection.storefronts) { section in
                    if let account = model.account(section) { notice(account) }
                }
            }
            .padding(.horizontal, ObeliskStyle.contentPadding)
        }
    }

    private func notice(_ account: any StorefrontAccount) -> StorefrontWarningBanner {
        StorefrontWarningBanner(message: Binding(get: { account.storageWarning }, set: { account.storageWarning = $0 }))
    }

    private func hero(_ game: HomeGame) -> some View {
        let section = game.id.section
        return LibraryHero(
            title: game.title,
            details: [
                section.logoAssetName.map { LibraryHeroDetail(logo: $0, text: section.storeTitle) },
                game.lastPlayed.map { LibraryHeroDetail(symbol: "calendar", text: GameFormatting.lastPlayed($0)) },
                game.playtime.map { LibraryHeroDetail(symbol: "clock", text: GameFormatting.playtime($0)) },
            ].compactMap { $0 },
            action: game.action,
            open: { open(game, .hero, nil) }
        ) {
            game.hero.artwork()
        } logo: {
            game.hero.logo()
        }
    }

    private func card(_ game: HomeGame, on shelf: String) -> some View {
        let card = AnyHashable(Card(shelf: shelf, game: game.id))
        return LibraryCoverCard(
            title: game.title,
            accessibilityLabel: L10n.string("ui.library.game_accessibility", game.title,
                                            GameFormatting.lastPlayed(game.lastPlayed)),
            help: [game.title, game.id.section.storeName, game.playtime.map(GameFormatting.playtime)]
                .compactMap { $0 }
                .joined(separator: " · "),
            store: game.id.section,
            action: game.action,
            matchID: card,
            open: { open(game, .cover, card) }
        ) {
            game.cover()
        }
    }

    private func libraries(_ libraries: Libraries) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(L10n.string("ui.library.your_libraries"))
                .font(.system(size: 22, weight: .bold))
                .frame(height: 36)
                .accessibilityAddTraits(.isHeader)
            StorefrontRows(inOneRow: tilesInOneRow) { section, _ in
                LibraryTile(
                    section: section,
                    title: section.storeTitle,
                    status: model.standing(of: section),
                    covers: libraries[section]?.covers.map { $0.cover() } ?? [],
                    open: { model.selection = section }
                )
            }
            .onGeometryChange(for: Bool.self, of: { Self.tilesInOneRow(width: $0.size.width) }) { tilesInOneRow = $0 }
        }
        .padding(.horizontal, ObeliskStyle.contentPadding)
    }

    /// The stores whose search Home offers for a game no library holds: those
    /// of the storefronts signed in.
    private var stores: [StoreSearch] {
        let steam = StoreSearch(section: model.steam.section, page: model.steam.storeSearch)
        let epic = StoreSearch(section: model.epic.section, page: model.epic.storeSearch)
        return [model.steam.isConnected ? steam : nil, model.epic.isConnected ? epic : nil].compactMap { $0 }
    }

    /// A signed-in library is still fetching its first games, so a search
    /// that finds nothing cannot tell yet.
    private var isFetching: Bool {
        model.accounts.contains { $0.isFetchingFirstGames }
    }

    private func results(_ games: [HomeGame]) -> some View {
        VStack(alignment: .leading, spacing: 22) {
            VStack(alignment: .leading, spacing: 3) {
                Text(L10n.string("ui.home.results"))
                    .font(.system(size: 22, weight: .bold))
                    .accessibilityAddTraits(.isHeader)
                Text(L10n.string("ui.library.game_count", Int64(games.count)))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            }
            if games.isEmpty, isFetching {
                ProgressView(L10n.string("ui.library.fetching")).frame(maxWidth: .infinity, minHeight: 260)
            } else if games.isEmpty {
                StorefrontEmptyLibrary(searchText: $model.searchText, stores: stores, refresh: model.refresh)
            } else {
                LazyVGrid(columns: Self.columns, alignment: .leading, spacing: 18) {
                    ForEach(games) { card($0, on: "search") }
                }
            }
        }
        .padding(.horizontal, ObeliskStyle.contentPadding)
    }
}

/// Home while no storefront is signed in: a sign-in page whose cards lead to
/// each storefront's own sign-in, Steam and Epic side by side over GOG.
private struct HomeSignIn: View {
    @Bindable var model: AppModel

    var body: some View {
        SignInPage(section: .home, title: L10n.string("ui.home.welcome_title", AppIdentity.name),
                   subtitle: L10n.string("ui.home.welcome_subtitle")) {
            StorefrontRows { section, spansRow in
                StorefrontSignInCard(section: section, isConnecting: model.standing(of: section).isBusy,
                                     spansRow: spansRow) {
                    model.selection = section
                }
            }
            .frame(maxWidth: 640)
        }
    }
}
