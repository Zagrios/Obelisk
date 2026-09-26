import Foundation
import Observation

/// What one storefront adds to its Game sessions: how its games and accounts
/// are named, and its errors.
protocol GameStorefront {
    associatedtype GameID: Hashable & Sendable
    associatedtype AccountID: Hashable & Sendable
    /// The game and account a supervisor snapshot names, when it is this
    /// storefront's.
    static func identity(of store: GameStore) -> (game: GameID, account: AccountID)?
    static func account(_ account: AccountID) -> GameAccount
    /// A refused or failed session call, as this storefront reports errors.
    static func failure(_ key: String) -> Error
    static var invalidSession: String { get }
    /// Localization key shown while a session is being recovered.
    static var recoveringSession: String { get }
    /// A failed session that reported no reason.
    static var sessionFailed: String { get }
}

enum SteamGames: GameStorefront {
    static func identity(of store: GameStore) -> (game: UInt32, account: UInt64)? {
        guard case let .steam(appId, accountId, _) = store else { return nil }
        return (appId, accountId)
    }
    static func account(_ account: UInt64) -> GameAccount { .steam(steamId: account) }
    static func failure(_ key: String) -> Error { SteamError.Failure(reason: key) }
    static let invalidSession = "backend.steam.runtime.invalidSession"
    static let recoveringSession = "model.steam.recoveringSession"
    static let sessionFailed = "backend.steam.runtime.startFailed"

    @MainActor static let sessions = GameSessions<SteamGames>(recoverExisting: true, recoverSession: recover)

    @MainActor private static func recover() async throws -> (any GameSessionProtocol)? {
        let locations = SailOverLocations.live
        return try await recoverSteamGameSession(sessionDirectory: locations.steamSessions, helper: locations.helper)
    }
}

enum EpicGames: GameStorefront {
    static func identity(of store: GameStore) -> (game: String, account: String)? {
        guard case let .epic(accountId, namespace, appName) = store else { return nil }
        return (epicLibraryID(namespace: namespace, appName: appName), accountId)
    }
    static func account(_ account: String) -> GameAccount { .epic(accountId: account) }
    static func failure(_ key: String) -> Error { EpicError.Service(message: key) }
    static let invalidSession = "backend.epic.cloud.invalidSession"
    static let recoveringSession = "model.epic.recoveringSession"
    static let sessionFailed = "backend.epic.cloud.failed"

    @MainActor static let sessions = GameSessions<EpicGames>(recoverExisting: true, recoverSession: recover)

    @MainActor private static func recover() async throws -> (any GameSessionProtocol)? {
        let locations = SailOverLocations.live
        return try await recoverEpicGameSession(sessionDirectory: locations.epicSessions, helper: locations.helper)
    }
}

typealias SteamGameSessions = GameSessions<SteamGames>
typealias EpicGameSessions = GameSessions<EpicGames>

extension GameSessions where Store == SteamGames {
    static var shared: SteamGameSessions { SteamGames.sessions }
}
extension GameSessions where Store == EpicGames {
    static var shared: EpicGameSessions { EpicGames.sessions }
}

/// A launch's handoff to its supervisor failed. The supervisor may have
/// accepted the game before its reply was lost, so only recovery may release
/// the Reservation.
struct GameHandoffFailure: Error {
    let cause: Error
}

/// Keeps a game's Reservation until its supervisor confirms that the session
/// is finished. Account revocation stops Cloud access without releasing a
/// running game. Only the supervisor owns the game and its processes.
@MainActor @Observable
final class GameSessions<Store: GameStorefront> {
    struct Session: Identifiable, Equatable {
        let id: String
        let game: Store.GameID
        var account: Store.AccountID
        var phase = GamePhase.preparing
        /// The control connection was lost; the game may still run.
        var reconnecting = false
        var cloud = CloudStatus(phase: .idle, decisionId: 0, error: nil, allowedChoices: [])
        var rosetta: RosettaRequest?
        /// Steam's native host.
        var host: SteamHostState?
        /// A supervisor snapshot backs this session, not only its admission.
        var observed = false
        var running: Bool { phase == .running }

        init(id: String = UUID().uuidString, game: Store.GameID, account: Store.AccountID) {
            self.id = id
            self.game = game
            self.account = account
        }

