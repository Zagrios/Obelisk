import AppKit
import Foundation
import Observation

enum SteamLibrarySort: String, CaseIterable, Identifiable {
    case lastPlayed, name, playtime

    var id: String { rawValue }

    var title: String {
        switch self {
        case .lastPlayed: L10n.string("model.steam.sort.lastPlayed")
        case .name: L10n.string("model.steam.sort.name")
        case .playtime: L10n.string("model.steam.sort.playtime")
        }
    }

    var menuTitle: String {
        switch self {
        case .lastPlayed: L10n.string("model.steam.sortMenu.lastPlayed")
        case .name: L10n.string("model.steam.sortMenu.name")
        case .playtime: L10n.string("model.steam.sortMenu.playtime")
        }
    }
}

/// How a Steam login is started. Both entry points go through one port, so
/// the model never holds a concrete `SteamLogin` and tests can drive sign-in
/// without opening a Steam HTTP session.
enum SteamLoginStart: Sendable, Equatable {
    case qr
    case credentials(account: String, password: String)
}

struct SteamInstallReceipt: InstallReceipt, Identifiable {
    var id: String { "\(appId)-\(platform)" }
    let appId: UInt32
    let name: String
    let directory: String
    let platform: String
    let language: String
    let totalBytes: UInt64
    var needsVerification: Bool? = nil
}

@MainActor
@Observable
final class SteamModel {
    static let verifyBeforeLaunchPreferenceKey = "forge.steam.verifyBeforeLaunch"
    private(set) var displayName: String?
    private(set) var avatarURL: URL?
    private(set) var friends: FriendsPresentationState?
    var isFriendsPresented = false
    private(set) var games: [SteamGame] = [] {
        didSet {
            libraryOrderCache.removeAll()
            filteredLibraryCache = nil
        }
    }
    private(set) var isRestoring = false
    private(set) var isConnecting = false
    private(set) var isLoadingGames = false
    private(set) var isSubmittingCredentials = false
    private(set) var isSubmittingGuard = false
    var qrURL: String? { challenge?.qrUrl }
    private(set) var challenge: SteamChallenge?
    private(set) var isCredentialChallenge = false
    private(set) var qrError: String?
    var errorMessage: String?
    var storageWarning: String?
    var rememberSession = true
    var searchText = ""
    var sort: SteamLibrarySort = .lastPlayed
    var guardKind: SteamGuardKind = .deviceConfirmation

    private(set) var selectedGame: SteamGame?
    private(set) var details: SteamGameDetails?
    private(set) var news: [SteamNews] = []
    private(set) var isLoadingDetails = false
    private(set) var isLoadingNews = false
    private(set) var detailsError: String?
    private(set) var newsError: String?
    var platform: String {
        if let details, details.appId == selectedGame?.appId { return details.preferredPlatform }
        // Installation receipts are available before remote details. Never use
        // the previous game's platform to present the newly selected game.
        return installations.contains { $0.appId == selectedGame?.appId && $0.platform == "macos" }
            ? "macos" : "windows"
    }
    private(set) var language: String
    private(set) var transfer: SteamTransfer?
    private(set) var isDownloading = false
    private(set) var isLaunching = false
    private(set) var isUninstalling = false
    private(set) var isVerifyingBeforeLaunch = false
    var verifyBeforeLaunch: Bool {
        didSet { preferences.set(verifyBeforeLaunch, forKey: Self.verifyBeforeLaunchPreferenceKey) }
    }
    private(set) var installations: [SteamInstallReceipt] = []

