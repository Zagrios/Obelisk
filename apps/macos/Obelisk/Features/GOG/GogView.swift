import SwiftUI

struct GogView: View {
    @Bindable var model: GogModel
    @State private var libraryScroll = LibraryScrollMemory()

    var body: some View {
        Group {
            if model.showsLibrary {
                browser
            } else {
                WebSignInView(
                    model: model,
                    section: .gog,
                    title: L10n.string("ui.gog.sign_in_title"),
                    subtitle: L10n.string("ui.gog.sign_in_subtitle"),
                    connecting: L10n.string("ui.gog.connecting"),
                    accountSignIn: L10n.string("ui.gog.account_sign_in"),
                    instructions: L10n.string("ui.gog.sign_in_instructions"),
                    signInLabel: L10n.string("ui.gog.sign_in")
                )
            }
        }
        .task { await model.start() }
        .errorAlert(L10n.string("ui.store.gog"), message: $model.errorMessage)
    }

    private var browser: some View {
        StorefrontBrowser(
            model: model,
            libraryTitle: L10n.string("ui.gog.library"),
            accountName: model.accountName,
            avatarURL: model.avatarURL,
            storePage: model.storePage,
            scrollMemory: libraryScroll,
            heroArtwork: Self.heroArtwork,
            heroLogo: Self.heroLogo,
            artwork: Self.cover
        ) {
            accountMenu
        } gamePage: { game, context in
            GogGameDetailView(model: model, game: game, context: context)
        }
    }

    /// A game's wide artwork, as its library's hero and its page show it.
    static func heroArtwork(_ game: GogGame) -> StorefrontArtwork {
        StorefrontArtwork(imageURL: game.headerImageUrl ?? game.imageUrl, maxPixelSize: 2048)
    }

    /// The game's title: GOG's backgrounds leave out its name.
    static func heroLogo(_ game: GogGame) -> LibraryHeroLogo {
        LibraryHeroLogo(url: nil, title: game.title)
    }

    static func cover(_ game: GogGame) -> StorefrontArtwork {
        StorefrontArtwork(imageURL: game.imageUrl ?? game.headerImageUrl)
    }

    @ViewBuilder private var accountMenu: some View {
        Text(model.accountName ?? L10n.string("ui.gog.account"))
        Text(L10n.string("ui.library.account_game_count", Int64(model.library.games.count)))
        Divider()
        Button(L10n.string("ui.library.refresh"), systemImage: "arrow.clockwise", action: model.refresh)
            .disabled(model.isLoadingGames)
        Toggle(L10n.string("ui.account.remember_me"), isOn: $model.rememberSession)
        Divider()
        Button(L10n.string("ui.account.sign_out"), systemImage: "rectangle.portrait.and.arrow.right", action: model.signOut)
    }
}
