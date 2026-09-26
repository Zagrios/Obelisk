import Foundation
import Observation

/// Epic library entries are unique per namespace and app name across platforms.
func epicLibraryID(namespace: String, appName: String) -> String { "\(namespace):\(appName)" }

struct EpicInstallReceipt: InstallReceipt {
    var id: String { "\(namespace):\(appName):\(platform)" }
    var libraryID: String { epicLibraryID(namespace: namespace, appName: appName) }
    var gameID: String { libraryID }
    let namespace: String
    let appName: String
    let name: String
    let directory: String
    let platform: String
    let totalBytes: UInt64
    var needsVerification: Bool? = false
}

@MainActor
@Observable
final class EpicModel {
    private(set) var friends: FriendsPresentationState?
    private(set) var isFriendsPresented = false
    private(set) var accountName: String?
    private(set) var games: [EpicGame] = [] {
        didSet {
            libraryOrderCache = nil
            filteredLibraryCache = nil
        }
    }

    let gameSessions: EpicGameSessions
    /// The Rust installations of an account a session signed in: its id, the
    /// language games launch in, what saves the credentials account calls
    /// rotate, and the file its receipts keep.
    typealias MakeInstallations = @MainActor (any EpicSessionProtocol, String, String, any EpicCredentialSave,
                                              StorefrontSessions, URL) -> any InstallationsBackend<EpicGames>
    @ObservationIgnored private let makeInstallations: MakeInstallations
    @ObservationIgnored private let maintainCloud: @MainActor (any EpicSessionProtocol, EpicCloudConfiguration) async -> any CloudMaintenanceProtocol
    let cloud: EpicCloudModel
    let rosetta: EpicRosettaModel
    let installations: EpicInstallations

    let achievements = GameAchievementsModel(storeID: "epic", describe: EpicModel.message)
    private(set) var selectedGame: EpicGame?
    private(set) var selectedGameDetails: EpicGameDetails?
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
    var searchText = ""
    var filter: StorefrontLibraryFilter = .all
    // Epic exposes playtime only in individual game details, not in its library.
    var availableFilters: [StorefrontLibraryFilter] { [.all, .installed, .macOS] }
    var rememberSession: Bool {
        get { rememberedSession.rememberSession }
        set { rememberedSession.rememberSession = newValue }
    }

    @ObservationIgnored private let rememberedSession: EpicRememberedSession
    @ObservationIgnored private let directory: URL
    @ObservationIgnored private let launchLanguage: String
    @ObservationIgnored private let authorize: @MainActor () async throws -> String
    @ObservationIgnored private let createSession: @Sendable (String) async throws -> any EpicSessionProtocol
    @ObservationIgnored private let restoreSession: @Sendable (EpicCredentials) async throws -> any EpicSessionProtocol
    @ObservationIgnored private var session: (any EpicSessionProtocol)?
    @ObservationIgnored private var friendsTask: Task<Void, Never>?
    @ObservationIgnored private var friendsObservationGeneration = 0
    @ObservationIgnored private var wakeObserver: SystemWakeObservation?

    @ObservationIgnored private var accountID: String?
    @ObservationIgnored private var started = false
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var authenticationTask: Task<Void, Never>?
    @ObservationIgnored private var libraryTask: Task<Void, Never>?
    @ObservationIgnored private var detailsTask: Task<Void, Never>?
    @ObservationIgnored private var detailsGeneration = 0
    @ObservationIgnored private var libraryOrderCache: [EpicGame]?
    @ObservationIgnored private var filteredLibraryCache: (
        query: String, filter: StorefrontLibraryFilter, installed: Set<String>, games: [EpicGame]
    )?

