import Foundation
import Observation

/// Epic library entries are unique per namespace and app name across platforms.
func epicLibraryID(namespace: String, appName: String) -> String { "\(namespace):\(appName)" }
/// The game a library ID names: an Epic namespace has no colon.
func epicGameID(_ libraryID: String) -> (namespace: String, appName: String)? {
    guard let colon = libraryID.firstIndex(of: ":") else { return nil }
    return (String(libraryID[..<colon]), String(libraryID[libraryID.index(after: colon)...]))
}

typealias EpicModel = WebAccountModel<EpicGames>

extension EpicGames: WebStorefront {
    typealias Session = any EpicSessionProtocol
    typealias Credentials = EpicCredentials
    typealias Game = EpicGame
    typealias Details = EpicGameDetails
    typealias Presence = EpicFriends

    static let storeID = "epic"
    static let storeURL = URL(string: "https://store.epicgames.com/")!
    /// The store's search for `query`.
    static func storeSearchURL(_ query: String) -> URL {
        var url = URLComponents(string: "https://store.epicgames.com/browse")!
        url.queryItems = [URLQueryItem(name: "q", value: query), URLQueryItem(name: "sortBy", value: "relevancy"),
                          URLQueryItem(name: "sortDir", value: "DESC")]
        // A `+` left as is reads as a space.
        url.percentEncodedQuery = url.percentEncodedQuery?.replacingOccurrences(of: "+", with: "%2B")
        return url.url!
    }
    static let webSignIn = WebSignIn.epic
    static func keychain() -> any CredentialStore<EpicCredentials> { EpicKeychain() }

    static func signIn(code: String, library: LibraryCache?) async throws -> Session {
        try await EpicSession.signIn(authorizationCode: code, library: library)
    }
    static func restore(_ credentials: EpicCredentials, library: LibraryCache?) async throws -> Session {
        try await EpicSession.restore(credentials: credentials, library: library)
    }
    static func accountID(_ credentials: EpicCredentials) -> String { credentials.accountId }
    static func accountName(_ credentials: EpicCredentials) -> String { credentials.displayName }
    static func rotating(_ session: Session) -> RotatingSession<EpicCredentials> {
        RotatingSession(
            identity: ObjectIdentifier(session),
            credentials: { try await session.credentials() },
            persist: { (saver: any CredentialSave) in await session.persist(credentials: saver) },
            shutdownObservers: { session.shutdownObservers() }
        )
    }
    static func disconnect(_ session: Session) async throws { try await session.disconnect() }
    /// Each address spends a new exchange code of the session.
    static func webSignInURL(session: Session, to destination: URL) async throws -> URL? {
        URL(string: try await session.webSignInUrl(redirectUrl: destination.absoluteString))
    }

    static func games(of session: Session) async throws -> [EpicGame] { try await session.games() }
    /// Epic answers every game's playtime in one request, by app name.
    static func playtimes(of session: Session, games: [EpicGame]) async throws -> [String: Duration] {
        let seconds = Dictionary(try await session.playtimes().map { ($0.appName, $0.seconds) }, uniquingKeysWith: max)
        return Dictionary(games.compactMap { game in seconds[game.appName].map { (game.id, Duration.seconds(Int64(clamping: $0))) } },
                          uniquingKeysWith: max)
    }
    /// Epic tells achievements by sandbox, which several games may share.
    static func achievementShares(of games: [EpicGame], session: Session, language: String) async throws -> [String: Double] {
        let shares = try await session.achievementShares(namespaces: Array(Set(games.map(\.namespace))).sorted(),
                                                         locale: achievementLocale(language))
        return Dictionary(games.compactMap { game in shares[game.namespace].map { (game.id, $0) } },
                          uniquingKeysWith: { first, _ in first })
    }
    static func cachedLibrary(in cache: LibraryCache, account: String)
        -> [(game: EpicGame, playtime: Duration?, achievements: Double?)] {
        cache.epicLibrary(accountId: account).map {
            ($0.game, $0.playtimeSeconds.map { .seconds(Int64(clamping: $0)) }, $0.achievementShare)
        }
    }
    static func details(of game: EpicGame, session: Session, language: String) async throws -> EpicGameDetails {
        try await session.gameDetails(namespace: game.namespace, catalogItemId: game.catalogItemId,
                                      appName: game.appName)
    }
    static func achievements(of game: EpicGame, session: Session, language: String,
                             fresh: Bool) async throws -> [GameAchievement] {
        try await session.achievements(namespace: game.namespace, locale: achievementLocale(language), fresh: fresh)
            .map(GameAchievement.init)
    }
    /// Epic's achievements take a bare language code, such as "fr".
    private static func achievementLocale(_ language: String) -> String {
        Locale(identifier: language).language.languageCode?.identifier ?? "en"
    }
    @MainActor static func installations(of session: Session, account: String, language: String,
                                          sessions: any StorefrontSessionsProtocol, receipts: URL,
                                          locations: ObeliskLocations) -> any InstallationsBackend<EpicGames> {
        guard let session = session as? EpicSession, let sessions = sessions as? StorefrontSessions else {
            return NoInstallations<EpicGames>()
        }
        return RustInstallations<EpicGames>(try! AccountInstallations.epic(
            session: session, accountId: account, language: language, sessions: sessions,
            receipts: receipts.path, locations: locations
        ))
    }
    static func isUnauthorized(_ error: Error) -> Bool {
        if case EpicError.Unauthorized = error { true } else { false }
    }
    @MainActor static func describe(_ error: Error) -> String {
        switch error {
        case EpicError.Network: L10n.string("model.epic.network")
        case EpicError.Unauthorized: L10n.string("model.epic.unauthorized")
        case EpicError.InvalidCode: L10n.string("model.epic.invalidCode")
        case EpicError.InvalidResponse: L10n.string("model.epic.invalidResponse")
        case let EpicError.Service(message): L10n.message(message)
        case let browserError as WebAuthenticationError: browserError.localizedDescription
        case let failure as GameInstallationFailure: failure.localizedDescription
        default: L10n.string("model.epic.failure")
        }
    }

