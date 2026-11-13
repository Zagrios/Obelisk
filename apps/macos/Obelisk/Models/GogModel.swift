import Foundation

typealias GogModel = WebAccountModel<GogGames>

extension GogGames: WebStorefront {
    typealias Game = GogGame

    static let storeID = "gog"
    /// The store through its site's own sign-in, as its Sign in button
    /// opens it: GOG's site takes no token of the app, but its sign-in page
    /// remembers the account Obelisk's sign-in signed in there, and then
    /// returns to the store signed in without asking.
    static let storeURL: URL = {
        let back = "https://www.gog.com/on_login_success?returnTo=/"
            .addingPercentEncoding(withAllowedCharacters: .alphanumerics)!
        return URL(string: "https://login.gog.com/auth?client_id=46755278331571209&response_type=code&redirect_uri=\(back)")!
    }()
    static let webSignIn = WebSignIn.gog
    static func keychain() -> any CredentialStore<WebAccountCredentials> { GogKeychain() }

    static func games(_ listed: WebAccountGames) -> [GogGame] {
        if case let .gog(games) = listed { games } else { [] }
    }
    static func cachedLibrary(in cache: LibraryCache, account: String)
        -> [(game: GogGame, playtimeSeconds: UInt64?, achievements: Double?)] {
        cache.gogLibrary(userId: account).map { ($0.game, $0.playtimeSeconds, $0.achievementShare) }
    }
    /// GOG names achievements in a full language tag, such as "fr-FR".
    static func achievementLocale(_ language: String) -> String {
        Locale(identifier: language).identifier(.bcp47)
    }
    @MainActor static func describe(_ error: Error) -> String {
        switch error {
        case WebAccountError.Network: L10n.string("model.gog.network")
        case WebAccountError.Unauthorized: L10n.string("model.gog.unauthorized")
        case WebAccountError.InvalidCode: L10n.string("model.gog.invalidCode")
        case WebAccountError.InvalidResponse: L10n.string("model.gog.invalidResponse")
        case let WebAccountError.Service(message): L10n.message(message)
        case let browserError as WebAuthenticationError: browserError.localizedDescription
        case let failure as GameInstallationFailure: failure.localizedDescription
        default: L10n.string("model.gog.failure")
        }
    }

    static var sessionNotSaved: String { L10n.string("model.gog.sessionNotSaved") }
    static var revocationUnconfirmed: String { L10n.string("model.gog.revocationUnconfirmed") }
    static var otherGameRunning: String { L10n.string("model.gog.otherGameRunning") }
    /// GOG asks no Cloud question: Obelisk does not sync its saves.
    static var cloudTitle: String { L10n.string("ui.store.gog") }
}

extension GogGame: WebGame {
    public var id: String { productId }

    var product: GameOffer { .gog(productId: productId, title: title) }

    func libraryValue(for sort: StorefrontLibrarySort) -> Double? {
        sort == .dateAdded ? addedAt.map { Double($0) } : nil
    }
}