    @ObservationIgnored private let directory: URL
    @ObservationIgnored private let keychain: any SteamCredentialStore
    @ObservationIgnored private let connectSession: @Sendable (SteamCredentials) async throws -> any SteamSessionProtocol
    @ObservationIgnored private let loadNews: @Sendable (UInt32) async throws -> [SteamNews]
    @ObservationIgnored private let beginLogin: @Sendable (SteamLoginStart) async throws -> any SteamLoginProtocol
    let gameSessions: SteamGameSessions
    @ObservationIgnored private let preferences: UserDefaults
    @ObservationIgnored private var session: (any SteamSessionProtocol)?
    @ObservationIgnored private var login: (any SteamLoginProtocol)?
    private var steamId: UInt64? { sessionCredentials?.steamId }
    @ObservationIgnored private var started = false
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var detailGeneration = 0
    @ObservationIgnored private var detailsRequestGeneration = 0
    @ObservationIgnored private var loginTask: Task<Void, Never>?
    @ObservationIgnored private var profileTask: Task<Void, Never>?
    @ObservationIgnored private var friendsTask: Task<Void, Never>?
    @ObservationIgnored private var friendsObservationGeneration = 0
    /// The connected account; observed because `isConnected` derives from it.
    private var sessionCredentials: SteamCredentials?
    @ObservationIgnored private let reconnectDelay: @Sendable (Int) async throws -> Void
    @ObservationIgnored private var wakeObserver: SystemWakeObservation?
    @ObservationIgnored private var connectionTask: Task<any SteamSessionProtocol, Error>?
    @ObservationIgnored private var pollingActive = false
    @ObservationIgnored private var downloadTask: Task<Void, Never>?
    @ObservationIgnored private var launchTask: Task<Void, Never>?
    @ObservationIgnored private var uninstallTask: Task<Void, Never>?
    @ObservationIgnored private var uninstallAppId: UInt32?
    @ObservationIgnored private var launchAppId: UInt32?
    @ObservationIgnored private var downloadCancellationRequested = false
    @ObservationIgnored private var progressTask: Task<Void, Never>?
    @ObservationIgnored private var lastDownload: (game: SteamGame, platform: String, language: String, directory: URL)?
    @ObservationIgnored private var libraryOrderCache: [SteamLibrarySort: [SteamGame]] = [:]
    @ObservationIgnored private var filteredLibraryCache: (query: String, sort: SteamLibrarySort, games: [SteamGame])?

    init(
        directory: URL,
        keychain: any SteamCredentialStore = SteamKeychain(),
        connect: @escaping @Sendable (SteamCredentials) async throws -> any SteamSessionProtocol = { try await SteamSessionConnection.connect($0) },
        news: @escaping @Sendable (UInt32) async throws -> [SteamNews] = { try await steamNews(appId: $0) },
        beginLogin: @escaping @Sendable (SteamLoginStart) async throws -> any SteamLoginProtocol = { start in
            switch start {
            case .qr:
                try await SteamLogin.beginQr()
            case let .credentials(account, password):
                try await SteamLogin.beginCredentials(accountName: account, password: password)
            }
        },
        localeIdentifier: String = Locale.preferredLanguages.first ?? Locale.current.identifier,
        preferences: UserDefaults = .standard,
        reconnectDelay: @escaping @Sendable (Int) async throws -> Void = { attempt in
            try await Task.sleep(for: .seconds(1 << attempt))
        },
        gameSessions: SteamGameSessions = .shared
    ) {
        self.directory = directory
        self.keychain = keychain
        self.connectSession = connect
        self.loadNews = news
        self.beginLogin = beginLogin
        self.language = steamLanguage(locale: localeIdentifier)
        self.preferences = preferences
        self.reconnectDelay = reconnectDelay
        self.verifyBeforeLaunch = preferences.object(forKey: Self.verifyBeforeLaunchPreferenceKey) == nil
            ? true : preferences.bool(forKey: Self.verifyBeforeLaunchPreferenceKey)
        self.gameSessions = gameSessions
        wakeObserver = SystemWakeObservation { [weak self] in
            Task { @MainActor [weak self] in self?.retryFriends() }
        }
    }

    deinit {
        loginTask?.cancel()
        friendsTask?.cancel()
        connectionTask?.cancel()
        session?.shutdown()
    }

    var isConnected: Bool { sessionCredentials != nil }

    /// Identifies the operation before its first progress snapshot is available.
    var activeTransferAppId: UInt32? {
        if isDownloading { return lastDownload?.game.appId }
        if isLaunching && isVerifyingBeforeLaunch { return launchAppId }
        return nil
    }

    /// Every installation-mutating entry point asks this, so a new one cannot
    /// silently gain permission to touch a running game's files.
    private func canModifyInstallation(of appId: UInt32?) -> Bool {
        !isDownloading && !isLaunching && !isUninstalling && !gameSessions.protects(appId)
    }

    var canResumeDownload: Bool {
        canModifyInstallation(of: lastDownload?.game.appId) && lastDownload != nil
            && lastDownload?.game.appId == selectedGame?.appId
    }

    var selectedInstallation: SteamInstallReceipt? {
        installations.first { $0.appId == selectedGame?.appId && $0.platform == platform }
    }

