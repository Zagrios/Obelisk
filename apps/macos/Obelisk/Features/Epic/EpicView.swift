import SwiftUI

struct EpicView: View {
    @Bindable var model: EpicModel
    @State private var libraryScroll = LibraryScrollMemory()

    var body: some View {
        Group {
            if model.showsLibrary {
                browser
            } else {
                WebSignInView(
                    model: model,
                    section: .epic,
                    title: L10n.string("ui.epic.sign_in_title"),
                    subtitle: L10n.string("ui.epic.sign_in_subtitle"),
                    connecting: L10n.string("ui.epic.connecting"),
                    accountSignIn: L10n.string("ui.epic.account_sign_in"),
                    instructions: L10n.string("ui.epic.sign_in_instructions"),
                    signInLabel: L10n.string("ui.epic.sign_in")
                )
            }
        }
        .task { await model.start() }
        .errorAlert(L10n.string("ui.store.epic"), message: $model.errorMessage)
    }

    private var browser: some View {
        StorefrontBrowser(
            model: model,
            libraryTitle: L10n.string("ui.epic.library"),
            accountName: model.accountName,
            storePage: model.storePage,
            storeSearch: model.storeSearch,
            scrollMemory: libraryScroll,
            heroArtwork: Self.heroArtwork,
            heroLogo: Self.heroLogo,
            artwork: Self.cover
        ) {
            accountMenu
        } gamePage: { game, context in
            EpicGameDetailView(model: model, game: game, context: context)
        }
    }

    /// A game's wide artwork, as its library's hero and its page show it.
    static func heroArtwork(_ game: EpicGame) -> StorefrontArtwork {
        StorefrontArtwork(imageURL: game.headerImageUrl ?? game.imageUrl, maxPixelSize: 2048)
    }

    /// None: Epic's wide artwork already shows the game's name, and its
    /// catalog rarely has a separate logo.
    static func heroLogo(_ game: EpicGame) -> EmptyView { EmptyView() }

    static func cover(_ game: EpicGame) -> StorefrontArtwork {
        StorefrontArtwork(imageURL: game.imageUrl)
    }

    @ViewBuilder private var accountMenu: some View {
        Text(model.accountName ?? L10n.string("ui.epic.account"))
        Text(L10n.string("ui.library.account_game_count", Int64(model.library.games.count)))
        Divider()
        EpicFriendsMenuButton(model: model)
        Button(L10n.string("ui.library.refresh"), systemImage: "arrow.clockwise", action: model.refresh)
            .disabled(model.isLoadingGames)
        Toggle(L10n.string("ui.account.remember_me"), isOn: $model.rememberSession)
        Divider()
        Button(L10n.string("ui.account.sign_out"), systemImage: "rectangle.portrait.and.arrow.right", action: model.signOut)
    }
}

/// Reads the friends in its own body: their every change redraws the button
/// alone, not the library.
private struct EpicFriendsMenuButton: View {
    let model: EpicModel

    var body: some View {
        Button(model.friends?.menuTitle ?? L10n.string("ui.friends.menuLoading"), systemImage: "person.2", action: model.openFriends)
            .disabled(model.friends == nil)
            .accessibilityIdentifier("epic-friends-menu")
    }
}
