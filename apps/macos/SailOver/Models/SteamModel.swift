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

@MainActor
@Observable
final class SteamModel {
    static let verifyBeforeLaunchPreferenceKey = "sailover.steam.verifyBeforeLaunch"
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
    var filter: StorefrontLibraryFilter = .all
    var availableFilters: [StorefrontLibraryFilter] { StorefrontLibraryFilter.allCases }
    var guardKind: SteamGuardKind = .deviceConfirmation

    private(set) var selectedGame: SteamGame? {
        // Every path that opens, closes or replaces the selection invalidates
        // the panel here, so no caller has to remember to.
        didSet { if selectedGame?.appId != oldValue?.appId { achievements.reset() } }
    }
    private(set) var details: SteamGameDetails?
    let achievements = GameAchievementsModel(storeID: "steam", describe: SteamModel.message)
    private(set) var news: [SteamNews] = []
    private(set) var isLoadingDetails = false
    private(set) var isLoadingNews = false
    private(set) var detailsError: String?
    private(set) var newsError: String?
    var platform: String {
        if let details, details.appId == selectedGame?.appId { return details.preferredPlatform }
        // Installation receipts are available before remote details. Never use
        // the previous game's platform to present the newly selected game.
        return installations.receipts.contains { $0.gameID == selectedGame?.appId && $0.platform == "macos" }
            ? "macos" : "windows"
    }
    private(set) var language: String
    var verifyBeforeLaunch: Bool {
        didSet { preferences.set(verifyBeforeLaunch, forKey: Self.verifyBeforeLaunchPreferenceKey) }
    }
    let installations: SteamInstallations