    var canUninstallSelectedGame: Bool {
        isConnected && selectedInstallation != nil && canModifyInstallation(of: selectedGame?.appId)
    }

    /// The active session when it belongs to the game on screen.
    private var selectedGameSession: SteamGameSessions.Session? {
        guard let active = gameSessions.active, active.appId == selectedGame?.appId else { return nil }
        return active
    }

    var gameSessionMessage: String? {
        if gameSessions.isRecovering { return L10n.string("model.steam.recoveringSession") }
        if let error = gameSessions.recoveryError { return error }
        guard gameSessions.active != nil else { return nil }
        guard let active = selectedGameSession else { return L10n.string("model.steam.otherGameRunning") }
        if active.connection == .reconnecting { return L10n.string("model.steam.recoveringSession") }
        switch active.host {
        case .disconnected: return L10n.string("model.steam.gameDisconnected")
        case .lost: return L10n.string("model.steam.gameHostLost")
        default: return nil
        }
    }

    var primaryActionTitle: String {
        if gameSessions.isRecovering { return L10n.string("model.steam.recoveringSession") }
        if selectedGameSession?.running == true { return L10n.string("model.steam.running") }
        if isUninstalling && uninstallAppId == selectedGame?.appId { return L10n.string("model.game.uninstalling") }
        if isLaunching && launchAppId == selectedGame?.appId {
            return isVerifyingBeforeLaunch ? L10n.string("model.steam.verifying") : L10n.string("model.steam.launching")
        }
        if selectedGameSession != nil { return L10n.string("model.steam.launching") }
        if selectedInstallation != nil { return L10n.string("model.steam.launch") }
        return canResumeDownload ? L10n.string("model.steam.resume") : L10n.string("model.steam.download")
    }

    var primaryActionSymbol: String {
        if selectedGameSession != nil { return "play.fill" }
        return selectedInstallation == nil ? "arrow.down" : "play.fill"
    }

    var primaryActionAvailable: Bool {
        guard let selectedGame, let details, details.appId == selectedGame.appId else { return false }
        return !isLoadingDetails && canModifyInstallation(of: selectedGame.appId)
            && !(selectedInstallation != nil && gameSessions.isBusy)
            && (details.macos || details.windows)
    }

    func primaryAction() {
        guard primaryActionAvailable, let game = selectedGame else { return }
        if let receipt = selectedInstallation { launchGame(game, receipt: receipt) }
        else { downloadSelected() }
    }

    var filteredGames: [SteamGame] {
        // Read every observable input even on cache hits so the library keeps
        // updating after a search, a sort change, or a refreshed session.
        let source = games
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        let order = sort
        if let cached = filteredLibraryCache, cached.query == query, cached.sort == order {
            return cached.games
        }
        let ordered: [SteamGame]
        if let cached = libraryOrderCache[order] {
            ordered = cached
        } else {
            ordered = Self.filterAndSort(source, query: "", sort: order)
            libraryOrderCache[order] = ordered
        }
        let filtered = query.isEmpty ? ordered : ordered.filter { $0.name.localizedStandardContains(query) }
        filteredLibraryCache = (query, order, filtered)
        return filtered
    }

    static func filterAndSort(_ games: [SteamGame], query: String, sort: SteamLibrarySort) -> [SteamGame] {
        let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
        return games.filter { query.isEmpty || $0.name.localizedStandardContains(query) }.sorted {
            switch sort {
            case .lastPlayed where $0.lastPlayed != $1.lastPlayed: return $0.lastPlayed > $1.lastPlayed
            case .playtime where $0.playtimeMinutes != $1.playtimeMinutes: return $0.playtimeMinutes > $1.playtimeMinutes
            default:
                let comparison = $0.name.localizedStandardCompare($1.name)
                return comparison == .orderedSame ? $0.appId < $1.appId : comparison == .orderedAscending
            }
        }
    }

    func start() async {
        guard !started else { return }
        started = true
        isRestoring = true
        let current = generation
        do {
            if let credentials = try await keychain.load() {
                guard current == generation else { return }
                await connect(credentials, generation: current, persist: false)
            }
        } catch {
            guard current == generation else { return }
            storageWarning = error.localizedDescription
        }
        guard current == generation else { return }
        isRestoring = false
        if !isConnected { beginQR() }
    }