        init(_ snapshot: GameSnapshot, game: Store.GameID, account: Store.AccountID) {
            self.init(id: snapshot.id, game: game, account: account)
            phase = snapshot.phase
            reconnecting = snapshot.reconnecting
            cloud = snapshot.cloud
            rosetta = snapshot.rosetta
            if case let .steam(_, _, host) = snapshot.store { self.host = host }
            observed = true
        }
    }

    private(set) var active: Session?
    /// The last session observed, kept after it ends.
    private(set) var last: Session?
    private(set) var isRecovering = false
    private(set) var recoveryError: String?
    var isBusy: Bool { active != nil || isRecovering }
    private var revokedAccounts: Set<Store.AccountID> = []
    private var cancelledRosetta: RosettaPrompt?
    @ObservationIgnored private var operation: Task<Void, Never>?
    @ObservationIgnored private var client: (any GameSessionProtocol)?
    @ObservationIgnored private var appliedRevocations: Set<Store.AccountID> = []
    @ObservationIgnored private let recoverSession: @MainActor () async throws -> (any GameSessionProtocol)?

    init(recoverExisting: Bool = false,
         recoverSession: @escaping @MainActor () async throws -> (any GameSessionProtocol)? = { nil }) {
        self.recoverSession = recoverSession
        if recoverExisting {
            isRecovering = true
            operation = Task {
                _ = try? await recoverAndObserve(onRunning: {})
                finish()
            }
        }
    }

