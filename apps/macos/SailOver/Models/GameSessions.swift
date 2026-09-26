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
    static func failure(_ key: String) -> Error { SteamError.Failure(reason: key) }
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
    @ObservationIgnored private let core: any StorefrontSessionsProtocol
    @ObservationIgnored private var revision: UInt64 = 0
    @ObservationIgnored private var operation: Task<Void, Never>?
    /// Told once the launch's game runs.
    @ObservationIgnored private var onRunning: (@MainActor () -> Void)?

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
        if let onRunning, active?.running == true {
            self.onRunning = nil
            onRunning()
        }
    }

    /// Admission happens before verification or authentication can suspend.
    /// `run` hands the admitted game to its supervisor and returns once the
    /// session is finished. Cancelling the returned task cancels preparation,
    /// never a live game.
    @discardableResult
    func launch(_ game: GameIdentity,
                run: @escaping @MainActor (GameAdmission) async throws -> Void,
                onRunning: @escaping @MainActor () -> Void = {},
                onCompletion: @escaping @MainActor (Error?) -> Void = { _ in }) -> Task<Void, Never>? {
        guard let admission = core.admit(game: game) else { return nil }
        apply(core.state())
        self.onRunning = onRunning
        let task = Task {
            var failure: Error?
            do {
                try Task.checkCancellation()
                try await withTaskCancellationHandler {
                    try await run(admission)
                } onCancel: { admission.cancel() }
            } catch { failure = Self.translated(error) }
            // An admission never handed off releases its Reservation.
            admission.cancel()
            self.onRunning = nil
            operation = nil
            apply(core.state())
            onCompletion(failure)
        }
        operation = task
        return task
    }

    /// The game's Reservation: no Transfer, uninstall or other launch of it.
    func protects(_ game: Store.GameID?) -> Bool {
        isRecovering || (game != nil && active?.game == game)
    }

    func revoked(_ account: Store.AccountID) -> Bool { revokedAccounts.contains(account) }

    /// Answers the Cloud question `expected` shows. Cancel stops a launch
    /// check still in progress.
    func choose(_ choice: CloudChoice, expected: Session) async throws {
        do { try await core.choose(sessionId: expected.id, decisionId: expected.cloud.decisionId, choice: choice) }
        catch { throw Self.translated(error) }
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
        catch { throw Self.translated(error) }
    }

    func cancelRosetta(_ prompt: RosettaPrompt) {
        guard rosettaPrompt(accountId: prompt.accountId) == prompt else { return }
        core.declineRosetta(sessionId: prompt.id, account: Store.account(prompt.accountId),
                            decisionId: prompt.decisionId)
        apply(core.state())
        // The launch ends as a cancellation, which reports no error.
        operation?.cancel()
    }

    /// Account revocation. Admission checks it at once; the supervisor learns
    /// it as soon as it can be reached.
    func signedOut(account: Store.AccountID) {
        core.revoke(account: Store.account(account))
        apply(core.state())
    }

    /// A session call's failure, reported as the storefront's error.
    private static func translated(_ error: Error) -> Error {
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
