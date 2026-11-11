import Foundation
import Observation

/// A game of a storefront whose account signs in on the storefront's web
/// page. Its id names it in receipts, Game sessions, the Library cache and
/// its account's session.
protocol WebGame: LibraryGame, Sendable where ID == String {
    var macos: Bool { get }
    var windows: Bool { get }
    /// The game as its storefront's requests name it, whichever builds it
    /// ships: what its page's details ask about, and what a Transfer installs.
    var product: GameOffer { get }
}

extension WebGame {
    /// The build the game shows before its page's details load: the Mac
    /// build when there is one.
    var preferredPlatform: GamePlatform { macos ? .mac : .windows }
    /// What the storefront can install of the game; nil when it ships no
    /// build. The storefront picks the build as the Transfer starts.
    var offer: GameOffer? { macos || windows ? product : nil }
    /// Neither web storefront tells a playtime in its library entry, nor a
    /// last played date: the playtimes load after the games.
    var playtime: Duration? { nil }
    var lastPlayedOnStorefront: Date? { nil }
}

/// The Rust session of a web storefront account, as its UniFFI protocol: the
/// same for Epic and GOG.
typealias WebSession = any WebAccountSessionProtocol

/// What lives as long as an account's connection besides its library, such
/// as Epic's friends. Account revocation stops it before the session goes.
@MainActor
protocol AccountPresence: AnyObject {
    init()
    /// The session became the connected account's.
    func connect(_ session: WebSession, account: String)
    /// Stops its network work at once: nothing the revoked session sends later shows.
    func revoke()
    /// The connected account's friends; nil signed out.
    var state: FriendsPresentationState? { get }
    /// Whether the account's menu opened them.
    var isPresented: Bool { get }
    func open()
    func close()
    /// Replaces only the social socket.
    func retry()
    /// Whether the account shows to its friends with a status the player picks.
    static var showsStatus: Bool { get }
    /// How the account shows to its friends, the player's choice: from the
    /// next connection on, and at once on a live one.
    func setStatus(_ status: OnlineStatus)
}

/// The presence of a storefront that has none.
@MainActor
final class NoPresence: AccountPresence {
    init() {}
    static var showsStatus: Bool { false }
    func setStatus(_ status: OnlineStatus) {}
    func connect(_ session: WebSession, account: String) {}
    func revoke() {}
    var state: FriendsPresentationState? { nil }
    var isPresented: Bool { false }
    func open() {}
    func close() {}
    func retry() {}
}

/// A storefront whose account signs in on its own web page and whose Rust
/// session rotates its refresh credentials: Epic and GOG. One Rust session
/// serves both, so its enum only names what is the storefront's own: its
/// texts, its Keychain item, its game record and how the player's language
/// reads to it. Each text is an `L10n.string` of a literal key, so the
/// localization check sees every storefront's.
///
/// `SendableMetatype`: the model's `@Sendable` closures call `Store`'s statics.
protocol WebStorefront: InstallationStorefront, SendableMetatype where GameID == String, AccountID == String {
    associatedtype Game: WebGame
    associatedtype Presence: AccountPresence = NoPresence

    /// Names the storefront in receipts files and achievements.
    static var storeID: String { get }
    static var webSignIn: WebSignIn { get }
    /// The storefront's store, opened in Obelisk.
    static var storeURL: URL { get }
    @MainActor static var sessions: GameSessions<Self> { get }
    static func keychain() -> any CredentialStore<WebAccountCredentials>

    /// The storefront's games among those a session listed.
    static func games(_ listed: WebAccountGames) -> [Game]
    /// The account's Library cache, with each game's playtime, in seconds,
    /// and achievement share when they were known.
    static func cachedLibrary(in cache: LibraryCache, account: String)
        -> [(game: Game, playtimeSeconds: UInt64?, achievements: Double?)]
    /// The player's `language`, such as "fr-FR", as the storefront's
    /// achievements name it.
    static func achievementLocale(_ language: String) -> String

    static var sessionNotSaved: String { get }
    /// Added to the storage warning when the sign-out's revocation fails.
    static var revocationUnconfirmed: String { get }
    static var otherGameRunning: String { get }
}

/// The storefront refused the account: a restore forgets the saved session,
/// and any later call signs the account out.
private func isUnauthorized(_ error: Error) -> Bool {
    if case WebAccountError.Unauthorized = error { true } else { false }
}

/// A playtime as the session and the Library cache tell it, in seconds. A
/// count too large to hold saturates.
private func played(seconds: UInt64) -> Duration { .seconds(Int64(clamping: seconds)) }