    func beginQR() {
        guard !isConnected && !isRestoring && !isConnecting else { return }
        generation += 1
        let current = generation
        loginTask?.cancel()
        login = nil
        qrError = nil
        challenge = nil
        isCredentialChallenge = false
        isSubmittingCredentials = false
        isSubmittingGuard = false
        pollingActive = false
        loginTask = Task {
            do {
                let request = try await beginLogin(.qr)
                guard current == generation && !Task.isCancelled else { return }
                login = request
                await poll(request, generation: current)
            } catch {
                guard current == generation && !Task.isCancelled else { return }
                qrError = Self.message(error)
            }
        }
    }

    func signIn(account: String, password: String) {
        guard !isConnecting && !isSubmittingCredentials && !isRestoring else { return }
        generation += 1
        let current = generation
        loginTask?.cancel()
        login = nil
        challenge = nil
        errorMessage = nil
        isCredentialChallenge = true
        isSubmittingCredentials = true
        pollingActive = false
        loginTask = Task {
            do {
                let request = try await beginLogin(.credentials(account: account, password: password))
                guard current == generation && !Task.isCancelled else { return }
                login = request
                isSubmittingCredentials = false
                await poll(request, generation: current)
            } catch {
                guard current == generation && !Task.isCancelled else { return }
                isSubmittingCredentials = false
                errorMessage = Self.message(error)
                isCredentialChallenge = false
                beginQR()
            }
        }
    }

    func submitGuard(_ code: String) {
        guard let login, !isSubmittingGuard, !isConnecting else { return }
        let current = generation
        let kind = guardKind
        isSubmittingGuard = true
        errorMessage = nil
        Task {
            do {
                try await login.submitGuard(code: code, kind: kind)
                guard current == generation && !isConnected else { return }
                isSubmittingGuard = false
                // Keep an in-flight poll: it may already be receiving the tokens.
                if !pollingActive { loginTask = Task { await poll(login, generation: current) } }
            } catch {
                guard current == generation && !isConnected else { return }
                isSubmittingGuard = false
                errorMessage = Self.message(error)
            }
        }
    }

    private func poll(_ request: any SteamLoginProtocol, generation current: Int) async {
        pollingActive = true
        defer { if current == generation { pollingActive = false } }
        do {
            var initial = true
            while current == generation && !Task.isCancelled {
                let snapshot = await request.challenge()
                guard current == generation && !Task.isCancelled else { return }
                challenge = snapshot
                if initial {
                    let methods = snapshot.confirmations.map(\.kind)
                    guardKind = methods.contains(.deviceConfirmation) ? .deviceConfirmation
                        : methods.contains(.deviceCode) ? .deviceCode
                        : methods.contains(.emailCode) ? .emailCode
                        : methods.contains(.emailConfirmation) ? .emailConfirmation : .none
                    initial = false
                }
                try await Task.sleep(for: .milliseconds(max(snapshot.pollIntervalMs, 1000)))
                guard current == generation && !Task.isCancelled else { return }
                if let credentials = try await request.poll() {
                    guard current == generation && !Task.isCancelled else { return }
                    await connect(credentials, generation: current, persist: rememberSession)
                    if current == generation && !isConnected && !Task.isCancelled { beginQR() }
                    return
                }
            }
        } catch is CancellationError {
            return
        } catch {
            guard current == generation && !Task.isCancelled else { return }
            if isCredentialChallenge { errorMessage = Self.message(error) }
            else { qrError = Self.message(error) }
        }
    }

    // Shared handoff for restored and freshly authenticated credentials. Keeping
    // it internal lets integration tests replace accounts without real login.
    func connect(_ credentials: SteamCredentials, generation current: Int, persist: Bool) async {
        guard current == generation && !Task.isCancelled else { return }
        isConnecting = true
        do {
            let connector = connectSession
            let attempt = Task { try await connector(credentials) }
            connectionTask?.cancel()
            connectionTask = attempt
            let connected = try await withTaskCancellationHandler {
                try await attempt.value
            } onCancel: {
                attempt.cancel()
            }
            guard current == generation && !Task.isCancelled else {
                connected.shutdown()
                return
            }
            stopFriends()
            session?.shutdown()
            session = connected
            sessionCredentials = credentials
            displayName = nil
            avatarURL = nil
            friends = Self.emptyFriends(accountID: credentials.steamId)
            observeFriends(connected, generation: current)
            login = nil
            challenge = nil
            isCredentialChallenge = false
            isSubmittingCredentials = false
            isSubmittingGuard = false
            errorMessage = nil
            isConnecting = false
            isRestoring = false
            loadInstallations()
            do {
                if persist { try await keychain.save(credentials) }
                else if !rememberSession { try await keychain.remove() }
            } catch {
                guard current == generation else { return }
                storageWarning = L10n.string("model.steam.sessionStillUsable", error.localizedDescription)
            }
            guard current == generation else { return }
            await reloadGames(connected, generation: current)
        } catch {
            guard current == generation && !Task.isCancelled else { return }
            isConnecting = false
            errorMessage = Self.message(error)
        }
    }

