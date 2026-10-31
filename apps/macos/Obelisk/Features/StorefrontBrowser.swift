import SwiftUI

/// A signed-in storefront: its library, or the page of the game it opened,
/// under one floating glass bar. Opening a game from its cover flies the cover
/// from the library to the game's page while the library falls back and the
/// page's artwork settles in; going back flies it home.
struct StorefrontBrowser<Model: LibraryStorefront, HeroArtwork: View, HeroLogo: View, Artwork: View,
                         AccountMenu: View, GamePage: View>: View {
    @Bindable var model: Model
    let libraryTitle: String
    let accountName: String?
    let avatarURL: URL?
    /// How the storefront's store opens, signed in as the account.
    let storePage: (@MainActor () async -> WebPageStart)?
    /// How its store's search opens, which the library offers when its own
    /// search finds nothing.
    let storeSearch: (@MainActor (String) async -> WebPageStart)?
    let scrollMemory: LibraryScrollMemory
    @ViewBuilder let heroArtwork: (Model.Game) -> HeroArtwork
    @ViewBuilder let heroLogo: (Model.Game) -> HeroLogo
    @ViewBuilder let artwork: (Model.Game) -> Artwork
    @ViewBuilder let accountMenu: AccountMenu
    @ViewBuilder let gamePage: (Model.Game, GamePageContext<HeroArtwork, Artwork>) -> GamePage
    @Namespace private var namespace
    @State private var bar = StorefrontBarState()
    /// Where the open game came from, once a game opened with motion.
    @State private var source: GameOpeningSource?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.presentStore) private var presentStore

    init(
        model: Model,
        libraryTitle: String,
        accountName: String?,
        avatarURL: URL? = nil,
        storePage: (@MainActor () async -> WebPageStart)? = nil,
        storeSearch: (@MainActor (String) async -> WebPageStart)? = nil,
        scrollMemory: LibraryScrollMemory,
        @ViewBuilder heroArtwork: @escaping (Model.Game) -> HeroArtwork,
        @ViewBuilder heroLogo: @escaping (Model.Game) -> HeroLogo,
        @ViewBuilder artwork: @escaping (Model.Game) -> Artwork,
        @ViewBuilder accountMenu: () -> AccountMenu,
        @ViewBuilder gamePage: @escaping (Model.Game, GamePageContext<HeroArtwork, Artwork>) -> GamePage
    ) {
        self.model = model
        self.libraryTitle = libraryTitle
        self.accountName = accountName
        self.avatarURL = avatarURL
        self.storePage = storePage
        self.storeSearch = storeSearch
        self.scrollMemory = scrollMemory
        self.heroArtwork = heroArtwork
        self.heroLogo = heroLogo
        self.artwork = artwork
        self.accountMenu = accountMenu()
        self.gamePage = gamePage
    }

    private var section: LibrarySection { model.section }
    private var storeTitle: String { section.storeTitle }

    var body: some View {
        ZStack {
            if let game = model.selectedGame {
                gamePage(game, GamePageContext(heroArtwork: heroArtwork(game), cover: artwork(game)))
                    .id(game.id)
                    // Above the library it replaces, which falls back beneath it.
                    .zIndex(1)
                    // The page reveals itself; leaving, it fades over the
                    // library as its cover flies home.
                    .transition(.asymmetric(insertion: .identity, removal: .opacity))
            } else {
                StorefrontLibraryPage(
                    model: model,
                    storeTitle: storeTitle,
                    libraryTitle: libraryTitle,
                    scrollMemory: scrollMemory,
                    stores: storeSearch.map { [StoreSearch(section: section, page: $0)] } ?? [],
                    open: open,
                    heroArtwork: heroArtwork,
                    heroLogo: heroLogo,
                    artwork: artwork
                )
                .transition(.asymmetric(insertion: .identity,
                                        removal: .opacity.combined(with: .scale(scale: reduceMotion ? 1 : 0.94))))
            }
        }
        // An overlay rather than a safe area bar: on macOS the bar's scroll
        // edge effect cannot be turned off, and it veils the hero's artwork.
        .overlay(alignment: .top) {
            StorefrontBrowserBar(
                model: model,
                bar: bar,
                storeTitle: storeTitle,
                libraryTitle: libraryTitle,
                accountName: accountName,
                avatarURL: avatarURL,
                openStore: openStore,
                back: back
            ) {
                accountMenu
            }
        }
        .environment(\.storefrontBar, bar)
        .environment(\.gameTransition, GameTransition(namespace: namespace, source: source))
        .ignoresSafeArea(.container, edges: .top)
    }

    /// Opens the store over the whole window, when the storefront has one.
    private var openStore: (() -> Void)? {
        guard let storePage, let presentStore else { return nil }
        let store = StorePresentation(section: section, start: storePage)
        return { presentStore(store) }
    }

    private func open(_ game: Model.Game, from source: GameOpeningSource) {
        self.source = source
        withAnimation(ObeliskStyle.pageAnimation(reduceMotion: reduceMotion)) { model.open(game) }
    }

    private func back() {
        withAnimation(ObeliskStyle.pageAnimation(reduceMotion: reduceMotion)) { model.closeGame() }
    }
}