extension RotatingSession where Credentials == WebAccountCredentials {
    /// How the remembered session saves each rotation of `session`'s credentials.
    init(_ session: WebSession) {
        self.init(
            identity: ObjectIdentifier(session),
            credentials: { try await session.credentials() },
            persist: { (saver: any CredentialSave) in await session.persist(credentials: saver) },
            shutdownObservers: { session.shutdownObservers() }
        )
    }
}

/// One account of a storefront that signs in on its web page: its sign-in
/// and remembered session, its library (from the Library cache while the
/// session restores), the game on screen, and its installations, Cloud,
/// Rosetta prompts and presence.
///
/// Every sign-in, cancellation and sign-out starts a new generation. An
/// answer from an older one changes nothing, and a session it produced is
/// revoked in its own task. One authentication runs at a time, and none
/// starts before a sign-out's Keychain removal ends.
@MainActor
@Observable
final class WebAccountModel<Store: WebStorefront> {
    typealias Game = Store.Game
    /// The Rust installations of an account a session signed in: its id, the
    /// player's language, and the file its receipts keep.
    typealias MakeInstallations = @MainActor (WebSession, String, String,
                                              any StorefrontSessionsProtocol, URL) -> any InstallationsBackend<Store>

    private(set) var accountName: String?
    private(set) var avatarURL: URL?
    /// The connected account.
    private var accountID: String?

    let gameSessions: GameSessions<Store>
    let cloud: CloudModel<Store>
    let rosetta: RosettaModel<GameSessions<Store>>
    let installations: GameInstallations<Store>
    /// The account's games, from the Library cache while the session
    /// restores. The share of each game's achievements the account unlocked
    /// loads after the playtimes.
    let library: AccountLibrary<Store, Game>
    let achievements = GameAchievementsModel(storeID: Store.storeID, describe: Store.describe)
    /// What lives with the connection, such as Epic's friends.
    let presence: Store.Presence
    /// How the account shows to its friends, from the account's menu; nil
    /// for a storefront without friends.
    let onlineStatus: OnlineStatusChoice?

    private(set) var selectedGame: Game?
    /// What the game's page reads besides its library entry.
    private(set) var selectedGameDetails: WebGameDetails?
    private(set) var isLoadingGameDetails = false
    private(set) var isRestoring = false
    private(set) var isConnecting = false
    private(set) var isLoadingGames = false
    private(set) var isSigningOut = false
    private(set) var canRetryRestore = false
    var errorMessage: String?
    var storageWarning: String? {
        get { rememberedSession.storageWarning }
        set { rememberedSession.storageWarning = newValue }
    }
    var rememberSession: Bool {
        get { rememberedSession.rememberSession }
        set { rememberedSession.rememberSession = newValue }
    }

    @ObservationIgnored private let rememberedSession: RememberedSession<WebAccountCredentials>
    @ObservationIgnored private let directory: URL
    @ObservationIgnored private let language: String
    @ObservationIgnored private let authorize: @MainActor () async throws -> String
    @ObservationIgnored private let createSession: @Sendable (String) async throws -> WebSession
    @ObservationIgnored private let restoreSession: @Sendable (WebAccountCredentials) async throws -> WebSession
    @ObservationIgnored private let makeInstallations: MakeInstallations
    @ObservationIgnored private var session: WebSession?
    @ObservationIgnored private var started = false
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var authenticationTask: Task<Void, Never>?
    @ObservationIgnored private var libraryTask: Task<Void, Never>?
    @ObservationIgnored private var detailsTask: Task<Void, Never>?