    func refresh() {
        guard let session, !isLoadingGames else { return }
        let current = generation
        Task { await reloadGames(session, generation: current) }
    }

    private func reloadGames(_ session: any SteamSessionProtocol, generation current: Int) async {
        guard current == generation && self.session === session && !Task.isCancelled else { return }
        isLoadingGames = true
        do {
            let loaded = try await session.games()
            guard current == generation && self.session === session else { return }
            games = loaded
            isLoadingGames = false
        } catch {
            guard current == generation && self.session === session else { return }
            isLoadingGames = false
            errorMessage = Self.message(error)
        }
        guard current == generation && self.session === session && !Task.isCancelled else { return }
        reloadProfile(session, generation: current)
    }

    private func reloadProfile(_ session: any SteamSessionProtocol, generation current: Int) {
        profileTask?.cancel()
        profileTask = Task {
            do {
                let profile = try await session.profile()
                guard current == generation && self.session === session && !Task.isCancelled else { return }
                displayName = profile.displayName
                avatarURL = profile.avatarUrl.flatMap(URL.init(string:))
            } catch {
                // Optional profile information must not interrupt the library or login.
            }
        }
    }

    func openFriends() {
        guard friends != nil else { return }
        isFriendsPresented = true
    }

    func closeFriends() { isFriendsPresented = false }

    /// Only explicit retry or system wake can restart an exhausted reconnect cycle.
    func retryFriends() {
        guard isConnected, sessionCredentials != nil,
              friends?.connection != .authenticationRequired else { return }
        stopFriends()
        friends?.connection = .interrupted
        session?.shutdown()
        let previous = session
        session = nil
        observeFriends(previous, generation: generation)
    }

    private func stopFriends() {
        friendsObservationGeneration += 1
        friendsTask?.cancel()
        friendsTask = nil
    }

    private func observeFriends(_ initial: (any SteamSessionProtocol)?, generation current: Int) {
        friendsObservationGeneration += 1
        let observation = friendsObservationGeneration
        let connect = connectSession
        let delay = reconnectDelay
        guard let credentials = sessionCredentials else { return }
        friendsTask = Task { [weak self] in
            var active = initial
            var revision: UInt64 = 0
            while !Task.isCancelled {
                if let observed = active {
                    let snapshot = await withTaskCancellationHandler {
                        await observed.nextFriends(afterRevision: revision)
                    } onCancel: {
                        // This wakes a pending Rust watch even while content owns the CM mutex.
                        observed.shutdown()
                    }
                    guard let self, current == self.generation,
                          observation == self.friendsObservationGeneration, !Task.isCancelled else { return }
                    revision = snapshot.revision
                    guard snapshot.accountId == credentials.steamId else { continue }
                    self.applyFriends(snapshot)
                    switch snapshot.connection {
                    case .loading, .ready, .error:
                        continue
                    case .authenticationRequired:
                        observed.shutdown()
                        self.session = nil
                        return
                    case .disconnected:
                        observed.shutdown()
                        self.session = nil
                        active = nil
                    }
                }

                // One bounded cycle per interruption, with no status polling.
                for attempt in 0..<3 {
                    do {
                        try await delay(attempt)
                        try Task.checkCancellation()
                        let replacement = try await connect(credentials)
                        guard let self, current == self.generation,
                              observation == self.friendsObservationGeneration, !Task.isCancelled else {
                            replacement.shutdown()
                            return
                        }
                        self.session = replacement
                        active = replacement
                        revision = 0
                        Task { [weak self] in
                            guard let self else { return }
                            await self.reloadGames(replacement, generation: current)
                            guard self.session === replacement else { return }
                            if self.selectedGame != nil { self.reloadDetails() }
                        }
                        break
                    } catch {
                        guard let self, current == self.generation,
                              observation == self.friendsObservationGeneration, !Task.isCancelled else { return }
                        if case SteamError.AuthenticationRequired = error {
                            self.friends?.connection = .authenticationRequired
                            return
                        }
                        if attempt == 2 {
                            self.friends?.connection = .failed(L10n.string("model.friends.reconnectFailed"))
                            return
                        }
                    }
                }
            }
        }
    }

