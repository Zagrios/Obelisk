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
    typealias Game = EpicGame
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
    static func keychain() -> any CredentialStore<WebAccountCredentials> { EpicKeychain() }

    static func games(_ listed: WebAccountGames) -> [EpicGame] {
        if case let .epic(games) = listed { games } else { [] }
    }
    static func cachedLibrary(in cache: LibraryCache, account: String)
        -> [(game: EpicGame, playtimeSeconds: UInt64?, achievements: Double?)] {
        cache.epicLibrary(accountId: account).map { ($0.game, $0.playtimeSeconds, $0.achievementShare) }
    }
    /// Epic's achievements take a bare language code, such as "fr".
    static func achievementLocale(_ language: String) -> String {
        Locale(identifier: language).language.languageCode?.identifier ?? "en"
    }
    @MainActor static func describe(_ error: Error) -> String {
        switch error {
        case WebAccountError.Network: L10n.string("model.epic.network")
        case WebAccountError.Unauthorized: L10n.string("model.epic.unauthorized")
        case WebAccountError.InvalidCode: L10n.string("model.epic.invalidCode")
        case WebAccountError.InvalidResponse: L10n.string("model.epic.invalidResponse")
        case let WebAccountError.Service(message): L10n.message(message)
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

extension EpicGame: WebGame {
    var libraryID: String { epicLibraryID(namespace: namespace, appName: appName) }
    public var id: String { libraryID }

    var product: GameOffer {
        .epic(namespace: namespace, catalogItemId: catalogItemId, appName: appName, title: title)
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
    @ObservationIgnored private var session: WebSession?
    @ObservationIgnored private var account: String?
    /// The player's choice, which every friends connection registers.
    @ObservationIgnored private var status = OnlineStatus.online
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

    static var showsStatus: Bool { true }

    func setStatus(_ status: OnlineStatus) {
        self.status = status
        session?.setStatus(status: status)
    }

    func connect(_ session: WebSession, account: String) {
        self.session = session
        self.account = account
        // Before its first registration, so it never shows Online first.
        session.setStatus(status: status)
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
                    // Only an Epic session has friends to tell.
                    let snapshot = await session.nextFriends(afterRevision: revision)
                    guard let self, !Task.isCancelled, let snapshot, snapshot.accountId == account else { return }
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