    init(
        directory: URL = AppIdentity.libraryDirectory,
        keychain: any EpicCredentialStore = EpicKeychain(),
        authorize: @escaping @MainActor () async throws -> String = { try await EpicWebAuthentication.authorize() },
        signIn: @escaping @Sendable (String) async throws -> any EpicSessionProtocol = { try await EpicSession.signIn(authorizationCode: $0) },
        restore: @escaping @Sendable (EpicCredentials) async throws -> any EpicSessionProtocol = { try await EpicSession.restore(credentials: $0) },
        language: String = Locale.preferredLanguages.first ?? "en",
        gameSessions: EpicGameSessions = .shared,
        cloudServices: EpicCloudServices = .live,
        installations makeInstallations: MakeInstallations? = nil,
        maintainCloud: @escaping @MainActor (any EpicSessionProtocol, EpicCloudConfiguration) async -> any CloudMaintenanceProtocol = {
            await $0.maintainCloud(configuration: $1)
        }
    ) {
        self.directory = directory
        self.rememberedSession = EpicRememberedSession(store: keychain)
        self.authorize = authorize
        self.createSession = signIn
        self.restoreSession = restore
        self.launchLanguage = language
        self.gameSessions = gameSessions
        let locations = SailOverLocations.of(library: directory)
        self.makeInstallations = makeInstallations ?? { session, account, language, credentials, sessions, receipts in
            EpicInstallationsBackend(try! Self.installations(of: session, account: account, language: language,
                                                             credentials: credentials, sessions: sessions,
                                                             receipts: receipts, locations: locations))
        }
        self.maintainCloud = maintainCloud
        self.rosetta = EpicRosettaModel(sessions: gameSessions)
        self.cloud = EpicCloudModel(locations: .of(library: directory), sessions: gameSessions,
                                    indicators: cloudServices.indicators)
        self.installations = EpicInstallations(directory: directory, sessions: gameSessions, cloud: cloud)
        wakeObserver = SystemWakeObservation { [weak self] in
            Task { @MainActor [weak self] in self?.retryFriends() }
        }
        installations.selection = { [weak self] in self?.installationSelection }
    }

    deinit {
        friendsTask?.cancel()
    }

    // Always read the observable account, even before the unobserved session exists.
    var isConnected: Bool { accountName != nil && session != nil }

    /// The game on screen, for its installation actions.
    private var installationSelection: InstallationSelection<EpicGames>? {
        guard let game = selectedGame else { return nil }
        return InstallationSelection(
            game: game.libraryID, platform: game.preferredPlatform, installable: game.macos || game.windows,
            request: { receipt, directory in
                EpicDownload(game: game, platform: receipt?.platform ?? game.preferredPlatform, directory: directory)
            }
        )
    }

    func syncCloud() {
        guard installations.canSyncCloud, let game = selectedGame else { return }
        let configuration = installations.selectedReceipt.map {
            cloud.configuration(game: game, receipt: $0, language: launchLanguage)
        } ?? cloud.savedConfiguration(for: game.libraryID)
        guard let configuration else { return }
        cloud.startMaintenance(configuration)
    }

    func refreshCloudIndicator() {
        guard let game = selectedGame else { return }
        cloud.refreshIndicators(for: [game])
    }

    /// Names the game a Cloud question or banner is about.
    func cloudTitle(for libraryID: String?) -> String {
        let title = L10n.string("ui.epic.cloud.title")
        guard let libraryID else { return title }
        let name = games.first { $0.libraryID == libraryID }?.title
            ?? installations.receipts.first { $0.libraryID == libraryID }?.name
            ?? libraryID
        return "\(title) · \(name)"
    }

    private var selectedGameSession: EpicGameSessions.Session? {
        guard let active = gameSessions.active, active.account == accountID,
              active.game == selectedGame?.libraryID else { return nil }
        return active
    }

    var gameSessionMessage: String? {
        if gameSessions.isRecovering || gameSessions.active?.reconnecting == true {
            return L10n.string("model.epic.recoveringSession")
        }
        if let error = gameSessions.recoveryError { return error }
        if gameSessions.active != nil && selectedGameSession == nil {
            return L10n.string("model.epic.otherGameRunning")
        }
        return nil
    }