    private static func emptyFriends(accountID: UInt64) -> FriendsPresentationState {
        FriendsPresentationState(
            store: FriendsStoreIdentity(id: "steam", name: L10n.string("ui.store.steam"), logoAssetName: "SteamLogo"),
            account: FriendsAccountIdentity(storeID: "steam", accountID: String(accountID)),
            friends: [], connection: .loading
        )
    }

    private func applyFriends(_ snapshot: SteamFriendsSnapshot) {
        guard snapshot.accountId == steamId else { return }
        if snapshot.connection == .loading, let previous = friends,
           previous.connection != .loading {
            // A new transport must finish full hydration before replacing stale rows.
            friends?.connection = .interrupted
            return
        }
        var presentation = Self.emptyFriends(accountID: snapshot.accountId)
        presentation.friends = snapshot.friends.map { friend in
            FriendPresentation(
                id: FriendIdentity(account: presentation.account, friendID: String(friend.steamId)),
                name: friend.displayName,
                avatarURL: friend.avatarUrl.flatMap(URL.init(string:)),
                presence: FriendPresence(friend.presence),
                activity: friend.game.map { FriendGameActivity(id: $0.appId.map(String.init) ?? "unknown", name: $0.name) }
            )
        }
        switch snapshot.connection {
        case .loading: presentation.connection = .loading
        case .ready: presentation.connection = .connected
        case .disconnected: presentation.connection = .interrupted
        case .authenticationRequired: presentation.connection = .authenticationRequired
        case .error: presentation.connection = .failed(L10n.string("model.friends.unavailable"))
        }
        if snapshot.connection != .ready, snapshot.connection != .loading,
           let previous = friends, !previous.friends.isEmpty {
            presentation.friends = previous.friends
        }
        presentation.lastKnownOnlineCount = snapshot.connection == .ready
            ? presentation.onlineCount : friends?.lastKnownOnlineCount
        friends = presentation
    }

    func signOut() {
        generation += 1
        detailGeneration += 1
        stopFriends()
        session?.shutdown()
        sessionCredentials = nil
        friends = nil
        isFriendsPresented = false
        loginTask?.cancel()
        connectionTask?.cancel()
        connectionTask = nil
        profileTask?.cancel()
        profileTask = nil
        session?.cancelDownload()
        downloadTask?.cancel()
        launchTask?.cancel()
        uninstallTask?.cancel()
        progressTask?.cancel()
        session = nil
        login = nil
        displayName = nil
        avatarURL = nil
        games = []
        installations = []
        selectedGame = nil
        details = nil
        news = []
        transfer = nil
        lastDownload = nil
        challenge = nil
        searchText = ""
        errorMessage = nil
        storageWarning = nil
        isConnecting = false
        isLoadingGames = false
        isDownloading = false
        isLaunching = false
        isVerifyingBeforeLaunch = false
        launchAppId = nil
        downloadCancellationRequested = true
        isRestoring = true
        let current = generation
        Task {
            do { try await keychain.remove() }
            catch {
                guard current == generation else { return }
                storageWarning = L10n.string("model.steam.sessionMayRestore", error.localizedDescription)
            }
            guard current == generation && !Task.isCancelled else { return }
            isRestoring = false
            beginQR()
        }
    }

    func open(_ game: SteamGame) {
        guard session != nil else { return }
        selectedGame = game
        details = nil
        news = []
        detailsError = nil
        newsError = nil
        isLoadingNews = true
        detailGeneration += 1
        let request = detailGeneration
        let current = generation
        reloadDetails()
        Task {
            do {
                let loaded = try await loadNews(game.appId)
                guard current == generation && request == detailGeneration else { return }
                news = loaded
                isLoadingNews = false
            } catch {
                guard current == generation && request == detailGeneration else { return }
                newsError = Self.message(error)
                isLoadingNews = false
            }
        }
    }