    static var sessionNotSaved: String { L10n.string("model.epic.sessionNotSaved") }
    static var revocationUnconfirmed: String { L10n.string("model.epic.revocationUnconfirmed") }
    static var otherGameRunning: String { L10n.string("model.epic.otherGameRunning") }
    static var cloudTitle: String { L10n.string("ui.epic.cloud.title") }
}

extension EpicGameDetails: WebGameDetails {}

extension EpicGame: WebGame {
    var libraryID: String { epicLibraryID(namespace: namespace, appName: appName) }
    public var id: String { libraryID }

    /// What Epic can install of the game. Epic picks the build as the
    /// Transfer starts.
    var offer: GameOffer? {
        guard macos || windows else { return nil }
        return .epic(namespace: namespace, catalogItemId: catalogItemId, appName: appName, title: title)
    }

    func libraryValue(for sort: StorefrontLibrarySort) -> Double? {
        sort == .dateAdded ? addedAt.map { Double($0) } : nil
    }
}

/// Epic's friends: the connected account's social socket. Waking the Mac retries it.
@MainActor
@Observable
final class EpicFriends: AccountPresence {
    private(set) var state: FriendsPresentationState?
    private(set) var isPresented = false
    @ObservationIgnored private var session: (any EpicSessionProtocol)?
    @ObservationIgnored private var account: String?
    /// Cancelled when the observation is revoked or replaced, which rejects its later snapshots.
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var wakeObserver: SystemWakeObservation?

    init() {
        wakeObserver = SystemWakeObservation { [weak self] in
            Task { @MainActor [weak self] in self?.retry() }
        }
    }

    deinit {
        task?.cancel()
    }

    func connect(_ session: any EpicSessionProtocol, account: String) {
        self.session = session
        self.account = account
        state = .epic(accountID: account)
        observe(reconnect: false)
    }

    func revoke() {
        task?.cancel()
        task = nil
        session = nil
        account = nil
        state = nil
        isPresented = false
    }

    func open() {
        guard state != nil else { return }
        isPresented = true
    }

    func close() { isPresented = false }

    /// Replaces only the social socket. The OAuth session and downloads stay
    /// alive. Does nothing once the account is revoked.
    func retry() {
        guard session != nil, state?.connection != .authenticationRequired else { return }
        observe(reconnect: true)
    }

    private func observe(reconnect: Bool) {
        guard let session, let account else { return }
        task?.cancel()
        task = Task { [weak self] in
            guard !Task.isCancelled else { return }
            await withTaskCancellationHandler {
                if reconnect { await session.reconnectFriends() }
                else { await session.startFriends() }
                var revision: UInt64 = 0
                while !Task.isCancelled {
                    let snapshot = await session.nextFriends(afterRevision: revision)
                    guard let self, !Task.isCancelled, snapshot.accountId == account else { return }
                    revision = snapshot.revision
                    if snapshot.connection == .stopped { return }
                    var state = self.state
                    state?.apply(snapshot)
                    // Repeated snapshots do not invalidate unchanged presentation.
                    if state != self.state { self.state = state }
                }
            } onCancel: { session.stopFriends() }
        }
    }
}

extension WebAccountModel where Store == EpicGames {
    /// Epic's store searching for `query`, signed in as the account.
    func storeSearch(_ query: String) async -> WebPageStart {
        await storePage(at: EpicGames.storeSearchURL(query))
    }
}