    /// Search and filters reuse the library's name order.
    var filteredGames: [EpicGame] {
        // Observe dependencies even when the cached result is reused.
        let source = games
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        let selectedFilter = filter
        let installedLibraryIDs = selectedFilter == .installed ? Set(installations.receipts.map(\.libraryID)) : []
        if let cached = filteredLibraryCache, cached.query == query, cached.filter == selectedFilter,
           cached.installed == installedLibraryIDs {
            return cached.games
        }
        let ordered: [EpicGame]
        if let cached = libraryOrderCache {
            ordered = cached
        } else {
            ordered = source.sorted {
                let comparison = $0.title.localizedStandardCompare($1.title)
                return comparison == .orderedSame ? $0.libraryID < $1.libraryID : comparison == .orderedAscending
            }
            libraryOrderCache = ordered
        }
        let filtered = query.isEmpty && selectedFilter == .all ? ordered : ordered.filter { game in
            guard query.isEmpty || game.title.localizedStandardContains(query) else { return false }
            switch selectedFilter {
            case .all: return true
            case .installed: return installedLibraryIDs.contains(game.libraryID)
            case .played: return false
            case .macOS: return game.macos
            }
        }
        filteredLibraryCache = (query, selectedFilter, installedLibraryIDs, filtered)
        return filtered
    }

    func start() async {
        guard !started && !isSigningOut && !isConnecting && !isRestoring && !isConnected else { return }
        beginRestore()
        await authenticationTask?.value
    }

    private func beginRestore() {
        started = true
        isRestoring = true
        canRetryRestore = false
        let current = generation
        authenticationTask = Task { await restoreSavedSession(generation: current) }
    }

    func retryRestore() {
        guard !isConnected && !isRestoring && !isConnecting && !isSigningOut else { return }
        errorMessage = nil
        beginRestore()
    }

    private func restoreSavedSession(generation current: Int) async {
        defer { if current == generation { isRestoring = false } }
        let credentials: EpicCredentials?
        do {
            credentials = try await rememberedSession.load()
        } catch {
            guard current == generation else { return }
            storageWarning = error.localizedDescription
            canRetryRestore = true
            return
        }
        guard current == generation && !Task.isCancelled, let credentials else { return }
        do {
            let connected = try await restoreSession(credentials)
            await accept(connected, generation: current)
        } catch {
            guard current == generation && !Task.isCancelled else { return }
            errorMessage = Self.message(error)
            if case EpicError.Unauthorized = error {
                await rememberedSession.forget().value
            } else {
                canRetryRestore = true
            }
        }
    }

    func signIn() {
        beginSignIn(authorization: authorize)
    }

    func signIn(input: String) {
        beginSignIn { try epicAuthorizationCode(input: input) }
    }

