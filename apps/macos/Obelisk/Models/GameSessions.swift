import AppKit
import Observation

/// What one storefront adds to its Game sessions: how its games and accounts
/// are named, and its errors.
protocol GameStorefront {
    associatedtype GameID: Hashable & Sendable
    associatedtype AccountID: Hashable & Sendable
    static var storefront: SessionStorefront { get }
    /// The game and account a session names, when it is this storefront's.
    static func identity(of game: GameIdentity) -> (game: GameID, account: AccountID)?
    /// How a session names `game` of `account`.
    static func identity(_ game: GameID, account: AccountID) -> GameIdentity?
    static func account(_ account: AccountID) -> GameAccount
    static func account(of account: GameAccount) -> AccountID?
    /// How getobelisk.app names `game`: its ID in the store.
    static func storeGameID(_ game: GameID) -> String?
    /// A refused or failed session call, as this storefront reports errors.
    static func failure(_ key: String) -> Error
    static var invalidSession: String { get }
    /// Localization key shown while a session is being recovered.
    static var recoveringSession: String { get }
}

enum SteamGames: GameStorefront {
    static let storefront = SessionStorefront.steam
    static func identity(of game: GameIdentity) -> (game: UInt32, account: UInt64)? {
        guard case let .steam(appId, accountId) = game else { return nil }
        return (appId, accountId)
    }
    static func identity(_ game: UInt32, account: UInt64) -> GameIdentity? { .steam(appId: game, accountId: account) }
    static func account(_ account: UInt64) -> GameAccount { .steam(steamId: account) }
    static func account(of account: GameAccount) -> UInt64? {
        guard case let .steam(steamId) = account else { return nil }
        return steamId
    }
    static func storeGameID(_ game: UInt32) -> String? { String(game) }
    /// Steam's own errors keep their meaning, such as a sign-in to renew.
    static func failure(_ key: String) -> Error {
        switch key {
        case "backend.steam.friends.authenticationRequired": SteamError.AuthenticationRequired
        case "backend.steam.content.cancelled": SteamError.Cancelled
        default: SteamError.Failure(reason: key)
        }
    }
    static let invalidSession = "backend.steam.runtime.invalidSession"
    static let recoveringSession = "model.steam.recoveringSession"

    @MainActor static let sessions = GameSessions<SteamGames>(
        StorefrontSessions(storefront: storefront, locations: .live), history: .live
    )
}

enum EpicGames: GameStorefront {
    static let storefront = SessionStorefront.epic
    static func identity(of game: GameIdentity) -> (game: String, account: String)? {
        guard case let .epic(accountId, namespace, appName) = game else { return nil }
        return (epicLibraryID(namespace: namespace, appName: appName), accountId)
    }
    static func identity(_ game: String, account: String) -> GameIdentity? {
        epicGameID(game).map { .epic(accountId: account, namespace: $0.namespace, appName: $0.appName) }
    }
    static func account(_ account: String) -> GameAccount { .epic(accountId: account) }
    static func account(of account: GameAccount) -> String? {
        guard case let .epic(accountId) = account else { return nil }
        return accountId
    }
    static func storeGameID(_ game: String) -> String? { epicGameID(game)?.appName }
    /// A signed-out account still signs Obelisk out, as any Epic call does.
    static func failure(_ key: String) -> Error {
        switch key {
        case "backend.epic.unauthorized": EpicError.Unauthorized
        case "backend.epic.network": EpicError.Network
        default: EpicError.Service(message: key)
        }
    }
    static let invalidSession = "backend.epic.cloud.invalidSession"
    static let recoveringSession = "model.epic.recoveringSession"

    @MainActor static let sessions = GameSessions<EpicGames>(
        StorefrontSessions(storefront: storefront, locations: .live), history: .live
    )
}