    /// Admission happens before verification or authentication can suspend.
    /// Cancelling the returned task cancels preparation, never a live game.
    @discardableResult
    func launch(game: Store.GameID, account: Store.AccountID,
                prepare: @escaping @MainActor () async throws -> any GameSessionProtocol,
                onRunning: @escaping @MainActor () -> Void = {},
                onCompletion: @escaping @MainActor (Error?) -> Void = { _ in }) -> Task<Void, Never>? {
        guard !isBusy else { return nil }
        // A newly admitted launch belongs to the currently authenticated
        // account. Revocations from a finished session must not disable it.
        revokedAccounts.removeAll()
        appliedRevocations.removeAll()
        active = Session(game: game, account: account)
        recoveryError = nil
        let task = Task {
            var failure: Error?
            do {
                try Task.checkCancellation()
                let started: any GameSessionProtocol
                do {
                    started = try await prepare()
                } catch let handoff as GameHandoffFailure {
                    // Only recovery may release a Reservation the supervisor
                    // may have accepted.
                    isRecovering = true
                    active?.reconnecting = true
                    if try await recoverAndObserve(onRunning: onRunning) {
                        finish()
                        onCompletion(nil)
                        return
                    }
                    throw handoff.cause
                }
                try await observe(started, begin: true, onRunning: onRunning)
            } catch { failure = error }
            finish()
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
        guard let active, let client, active.id == expected.id,
              active.cloud.decisionId == expected.cloud.decisionId,
              !revokedAccounts.contains(active.account), !active.reconnecting else {
            throw Store.failure(Store.invalidSession)
        }
        if choice == .cancel, active.phase == .preparing, [.checking, .syncing].contains(active.cloud.phase) {
            await Task.detached { client.cancelPreparation() }.value
            return
        }
        guard active.cloud.allowedChoices.contains(choice) else { throw Store.failure(Store.invalidSession) }
        let account = Store.account(active.account)
        try await call { try await client.choose(account: account, decisionId: expected.cloud.decisionId, choice: choice) }
    }

    typealias RosettaPrompt = RosettaLaunchPrompt<String, Store.AccountID>

    func rosettaPrompt(accountId: Store.AccountID) -> RosettaPrompt? {
        guard let active, active.phase == .preparing, active.account == accountId, !active.reconnecting,
              !revokedAccounts.contains(accountId), let request = active.rosetta else { return nil }
        let prompt = RosettaPrompt(id: active.id, accountId: accountId, decisionId: request.decisionId)
        return prompt == cancelledRosetta ? nil : prompt
    }

    func resumeAfterRosetta(_ prompt: RosettaPrompt) async throws {
        guard rosettaPrompt(accountId: prompt.accountId) == prompt, let client else {
            throw Store.failure(Store.invalidSession)
        }
        try Task.checkCancellation()
        let account = Store.account(prompt.accountId)
        try await call { try await client.resumeAfterRosetta(account: account, decisionId: prompt.decisionId) }
    }

    func cancelRosetta(_ prompt: RosettaPrompt) {
        guard rosettaPrompt(accountId: prompt.accountId) == prompt else { return }
        cancelledRosetta = prompt
        // Observation survives cancellation until the supervisor confirms its
        // terminal state. Mark the operation cancelled to suppress a user error.
        operation?.cancel()
    }

    /// Account revocation. Admission checks this synchronously; the network
    /// revocation may wait for an in-flight handoff to return its client.
    func signedOut(account: Store.AccountID) {
        revokedAccounts.insert(account)
        guard let client, active?.account == account else { return }
        Task { try? await applyRevocation(account, client: client) }
    }

    private func applyRevocation(_ account: Store.AccountID, client: any GameSessionProtocol) async throws {
        guard revokedAccounts.contains(account), !appliedRevocations.contains(account) else { return }
        // Sign-out cancels the launch observer, but must still reach its
        // detached supervisor. Success stays tied to this exact client.
        let target = Store.account(account)
        try await call { try await Task.detached { try await client.revoke(account: target) }.value }
        if self.client === client { appliedRevocations.insert(account) }
    }

    /// Native recovery validates process identities and paces unresolved
    /// attempts. No session in reply confirms that none remains.
    private func recoverAndObserve(onRunning: @escaping @MainActor () -> Void) async throws -> Bool {
        while true {
            let recovered: (any GameSessionProtocol)?
            do {
                // Cancellation must not turn unknown process ownership into exit.
                recovered = try await Task { try await self.recoverSession() }.value
            } catch {
                recoveryError = L10n.string(Store.recoveringSession)
                await Task.yield()
                continue
            }
            guard let recovered else {
                recoveryError = nil
                return false
            }
            try await observe(recovered, onRunning: onRunning)
            return true
        }
    }

    private func observe(_ observed: any GameSessionProtocol, begin: Bool = false,
                         onRunning: @escaping @MainActor () -> Void) async throws {
        client = observed
        appliedRevocations.removeAll()
        try await withTaskCancellationHandler {
            // The cancellation handler already forwards a cancelled task.
            if begin && !Task.isCancelled {
                if active.map({ revokedAccounts.contains($0.account) }) == true {
                    await Task.detached { observed.cancelPreparation() }.value
                } else {
                    do { try await observed.begin() }
                    catch {
                        // A lost Begin reply cannot establish whether admission
                        // happened. Keep ownership until a terminal snapshot.
                        active?.reconnecting = true
                    }
                }
            }
            var revision: UInt64?
            var reportedRunning = false
            while true {
                let snapshot: GameSnapshot
                var revocationFailed = false
                do {
                    if let account = active?.account {
                        do { try await applyRevocation(account, client: observed) }
                        catch { revocationFailed = true }
                    }
                    let after = revision
                    snapshot = try await Task.detached { try await observed.snapshot(after: after) }.value
                } catch {
                    // Control loss is uncertainty, not evidence of game exit.
                    if active?.reconnecting != true { active?.reconnecting = true }
                    await Task.yield()
                    continue
                }
                guard let identity = Store.identity(of: snapshot.store) else { throw Store.failure(Store.invalidSession) }
                if !snapshot.finished {
                    do { try await applyRevocation(identity.account, client: observed) }
                    catch { revocationFailed = true }
                }
                revision = snapshot.revision
                var updated = Session(snapshot, game: identity.game, account: identity.account)
                // A failed revocation cannot suppress the snapshot that
                // establishes real exit after a supervisor loses its socket.
                if revocationFailed && !snapshot.finished { updated.reconnecting = true }
                // Repeated snapshots do not invalidate unchanged presentation.
                if active != updated { active = updated }
                if last != updated { last = updated }
                if isRecovering { isRecovering = false }
                if recoveryError != nil { recoveryError = nil }
                if snapshot.finished {
                    if snapshot.phase == .failed {
                        if snapshot.failureReason == .cancelled { throw CancellationError() }
                        throw Store.failure(snapshot.error ?? Store.sessionFailed)
                    }
                    return
                }
                if snapshot.phase == .running && !reportedRunning {
                    reportedRunning = true
                    onRunning()
                }
            }
        } onCancel: {
            Task.detached { observed.cancelPreparation() }
        }
    }

    /// A session call's failure, reported as the storefront's error.
    private func call<T>(_ work: () async throws -> T) async throws -> T {
        do { return try await work() }
        catch let GameSessionError.Failure(reason) { throw Store.failure(reason) }
    }

    private func finish() {
        cancelledRosetta = nil
        active = nil
        client = nil
        appliedRevocations.removeAll()
        isRecovering = false
        recoveryError = nil
        operation = nil
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
