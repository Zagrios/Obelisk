import AppKit
import Foundation
import Observation

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
    private(set) var displayName: String?
    private(set) var avatarURL: URL?
    private(set) var friends: FriendsPresentationState?
    var isFriendsPresented = false
    /// Shows the game's invite picker over the running game.
    @ObservationIgnored weak var overlay: GameOverlayController?
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
    var platform: GamePlatform {
        if let details, details.appId == selectedGame?.appId { return details.platform ?? .windows }
        // Installation receipts are available before remote details. Never use
        // the previous game's platform to present the newly selected game.
        return installations.receipts.contains { $0.gameID == selectedGame?.appId && $0.platform == .mac }
            ? .mac : .windows
    }
    private(set) var language: String
    let installations: SteamInstallations
    /// The account's games, from the Library cache while the session
    /// connects. When the account got each game and its achievements'
    /// progress load after them.
    let library: AccountLibrary<SteamGames, SteamGame>

    @ObservationIgnored private let directory: URL
    @ObservationIgnored private let keychain: any CredentialStore<SteamCredentials>
    @ObservationIgnored private let connectSession: @Sendable (SteamCredentials) async throws -> any SteamSessionProtocol
    @ObservationIgnored private let loadNews: @Sendable (UInt32) async throws -> [SteamNews]
    @ObservationIgnored private let loadWebCookies: @Sendable (any SteamSessionProtocol) async throws -> [SteamWebCookie]
    /// The account's web sign-in, kept a while: its access token lasts a day.
    @ObservationIgnored private var webSignIn: (steamId: UInt64, date: Date, cookies: [HTTPCookie])?
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
    let libraryArtwork = SteamLibraryArtwork()
    /// Chat and game invitations, through the session the friends list follows.
    let chat: SteamChatModel
    /// Friends' invitations, into the running game or another.
    let invitations: SteamReceivedInvitations
    /// Tells of an invitation while no running game shows the overlay.
    @ObservationIgnored var notifier: (any SteamInviteNotifying)? {
        didSet {
            notifier?.answer = { [weak self] answer, invite in self?.answerNotice(answer, invite) }
            notifier?.openMessage = { [weak self] friend in
                guard let self else { return }
                if self.overlay?.present(.user(steamId: friend), userInitiated: true) == true { return }
                self.chat.requestedFriend = friend
                self.overlay?.showLibrary()
                self.openFriends()
            }
        }
    }
    /// The in-game purchase the running game waits on.
    let purchase: SteamPurchaseAuthorization
    @ObservationIgnored private var session: (any SteamSessionProtocol)?
    /// The session's CM connection of the moment, as it last published it;
    /// nil while it reconnects, after it gave up, or once Steam asked to sign
    /// in again.
    @ObservationIgnored private var transport: UInt64?
    @ObservationIgnored private var login: (any SteamLoginProtocol)?
    private var steamId: UInt64? { sessionCredentials?.steamId }
    /// The saved session's restoration; true when it ended without a newer
    /// sign-in or sign-out.
    @ObservationIgnored private var restoration: Task<Bool, Never>?
    /// Whether the QR sign-in was offered once the restoration ended.
    @ObservationIgnored private var offeredSignIn = false
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var detailGeneration = 0
    @ObservationIgnored private var detailsRequestGeneration = 0
    @ObservationIgnored private var loginTask: Task<Void, Never>?
    @ObservationIgnored private var profileTask: Task<Void, Never>?
    @ObservationIgnored private var friendsTask: Task<Void, Never>?
    /// The connected account; observed because `isConnected` derives from it.
    private var sessionCredentials: SteamCredentials?
    @ObservationIgnored private var wakeObserver: SystemWakeObservation?
    @ObservationIgnored private var connectionTask: Task<any SteamSessionProtocol, Error>?
    @ObservationIgnored private var pollingActive = false
    /// The Game session last seen, to tell when one ends.
    @ObservationIgnored private var observedGameSession: SteamGameSessions.Session?
    /// The running game's session and the last overlay request it showed.
    @ObservationIgnored private var followedOverlay = SessionSequence()

    init(
        directory: URL,
        keychain: any CredentialStore<SteamCredentials> = SteamKeychain(),
        connect: (@Sendable (SteamCredentials) async throws -> any SteamSessionProtocol)? = nil,
        news: @escaping @Sendable (UInt32) async throws -> [SteamNews] = { try await steamNews(appId: $0) },
        webCookies: @escaping @Sendable (any SteamSessionProtocol) async throws -> [SteamWebCookie] = {
            try await $0.webCookies()
        },
        beginLogin: @escaping @Sendable (SteamLoginStart) async throws -> any SteamLoginProtocol = { start in
            switch start {
            case .qr:
                try await SteamLogin.beginQr()
            case let .credentials(account, password):
                try await SteamLogin.beginCredentials(accountName: account, password: password)
            }
        },
        localeIdentifier: String = Locale.preferredLanguages.first ?? Locale.current.identifier,
        gameSessions: SteamGameSessions = .shared,
        libraryCache: LibraryCache? = nil,
        installations makeInstallations: MakeInstallations? = nil,
        chat: SteamChatModel? = nil
    ) {
        self.directory = directory
        self.keychain = keychain
        self.connectSession = connect ?? { [libraryCache] in try await SteamSessionConnection.connect($0, library: libraryCache) }
        self.loadNews = news
        self.loadWebCookies = webCookies
        self.beginLogin = beginLogin
        self.language = steamLanguage(locale: localeIdentifier)
        self.gameSessions = gameSessions
        let locations = ObeliskLocations.of(library: directory)
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
        self.library = AccountLibrary(history: gameSessions.history, cache: libraryCache,
                                      installations: installations)
        self.chat = chat ?? SteamChatModel()
        self.invitations = SteamReceivedInvitations(sessions: gameSessions, installations: installations)
        self.purchase = SteamPurchaseAuthorization(sessions: gameSessions)
        wakeObserver = SystemWakeObservation { [weak self] in
            Task { @MainActor [weak self] in self?.retryFriends() }
        }
        installations.selection = { [weak self] in self?.installationSelection }
        self.chat.gameName = { [weak self] in self?.gameName(appId: $0) }
        self.chat.invitations = invitations
        self.chat.onIncomingMessage = { [weak self] friend in
            guard let self else { return }
            if self.overlay?.announceMessage(friend) == true { return }
            let name = self.friends?.friend(friend)?.displayName ?? L10n.string("ui.overlay.invites.someone")
            self.notifier?.notifyMessage(friend: friend, name: name)
        }
        invitations.friendMayPlay = { [weak self] in self?.friendMayPlay($0, $1) ?? true }
        invitations.onReceived = { [weak self] in self?.announce($0) }
        invitations.onGone = { [weak self] in self?.notifier?.withdraw($0) }
        observeGameSessions()
    }

    deinit {
        loginTask?.cancel()
        friendsTask?.cancel()
        connectionTask?.cancel()
        session?.shutdown()
    }

    var isConnected: Bool { sessionCredentials != nil }

    var unreadCount: Int { chat.totalUnread }

    /// Whether the library shows: once connected, or from the cache while the
    /// saved session restores.
    var showsLibrary: Bool { library.shows(connected: isConnected, restoring: isRestoring) }

    /// The game on screen, for its installation actions. Only remote details
    /// tell which builds the account can install.
    private var installationSelection: InstallationSelection<SteamGames>? {
        guard let game = selectedGame else { return nil }
        let details = details?.appId == game.appId && !isLoadingDetails ? details : nil
        let offer = details.flatMap { $0.offer(for: game, language: language) }
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

    /// Loads what Steam tells apart from the library, once it loaded through
    /// a connection: when the account got each game, from its licenses, and
    /// the share of each game's achievements it unlocked. The session keeps
    /// both in the Library cache, and reads again only the achievements of
    /// the games played since.
    private func loadLibraryRecords() {
        guard let session, transport != nil, !library.games.isEmpty else { return }
        let appIds = library.games.map(\.appId), language = language
        // The licenses also grant DLC and tools, which the library leaves out.
        let owned = Set(appIds)
        library.load(.dateAdded) {
            try await session.acquisitions().filter { owned.contains($0.key) }.mapValues { Double($0) }
        }
        library.load(.achievements) { try await session.achievementShares(appIds: appIds, language: language) }
    }

    /// Restores the saved session once, then offers the QR sign-in when none
    /// restored, unless `signIn` is false: Home restores every storefront at
    /// launch, and a signed-out Steam waits for its own tab to ask Steam for a
    /// QR code. Returns once the restoration ends.
    func start(signIn: Bool = true) async {
        if restoration == nil {
            isRestoring = true
            restoration = Task { await restore() }
        }
        guard await restoration?.value == true, signIn, !offeredSignIn else { return }
        offeredSignIn = true
        if !isConnected { beginQR() }
    }

    private func restore() async -> Bool {
        let current = generation
        do {
            if let credentials = try await keychain.load() {
                guard current == generation else { return false }
                showCachedLibrary(of: credentials.steamId)
                await connect(credentials, generation: current, persist: false)
            }
        } catch {
            guard current == generation else { return false }
            storageWarning = error.localizedDescription
        }
        guard current == generation else { return false }
        isRestoring = false
        // Another account may sign in next; the cache keeps the library for a
        // later restoration.
        if !isConnected, library.dropCache() { libraryArtwork.reset() }
        return true
    }

    /// Shows the account's last library at once, while its session connects.
    private func showCachedLibrary(of account: UInt64) {
        let language = language
        var artwork: [UInt32: SteamGameArtwork] = [:]
        let shown = library.showCache(of: account) { cache in
            let cached = cache.steamLibrary(steamId: account, language: language)
            artwork = Dictionary(cached.compactMap { entry in entry.artwork.map { (entry.game.appId, $0) } },
                                 uniquingKeysWith: { first, _ in first })
            return cached.map { entry in
                CachedLibraryGame(game: entry.game, values: [
                    .dateAdded: entry.addedAt.map { Double($0) }, .achievements: entry.achievementShare,
                ].compactMapValues { $0 })
            }
        }
        if shown { libraryArtwork.show(artwork) }
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
            // The cached library's platforms show until the reload replaces them.
            if library.connect(credentials.steamId) {
                libraryArtwork.cancel()
            } else {
                libraryArtwork.reset()
            }
            session = connected
            transport = 0
            sessionCredentials = credentials
            displayName = nil
            avatarURL = nil
            friends = Self.emptyFriends(accountID: credentials.steamId)
            // Rust's session logged on Online; the chat follows it from here.
            chat.connect(connected, account: credentials.steamId)
            invitations.connect(connected, account: credentials.steamId)
            followGameSession(account: credentials.steamId)
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
        guard let session, transport != nil, !isLoadingGames else { return }
        let current = generation
        Task { await reloadGames(session, generation: current) }
    }

    #if DEBUG
    /// A Cloud diagnostic needs the storefront idle, and keeps it fenced.
    private func reserveCloudProbe() -> Bool {
        let idle = InstallationOpening<UInt32>.open(except: [])
        guard installations.gate.launch == idle, installations.gate.modify == idle,
              gameSessions.recoveryError == nil else { return false }
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
        let transport = self.transport
        guard let index = arguments.firstIndex(of: probe.flag),
              arguments.count > index + 2, let appID = UInt32(arguments[index + 1]),
              appID != 0, current == generation, transport != nil,
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
            guard current == generation, self.transport == transport else { return }
            try Data(json.utf8).write(to: report, options: .atomic)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: report.path)
        } catch {
            // Steam errors contain only typed diagnostics, not request URLs.
            guard current == generation, self.transport == transport else { return }
            let data = try? JSONSerialization.data(withJSONObject: ["status": "failed", "reason": error.localizedDescription])
            try? data?.write(to: report, options: .atomic)
        }
    }
    #endif

    private func reloadGames(_ session: any SteamSessionProtocol, generation current: Int) async {
        let transport = self.transport
        guard current == generation && transport != nil && !Task.isCancelled else { return }
        libraryArtwork.cancel()
        isLoadingGames = true
        do {
            let loaded = try await session.games()
            guard current == generation && self.transport == transport else { return }
            library.show(loaded)
            if let selected = selectedGame, let updated = loaded.first(where: { $0.appId == selected.appId }) {
                selectedGame = updated
            }
            isLoadingGames = false
            libraryArtwork.load(appIds: loaded.map(\.appId), language: language, session: session)
            loadLibraryRecords()
        } catch {
            guard current == generation && self.transport == transport else { return }
            isLoadingGames = false
            errorMessage = Self.message(error)
        }
        guard current == generation && self.transport == transport && !Task.isCancelled else { return }
        reloadProfile(session, generation: current)
    }

    private func reloadProfile(_ session: any SteamSessionProtocol, generation current: Int) {
        let transport = self.transport
        profileTask?.cancel()
        profileTask = Task {
            do {
                let profile = try await session.profile()
                guard current == generation && self.transport == transport && !Task.isCancelled else { return }
                displayName = profile.displayName
                avatarURL = profile.avatarUrl.flatMap(URL.init(string:))
            } catch {
                // Optional profile information must not interrupt the library or login.
            }
        }
    }

    /// The game's name in the library, or as a friend playing it shows it.
    func gameName(appId: UInt32) -> String? {
        if let game = library.game(appId) { return game.name }
        return friends?.friends.lazy.compactMap(\.activity).first { $0.id == String(appId) }?.name
    }

    /// A game's own Steam session can leave the account's presence behind it,
    /// so the account shows Online again once its Game session ends. While it
    /// runs, the chat follows what the game can invite into.
    private func observeGameSessions() {
        let active = withObservationTracking { gameSessions.active } onChange: { [weak self] in
            Task { @MainActor [weak self] in self?.observeGameSessions() }
        }
        if let ended = observedGameSession, active?.id != ended.id, ended.account == steamId { chat.setOnline() }
        observedGameSession = active
        followGameSession(account: steamId)
    }

    /// The chat follows the Game session of `account`: whether it plays, and
    /// what its running game asked to invite friends into.
    private func followGameSession(account: UInt64?) {
        let active = gameSessions.active
        let playing = account != nil && active?.account == account ? active : nil
        let running = playing?.running == true ? playing : nil
        if chat.followGame(session: running?.id, appId: running?.game ?? 0, request: running?.inviteRequest,
                           lobby: running?.lobbyId) {
            openInvitePicker()
        }
        followOverlayRequest(of: running)
        invitations.follow(session: running?.id, invites: running?.receivedInvites ?? [])
        if purchase.follow(session: running?.id, microTxn: running?.microTxn), let pending = purchase.pending {
            overlay?.present(.microTxn(orderId: pending.orderId))
        }
    }

    /// The running game asked Steam's overlay for one of its pages: Obelisk's
    /// overlay shows its own. As for invite dialogs, a request the session
    /// showed when first seen opens nothing.
    private func followOverlayRequest(of running: SteamGameSessions.Session?) {
        guard let running else {
            followedOverlay = SessionSequence()
            return
        }
        guard followedOverlay.advance(session: running.id, sequence: running.overlayRequest?.sequence),
              let request = running.overlayRequest,
              let shown = OverlayRequest.steam(request.kind, appId: running.game, account: steamId) else { return }
        overlay?.present(shown)
    }

    /// The running game asked Steam's overlay to invite friends: Obelisk's
    /// overlay shows over it, its friends list as the picker.
    private func openInvitePicker() {
        guard friends != nil, let picker = chat.invitePicker, overlay?.present(.invite(picker)) == true else {
            chat.closeInvitePicker()
            return
        }
    }

    /// Tells of the newest of `invites` that just arrived: over the running
    /// game when its overlay can show, else in a macOS notification.
    private func announce(_ invites: [SteamReceivedInvite]) {
        guard let invite = invites.first else { return }
        if overlay?.announce(invite) == true { return }
        let name = friends?.friend(invite.friendSteamId)?.displayName ?? L10n.string("ui.overlay.invites.someone")
        notifier?.notify(invite, friend: name, game: gameName(appId: invite.appId))
    }

    /// The player answered an invitation's notification.
    private func answerNotice(_ answer: SteamInviteAnswer, _ invite: SteamReceivedInvite) {
        switch answer {
        case .join:
            // A notice outlives its invitation: only one still listed joins.
            guard let listed = invitations.listed(invite) else { return }
            let invitations = invitations
            Task { _ = await invitations.accept(listed) }
        case .dismiss:
            invitations.dismiss(invite)
        case .open:
            overlay?.showLibrary()
            openFriends()
        }
    }

    /// Whether the friends list lets `friend` still play `appId`, for the
    /// invitations: one it does not know yet, or shows online without a
    /// Steam game, may.
    private func friendMayPlay(_ friend: UInt64, _ appId: UInt32) -> Bool {
        guard let presentation = friends?.friend(friend), presentation.presence != .unknown else { return true }
        guard presentation.presence.isOnline else { return false }
        return presentation.activity.flatMap { UInt32($0.id) }.map { $0 == appId } ?? true
    }

    func openFriends() {
        guard friends != nil else { return }
        isFriendsPresented = true
    }

    func closeFriends() {
        isFriendsPresented = false
    }

    /// The player's retry, or the Mac's wake, asks the session to connect
    /// again: only these restart a reconnection that gave up. The session
    /// decides whether it may, and its next state shows it.
    func retryFriends() {
        guard isConnected, let session else { return }
        session.reconnect()
    }

    private func stopFriends() {
        friendsTask?.cancel()
        friendsTask = nil
    }

    /// Follows the session's friends and connection until it stops. The
    /// session connects again by itself; what follows its CM connection is
    /// in `follow`.
    private func observeFriends(_ session: any SteamSessionProtocol, generation current: Int) {
        guard let credentials = sessionCredentials else { return }
        friendsTask = Task { [weak self] in
            var revision: UInt64 = 0
            while !Task.isCancelled {
                let snapshot = await withTaskCancellationHandler {
                    await session.nextFriends(afterRevision: revision)
                } onCancel: {
                    // Closes the account session, whose Closed state wakes the
                    // pending Rust wait: only sign-out, a new sign-in or the
                    // model's end cancel this task, and each ends the session.
                    session.shutdown()
                }
                guard let self, current == self.generation, !Task.isCancelled else { return }
                revision = snapshot.revision
                guard snapshot.accountId == credentials.steamId else { continue }
                self.applyFriends(snapshot)
                self.follow(snapshot, of: session, account: credentials.steamId, generation: current)
                if snapshot.connection == .stopped { return }
            }
        }
    }

    /// What Swift loads through the session's CM connection follows it: a
    /// new one reloads the library and the game on screen, and while there
    /// is none, platform loads wait. Steam's chat and the account's own game
    /// reach Swift as streams that end with their CM connection, so the chat
    /// follows the connection here too: it waits while there is none, and a
    /// new one reloads what it missed.
    private func follow(_ snapshot: SteamFriendsSnapshot, of session: any SteamSessionProtocol, account: UInt64,
                        generation current: Int) {
        switch snapshot.connection {
        case .loading, .ready, .error:
            guard snapshot.transport != transport else { return }
            let reconnected = snapshot.transport
            transport = reconnected
            libraryArtwork.cancel()
            chat.connect(session, account: account)
            invitations.connect(session, account: account)
            Task { [weak self] in
                guard let self else { return }
                await self.reloadGames(session, generation: current)
                guard current == self.generation, self.transport == reconnected else { return }
                if self.selectedGame != nil { self.reloadDetails() }
            }
        case .disconnected, .unreachable, .authenticationRequired, .stopped:
            guard transport != nil else { return }
            transport = nil
            libraryArtwork.cancel()
            chat.suspend()
            invitations.suspend()
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
                activity: friend.game.map { FriendGameActivity(id: $0.appId.map(String.init) ?? "unknown", name: $0.name) },
                steamJoin: friend.join
            )
        }
        switch snapshot.connection {
        case .loading: presentation.connection = .loading
        case .ready: presentation.connection = .connected
        case .disconnected, .stopped: presentation.connection = .interrupted
        case .authenticationRequired: presentation.connection = .authenticationRequired
        case .error: presentation.connection = .failed(L10n.string("model.friends.unavailable"))
        case .unreachable: presentation.connection = .failed(L10n.string("model.friends.reconnectFailed"))
        }
        if snapshot.connection != .ready, snapshot.connection != .loading,
           let previous = friends, !previous.friends.isEmpty {
            presentation.friends = previous.friends
        }
        presentation.lastKnownOnlineCount = snapshot.connection == .ready
            ? presentation.onlineCount : friends?.lastKnownOnlineCount
        if friends != presentation {
            friends = presentation
            invitations.friendsChanged()
        }
    }

    func signOut() {
        library.forget()
        libraryArtwork.reset()
        rosetta.connect(accountId: nil)
        installations.signedOut()
        cloud.signedOut()
        generation += 1
        detailGeneration += 1
        stopFriends()
        session?.shutdown()
        chat.reset()
        followedOverlay = SessionSequence()
        invitations.reset()
        purchase.follow(session: nil, microTxn: nil)
        sessionCredentials = nil
        webSignIn = nil
        friends = nil
        isFriendsPresented = false
        loginTask?.cancel()
        connectionTask?.cancel()
        connectionTask = nil
        profileTask?.cancel()
        profileTask = nil
        session = nil
        transport = nil
        login = nil
        displayName = nil
        avatarURL = nil
        selectedGame = nil
        details = nil
        news = []
        challenge = nil
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
        guard transport != nil else { return }
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
        loadAchievements(appId: game.appId, into: achievements, fresh: false)
        installations.refreshCloudIndicator()
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

    /// Reads the game's achievements from Steam again, as the page's retry asks.
    func reloadAchievements() {
        guard let game = selectedGame else { return }
        loadAchievements(appId: game.appId, into: achievements, fresh: true)
    }

    /// Loads the achievements of `appId` into `model`: the Library cache's
    /// while the game was not played since, unless `fresh`, as the overlay
    /// asks for the running game.
    func loadAchievements(appId: UInt32, into model: GameAchievementsModel, fresh: Bool = true) {
        guard let session, transport != nil else { return }
        let language = language
        model.load {
            try await session.achievements(appId: appId, language: language, fresh: fresh).map(GameAchievement.init)
        }
    }

    /// Remote details tell which builds the account can download, so an
    /// installed game never waits for them.
    func reloadDetails() {
        let transport = self.transport
        guard let game = selectedGame, let session, transport != nil else { return }
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
                guard current == generation && self.transport == transport && selection == detailGeneration && detailsRequest == detailsRequestGeneration else { return }
                details = loaded
                isLoadingDetails = false
            } catch {
                guard current == generation && self.transport == transport && selection == detailGeneration && detailsRequest == detailsRequestGeneration else { return }
                detailsError = Self.message(error)
                isLoadingDetails = false
            }
        }
    }

    /// The library downloads a game without opening its page: Steam picks the
    /// build the account can install, the native one first, as the Transfer
    /// starts. It needs the session's CM connection to.
    func libraryOffer(of game: SteamGame) -> GameOffer? {
        guard transport != nil else { return nil }
        return .steam(appId: game.appId, name: game.name, language: language)
    }

    func closeGame() {
        detailGeneration += 1
        selectedGame = nil
        details = nil
        news = []
    }

    /// Cookies that sign Steam's sites in as the connected account, as the
    /// Steam client signs its own browser in: its session asks Steam for a web
    /// access token. None while signed out, or when Steam refuses them: the
    /// page then shows signed out.
    func webCookies() async -> [HTTPCookie] {
        guard let credentials = sessionCredentials, let session, transport != nil else { return [] }
        if let webSignIn, webSignIn.steamId == credentials.steamId, webSignIn.date.timeIntervalSinceNow > -3_600 {
            return webSignIn.cookies
        }
        guard let loaded = try? await loadWebCookies(session),
              sessionCredentials?.steamId == credentials.steamId else { return [] }
        let cookies = loaded.compactMap(Self.cookie)
        webSignIn = (credentials.steamId, Date(), cookies)
        return cookies
    }

    /// Steam's store, signed in as the connected account.
    func storePage() async -> WebPageStart {
        WebPageStart(url: GameOverlayController.storeURL(appId: 0), cookies: await webCookies())
    }

    /// Steam's store searching for `query`, signed in as the connected account.
    func storeSearch(_ query: String) async -> WebPageStart {
        WebPageStart(url: GameOverlayController.storeSearchURL(query), cookies: await webCookies())
    }

    nonisolated static func cookie(_ cookie: SteamWebCookie) -> HTTPCookie? {
        var properties: [HTTPCookiePropertyKey: Any] = [
            .domain: cookie.domain, .path: "/", .name: cookie.name, .value: cookie.value, .secure: "TRUE",
        ]
        if cookie.httpOnly { properties[HTTPCookiePropertyKey("HttpOnly")] = "TRUE" }
        return HTTPCookie(properties: properties)
    }

    private func connectInstallations(_ session: any SteamSessionProtocol, credentials: SteamCredentials) {
        let receipts = directory.appending(path: "steam-installations-\(credentials.steamId).json")
        installations.connect(account: credentials.steamId,
                              backend: makeInstallations(session, credentials, gameSessions.core, receipts),
                              port: installationPort())
    }

    /// Steam's part of the installation actions.
    private func installationPort() -> InstallationPort<SteamGames> {
        InstallationPort(
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

extension SteamGame: Identifiable {
    public var id: UInt32 { appId }

    /// Nil when the game was never played.
    var playtime: Duration? { playtimeMinutes > 0 ? .seconds(Int64(playtimeMinutes) * 60) : nil }

    /// When Steam last saw the game played; nil when it never did.
    var lastPlayedOnSteam: Date? { lastPlayed > 0 ? Date(timeIntervalSince1970: TimeInterval(lastPlayed)) : nil }
}

extension SteamGame: LibraryGame {
    var title: String { name }
    var lastPlayedOnStorefront: Date? { lastPlayedOnSteam }

    /// Steam tells when the account got each game and its achievements apart
    /// from the library.
    func libraryValue(for sort: StorefrontLibrarySort) -> Double? { nil }
}

extension SteamGameDetails {
    /// What Steam can install of `game` for the account. Steam picks the
    /// build again as the Transfer starts.
    func offer(for game: SteamGame, language: String) -> GameOffer? {
        guard platform != nil else { return nil }
        return .steam(appId: game.appId, name: game.name, language: language)
    }
}