    init(
        directory: URL = AppIdentity.libraryDirectory,
        keychain: any CredentialStore<WebAccountCredentials> = Store.keychain(),
        authorize: @escaping @MainActor () async throws -> String = { try await WebAuthentication.authorize(Store.webSignIn) },
        signIn: (@Sendable (String) async throws -> WebSession)? = nil,
        restore: (@Sendable (WebAccountCredentials) async throws -> WebSession)? = nil,
        language: String = Locale.preferredLanguages.first ?? "en-US",
        gameSessions: GameSessions<Store> = Store.sessions,
        libraryCache: LibraryCache? = nil,
        installations makeInstallations: MakeInstallations? = nil,
        onlineStatus: OnlineStatusChoice? = nil
    ) {
        self.directory = directory
        rememberedSession = RememberedSession(keychain: keychain, accountID: \.accountId,
                                              notSaved: Store.sessionNotSaved)
        self.authorize = authorize
        createSession = signIn ?? { [libraryCache] in
            try await WebAccountSession.signIn(storefront: Store.storefront, authorizationCode: $0, library: libraryCache)
        }
        restoreSession = restore ?? { [libraryCache] in
            try await WebAccountSession.restore(storefront: Store.storefront, credentials: $0, library: libraryCache)
        }
        self.language = language
        self.gameSessions = gameSessions
        let locations = ObeliskLocations.of(library: directory)
        // The account's installations in Rust; none for a session that is not Rust's.
        self.makeInstallations = makeInstallations ?? { session, account, language, sessions, receipts in
            guard let session = session as? WebAccountSession, let sessions = sessions as? StorefrontSessions else {
                return NoInstallations<Store>()
            }
            return RustInstallations<Store>(try! AccountInstallations.web(
                session: session, accountId: account, language: language, sessions: sessions,
                receipts: receipts.path, locations: locations
            ))
        }
        rosetta = RosettaModel(sessions: gameSessions)
        cloud = CloudModel(sessions: gameSessions)
        installations = GameInstallations(sessions: gameSessions, cloud: cloud)
        library = AccountLibrary(history: gameSessions.history, cache: libraryCache,
                                 installations: installations)
        presence = Store.Presence()
        self.onlineStatus = Store.Presence.showsStatus
            ? onlineStatus ?? OnlineStatusChoice(storeID: Store.storeID) : nil
        self.onlineStatus?.follow(presence.setStatus)
        self.onlineStatus?.follow(gameSessions.setStatus)
        installations.selection = { [weak self] in self?.installationSelection }
    }

    // Always read the observable account, even before the unobserved session exists.
    var isConnected: Bool { accountName != nil && session != nil }

    /// Whether the library shows: once connected, or from the cache while the
    /// saved session restores.
    var showsLibrary: Bool { library.shows(connected: isConnected, restoring: isRestoring) }

    /// No authentication or sign-out runs, and no account is connected.
    private var isIdle: Bool { !isConnected && !isRestoring && !isConnecting && !isSigningOut }

    var gameSessionMessage: String? {
        gameSessions.message(for: selectedGame?.id, of: accountID, otherGameRunning: Store.otherGameRunning)
    }

    // MARK: Account

    /// Restores the saved session once. Returns when the restoration and its
    /// first library load end.
    func start() async {
        guard !started && isIdle else { return }
        beginRestore()
        await authenticationTask?.value
    }

    func retryRestore() {
        guard isIdle else { return }
        errorMessage = nil
        beginRestore()
    }

    /// Opens the storefront's page and exchanges its code. Closing the page is not an error.
    func signIn() {
        guard isIdle else { return }
        generation += 1
        let current = generation
        isConnecting = true
        canRetryRestore = false
        errorMessage = nil
        authenticationTask = Task {
            defer { if current == generation { isConnecting = false } }
            do {
                let code = try await authorize()
                guard isCurrent(current) else { return }
                let connected = try await createSession(code)
                await accept(connected, generation: current)
            } catch is CancellationError {
                // Closing the storefront's window is a normal cancellation.
            } catch {
                guard isCurrent(current) else { return }
                errorMessage = Store.describe(error)
            }
        }
    }

    func cancelSignIn() {
        guard isConnecting || isRestoring else { return }
        generation += 1
        authenticationTask?.cancel()
        authenticationTask = nil
        isConnecting = false
        isRestoring = false
        canRetryRestore = false
        errorMessage = nil
    }

    /// Ignored while a load runs.
    func refresh() {
        guard let session, !isLoadingGames else { return }
        // Set synchronously so two clicks in the same run loop cannot overlap.
        isLoadingGames = true
        errorMessage = nil
        let current = generation
        libraryTask = Task { await reloadGames(session, generation: current) }
    }

    /// The storefront's store: signed in as the account when its storefront
    /// can sign it in, else as its site remembers.
    func storePage() async -> WebPageStart {
        await storePage(at: Store.storeURL)
    }

    /// A page of the storefront's store, signed in as `storePage` is. The
    /// session's address opens the storefront's site signed in as its
    /// account and goes on to the page; a storefront whose site takes no
    /// sign-in from the app has none.
    func storePage(at page: URL) async -> WebPageStart {
        var url = page
        if let session, let address = try? await session.webSignInUrl(redirectUrl: page.absoluteString),
           let signedIn = URL(string: address) { url = signedIn }
        return WebPageStart(url: url, dataStore: Store.webSignIn.store.dataStore)
    }

