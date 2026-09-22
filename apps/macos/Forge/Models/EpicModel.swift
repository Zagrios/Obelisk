import Foundation
import Observation

/// Epic library entries are unique per namespace and app name across platforms.
func epicLibraryID(namespace: String, appName: String) -> String { "\(namespace):\(appName)" }

struct EpicInstallReceipt: InstallReceipt, Identifiable {
    var id: String { "\(namespace):\(appName):\(platform)" }
    var libraryID: String { epicLibraryID(namespace: namespace, appName: appName) }
    let namespace: String
    let appName: String
    let name: String
    let directory: String
    let platform: String
    let totalBytes: UInt64
    var needsVerification = false
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
    let cloud: EpicCloudModel
    let rosetta: EpicRosettaModel

    let achievements = GameAchievementsModel(storeID: "epic", describe: EpicModel.message)
    private(set) var selectedGame: EpicGame?
    private(set) var selectedGameDetails: EpicGameDetails?
    private(set) var isLoadingGameDetails = false
    private(set) var isRestoring = false
    private(set) var isConnecting = false
    private(set) var isLoadingGames = false
    private(set) var isSigningOut = false
    private(set) var canRetryRestore = false
    private(set) var transfer: EpicTransfer?
    private(set) var transferLibraryID: String?
    private(set) var isDownloading = false
    private(set) var isLaunching = false
    private(set) var isUninstalling = false
    private(set) var isVerifyingBeforeLaunch = false
    private(set) var launchLibraryID: String?
    private(set) var installations: [EpicInstallReceipt] = [] {
        didSet { filteredLibraryCache = nil }
    }
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
    @ObservationIgnored private var networkObserver: NetworkReturnObservation?

    @ObservationIgnored private var accountID: String?
    @ObservationIgnored private var started = false
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var authenticationTask: Task<Void, Never>?
    @ObservationIgnored private var libraryTask: Task<Void, Never>?
    @ObservationIgnored private var detailsTask: Task<Void, Never>?
    @ObservationIgnored private var detailsGeneration = 0
    @ObservationIgnored private var downloadTask: Task<Void, Never>?
    @ObservationIgnored private var uninstallTask: Task<Void, Never>?
    @ObservationIgnored private var uninstallLibraryID: String?
    @ObservationIgnored private var progressTask: Task<Void, Never>?
    @ObservationIgnored private var downloadCancellationRequested = false
    @ObservationIgnored private var lastDownload: (game: EpicGame, platform: String, directory: URL)?
    @ObservationIgnored private var libraryOrderCache: [EpicGame]?
    @ObservationIgnored private var filteredLibraryCache: (query: String, filter: StorefrontLibraryFilter, games: [EpicGame])?

    init(
        directory: URL = AppIdentity.libraryDirectory,
        keychain: any EpicCredentialStore = EpicKeychain(),
        authorize: @escaping @MainActor () async throws -> String = { try await EpicWebAuthentication.authorize() },
        signIn: @escaping @Sendable (String) async throws -> any EpicSessionProtocol = { try await EpicSession.signIn(authorizationCode: $0) },
        restore: @escaping @Sendable (EpicCredentials) async throws -> any EpicSessionProtocol = { try await EpicSession.restore(credentials: $0) },
        language: String = Locale.preferredLanguages.first ?? "en",
        gameSessions: EpicGameSessions = .shared
    ) {
        self.directory = directory
        self.rememberedSession = EpicRememberedSession(store: keychain)
        self.authorize = authorize
        self.createSession = signIn
        self.restoreSession = restore
        self.launchLanguage = language
        self.gameSessions = gameSessions
        self.rosetta = EpicRosettaModel(sessions: gameSessions)
        self.cloud = EpicCloudModel(root: SteamGameLauncher.cloudDirectory(in: directory),
                                    sessionDirectory: EpicGameSessions.sessionDirectory(in: directory),
                                    sessions: gameSessions)
        cloud.canResume = { [weak self] in self?.canModifyInstallations ?? false }
        wakeObserver = SystemWakeObservation { [weak self] in
            Task { @MainActor [weak self] in
                self?.retryFriends()
                self?.cloud.resume()
            }
        }
        networkObserver = NetworkReturnObservation { [weak self] in self?.cloud.resume() }
    }