    private func beginSignIn(authorization: @escaping @MainActor () async throws -> String) {
        guard !isConnected && !isRestoring && !isConnecting && !isSigningOut else { return }
        generation += 1
        let current = generation
        isConnecting = true
        canRetryRestore = false
        errorMessage = nil
        authenticationTask = Task {
            defer { if current == generation { isConnecting = false } }
            do {
                let code = try await authorization()
                guard current == generation && !Task.isCancelled else { return }
                let connected = try await createSession(code)
                await accept(connected, generation: current)
            } catch is CancellationError {
                // Closing the Epic window is a normal cancellation.
            } catch {
                guard current == generation && !Task.isCancelled else { return }
                errorMessage = Self.message(error)
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

    private func accept(_ connected: any EpicSessionProtocol, generation current: Int) async {
        guard current == generation && !Task.isCancelled else {
            await disconnectAbandonedSession(connected)
            return
        }
        do {
            let credentials = try await connected.credentials()
            guard current == generation && !Task.isCancelled else {
                await disconnectAbandonedSession(connected)
                return
            }
            session = connected
            accountID = credentials.accountId
            accountName = credentials.displayName.isEmpty ? credentials.accountId : credentials.displayName
            friends = .epic(accountID: credentials.accountId)
            observeFriends(connected, account: credentials.accountId, generation: current)
            isConnecting = false
            isRestoring = false
            errorMessage = nil
            isLoadingGames = true
            await rememberedSession.activate(connected, accountID: credentials.accountId)
            guard current == generation else { return }
            rosetta.connect(accountId: credentials.accountId)
            let maintainCloud = maintainCloud
            cloud.connect(account: credentials.accountId) { configuration in
                await maintainCloud(connected, configuration)
            }
            let backend = makeInstallations(connected, credentials.accountId, launchLanguage,
                                            RememberedCredentials(rememberedSession, session: connected),
                                            gameSessions.core, receiptsFile(for: credentials.accountId))
            installations.connect(account: credentials.accountId, backend: backend, port: installationPort())

            await reloadGames(connected, generation: current)
        } catch {
            await disconnectAbandonedSession(connected)
            guard current == generation && !Task.isCancelled else { return }
            errorMessage = Self.message(error)
        }
    }

    private func disconnectAbandonedSession(_ connected: any EpicSessionProtocol) async {
        // The auth task may already be cancelled. Give revocation its own task.
        await Task { try? await connected.disconnect() }.value
    }

    func refresh() {
        guard let session, !isLoadingGames else { return }
        // Set synchronously so two clicks in the same run loop cannot overlap.
        isLoadingGames = true
        errorMessage = nil
        let current = generation
        libraryTask = Task { await reloadGames(session, generation: current) }
    }

    private func reloadGames(_ connected: any EpicSessionProtocol, generation current: Int) async {
        defer { if current == generation { isLoadingGames = false } }
        do {
            let loaded = try await rememberedSession.perform(on: connected) {
                try await connected.games()
            }
            guard current == generation && !Task.isCancelled else { return }
            games = loaded
            if let selectedGame {
                if let updated = loaded.first(where: { $0.libraryID == selectedGame.libraryID }) { open(updated) }
                else { closeGame() }
            }
        } catch {
            guard current == generation && !Task.isCancelled else { return }
            if case EpicError.Unauthorized = error { signOut() }
            errorMessage = Self.message(error)
        }
    }

    func signOut() {
        guard !isSigningOut else { return }
        generation += 1
        let current = generation
        let disconnected = session
        rosetta.connect(accountId: nil)
        installations.signedOut()
        cloud.signedOut()
        let removal = rememberedSession.forget()
        friendsObservationGeneration += 1
        friendsTask?.cancel()
        friendsTask = nil
        friends = nil
        isFriendsPresented = false
        authenticationTask?.cancel()
        libraryTask?.cancel()
        authenticationTask = nil
        libraryTask = nil
        session = nil
        accountID = nil
        accountName = nil
        games = []
        closeGame()
        searchText = ""
        filter = .all
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
            do { try await disconnected?.disconnect() }
            catch {
                guard current == generation else { return }
                let warning = L10n.string("model.epic.revocationUnconfirmed")
                storageWarning = storageWarning.map { $0 + " " + warning } ?? warning
            }
        }
    }

    func openFriends() {
        guard friends != nil, isConnected else { return }
        isFriendsPresented = true
    }

    func closeFriends() { isFriendsPresented = false }

    /// Replaces only the social socket. The OAuth session and downloads stay alive.
    func retryFriends() {
        guard let session, let accountID, isConnected,
              friends?.connection != .authenticationRequired else { return }
        observeFriends(session, account: accountID, generation: generation, reconnect: true)
    }

    private func observeFriends(_ connected: any EpicSessionProtocol, account: String, generation current: Int, reconnect: Bool = false) {
        friendsObservationGeneration += 1
        friendsTask?.cancel()
        let observation = friendsObservationGeneration
        friendsTask = Task { [weak self] in
            guard !Task.isCancelled else { return }
            await withTaskCancellationHandler {
                if reconnect { await connected.reconnectFriends() }
                else { await connected.startFriends() }
                var revision: UInt64 = 0
                while !Task.isCancelled {
                    let snapshot = await connected.nextFriends(afterRevision: revision)
                    guard let self, current == self.generation, account == self.accountID,
                          observation == self.friendsObservationGeneration, !Task.isCancelled else { return }
                    guard snapshot.accountId == account else { return }
                    revision = snapshot.revision
                    if snapshot.connection == .stopped { return }
                    self.friends?.apply(snapshot)
                }
            } onCancel: { connected.stopFriends() }
        }
    }

    func open(_ game: EpicGame) {
        closeGame()
        selectedGame = game
        reloadAchievements()
        refreshCloudIndicator()
        guard let session else { return }
        isLoadingGameDetails = true
        let current = generation
        let selection = detailsGeneration
        detailsTask = Task { await loadDetails(game, session: session, generation: current, selection: selection) }
    }

    func reloadAchievements() {
        guard let game = selectedGame, let session else { return }
        let locale = Locale(identifier: launchLanguage).language.languageCode?.identifier ?? "en"
        let remembered = rememberedSession
        achievements.load {
            try await remembered.perform(on: session) {
                try await session.achievements(namespace: game.namespace, locale: locale).map(GameAchievement.init)
            }
        }
    }

    func closeGame() {
        achievements.reset()
        detailsGeneration += 1
        detailsTask?.cancel()
        detailsTask = nil
        selectedGame = nil
        selectedGameDetails = nil
        isLoadingGameDetails = false
    }

    private func loadDetails(
        _ game: EpicGame,
        session: any EpicSessionProtocol,
        generation current: Int,
        selection: Int
    ) async {
        func isCurrent() -> Bool {
            current == generation && selection == detailsGeneration
                && selectedGame?.libraryID == game.libraryID && !Task.isCancelled
        }
        defer { if isCurrent() { isLoadingGameDetails = false } }
        guard isCurrent() else { return }
        do {
            let details = try await rememberedSession.perform(on: session) {
                guard isCurrent() else { throw CancellationError() }
                return try await session.gameDetails(
                    namespace: game.namespace,
                    catalogItemId: game.catalogItemId,
                    appName: game.appName,
                    platform: game.preferredPlatform
                )
            }
            if isCurrent() { selectedGameDetails = details }
        } catch {
            // Secondary metadata must not prevent browsing the library.
        }
    }

    private func receiptsFile(for account: String) -> URL {
        // Account identifiers are opaque. Encode them before using them in a file name.
        let component = account.utf8.map { String(format: "%02x", $0) }.joined()
        return directory.appending(path: "epic-installations-\(component).json")
    }

    /// Rust's installations of the account `session` signed in.
    private static func installations(of session: any EpicSessionProtocol, account: String, language: String,
                                      credentials: any EpicCredentialSave, sessions: StorefrontSessions,
                                      receipts: URL, locations: SailOverLocations) throws -> EpicAccountInstallations {
        if let session = session as? EpicSession {
            return try EpicAccountInstallations(session: session, accountId: account, language: language,
                                                credentials: credentials, sessions: sessions,
                                                receipts: receipts.path, locations: locations)
        }
        #if DEBUG
        return try EpicAccountInstallations.withContent(
            content: SessionEpicContent(session, start: { _ in
                throw EpicError.Service(message: "backend.epic.cloud.failed")
            }),
            accountId: account, language: language, credentials: credentials, sessions: sessions,
            receipts: receipts.path, locations: locations
        )
        #else
        preconditionFailure("Only a Rust session signs in")
        #endif
    }

    /// Epic's part of the installation actions.
    private func installationPort() -> InstallationPort<EpicGames> {
        InstallationPort(
            verifiesBeforeLaunch: { true },
            uninstallRequest: { [weak self] receipt in
                guard let game = self?.games.first(where: { $0.libraryID == receipt.libraryID }) else { return nil }
                return EpicDownload(game: game, platform: receipt.platform,
                                    directory: URL(fileURLWithPath: receipt.directory).deletingLastPathComponent())
            },
            report: { [weak self] error in
                guard let self else { return }
                guard let error else { return errorMessage = nil }
                if case EpicError.Unauthorized = error { signOut() }
                errorMessage = Self.message(error)
            },
            warn: { [weak self] in self?.storageWarning = $0 }
        )
    }

    static func message(_ error: Error) -> String {
        switch error {
        case EpicError.Network: L10n.string("model.epic.network")
        case EpicError.Unauthorized: L10n.string("model.epic.unauthorized")
        case EpicError.InvalidCode: L10n.string("model.epic.invalidCode")
        case EpicError.InvalidResponse: L10n.string("model.epic.invalidResponse")
        case let EpicError.Service(message): L10n.message(message)
        case let browserError as EpicWebAuthenticationError: browserError.localizedDescription
        case let failure as GameInstallationFailure: failure.localizedDescription
        default: L10n.string("model.epic.failure")
        }
    }
}

extension EpicGame {
    var preferredPlatform: String { macos ? "Mac" : "Windows" }
    var libraryID: String { epicLibraryID(namespace: namespace, appName: appName) }
}