    /// Account revocation: forgets the Library cache, the launches and the
    /// saved session, revokes the presence, installations and Cloud, then
    /// revokes the session once the Keychain removal ends.
    func signOut() {
        guard !isSigningOut else { return }
        Store.webSignIn.store.forgetSite()
        library.forget()
        generation += 1
        let current = generation
        let disconnected = session
        rosetta.connect(accountId: nil)
        installations.signedOut()
        cloud.signedOut()
        let removal = rememberedSession.forget()
        presence.revoke()
        authenticationTask?.cancel()
        authenticationTask = nil
        libraryTask?.cancel()
        libraryTask = nil
        session = nil
        accountID = nil
        accountName = nil
        avatarURL = nil
        closeGame()
        errorMessage = nil
        storageWarning = nil
        isConnecting = false
        isLoadingGames = false
        isRestoring = false
        isSigningOut = true
        canRetryRestore = false
        Task {
            await removal.value
            if current == generation { isSigningOut = false }
            guard let disconnected else { return }
            // Throws when the storefront did not confirm the revocation.
            do { try await disconnected.disconnect() }
            catch {
                guard current == generation else { return }
                let warning = Store.revocationUnconfirmed
                storageWarning = storageWarning.map { $0 + " " + warning } ?? warning
            }
        }
    }

    /// Whether the running task still answers for the latest sign-in, cancellation or sign-out.
    private func isCurrent(_ current: Int) -> Bool { current == generation && !Task.isCancelled }

    private func beginRestore() {
        started = true
        isRestoring = true
        canRetryRestore = false
        let current = generation
        authenticationTask = Task { await restoreSavedSession(generation: current) }
    }

    private func restoreSavedSession(generation current: Int) async {
        defer { if current == generation { isRestoring = false } }
        let credentials: WebAccountCredentials?
        do {
            credentials = try await rememberedSession.load()
        } catch {
            guard current == generation else { return }
            storageWarning = error.localizedDescription
            canRetryRestore = true
            return
        }
        guard isCurrent(current), let credentials else { return }
        let account = credentials.accountId
        library.showCache(of: account) { cache in
            Store.cachedLibrary(in: cache, account: account).map { entry in
                CachedLibraryGame(game: entry.game, playtime: entry.playtimeSeconds.map(played(seconds:)),
                                  values: entry.achievements.map { [.achievements: $0] } ?? [:])
            }
        }
        do {
            let connected = try await restoreSession(credentials)
            await accept(connected, generation: current)
        } catch {
            guard isCurrent(current) else { return }
            library.dropCache()
            errorMessage = Store.describe(error)
            if isUnauthorized(error) {
                await rememberedSession.forget().value
            } else {
                canRetryRestore = true
            }
        }
    }

    private func accept(_ connected: WebSession, generation current: Int) async {
        guard isCurrent(current) else {
            await disconnectAbandonedSession(connected)
            return
        }
        let rotating = RotatingSession(connected)
        do {
            let credentials = try await rotating.credentials()
            guard isCurrent(current) else {
                await disconnectAbandonedSession(connected)
                return
            }
            let account = credentials.accountId
            library.connect(account)
            accountID = account
            session = connected
            // The account shows its id when its storefront names it nothing.
            accountName = credentials.name.isEmpty ? account : credentials.name
            avatarURL = connected.avatarUrl().flatMap(URL.init(string:))
            presence.connect(connected, account: account)
            isConnecting = false
            isRestoring = false
            errorMessage = nil
            isLoadingGames = true
            await rememberedSession.activate(rotating: rotating, accountID: account)
            guard current == generation else { return }
            rosetta.connect(accountId: account)
            let backend = makeInstallations(connected, account, language, gameSessions.core,
                                            receiptsFile(in: directory, storefront: Store.storeID, account: account))
            installations.connect(account: account, backend: backend, port: installationPort())

            await reloadGames(connected, generation: current)
        } catch {
            await disconnectAbandonedSession(connected)
            guard isCurrent(current) else { return }
            errorMessage = Store.describe(error)
        }
    }

    private func disconnectAbandonedSession(_ connected: WebSession) async {
        // The authentication task may already be cancelled: Account revocation gets its own task.
        await Task { try? await connected.disconnect() }.value
    }

    // MARK: Library