    deinit {
        friendsTask?.cancel()
    }

    // Always read the observable account, even before the unobserved session exists.
    var isConnected: Bool { accountName != nil && session != nil }

    var activeTransferLibraryID: String? { isDownloading ? transferLibraryID : nil }

    var canResumeDownload: Bool {
        canModifyInstallations && lastDownload != nil && lastDownload?.game.libraryID == selectedGame?.libraryID
    }

    var selectedInstallation: EpicInstallReceipt? {
        guard let selectedGame else { return nil }
        return installations.first { $0.libraryID == selectedGame.libraryID && $0.platform == selectedGame.preferredPlatform }
    }

    var canUninstallSelectedGame: Bool {
        isConnected && selectedInstallation != nil && canModifyInstallations
    }

    var primaryActionAvailable: Bool {
        guard isConnected, let selectedGame else { return false }
        return canModifyInstallations && (selectedGame.macos || selectedGame.windows)
    }

    var canModifyInstallations: Bool {
        !isDownloading && !isLaunching && !isUninstalling && !gameSessions.isBusy
    }

    var isSynchronizingSaves: Bool {
        selectedGame.map { cloud.isSynchronizing(for: $0.libraryID) } ?? false
    }

    private var selectedGameSession: EpicGameSessions.Session? {
        guard let active = gameSessions.active, active.accountId == accountID,
              active.libraryID == selectedGame?.libraryID else { return nil }
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

    var primaryActionTitle: String {
        if isSynchronizingSaves { return L10n.string("ui.cloud.sync_short") }
        if gameSessions.isRecovering { return L10n.string("model.epic.recoveringSession") }
        if selectedGameSession?.phase == .running { return L10n.string("model.steam.running") }
        if isUninstalling && uninstallLibraryID == selectedGame?.libraryID { return L10n.string("model.game.uninstalling") }
        if isLaunching && launchLibraryID == selectedGame?.libraryID {
            return L10n.string(isVerifyingBeforeLaunch ? "model.steam.verifying" : "model.steam.launching")
        }
        if selectedGameSession != nil { return L10n.string("model.steam.launching") }
        if selectedInstallation != nil { return L10n.string("model.steam.launch") }
        if canResumeDownload { return L10n.string("model.steam.resume") }
        return L10n.string("ui.game.download")
    }

    var primaryActionSymbol: String { selectedInstallation == nil ? "arrow.down" : "play.fill" }

    func primaryAction() {
        guard primaryActionAvailable, let game = selectedGame else { return }
        if let receipt = selectedInstallation {
            let destination = URL(fileURLWithPath: receipt.directory).deletingLastPathComponent()
            download(game, platform: receipt.platform, directory: destination, launchAfterDownload: true)
        } else {
            downloadSelected()
        }
    }

    /// Search and filters reuse the library's name order.
    var filteredGames: [EpicGame] {
        // Observe dependencies even when the cached result is reused.
        let source = games
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        let selectedFilter = filter
        let receipts = selectedFilter == .installed ? installations : []
        if let cached = filteredLibraryCache, cached.query == query, cached.filter == selectedFilter {
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
        let installedLibraryIDs = Set(receipts.map(\.libraryID))
        let filtered = query.isEmpty && selectedFilter == .all ? ordered : ordered.filter { game in
            guard query.isEmpty || game.title.localizedStandardContains(query) else { return false }
            switch selectedFilter {
            case .all: return true
            case .installed: return installedLibraryIDs.contains(game.libraryID)
            case .played: return false
            case .macOS: return game.macos
            }
        }
        filteredLibraryCache = (query, selectedFilter, filtered)
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
            cloud.connect(accountId: credentials.accountId) { [weak self] configuration in
                guard let self, current == self.generation else { throw CancellationError() }
                return try await self.supervise(on: connected, accountId: credentials.accountId, generation: current) {
                    try await connected.resumeCloud(configuration: configuration)
                }
            }

            await reloadGames(connected, generation: current)
            if current == generation { loadInstallations() }
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
        cloud.signedOut()
        let removal = rememberedSession.forget()
        friendsObservationGeneration += 1
        friendsTask?.cancel()
        friendsTask = nil
        friends = nil
        isFriendsPresented = false
        disconnected?.cancelDownload()
        downloadCancellationRequested = true
        downloadTask?.cancel()
        uninstallTask?.cancel()
        progressTask?.cancel()
        downloadTask = nil
        progressTask = nil
        authenticationTask?.cancel()
        libraryTask?.cancel()
        authenticationTask = nil
        libraryTask = nil
        session = nil
        accountID = nil
        accountName = nil
        games = []
        installations = []
        transfer = nil
        transferLibraryID = nil
        lastDownload = nil
        isDownloading = false
        isLaunching = false
        isVerifyingBeforeLaunch = false
        launchLibraryID = nil
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

    func downloadSelected() {
        guard primaryActionAvailable, let game = selectedGame else { return }
        if canResumeDownload { resumeDownload(); return }
        let destination = selectedInstallation.map { URL(fileURLWithPath: $0.directory).deletingLastPathComponent() }
            ?? directory.appending(path: "Games/Epic", directoryHint: .isDirectory)
        download(game, platform: game.preferredPlatform, directory: destination)
    }

    private func download(_ game: EpicGame, platform: String, directory destination: URL, launchAfterDownload: Bool = false) {
        guard let session, let accountID, canModifyInstallations else { return }
        do {
            try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
        } catch {
            errorMessage = L10n.string("model.epic.downloadDirectoryFailed", error.localizedDescription)
            return
        }
        let current = generation
        lastDownload = (game, platform, destination)
        transferLibraryID = game.libraryID
        transfer = EpicTransfer(appName: game.appName, state: .preparing, completedBytes: 0, totalBytes: 0,
                                networkBytes: 0, contentBytes: 0, currentFile: "", message: "")
        isDownloading = true
        isLaunching = launchAfterDownload
        isVerifyingBeforeLaunch = launchAfterDownload
        launchLibraryID = launchAfterDownload ? game.libraryID : nil
        downloadCancellationRequested = false
        errorMessage = nil
        if let index = installations.firstIndex(where: { $0.libraryID == game.libraryID && $0.platform == platform }) {
            installations[index].needsVerification = true
            saveInstallations()
        }
        if launchAfterDownload {
            // The shared session store outlives this model: a strong capture
            // here would pin the whole library graph for the game's lifetime.
            downloadTask = gameSessions.launch(libraryID: game.libraryID, accountId: accountID, prepare: { [weak self] in
                guard let self else { throw CancellationError() }
                let receipt = try await self.performDownload(game, platform: platform, directory: destination, session: session, generation: current)
                try self.requireCurrentDownload(generation: current)
                self.saveInstallations()
                self.isDownloading = false
                self.isVerifyingBeforeLaunch = false
                let configuration = self.cloud.configuration(game: game, receipt: receipt, language: self.launchLanguage)
                return try await self.supervise(on: session, accountId: accountID, generation: current, preparing: false) { [weak self] in
                    guard let self else { throw CancellationError() }
                    try self.requireCurrentDownload(generation: current)
                    return try await session.launchGame(configuration: configuration)
                }
            }, onRunning: { [weak self] in
                guard let self, current == self.generation else { return }
                self.isLaunching = false
            }, onCompletion: { [weak self] error in
                guard let self else { return }
                guard current == self.generation else {
                    self.cloud.sessionFinished()
                    return
                }
                self.isDownloading = false
                self.isLaunching = false
                self.isVerifyingBeforeLaunch = false
                self.launchLibraryID = nil
                self.downloadTask = nil
                self.saveInstallations()
                self.cloud.sessionFinished()
                if let error, !(error is CancellationError), !self.downloadCancellationRequested {
                    if case EpicError.Unauthorized = error { self.signOut() }
                    self.errorMessage = Self.message(error)
                }
            })
            return
        }
        downloadTask = Task {
            defer {
                if current == generation {
                    isDownloading = false
                    isLaunching = false
                    isVerifyingBeforeLaunch = false
                    launchLibraryID = nil
                    downloadTask = nil
                    cloud.sessionFinished()
                }
            }
            do {
                _ = try await performDownload(game, platform: platform, directory: destination, session: session, generation: current)
                try requireCurrentDownload(generation: current)
                saveInstallations()
                isDownloading = false
            } catch {
                guard current == generation else { return }
                saveInstallations()
                if !(error is CancellationError) && !downloadCancellationRequested {
                    if case EpicError.Unauthorized = error { signOut() }
                    errorMessage = Self.message(error)
                }
            }
        }
    }

    private func requireCurrentDownload(generation current: Int) throws {
        guard current == generation && !Task.isCancelled && !downloadCancellationRequested else {
            throw CancellationError()
        }
    }

    private func supervise(on session: any EpicSessionProtocol, accountId: String, generation current: Int,
                           preparing: Bool = true,
                           create: @MainActor () async throws -> EpicGameSession) async throws -> EpicGameSession {
        let handoff = EpicGameHandoff()
        do {
            return try await withTaskCancellationHandler {
                try await rememberedSession.perform(on: session, preparing: preparing) {
                    let client = try await create()
                    handoff.receive(client)
                    return client
                }
            } onCancel: { handoff.cancel() }
        } catch {
            handoff.cancel()
            if current != generation { try? await handoff.revoke(accountId: accountId) }
            throw error
        }
    }

    /// Download, resume and every launch share the same verified installation path.
    private func performDownload(_ game: EpicGame, platform: String, directory destination: URL,
                                 session: any EpicSessionProtocol, generation current: Int) async throws -> EpicInstallReceipt {
        var terminalState: TransferState = .failed
        var startedDownload = false
        var finishedProgress = false
        func finishProgress() {
            if current == generation && !finishedProgress {
                finishedProgress = true
                progressTask?.cancel()
                progressTask = nil
                if startedDownload, let snapshot = session.downloadProgress(), snapshot.appName == game.appName, snapshot.state == terminalState {
                    transfer = snapshot
                } else {
                    transfer?.message = ""
                }
                transfer?.state = terminalState
            }
        }
        defer { finishProgress() }
        do {
            try requireCurrentDownload(generation: current)
            return try await rememberedSession.perform(on: session) {
                try requireCurrentDownload(generation: current)
                progressTask = Task {
                    while current == generation && !Task.isCancelled {
                        if let snapshot = session.downloadProgress(), snapshot.appName == game.appName,
                           snapshot.state == .preparing || snapshot.state == .verifying || snapshot.state == .downloading {
                            if snapshot != transfer { transfer = snapshot }
                        }
                        try? await Task.sleep(for: .milliseconds(250))
                    }
                }
                startedDownload = true
                let installed = try await session.download(namespace: game.namespace, catalogItemId: game.catalogItemId,
                                                           appName: game.appName, platform: platform, directory: destination.path)
                try requireCurrentDownload(generation: current)
                guard session.downloadProgress()?.state != .cancelled else { throw CancellationError() }
                let receipt = EpicInstallReceipt(namespace: game.namespace, appName: installed.appName, name: game.title,
                                                 directory: installed.directory, platform: installed.platform, totalBytes: installed.totalBytes)
                installations.removeAll { $0.id == receipt.id }
                installations.append(receipt)
                saveInstallations()
                lastDownload = nil
                terminalState = .complete
                finishProgress()
                return receipt
            }
        } catch {
            let cancelled = error is CancellationError || downloadCancellationRequested
                || (startedDownload && session.downloadProgress()?.state == .cancelled)
            terminalState = cancelled ? .cancelled : .failed
            if cancelled { throw CancellationError() }
            throw error
        }
    }

    func cancelDownload() {
        guard isDownloading || isLaunching else { return }
        downloadCancellationRequested = true
        session?.cancelDownload()
        if isLaunching && !isDownloading { downloadTask?.cancel() }
    }

    func resumeDownload() {
        guard canResumeDownload, let previous = lastDownload else { return }
        download(previous.game, platform: previous.platform, directory: previous.directory)
    }

    func uninstall(_ receipt: EpicInstallReceipt) {
        guard isConnected, let session, let removalAccount = accountID, canModifyInstallations,
              installations.contains(receipt), let file = receiptsURL,
              let game = games.first(where: { $0.libraryID == receipt.libraryID }) else { return }
        let current = generation
        let remaining = installations.filter { $0 != receipt }
        isUninstalling = true
        uninstallLibraryID = receipt.libraryID
        errorMessage = nil
        uninstallTask = Task {
            defer {
                isUninstalling = false
                uninstallLibraryID = nil
                uninstallTask = nil
                cloud.sessionFinished()
            }
            do {
                guard current == generation && !Task.isCancelled else { return }
                let configuration = cloud.configuration(game: game, receipt: receipt, language: launchLanguage)
                try await rememberedSession.perform(on: session) {
                    try await session.archiveCloud(configuration: configuration)
                }
                guard current == generation && !Task.isCancelled else { return }
                // The engine owns the directory naming scheme; never re-derive it here.
                let name = epicInstallationDirectoryName(
                    namespace: receipt.namespace, catalogItemId: game.catalogItemId,
                    appName: receipt.appName, platform: receipt.platform
                )
                try await GameInstallationManager.uninstall(
                    directory: receipt.directory, expectedDirectoryName: name,
                    protecting: directory, receiptFile: file, remaining: remaining
                )
                // Persist removal even if sign-out occurred during the file operation.
                try await Task.detached {
                    try await epicCloudRemoved(configuration: configuration, accountId: removalAccount)
                }.value
                guard current == generation else {
                    if isConnected { loadInstallations() }
                    return
                }
                installations = remaining
                if lastDownload?.game.libraryID == receipt.libraryID && lastDownload?.platform == receipt.platform {
                    lastDownload = nil
                }
                if transferLibraryID == receipt.libraryID {
                    transfer = nil
                    transferLibraryID = nil
                }
            } catch {
                guard current == generation && !(error is CancellationError) else { return }
                installations = InstallationReceipts.retainingExistingDirectories(installations)
                errorMessage = L10n.string("model.game.uninstallFailed", error.localizedDescription)
            }
        }
    }

    private var receiptsURL: URL? {
        // Account identifiers are opaque. Encode them before using them in a file name.
        accountID.map { account in
            let component = account.utf8.map { String(format: "%02x", $0) }.joined()
            return directory.appending(path: "epic-installations-\(component).json")
        }
    }

    private func loadInstallations() {
        installations = []
        guard let file = receiptsURL else { return }
        do { installations = try InstallationReceipts.load(EpicInstallReceipt.self, from: file) }
        catch { storageWarning = L10n.string("model.epic.receiptsUnavailable") }
    }

    private func saveInstallations() {
        guard let file = receiptsURL else { return }
        do { try InstallationReceipts.save(installations, to: file) }
        catch { storageWarning = L10n.string("model.epic.receiptsNotSaved") }
    }

    static func message(_ error: Error) -> String {
        switch error {
        case EpicError.Network: L10n.string("model.epic.network")
        case EpicError.Unauthorized: L10n.string("model.epic.unauthorized")
        case EpicError.InvalidCode: L10n.string("model.epic.invalidCode")
        case EpicError.InvalidResponse: L10n.string("model.epic.invalidResponse")
        case let EpicError.Service(message): L10n.message(message)
        case let browserError as EpicWebAuthenticationError: browserError.localizedDescription
        case let launchError as GameLaunchFailure: launchError.localizedDescription
        default: L10n.string("model.epic.failure")
        }
    }
}

extension EpicGame {
    var preferredPlatform: String { macos ? "Mac" : "Windows" }
    var libraryID: String { epicLibraryID(namespace: namespace, appName: appName) }
}
