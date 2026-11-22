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

    /// An account's game whose page Home opened.
    private enum OpenGame {
        case steam(SteamModel, SteamGame), epic(EpicModel, EpicGame), gog(GogModel, GogGame)

        var title: String {
            switch self {
            case let .steam(_, game): game.name
            case let .epic(_, game): game.title
            case let .gog(_, game): game.title
            }
        }

        var slot: AccountSlot {
            switch self {
            case let .steam(account, _): account.slot
            case let .epic(account, _): account.slot
            case let .gog(account, _): account.slot
            }
        }
    }

    /// The game Home opened, while its account still shows its page.
    private var openGame: OpenGame? {
        guard let id = model.homeGame else { return nil }
        func opened<Game: Identifiable>(_ game: Game?) -> Game? {
            game.flatMap { "\($0.id)" == id.game ? $0 : nil }
        }
        switch id.section {
        case .steam: return model.steam[id.slot].flatMap { account in opened(account.selectedGame).map { .steam(account, $0) } }
        case .epic: return model.epic[id.slot].flatMap { account in opened(account.selectedGame).map { .epic(account, $0) } }
        case .gog: return model.gog[id.slot].flatMap { account in opened(account.selectedGame).map { .gog(account, $0) } }
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

    /// What an account of a storefront failed with, which its alert
    /// dismisses: the first that failed, then the next.
    private func errorMessage(of section: LibrarySection) -> Binding<String?> {
        Binding(get: { failed(in: section)?.errorMessage }, set: { failed(in: section)?.errorMessage = $0 })
    }

    private func failed(in section: LibrarySection) -> (any StorefrontAccount)? {
        model.accounts(of: section)?.accounts.first { $0.errorMessage != nil }
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
            HomeBar(model: model, bar: bar, game: openGame.map { ($0.title, $0.slot) }, back: back,
                    openLibrary: { model.show(.account($0)) })
        }
        .environment(\.storefrontBar, bar)
        .environment(\.gameTransition, GameTransition(namespace: namespace, source: source, card: card))
        .ignoresSafeArea(.container, edges: .top)
    }

    @ViewBuilder private func gamePage(_ game: OpenGame) -> some View {
        switch game {
        case let .steam(account, game):
            SteamGameDetailView(model: account, game: game, context: GamePageContext(
                heroArtwork: account.heroArtwork(game), cover: account.cover(game),
                heroLogo: AnyView(account.heroLogo(game))
            ))
            .id(game.id)
        case let .epic(account, game):
            EpicGameDetailView(model: account, game: game, context: GamePageContext(
                heroArtwork: EpicView.heroArtwork(game), cover: EpicView.cover(game),
                heroLogo: AnyView(EpicView.heroLogo(game))
            ))
            .id(game.id)
        case let .gog(account, game):
            GogGameDetailView(model: account, game: game, context: GamePageContext(
                heroArtwork: GogView.heroArtwork(game), cover: GogView.cover(game),
                heroLogo: AnyView(GogView.heroLogo(game))
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
    /// The title and account of the game whose page shows.
    let game: (title: String, slot: AccountSlot)?
    let back: () -> Void
    let openLibrary: (AccountSlot) -> Void

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
                storeButton(game.slot)
            }
        }
    }

    /// The game's storefront, opening its account's library.
    private func storeButton(_ slot: AccountSlot) -> some View {
        let section = slot.section
        let name = model.storeTitle(of: slot)
        let label = L10n.string("ui.library.store_accessibility", name)
        return Button { openLibrary(slot) } label: {
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

    /// An account's library as Home shows it.
    private struct AccountHome: Identifiable {
        let slot: AccountSlot
        let library: HomeLibrary
        var id: AccountSlot { slot }
    }

    /// Every account's library, in Home's order: the sidebar's.
    private var homeLibraries: [AccountHome] {
        let steam = model.steam.accounts.map { account in
            AccountHome(slot: account.slot, library: account.homeLibrary(
                cover: account.cover, heroArtwork: account.heroArtwork, heroLogo: account.heroLogo))
        }
        let epic = model.epic.accounts.map { account in
            AccountHome(slot: account.slot, library: account.homeLibrary(
                cover: EpicView.cover, heroArtwork: EpicView.heroArtwork, heroLogo: EpicView.heroLogo))
        }
        let gog = model.gog.accounts.map { account in
            AccountHome(slot: account.slot, library: account.homeLibrary(
                cover: GogView.cover, heroArtwork: GogView.heroArtwork, heroLogo: GogView.heroLogo))
        }
        return steam + epic + gog
    }

    /// What every library finds for `query`, by name.
    private func homeResults(matching query: String) -> [HomeGame] {
        let steam = model.steam.accounts.map { account in
            account.homeResults(matching: query, cover: account.cover, heroArtwork: account.heroArtwork,
                                heroLogo: account.heroLogo)
        }
        let epic = model.epic.accounts.map { account in
            account.homeResults(matching: query, cover: EpicView.cover, heroArtwork: EpicView.heroArtwork,
                                heroLogo: EpicView.heroLogo)
        }
        let gog = model.gog.accounts.map { account in
            account.homeResults(matching: query, cover: GogView.cover, heroArtwork: GogView.heroArtwork,
                                heroLogo: GogView.heroLogo)
        }
        return LibraryOrder.merge(steam + epic + gog, title: \.title, rank: { _ in nil }).oncePerStorefront
    }

    var body: some View {
        let query = model.searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        let libraries = query.isEmpty ? homeLibraries : nil
        let shelves = libraries.map { HomeShelves($0.map(\.library)) }
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
                ForEach(model.accounts, id: \.slot) { notice($0) }
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
                section.logoAssetName.map { LibraryHeroDetail(logo: $0, text: model.storeTitle(of: game.id.slot)) },
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
            help: [game.title, model.storeTitle(of: game.id.slot), game.playtime.map(GameFormatting.playtime)]
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

    private func libraries(_ libraries: [AccountHome]) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(L10n.string("ui.library.your_libraries"))
                .font(.system(size: 22, weight: .bold))
                .frame(height: 36)
                .accessibilityAddTraits(.isHeader)
            StorefrontRows(items: libraries, inOneRow: tilesInOneRow) { account, _ in
                LibraryTile(
                    slot: account.slot,
                    title: account.slot.section.storeTitle,
                    status: model.standing(of: account.slot),
                    covers: account.library.covers.map { $0.cover() },
                    open: { model.selection = .account(account.slot) }
                )
            }
            .onGeometryChange(for: Bool.self, of: { Self.tilesInOneRow(width: $0.size.width) }) { tilesInOneRow = $0 }
        }
        .padding(.horizontal, ObeliskStyle.contentPadding)
    }

    /// The stores whose search Home offers for a game no library holds: those
    /// of the storefronts signed in, each as its first account signed in.
    private var stores: [StoreSearch] {
        let steam = model.steam.accounts.first(where: \.isConnected).map {
            StoreSearch(section: .steam, page: $0.storeSearch)
        }
        let epic = model.epic.accounts.first(where: \.isConnected).map {
            StoreSearch(section: .epic, page: $0.storeSearch)
        }
        return [steam, epic].compactMap { $0 }
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
            // Signed in nowhere, each storefront's first slot takes its sign-in.
            StorefrontRows(items: LibrarySection.storefronts.compactMap { model.accounts(of: $0)?.accounts.first?.slot }) {
                slot, spansRow in
                StorefrontSignInCard(section: slot.section, isConnecting: model.standing(of: slot).isBusy,
                                     spansRow: spansRow) {
                    model.selection = .account(slot)
                }
            }
            .frame(maxWidth: 640)
        }
    }
}