    func reloadDetails() {
        guard let game = selectedGame, let session else { return }
        detailsRequestGeneration += 1
        let detailsRequest = detailsRequestGeneration
        let selection = detailGeneration
        let current = generation
        let requestedLanguage = language
        isLoadingDetails = true
        details = nil
        detailsError = nil
        Task {
            do {
                let loaded = try await session.details(appId: game.appId, language: requestedLanguage)
                guard current == generation && self.session === session && selection == detailGeneration && detailsRequest == detailsRequestGeneration else { return }
                details = loaded
                isLoadingDetails = false
            } catch {
                guard current == generation && self.session === session && selection == detailGeneration && detailsRequest == detailsRequestGeneration else { return }
                detailsError = Self.message(error)
                isLoadingDetails = false
            }
        }
    }

    func backToLibrary() {
        detailGeneration += 1
        selectedGame = nil
        details = nil
        news = []
    }

    func downloadSelected() {
        guard let game = selectedGame, let details, details.appId == game.appId,
              session != nil, canModifyInstallation(of: game.appId), details.macos || details.windows else { return }
        if canResumeDownload {
            resumeDownload()
            return
        }
        // `platform` already follows these details (see `open`); `selectedInstallation` matches it.
        let selectedPlatform = details.preferredPlatform
        let destination = selectedInstallation.map { URL(fileURLWithPath: $0.directory).deletingLastPathComponent() }
            ?? directory.appending(path: "Games/Steam", directoryHint: .isDirectory)
        do {
            try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
            download(game, platform: selectedPlatform, language: language, directory: destination)
        } catch {
            errorMessage = L10n.string("model.steam.downloadDirectoryFailed", error.localizedDescription)
        }
    }

    func download(_ game: SteamGame, platform: String, language: String, directory: URL) {
        guard let session, canModifyInstallation(of: game.appId) else { return }
        let current = generation
        downloadCancellationRequested = false
        beginTransfer(game, platform: platform, language: language, directory: directory, session: session, generation: current)
        downloadTask = Task {
            do {
                _ = try await performDownload(game, platform: platform, language: language, directory: directory, session: session, generation: current)
            } catch {
                guard current == generation else { return }
                if !(error is CancellationError) && !downloadCancellationRequested && transfer?.state != .cancelled {
                    errorMessage = Self.message(error)
                }
            }
        }
    }

    private func beginTransfer(_ game: SteamGame, platform: String, language: String, directory: URL, session: any SteamSessionProtocol, generation current: Int) {
        lastDownload = (game, platform, language, directory)
        isDownloading = true
        transfer = nil
        errorMessage = nil
        if let index = installations.firstIndex(where: { $0.appId == game.appId && $0.platform == platform }) {
            installations[index].needsVerification = true
        }
        saveInstallations()
        progressTask?.cancel()
        progressTask = Task {
            while current == generation && !Task.isCancelled {
                let snapshot = session.downloadProgress()
                if snapshot != transfer { transfer = snapshot }
                try? await Task.sleep(for: .milliseconds(250))
            }
        }
    }

    private func performDownload(_ game: SteamGame, platform: String, language: String, directory: URL, session: any SteamSessionProtocol, generation current: Int) async throws -> SteamInstallReceipt {
        defer {
            if current == generation {
                progressTask?.cancel()
                transfer = session.downloadProgress()
                isDownloading = false
            }
        }
        guard current == generation && !Task.isCancelled && !downloadCancellationRequested else { throw CancellationError() }
        let installed = try await session.download(appId: game.appId, platform: platform, language: language, directory: directory.path)
        guard current == generation && !Task.isCancelled && !downloadCancellationRequested
                && session.downloadProgress()?.state != .cancelled else { throw CancellationError() }
        let receipt = SteamInstallReceipt(appId: installed.appId, name: installed.name, directory: installed.directory, platform: installed.platform, language: installed.language, totalBytes: installed.totalBytes)
        installations.removeAll { $0.appId == installed.appId && $0.platform == installed.platform }
        installations.append(receipt)
        lastDownload = nil
        saveInstallations()
        return receipt
    }

