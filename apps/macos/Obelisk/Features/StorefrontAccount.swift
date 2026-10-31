import SwiftUI

/// One storefront account as the window tells it, whichever storefront it is:
/// how it stands, the sheet it asks over the whole window, its friends, and
/// what the app does with it apart from its library. `SteamModel` and
/// `WebAccountModel` (Epic, GOG) are its adapters, through `LibraryStorefront`.
@MainActor
protocol StorefrontAccount: AnyObject {
    var section: LibrarySection { get }
    /// Whether its library shows: connected, or its Library cache while the
    /// saved session restores.
    var showsLibrary: Bool { get }
    /// What its tile, its Settings row and its sign-in card tell.
    var standing: AccountStanding { get }
    /// What its last failed sign-in or load tells; clearing it dismisses it.
    var errorMessage: String? { get set }
    /// What a Keychain or receipt storage problem warns of.
    var storageWarning: String? { get set }
    /// Whether it is signed in and loads its first games: a search that
    /// finds nothing cannot tell yet.
    var isFetchingFirstGames: Bool { get }

    /// The question it asks over the whole window now: a newer Voile, then
    /// Rosetta, then a Cloud question.
    var pendingPrompt: AccountPrompt.Kind? { get }
    func promptSheet(_ kind: AccountPrompt.Kind) -> AnyView

    /// Its friends; nil signed out, and for a storefront without any (GOG).
    var friends: FriendsPresentationState? { get }
    var isFriendsPresented: Bool { get }
    /// Nothing for a storefront without friends.
    func closeFriends()
    func retryFriends()

    func signOut()
    func refresh()
    /// Restores its saved session, offering no sign-in. Returns when the
    /// restoration and the first library load end.
    func restoreSavedSession() async

    /// Returns from its game page to its library; nothing when none shows.
    func closeGamePage()
    /// Opens the page of the game Rust names `game`. False when the library
    /// does not list it, or the name is not a game of this storefront.
    func openGame(named game: String) -> Bool
    /// The running Transfer's progress, when it is the game Rust names
    /// `game`'s.
    func transfer(named game: String) -> GameTransfer?
    /// Any Game session, one still preparing included.
    var hasGameSession: Bool { get }
    func stopGames() async

    /// The directories its Installation receipts name.
    var installedDirectories: [String] { get }
    func setUpdatesAutomatically(_ updates: Bool)
}

/// What a storefront's tile, row and sign-in card tell of its account.
enum AccountStanding: Equatable {
    /// Its library shows. `installed` counts the library's games an
    /// Installation receipt names.
    case signedIn(account: String?, avatarURL: URL?, games: Int, installed: Int)
    /// Its saved session restores and no library shows yet.
    case restoring
    /// `connecting`: a sign-in runs.
    case signedOut(connecting: Bool)

    /// Whether a library is on its way or a sign-in runs, which the sign-in
    /// card shows as busy.
    var isBusy: Bool {
        switch self {
        case .restoring, .signedOut(connecting: true): true
        case .signedIn, .signedOut(connecting: false): false
        }
    }
}

/// A question an account asks over the whole window; one shows at a time.
struct AccountPrompt: Hashable {
    /// In the order an account asks them.
    enum Kind: Hashable { case voile, rosetta, cloud }
    let section: LibrarySection
    let kind: Kind
}

// MARK: Library storefronts

extension LibraryStorefront {
    var section: LibrarySection { Store.storefront.section }

    var standing: AccountStanding {
        if showsLibrary {
            return .signedIn(account: accountName, avatarURL: avatarURL, games: library.games.count,
                             installed: library.installedCount)
        }
        return isRestoring ? .restoring : .signedOut(connecting: isConnecting)
    }

    var isFetchingFirstGames: Bool { isLoadingGames && library.games.isEmpty }

    var pendingPrompt: AccountPrompt.Kind? {
        if gameSessions.voileUpdate != nil { return .voile }
        if rosetta.prompt != nil { return .rosetta }
        if Store.syncsCloud, cloud.question != nil { return .cloud }
        return nil
    }

    func promptSheet(_ kind: AccountPrompt.Kind) -> AnyView {
        switch kind {
        case .voile:
            guard let offer = gameSessions.voileUpdate else { return AnyView(EmptyView()) }
            return AnyView(VoileUpdateSheet(offer: offer, answer: gameSessions.answerVoileUpdate))
        case .rosetta:
            return AnyView(RosettaPromptView(model: rosetta, storefront: section.rawValue))
        case .cloud:
            return AnyView(CloudQuestionSheet(model: self))
        }
    }

    /// The guard stays: a bare `closeGame()` assigns observed state.
    func closeGamePage() {
        if selectedGame != nil { closeGame() }
    }

    func openGame(named game: String) -> Bool {
        guard let id = Store.game(game), let game = library.game(id) else { return false }
        open(game)
        return true
    }

    func transfer(named game: String) -> GameTransfer? { installations.transfer(named: game) }

    var hasGameSession: Bool { gameSessions.active != nil }

    func stopGames() async { await gameSessions.stop() }

    var installedDirectories: [String] { installations.receipts.map(\.directory) }

    func setUpdatesAutomatically(_ updates: Bool) { installations.updatesAutomatically = updates }
}

// MARK: Steam

extension SteamModel {
    var accountName: String? { displayName }

    func restoreSavedSession() async { await start(signIn: false) }
}

// MARK: Web storefronts

extension WebAccountModel {
    func restoreSavedSession() async { await start() }
}