    @ObservationIgnored private let directory: URL
    @ObservationIgnored private let keychain: any SteamCredentialStore
    @ObservationIgnored private let connectSession: @Sendable (SteamCredentials) async throws -> any SteamSessionProtocol
    @ObservationIgnored private let loadNews: @Sendable (UInt32) async throws -> [SteamNews]
    @ObservationIgnored private let beginLogin: @Sendable (SteamLoginStart) async throws -> any SteamLoginProtocol
    let gameSessions: SteamGameSessions
    /// The Rust installations of the account a session signed in, whose
    /// receipts a file keeps.
    typealias MakeInstallations = @MainActor (any SteamSessionProtocol, SteamCredentials,
                                              any StorefrontSessionsProtocol, URL)
        -> any InstallationsBackend<SteamGames>
    @ObservationIgnored private let makeInstallations: MakeInstallations
    let cloud: SteamCloudModel
    let rosetta: SteamRosettaModel
    let libraryPlatforms = SteamLibraryPlatforms()
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
    @ObservationIgnored private var libraryOrderCache: [SteamLibrarySort: [SteamGame]] = [:]
    @ObservationIgnored private var filteredLibraryCache: (
        query: String, sort: SteamLibrarySort, filter: StorefrontLibraryFilter,
        installed: Set<UInt32>, platforms: [UInt32: [String]], games: [SteamGame]
    )?

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
        gameSessions: SteamGameSessions = .shared,
        installations makeInstallations: MakeInstallations? = nil
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
        let locations = SailOverLocations.of(library: directory)
        self.makeInstallations = makeInstallations ?? { session, credentials, sessions, receipts in
            guard let session = session as? SteamSession, let sessions = sessions as? StorefrontSessions else {
                return NoInstallations<SteamGames>()
            }
            return RustInstallations<SteamGames>(try! AccountInstallations.steam(
                session: session, credentials: credentials, sessions: sessions, receipts: receipts.path,
                locations: locations
            ))
        }
        self.cloud = SteamCloudModel(sessions: gameSessions)
        self.rosetta = SteamRosettaModel(sessions: gameSessions)
        self.installations = SteamInstallations(sessions: gameSessions, cloud: cloud)
        wakeObserver = SystemWakeObservation { [weak self] in
            Task { @MainActor [weak self] in self?.retryFriends() }
        }
        installations.selection = { [weak self] in self?.installationSelection }
    }

    deinit {
        loginTask?.cancel()
        friendsTask?.cancel()
        connectionTask?.cancel()
        session?.shutdown()
    }

    var isConnected: Bool { sessionCredentials != nil }

    func syncCloud() { installations.syncCloud() }

    func refreshCloudIndicator() {
        guard let game = selectedGame else { return }
        cloud.refreshIndicators(for: [game.appId])
    }

    /// Names the game a Cloud question or banner is about.
    func cloudTitle(for appId: UInt32?) -> String {
        let title = L10n.string("ui.steam.cloud.title")
        guard let appId else { return title }
        let name = games.first { $0.appId == appId }?.name
            ?? installations.receipts.first { $0.gameID == appId }?.name
            ?? String(appId)
        return "\(title) · \(name)"
    }

    /// The game on screen, for its installation actions. Only remote details
    /// tell which builds the account can install.
    private var installationSelection: InstallationSelection<SteamGames>? {
        guard let game = selectedGame else { return nil }
        let details = details?.appId == game.appId && !isLoadingDetails ? details : nil
        let offer = details.flatMap { details in
            details.macos || details.windows
                ? GameOffer.steam(appId: game.appId, name: game.name, platform: details.preferredPlatform, language: language)
                : nil
        }
        return InstallationSelection(game: game.appId, platform: platform, offer: offer)
    }

    /// The active session when it belongs to the game on screen.
    private var selectedGameSession: SteamGameSessions.Session? {
        guard let active = gameSessions.active, active.game == selectedGame?.appId else { return nil }
        return active
    }

    var gameSessionMessage: String? {
        if gameSessions.isRecovering { return L10n.string("model.steam.recoveringSession") }
        if let error = gameSessions.recoveryError { return error }
        guard gameSessions.active != nil else { return nil }
        guard let active = selectedGameSession else { return L10n.string("model.steam.otherGameRunning") }
        if active.reconnecting { return L10n.string("model.steam.recoveringSession") }
        switch active.host {
        case .disconnected?: return L10n.string("model.steam.gameDisconnected")
        case .lost?: return L10n.string("model.steam.gameHostLost")
        default: return nil
        }
    }

    var filteredGames: [SteamGame] {
        // Read every observable input even on cache hits so the library keeps
        // updating after search, filters, installation changes or platform replies.
        let source = games
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        let order = sort
        let selectedFilter = filter
        let installedAppIds = selectedFilter == .installed ? Set(installations.receipts.map(\.gameID)) : []
        // Dictionary snapshots share storage until the next metadata batch changes it.
        let platforms = selectedFilter == .macOS ? libraryPlatforms.platforms : [:]
        if let cached = filteredLibraryCache, cached.query == query, cached.sort == order,
           cached.filter == selectedFilter, cached.installed == installedAppIds, cached.platforms == platforms {
            return cached.games
        }
        let ordered: [SteamGame]
        if let cached = libraryOrderCache[order] {
            ordered = cached
        } else {
            ordered = Self.filterAndSort(source, query: "", sort: order)
            libraryOrderCache[order] = ordered
        }
        let filtered = query.isEmpty && selectedFilter == .all ? ordered : ordered.filter { game in
            guard query.isEmpty || game.name.localizedStandardContains(query) else { return false }
            switch selectedFilter {
            case .all: return true
            case .installed: return installedAppIds.contains(game.appId)
            case .played: return game.lastPlayed > 0 || game.playtimeMinutes > 0
            case .macOS: return platforms[game.appId]?.contains("macos") == true
            }
        }
        filteredLibraryCache = (query, order, selectedFilter, installedAppIds, platforms, filtered)
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
            libraryPlatforms.reset()
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
            connectInstallations(connected, credentials: credentials)
            rosetta.connect(accountId: credentials.steamId)
            do {
                if persist { try await keychain.save(credentials) }
                else if !rememberSession { try await keychain.remove() }
            } catch {
                guard current == generation else { return }
                storageWarning = L10n.string("model.steam.sessionStillUsable", error.localizedDescription)
            }
            guard current == generation else { return }
            await reloadGames(connected, generation: current)
            #if DEBUG
            await runCloudBackupIfRequested(connected, generation: current)
            #endif
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

    #if DEBUG
    /// A Cloud diagnostic needs the storefront idle, and keeps it fenced.
    private func reserveCloudProbe() -> Bool {
        guard installations.gate.launch, gameSessions.recoveryError == nil else { return false }
        installations.diagnosticReservation = true
        return true
    }

    /// Opt-in developer probe. It consumes the normal in-memory session and
    /// writes only sanitized results. No credential leaves this process.
    private func runCloudBackupIfRequested(_ connected: any SteamSessionProtocol, generation current: Int) async {
        // Ordered by precedence: the first flag present wins, and a plain
        // backup is the default. Flag and report name stay in one row so a new
        // probe cannot pick up another probe's filename.
        let probes: [(flag: String, report: String)] = [
            ("--steam-cloud-autocloud-metadata", "cloud-autocloud-metadata.json"),
            ("--steam-cloud-sdk-candidates", "cloud-sdk-candidates.json"),
            ("--steam-cloud-preferences", "cloud-preferences-result.json"),
            ("--steam-cloud-interruption-probe", "cloud-interruption-result.json"),
            ("--steam-cloud-transfer-probe", "cloud-transfer-result.json"),
            ("--steam-cloud-backup", "cloud-backup-result.json"),
        ]
        let arguments = ProcessInfo.processInfo.arguments
        let probe = probes.first { arguments.contains($0.flag) } ?? probes[probes.count - 1]
        guard let index = arguments.firstIndex(of: probe.flag),
              arguments.count > index + 2, let appID = UInt32(arguments[index + 1]),
              appID != 0, current == generation, session === connected,
              let native = connected as? SteamSession else { return }
        let directory = URL(fileURLWithPath: arguments[index + 2], isDirectory: true)
        let report = directory.appending(path: probe.report)
        do {
            let json: String
            switch probe.flag {
            case "--steam-cloud-autocloud-metadata":
                json = try await native.cloudAutocloudMetadata()
            case "--steam-cloud-sdk-candidates":
                json = try await native.cloudSdkCandidates()
            case "--steam-cloud-preferences":
                json = try await native.cloudPreferences(appId: appID)
            case "--steam-cloud-interruption-probe":
                guard reserveCloudProbe() else { throw CocoaError(.featureUnsupported) }
                json = try await native.cloudInterruptionProbe(appId: appID, directory: directory.path)
                // Keep game admission fenced while the interrupted diagnostic
                // awaits a process restart, including after a diagnostic error.
                let result = try JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any]
                if result?["status"] as? String == "verified" { installations.diagnosticReservation = false }
            case "--steam-cloud-transfer-probe":
                guard reserveCloudProbe() else { throw CocoaError(.featureUnsupported) }
                defer { installations.diagnosticReservation = false }
                json = try await native.cloudTransferProbe(appId: appID, directory: directory.path)
            default:
                json = try await native.cloudBackup(appId: appID, directory: directory.path)
            }
            guard current == generation, session === connected else { return }
            try Data(json.utf8).write(to: report, options: .atomic)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: report.path)
        } catch {
            // Steam errors contain only typed diagnostics, not request URLs.
            guard current == generation, session === connected else { return }
            let data = try? JSONSerialization.data(withJSONObject: ["status": "failed", "reason": error.localizedDescription])
            try? data?.write(to: report, options: .atomic)
        }
    }
    #endif

    private func reloadGames(_ session: any SteamSessionProtocol, generation current: Int) async {
        guard current == generation && self.session === session && !Task.isCancelled else { return }
        libraryPlatforms.reset()
        isLoadingGames = true
        do {
            let loaded = try await session.games()
            guard current == generation && self.session === session else { return }
            games = loaded
            if let selected = selectedGame, let updated = loaded.first(where: { $0.appId == selected.appId }) {
                selectedGame = updated
            }
            isLoadingGames = false
            libraryPlatforms.load(appIds: loaded.map(\.appId), session: session)
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
        libraryPlatforms.reset()
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
                        self.libraryPlatforms.reset()
                        self.session = nil
                        return
                    case .disconnected:
                        observed.shutdown()
                        self.libraryPlatforms.reset()
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
                        self.libraryPlatforms.reset()
                        self.session = replacement
                        // The previous session is shut down: Transfers,
                        // launches and Cloud maintenance go through this one.
                        self.connectInstallations(replacement, credentials: credentials)
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
        libraryPlatforms.reset()
        rosetta.connect(accountId: nil)
        installations.signedOut()
        cloud.signedOut()
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
        session = nil
        login = nil
        displayName = nil
        avatarURL = nil
        games = []
        selectedGame = nil
        details = nil
        news = []
        challenge = nil
        searchText = ""
        filter = .all
        errorMessage = nil
        storageWarning = nil
        isConnecting = false
        isLoadingGames = false
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
        reloadAchievements()
        refreshCloudIndicator()
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

    func reloadAchievements() {
        guard let game = selectedGame, let session else { return }
        let language = language
        achievements.load { try await session.achievements(appId: game.appId, language: language).map(GameAchievement.init) }
    }

    /// Remote details tell which builds the account can download, so an
    /// installed game never waits for them.
    func reloadDetails() {
        guard let game = selectedGame, let session else { return }
        detailsRequestGeneration += 1
        let detailsRequest = detailsRequestGeneration
        details = nil
        detailsError = nil
        isLoadingDetails = false
        guard installations.selectedReceipt == nil else { return }
        let selection = detailGeneration
        let current = generation
        let requestedLanguage = language
        isLoadingDetails = true
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

    /// The account's installations, behind `session`.
    private func connectInstallations(_ session: any SteamSessionProtocol, credentials: SteamCredentials) {
        let receipts = directory.appending(path: "steam-installations-\(credentials.steamId).json")
        installations.connect(account: credentials.steamId,
                              backend: makeInstallations(session, credentials, gameSessions.core, receipts),
                              port: installationPort())
    }

    /// Steam's part of the installation actions.
    private func installationPort() -> InstallationPort<SteamGames> {
        InstallationPort(
            verifiesBeforeLaunch: { [weak self] in self?.verifyBeforeLaunch ?? true },
            report: { [weak self] error in self?.errorMessage = error.map(Self.message) },
            warn: { [weak self] in self?.storageWarning = $0 },
            uninstalled: { [weak self] game in
                if self?.selectedGame?.appId == game { self?.reloadDetails() }
            }
        )
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
    /// SailOver installs the native build when Steam ships one, otherwise Windows.
    var preferredPlatform: String { macos ? "macos" : "windows" }
}