enum GogGames: GameStorefront {
    static let storefront = SessionStorefront.gog
    static func identity(of game: GameIdentity) -> (game: String, account: String)? {
        guard case let .gog(accountId, productId) = game else { return nil }
        return (productId, accountId)
    }
    static func identity(_ game: String, account: String) -> GameIdentity? {
        .gog(accountId: account, productId: game)
    }
    static func account(_ account: String) -> GameAccount { .gog(userId: account) }
    static func account(of account: GameAccount) -> String? {
        guard case let .gog(userId) = account else { return nil }
        return userId
    }
    static func storeGameID(_ game: String) -> String? { game }
    /// A signed-out account still signs Obelisk out, as any GOG call does.
    static func failure(_ key: String) -> Error {
        switch key {
        case "backend.gog.unauthorized": GogError.Unauthorized
        case "backend.gog.network": GogError.Network
        default: GogError.Service(message: key)
        }
    }
    static let invalidSession = "backend.gog.invalidSession"
    static let recoveringSession = "model.gog.recoveringSession"

    @MainActor static let sessions = GameSessions<GogGames>(
        StorefrontSessions(storefront: storefront, locations: .live), history: .live
    )
}

/// How the player ends the running game: Quit asks it to close, then while
/// it closes nothing more can be asked until a frozen game may be forced.
enum GameQuitStage: Equatable {
    case quit
    case closing
    case forceQuit

    var title: String {
        switch self {
        case .quit: L10n.string("model.game.quit")
        case .closing: L10n.string("model.game.closing")
        case .forceQuit: L10n.string("model.game.forceQuit")
        }
    }

    /// What the stage's button does for the game named `title`.
    func label(title: String) -> String {
        switch self {
        case .quit: L10n.string("ui.game.quit_named", title)
        case .closing: L10n.string("model.game.closing")
        case .forceQuit: L10n.string("ui.game.force_quit_named", title)
        }
    }
}

/// How the player's Quit reaches a running game, and how long it gets before
/// Force Quit.
struct GameQuitting: Sendable {
    /// How long a game asked to quit gets before the player may force it.
    var delay: Duration = .seconds(10)
    /// How long Force Quit stays offered. A game may answer Quit with a
    /// question of its own, such as saving first, and play on: its player can
    /// then ask it to quit again rather than only kill it.
    var window: Duration = .seconds(60)
    var sleep: @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) }
    /// Asks the Mac apps running from these folders to quit, as their own
    /// Quit menu does, and tells whether one was asked.
    var quitMacApps: @MainActor @Sendable ([String]) -> Bool = GameQuitting.quitApps(in:)

    /// A Mac game handles its app's Quit: SIGTERM, which the supervisor
    /// otherwise sends, ends most of them at once.
    @MainActor static func quitApps(in folders: [String]) -> Bool {
        let roots = folders.map { URL(fileURLWithPath: $0).resolvingSymlinksInPath().path + "/" }
        let apps = NSWorkspace.shared.runningApplications.filter { app in
            guard let executable = app.executableURL?.resolvingSymlinksInPath().path else { return false }
            return roots.contains { executable.hasPrefix($0) }
        }
        return apps.map { $0.terminate() }.contains(true)
    }
}

typealias SteamGameSessions = GameSessions<SteamGames>
typealias EpicGameSessions = GameSessions<EpicGames>
typealias GogGameSessions = GameSessions<GogGames>

extension GameSessions where Store == SteamGames {
    static var shared: SteamGameSessions { SteamGames.sessions }
}

/// A storefront's Game sessions as Rust keeps them: the Reservation, recovery,
/// Account revocation and the checks on every answer. This presents their
/// state and forwards the player's answers.
@MainActor @Observable
final class GameSessions<Store: GameStorefront> {
    struct Session: Identifiable, Equatable {
        let id: String
        let game: Store.GameID
        var account: Store.AccountID
        var phase: GamePhase
        /// The control connection was lost; the game may still run.
        var reconnecting: Bool
        var cloud: CloudStatus
        /// Steam's native host.
        var host: SteamHostState?
        /// The latest invite dialog a Steam game asked for.
        var inviteRequest: SteamInviteRequest?
        /// The Steam lobby a Steam game is in now.
        var lobbyId: UInt64?
        /// The latest overlay a Steam game asked for, invite dialogs included.
        var overlayRequest: SteamOverlayRequest?
        /// Invitations friends sent into a Steam game, newest first.
        var receivedInvites: [SteamReceivedInvite]
        /// The purchase a Steam game waits on the player to authorize.
        var microTxn: SteamMicroTxn?
        /// A supervisor snapshot backs this session, not only its admission.
        var observed: Bool
        /// Which runtime downloads while the launch waits for it, and how much.
        var runtimeInstall: RuntimeInstallState?
        /// The newer Voile release the launch waits for the player to take or skip.
        var voileUpdate: VoileUpdateOffer?
        /// When this Obelisk saw the game start, even if it already exited. A
        /// game recovered running started before, and has none.
        var startedAt: Date?
        /// What the game did, once it ran and exited.
        var played: GamePlayed?
        var running: Bool { phase == .running }

