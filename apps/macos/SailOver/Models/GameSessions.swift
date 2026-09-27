import Foundation
import Observation

/// What one storefront adds to its Game sessions: how its games and accounts
/// are named, and its errors.
protocol GameStorefront {
    associatedtype GameID: Hashable & Sendable
    associatedtype AccountID: Hashable & Sendable
    static var storefront: SessionStorefront { get }
    /// The game and account a session names, when it is this storefront's.
    static func identity(of game: GameIdentity) -> (game: GameID, account: AccountID)?
    static func account(_ account: AccountID) -> GameAccount
    static func account(of account: GameAccount) -> AccountID?
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
    static func account(_ account: UInt64) -> GameAccount { .steam(steamId: account) }
    static func account(of account: GameAccount) -> UInt64? {
        guard case let .steam(steamId) = account else { return nil }
        return steamId
    }
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
        StorefrontSessions(storefront: storefront, locations: .live)
    )
}

enum EpicGames: GameStorefront {
    static let storefront = SessionStorefront.epic
    static func identity(of game: GameIdentity) -> (game: String, account: String)? {
        guard case let .epic(accountId, namespace, appName) = game else { return nil }
        return (epicLibraryID(namespace: namespace, appName: appName), accountId)
    }
    static func account(_ account: String) -> GameAccount { .epic(accountId: account) }
    static func account(of account: GameAccount) -> String? {
        guard case let .epic(accountId) = account else { return nil }
        return accountId
    }
    /// A signed-out account still signs SailOver out, as any Epic call does.
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
        StorefrontSessions(storefront: storefront, locations: .live)
    )
}

typealias SteamGameSessions = GameSessions<SteamGames>
typealias EpicGameSessions = GameSessions<EpicGames>

extension GameSessions where Store == SteamGames {
    static var shared: SteamGameSessions { SteamGames.sessions }
}
extension GameSessions where Store == EpicGames {
    static var shared: EpicGameSessions { EpicGames.sessions }
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
        /// A supervisor snapshot backs this session, not only its admission.
        var observed: Bool
        /// How much of Voile has downloaded while the launch waits for it.
        var runtimeInstall: Double?
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
            observed = state.observed
            runtimeInstall = state.runtimeInstall
        }
    }

    private(set) var active: Session?
    /// The last session observed, kept after it ends.
    private(set) var last: Session?
    private(set) var isRecovering = true
    private(set) var recoveryError: String?
    var isBusy: Bool { active != nil || isRecovering }
    private var revokedAccounts: [Store.AccountID] = []
    private var rosetta: RosettaPrompt?
    /// Rust's Game sessions, which the account's installations admit launches into.
    @ObservationIgnored let core: any StorefrontSessionsProtocol
    @ObservationIgnored private var revision: UInt64 = 0

    init(_ core: any StorefrontSessionsProtocol) {
        self.core = core
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
        // Repeated states do not invalidate unchanged presentation.
        if self.active != active { self.active = active }
        let last = state.last.flatMap(Session.init)
        if self.last != last { self.last = last }
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
    }

    /// The game's Reservation: no Transfer, uninstall or other launch of it.
    func protects(_ game: Store.GameID?) -> Bool {
        isRecovering || (game != nil && active?.game == game)
    }

    func revoked(_ account: Store.AccountID) -> Bool { revokedAccounts.contains(account) }

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
        catch { throw Self.translated(error) }
    }

    func cancelRosetta(_ prompt: RosettaPrompt) {
        guard rosettaPrompt(accountId: prompt.accountId) == prompt else { return }
        core.declineRosetta(sessionId: prompt.id, account: Store.account(prompt.accountId),
                            decisionId: prompt.decisionId)
        apply(core.state())
    }

    /// Ends the running game, as quitting SailOver does: it gets a few seconds
    /// to quit, and its session still settles its Cloud saves.
    func stop() async {
        try? await core.stop()
        apply(core.state())
    }

    /// Account revocation. Admission checks it at once; the supervisor learns
    /// it as soon as it can be reached.
    func signedOut(account: Store.AccountID) {
        core.revoke(account: Store.account(account))
        apply(core.state())
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

extension GameSessions: RosettaSessions {
    static func rosettaUnavailable(_ error: Error) -> Bool {
        switch error {
        case let SteamError.Failure(reason): reason == rosettaUnavailableKey
        case let EpicError.Service(message): message == rosettaUnavailableKey
        default: false
        }
    }
}