    private func launchGame(_ game: SteamGame, receipt: SteamInstallReceipt) {
        guard let session, canModifyInstallation(of: game.appId), !gameSessions.isBusy else { return }
        let current = generation
        let mustVerify = verifyBeforeLaunch || receipt.needsVerification == true
        isLaunching = true
        isVerifyingBeforeLaunch = mustVerify
        launchAppId = game.appId
        downloadCancellationRequested = false
        errorMessage = nil
        var gameWasRunning = false
        launchTask = gameSessions.launch(appId: game.appId, prepare: { [self] in
            guard current == generation && !Task.isCancelled && !downloadCancellationRequested else { throw CancellationError() }
            var ready = receipt
            if mustVerify {
                let parent = URL(fileURLWithPath: receipt.directory).deletingLastPathComponent()
                beginTransfer(game, platform: receipt.platform, language: receipt.language, directory: parent, session: session, generation: current)
                ready = try await performDownload(game, platform: receipt.platform, language: receipt.language, directory: parent, session: session, generation: current)
            }
            guard current == generation && !Task.isCancelled && !downloadCancellationRequested else { throw CancellationError() }
            isVerifyingBeforeLaunch = false
            let plan = try await session.launchPlan(appId: game.appId, platform: ready.platform, directory: ready.directory)
            guard current == generation && !Task.isCancelled && !downloadCancellationRequested else { throw CancellationError() }
            guard let credentials = sessionCredentials else { throw SteamError.AuthenticationRequired }
            return SteamGameSessions.Launch(plan: plan, credentials: credentials)
        }, onRunning: { [weak self] in
            gameWasRunning = true
            guard let self, self.generation == current else { return }
            self.isLaunching = false
        }, onCompletion: { [weak self] error in
            guard let self, self.generation == current else { return }
            self.isLaunching = false
            self.isVerifyingBeforeLaunch = false
            self.launchAppId = nil
            self.launchTask = nil
            guard let error, !(error is CancellationError),
                  gameWasRunning || (!Task.isCancelled && !self.downloadCancellationRequested) else { return }
            self.errorMessage = Self.message(error)
        })
    }

    func cancelDownload() {
        downloadCancellationRequested = true
        session?.cancelDownload()
    }

    func resumeDownload() {
        guard canResumeDownload, let previous = lastDownload else { return }
        download(previous.game, platform: previous.platform, language: previous.language, directory: previous.directory)
    }

    func uninstall(_ receipt: SteamInstallReceipt) {
        guard isConnected, canModifyInstallation(of: receipt.appId),
              installations.contains(receipt), let file = receiptsURL else { return }
        let current = generation
        let remaining = installations.filter { $0 != receipt }
        isUninstalling = true
        uninstallAppId = receipt.appId
        errorMessage = nil
        uninstallTask = Task {
            defer {
                isUninstalling = false
                uninstallAppId = nil
                uninstallTask = nil
            }
            do {
                guard current == generation && !Task.isCancelled else { return }
                try await GameInstallationManager.uninstall(
                    directory: receipt.directory, expectedDirectoryName: "steam-\(receipt.appId)-\(receipt.platform)",
                    protecting: directory, receiptFile: file, remaining: remaining
                )
                guard current == generation else {
                    // A reconnected account may have loaded its receipts before
                    // the removal completed. Refresh only its own receipt file.
                    if isConnected { loadInstallations() }
                    return
                }
                installations = remaining
                if lastDownload?.game.appId == receipt.appId && lastDownload?.platform == receipt.platform {
                    lastDownload = nil
                }
                if transfer?.appId == receipt.appId { transfer = nil }
            } catch {
                guard current == generation && !(error is CancellationError) else { return }
                installations = InstallationReceipts.retainingExistingDirectories(installations)
                errorMessage = L10n.string("model.game.uninstallFailed", error.localizedDescription)
            }
        }
    }

    private var receiptsURL: URL? {
        steamId.map { directory.appending(path: "steam-installations-\($0).json") }
    }

    private func loadInstallations() {
        installations = []
        guard let file = receiptsURL else { return }
        do { installations = try InstallationReceipts.load(SteamInstallReceipt.self, from: file) }
        catch { storageWarning = L10n.string("model.steam.receiptsUnavailable") }
    }

    private func saveInstallations() {
        guard let file = receiptsURL else { return }
        do { try InstallationReceipts.save(installations, to: file) }
        catch { storageWarning = L10n.string("model.steam.receiptsNotSaved") }
    }

    static func message(_ error: Error) -> String {
        if case SteamError.AuthenticationRequired = error {
            return L10n.string("backend.steam.friends.authenticationRequired")
        }
        if case SteamError.Cancelled = error { return L10n.string("backend.steam.content.cancelled") }
        if case let SteamError.Failure(reason) = error { return L10n.message(reason) }
        return error.localizedDescription
    }
}

extension SteamGameDetails {
    /// Forge installs the native build when Steam ships one, otherwise Windows.
    var preferredPlatform: String { macos ? "macos" : "windows" }
}