        init?(_ state: GameSessionState) {
            guard let identity = Store.identity(of: state.game) else { return nil }
            id = state.id
            game = identity.game
            account = identity.account
            phase = state.phase
            reconnecting = state.reconnecting
            cloud = state.cloud
            host = state.host
            inviteRequest = state.inviteRequest
            lobbyId = state.lobbyId
            overlayRequest = state.overlayRequest
            receivedInvites = state.receivedInvites
            microTxn = state.microTxn
            observed = state.observed
            runtimeInstall = state.runtimeInstall
            voileUpdate = state.voileUpdate
            startedAt = state.startedAt.map { Date(timeIntervalSince1970: TimeInterval($0)) }
            played = state.played
        }
    }

    /// The active session's game, as the library and the Cloud indicator read it.
    struct ActiveGame: Equatable {
        let game: Store.GameID
        let account: Store.AccountID
        let running: Bool
        /// Its game exited and the session settles before it ends.
        let ended: Bool
        /// The runtime the launch waits for, while it downloads.
        let runtime: RuntimeComponent?
    }

    private(set) var active: Session?
    /// Changes with `active`'s game, phase and awaited runtime only, not with
    /// how far that runtime downloaded.
    private(set) var activeGame: ActiveGame?
    private(set) var isRecovering = true
    private(set) var recoveryError: String?
    var isBusy: Bool { activeGame != nil || isRecovering }
    /// The player's Quit of the active session's game, until that session
    /// stops running.
    private(set) var quitRequest: QuitRequest?
    @ObservationIgnored let quitting: GameQuitting
    private var revokedAccounts: [Store.AccountID] = []
    private var rosetta: RosettaPrompt?
    /// Rust's Game sessions, which the account's installations admit launches into.
    @ObservationIgnored let core: any StorefrontSessionsProtocol
    /// The status last told, which the next waits for: they arrive in order.
    @ObservationIgnored private var statusTask: Task<Void, Never>?
    /// Where every game this Obelisk saw start is recorded.
    @ObservationIgnored let history: LaunchHistory
    /// The last session whose launch `history` recorded.
    @ObservationIgnored private var recordedLaunch: String?
    /// Told once of each session whose game ran and exited, with what it played.
    @ObservationIgnored var reportPlayed: ((Store.GameID, GamePlayed) -> Void)?
    /// The last session `reportPlayed` was told of.
    @ObservationIgnored private var reportedSession: String?
    @ObservationIgnored private var revision: UInt64 = 0
    @ObservationIgnored private var escalation: Task<Void, Never>?

    struct QuitRequest: Equatable {
        let session: String
        let requestedAt: Date
        /// `quitting.delay` passed and the game still runs.
        var forceable = false
    }

    init(_ core: any StorefrontSessionsProtocol, history: LaunchHistory, quitting: GameQuitting = GameQuitting()) {
        self.core = core
        self.history = history
        self.quitting = quitting
        apply(core.state())
        let revision = revision
        Task { [weak self] in
            var after = revision
            while true {
                let state = await core.changed(after: after)
                guard let self else { return }
                after = state.revision
                apply(state)
            }
        }
    }

    /// Shows Rust's state at once, such as an admission the account's
    /// installations just made.
    func refresh() {
        apply(core.state())
    }