    private func reloadGames(_ connected: WebSession, generation current: Int) async {
        defer { if current == generation { isLoadingGames = false } }
        // The playtimes reload with the games: a refresh shows them loading at
        // once. The session tells them for the games it is asked about, so
        // they wait for the games.
        let listing = Task { Store.games(try await connected.games()) }
        let locale = Store.achievementLocale(language)
        library.loadPlaytimes({
            let games = try await listing.value
            // Nothing asks the storefront once the account signed out.
            try Task.checkCancellation()
            return try await connected.playtimes(games: games.map(\.id)).mapValues(played(seconds:))
        }, then: { [weak self] in
            // The share of each game's achievements the account unlocked: the
            // Library cache's for a game not played since they were read, else
            // the storefront's, which the session then keeps. A game without
            // achievements, or that the storefront did not answer for, has
            // none; it throws when no game could be told.
            self?.library.load(.achievements) {
                try await connected.achievementShares(games: listing.value.map(\.id), locale: locale)
            }
        })
        do {
            let loaded = try await withTaskCancellationHandler {
                try await listing.value
            } onCancel: {
                listing.cancel()
            }
            guard isCurrent(current) else { return }
            library.show(loaded)
            if let selectedGame {
                if let updated = loaded.first(where: { $0.id == selectedGame.id }) { open(updated) }
                else { closeGame() }
            }
        } catch {
            guard isCurrent(current) else { return }
            // Without the games, the playtimes failed too: the library offers the retry.
            report(error)
        }
    }

    // MARK: Game page

    func open(_ game: Game) {
        closeGame()
        selectedGame = game
        load(achievementsOf: game, into: achievements, fresh: false)
        installations.refreshCloudIndicator()
        guard let session else { return }
        isLoadingGameDetails = true
        detailsTask = Task { await loadDetails(game, session: session) }
    }

    func closeGame() {
        achievements.reset()
        detailsTask?.cancel()
        detailsTask = nil
        selectedGame = nil
        selectedGameDetails = nil
        isLoadingGameDetails = false
    }

    /// The library already tells which builds a game ships, so it downloads
    /// one without opening the game's page.
    func libraryOffer(of game: Game) -> GameOffer? { game.offer }

    /// Reads the game's achievements from the storefront again, as the
    /// page's retry asks.
    func reloadAchievements() {
        guard let game = selectedGame else { return }
        load(achievementsOf: game, into: achievements, fresh: true)
    }

    /// Reads the achievements of the library's game `id` from the storefront
    /// into `model`, as the overlay asks for the running game.
    func loadAchievements(of id: Game.ID, into model: GameAchievementsModel) {
        guard let game = library.game(id) else { return }
        load(achievementsOf: game, into: model, fresh: true)
    }

    /// The Library cache's achievements while the game was not played since,
    /// unless `fresh`.
    private func load(achievementsOf game: Game, into model: GameAchievementsModel, fresh: Bool) {
        guard let session else { return }
        let locale = Store.achievementLocale(language)
        model.load {
            try await session.achievements(game: game.id, locale: locale, fresh: fresh).map(GameAchievement.init)
        }
    }

    /// Every change of the game on screen, and the sign-out, cancels it in `closeGame`.
    private func loadDetails(_ game: Game, session: WebSession) async {
        defer { if !Task.isCancelled { isLoadingGameDetails = false } }
        guard !Task.isCancelled else { return }
        do {
            let details = try await session.gameDetails(offer: game.product, language: language)
            if !Task.isCancelled { selectedGameDetails = details }
        } catch {
            // Secondary metadata must not prevent browsing the library.
        }
    }

    /// Shows a failed call. One the storefront refused signs the account out.
    private func report(_ error: Error) {
        if isUnauthorized(error) { signOut() }
        errorMessage = Store.describe(error)
    }

    /// The game on screen, for its installation actions.
    private var installationSelection: InstallationSelection<Store>? {
        guard let game = selectedGame else { return nil }
        let platform = selectedGameDetails?.platform ?? game.preferredPlatform
        return InstallationSelection(game: game.id, platform: platform, offer: game.offer)
    }

    /// The account's part of the installation actions.
    private func installationPort() -> InstallationPort<Store> {
        InstallationPort(
            report: { [weak self] error in
                guard let self else { return }
                guard let error else { return errorMessage = nil }
                report(error)
            },
            warn: { [weak self] in self?.storageWarning = $0 }
        )
    }
}

extension WebAccountModel {
    /// The account's friends, whichever its presence is: none for a storefront
    /// without any.
    var friends: FriendsPresentationState? { presence.state }
    var isFriendsPresented: Bool { presence.isPresented }

    func openFriends() { presence.open() }
    func closeFriends() { presence.close() }
    func retryFriends() { presence.retry() }
}