/// The browser's bar. It reads the scroll state and the unread messages in its
/// own body, so they redraw the bar alone.
private struct StorefrontBrowserBar<Model: LibraryStorefront, AccountMenu: View>: View {
    @Bindable var model: Model
    let bar: StorefrontBarState
    let storeTitle: String
    let libraryTitle: String
    let accountName: String?
    let avatarURL: URL?
    let openStore: (() -> Void)?
    let back: () -> Void
    @ViewBuilder let accountMenu: AccountMenu

    var body: some View {
        LibraryGlassBar(
            searchText: Bindable(model.library).searchText,
            searchLabel: L10n.string("ui.search.store_library", storeTitle),
            showsBackdrop: bar.showsBackdrop,
            game: model.selectedGame.map { game in
                LibraryGlassBar.GameBar(title: game.title, libraryTitle: libraryTitle,
                                        showsTitle: bar.showsGameTitle, back: back)
            }
        ) {
            LibraryAccountPill(storeTitle: storeTitle, accountName: accountName, avatarURL: avatarURL,
                               unreadCount: model.unreadCount) {
                accountMenu
            }
            if let openStore {
                StorefrontGlassButton(systemName: "storefront", label: L10n.string("ui.store.openStore", storeTitle),
                                      identifier: "storefront-store", action: openStore)
            }
        }
    }
}

/// A storefront's store, as the window presents it in a sheet.
struct StorePresentation: Identifiable {
    let section: LibrarySection
    let start: @MainActor () async -> WebPageStart

    var id: LibrarySection { section }
    var storeTitle: String { section.storeTitle }
}

/// The storefront's store in a sheet, in a web page signed in as the
/// account. Its close button or Escape closes it.
struct StorefrontStoreModal: View {
    let store: StorePresentation
    let close: () -> Void

    var body: some View {
        let title = L10n.string("ui.store.openStore", store.storeTitle)
        WebPagePanel(start: store.start, close: close, showsCloseButton: true, placeholderTitle: title,
                     identifier: "\(store.section.rawValue)-store", isPanel: false,
                     store: store.section)
            .presentationSizing(.fitted)
            .accessibilityLabel(title)
    }
}

/// What the page under a storefront's bar tells it as it scrolls.
@MainActor @Observable
final class StorefrontBarState {
    /// Whether content, rather than a hero's artwork or nothing, is under the bar.
    var showsBackdrop = false
    /// Whether a game's page scrolled its title under the bar.
    var showsGameTitle = false
}

/// Where a game's page was opened from.
enum GameOpeningSource {
    /// The game's cover in the library grid, which flies to the page.
    case cover
    /// The library's hero, whose artwork the page's own replaces in place.
    case hero
}

/// What a storefront's browser hands the page of the game it opened. The
/// artwork is the library's own, so the cover flies between the same image.
struct GamePageContext<HeroArtwork: View, Cover: View> {
    let heroArtwork: HeroArtwork
    let cover: Cover
}

/// How a library and a game's page hand the game's cover to each other.
struct GameTransition {
    let namespace: Namespace.ID
    /// Where the open game came from; nil when it opened without motion, as
    /// when a test or a refresh opens it.
    let source: GameOpeningSource?
    /// The card the page's cover flies from and back to, when its game alone
    /// does not name it: on Home, a game may show on more than one shelf.
    var card: AnyHashable? = nil
}

extension EnvironmentValues {
    @Entry var gameTransition: GameTransition?
    @Entry var storefrontBar: StorefrontBarState?
    /// Opens a storefront's store above the whole window.
    @Entry var presentStore: (@MainActor (StorePresentation) -> Void)?
}

/// Matches a game's cover in the library with its cover on the game's page.
/// The page's cover only matches when the game opened from its cover, so a
/// game opened from the hero does not pull a card from the grid.
struct GameCoverMatch: ViewModifier {
    let game: AnyHashable
    let isPage: Bool
    @Environment(\.gameTransition) private var transition
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private struct ID: Hashable {
        let game: AnyHashable
        let matched: Bool
    }

    func body(content: Content) -> some View {
        if let transition {
            content.matchedGeometryEffect(
                // While motion is reduced, the page only fades in and out.
                id: ID(game: isPage ? transition.card ?? game : game,
                       matched: !isPage || (transition.source == .cover && !reduceMotion)),
                in: transition.namespace
            )
        } else {
            content
        }
    }
}