    private func apply(_ state: GameSessionsState) {
        guard state.revision > revision else { return }
        revision = state.revision
        let active = state.active.flatMap(Session.init)
        let last = state.last.flatMap(Session.init)
        // The last session is the one observed most recently: a short game may
        // show only there, already finished, as Rust publishes only its latest
        // state.
        if let last, let startedAt = last.startedAt, last.id != recordedLaunch {
            recordedLaunch = last.id
            history.record(last.game, account: last.account, in: Store.self, at: startedAt)
        }
        if let last, let played = last.played, last.id != reportedSession {
            reportedSession = last.id
            reportPlayed?(last.game, played)
        }
        // Repeated states do not invalidate unchanged presentation.
        if self.active != active { self.active = active }
        let activeGame = active.map {
            ActiveGame(game: $0.game, account: $0.account, running: $0.running,
                       ended: $0.phase == .finalizing || $0.phase == .finished,
                       runtime: $0.runtimeInstall?.component)
        }
        if self.activeGame != activeGame { self.activeGame = activeGame }
        if isRecovering != state.recovering { isRecovering = state.recovering }
        let recoveryError = state.recoveryFailed ? L10n.string(Store.recoveringSession) : nil
        if self.recoveryError != recoveryError { self.recoveryError = recoveryError }
        let revoked = state.revoked.compactMap(Store.account(of:))
        if revokedAccounts != revoked { revokedAccounts = revoked }
        let rosetta = state.rosetta.flatMap { prompt in
            Store.account(of: prompt.account).map {
                RosettaPrompt(id: prompt.sessionId, accountId: $0, decisionId: prompt.decisionId)
            }
        }
        if self.rosetta != rosetta { self.rosetta = rosetta }
        if let quitRequest, active?.id != quitRequest.session || active?.running != true {
            self.quitRequest = nil
            escalation?.cancel()
            escalation = nil
        }
    }

    func revoked(_ account: Store.AccountID) -> Bool { revokedAccounts.contains(account) }

    /// What the page of `game` tells of the Game sessions while `account` is
    /// signed in: one recovering, its recovery's failure, or that
    /// `otherGameRunning`.
    func message(for game: Store.GameID?, of account: Store.AccountID?, otherGameRunning: String) -> String? {
        if isRecovering || active?.reconnecting == true { return L10n.string(Store.recoveringSession) }
        if let recoveryError { return recoveryError }
        if let active, active.account != account || active.game != game { return otherGameRunning }
        return nil
    }

    typealias RosettaPrompt = RosettaLaunchPrompt<String, Store.AccountID>

    func rosettaPrompt(accountId: Store.AccountID) -> RosettaPrompt? {
        rosetta?.accountId == accountId ? rosetta : nil
    }

    func resumeAfterRosetta(_ prompt: RosettaPrompt) async throws {
        try Task.checkCancellation()
        do {
            try await core.resumeAfterRosetta(sessionId: prompt.id, account: Store.account(prompt.accountId),
                                              decisionId: prompt.decisionId)
        }
        catch let GameSessionError.Failure(reason) where reason == Self.rosettaUnavailableKey {
            throw RosettaStillUnavailable()
        }
        catch { throw Self.translated(error) }
    }

    func cancelRosetta(_ prompt: RosettaPrompt) {
        guard rosettaPrompt(accountId: prompt.accountId) == prompt else { return }
        core.declineRosetta(sessionId: prompt.id, account: Store.account(prompt.accountId),
                            decisionId: prompt.decisionId)
        apply(core.state())
    }

    /// The newer Voile release the active launch waits for the player to take or skip.
    var voileUpdate: VoileUpdateOffer? { active?.voileUpdate }

    /// The player's answer to `voileUpdate`: Cancel stops the launch.
    func answerVoileUpdate(_ answer: VoileUpdateAnswer) {
        guard let active, active.voileUpdate != nil else { return }
        // A refused answer finds the launch gone, or a release its game needs.
        try? core.answerVoileUpdate(sessionId: active.id, answer: answer)
        apply(core.state())
    }

    /// Ends the running game, as quitting Obelisk does: it gets a few seconds
    /// to quit, and its session still settles its Cloud saves.
    func stop() async {
        try? await core.stop()
        apply(core.state())
    }

    /// What ending the active session's running game asks of the player now.
    var quitStage: GameQuitStage? {
        // A session reconnecting to its supervisor cannot be asked anything.
        guard let active, active.running, !active.reconnecting else { return nil }
        guard let quitRequest, quitRequest.session == active.id else { return .quit }
        return quitRequest.forceable ? .forceQuit : .closing
    }

    /// The player's Quit: the running game is asked to close its windows, and
    /// nothing is killed. A Mac game running from one of `macApps` is asked
    /// as its own Quit menu would. Once `quitting.delay` passed with the game
    /// still running, the player may force it to quit for `quitting.window`.
    func quit(macApps: [String] = []) async {
        guard quitStage == .quit, let active else { return }
        let session = active.id
        quitRequest = QuitRequest(session: session, requestedAt: .now)
        escalation = Task { [weak self, quitting] in
            try? await quitting.sleep(quitting.delay)
            guard !Task.isCancelled, self?.quitRequest?.session == session else { return }
            self?.quitRequest?.forceable = true
            try? await quitting.sleep(quitting.window)
            guard !Task.isCancelled, self?.quitRequest?.session == session else { return }
            self?.quitRequest = nil
        }
        if quitting.quitMacApps(macApps) { return }
        // A refused Quit still lets the player force the game once it is due.
        try? await core.quit(sessionId: session)
        apply(core.state())
    }

    /// The player's Force Quit of a game that ignored Quit: it ends now, and
    /// its session still settles its Cloud saves. The player confirms it
    /// first: what the game did not save is lost.
    func forceQuit() async {
        guard quitStage == .forceQuit, let active else { return }
        try? await core.forceQuit(sessionId: active.id)
        apply(core.state())
    }

    /// Account revocation. Admission checks it at once; the supervisor learns
    /// it as soon as it can be reached.
    func signedOut(account: Store.AccountID) {
        core.revoke(account: Store.account(account))
        apply(core.state())
    }

    /// How the player shows to their friends: the next game starts showing
    /// it, and the running one shows it at once.
    func setStatus(_ status: OnlineStatus) {
        let (core, previous) = (core, statusTask)
        statusTask = Task {
            await previous?.value
            await core.setStatus(status: status)
        }
    }

    /// A session call's failure, reported as the storefront's error.
    static func translated(_ error: Error) -> Error {
        switch error {
        case GameSessionError.Cancelled: CancellationError()
        case let GameSessionError.Failure(reason): Store.failure(reason)
        default: error
        }
    }
}

extension GameSessions where Store == SteamGames {
    /// Obelisk's overlay opened or closed over session `id`'s running game,
    /// which learns it as from Steam's overlay; `userInitiated` when the
    /// player opened it rather than the game.
    func overlayState(sessionId id: String, active: Bool, userInitiated: Bool) async throws {
        do { try await core.overlayState(sessionId: id, active: active, userInitiated: userInitiated) }
        catch { throw Self.translated(error) }
    }

    /// The player joins `friend`'s invitation into session `id`'s running
    /// game, as Steam's overlay would accept it.
    func acceptInvite(sessionId id: String, friendSteamId friend: UInt64, invite: SteamGameInvite) async throws {
        do { try await core.acceptInvite(sessionId: id, friendSteamId: friend, invite: invite) }
        catch { throw Self.translated(error) }
        refresh()
    }

    /// The player's answer to the purchase `orderId` session `id`'s game waits on.
    func authorizeMicroTxn(sessionId id: String, orderId: UInt64, approve: Bool) async throws {
        do { try await core.authorizeMicroTxn(sessionId: id, orderId: orderId, approve: approve) }
        catch { throw Self.translated(error) }
        refresh()
    }
}

/// The supervisor's "Rosetta still unavailable" reply to a resumed launch.
struct RosettaStillUnavailable: Error {}

extension GameSessions: RosettaSessions {
    static func rosettaUnavailable(_ error: Error) -> Bool { error is RosettaStillUnavailable }
}
