import Foundation
import XCTest
@testable import SailOver

/// A Game session supervisor driven by the test. Rust's Game sessions follow
/// it through the production supervisor interface: tests cannot mutate the
/// session owner directly.
final class RecoveredGameFixture: GameSupervisor, @unchecked Sendable {
    struct CloudDecision: Equatable, Sendable {
        let account: GameAccount
        let decisionID: UInt64
        let choice: CloudChoice

        init(account: GameAccount, decisionID: UInt64, choice: CloudChoice) {
            self.account = account
            self.decisionID = decisionID
            self.choice = choice
        }
        init(accountID: UInt64, decisionID: UInt64, choice: CloudChoice) {
            self.init(account: .steam(steamId: accountID), decisionID: decisionID, choice: choice)
        }
    }

    struct RosettaDecision: Equatable, Sendable {
        let account: GameAccount
        let decisionID: UInt64

        init(account: GameAccount, decisionID: UInt64) {
            self.account = account
            self.decisionID = decisionID
        }
        init(accountID: UInt64, decisionID: UInt64) {
            self.init(account: .steam(steamId: accountID), decisionID: decisionID)
        }
    }

    let sessionID = UUID().uuidString
    private let condition = NSCondition()
    private var current: GameSnapshot
    /// A final phase with a process still owned: not finished yet.
    private var ownsProcess = false
    private var observationFailure = false
    private var beginFailure = false
    private var _begins = 0
    private var _cancellations = 0
    private var _revocations: [GameAccount] = []
    private var _cloudChoices: [CloudDecision] = []
    private var _rosettaResumes: [RosettaDecision] = []
    private var _rosettaChecks: [RosettaDecision] = []
    private var rosettaResumeFailure = false
    private var rosettaFailuresRemaining = 0
    private var rosettaError: String?

    var begins: Int { locked { _begins } }
    var cancellations: Int { locked { _cancellations } }
    var revocations: [GameAccount] { locked { _revocations } }
    var cloudChoices: [CloudDecision] { locked { _cloudChoices } }
    var rosettaResumes: [RosettaDecision] { locked { _rosettaResumes } }
    var rosettaChecks: [RosettaDecision] { locked { _rosettaChecks } }

    /// A Steam game.
    convenience init(appID: UInt32 = 42, accountID: UInt64 = 42, phase: GamePhase = .running,
                     cloudPhase: CloudPhase = .idle, decisionID: UInt64 = 0) {
        self.init(store: .steam(appId: appID, accountId: accountID, host: .online), phase: phase,
                  cloudPhase: cloudPhase, decisionID: decisionID)
    }

    /// An Epic game.
    convenience init(epicAccount: String, namespace: String = "namespace", appName: String = "game",
                     phase: GamePhase = .running, cloudPhase: CloudPhase = .idle, decisionID: UInt64 = 0) {
        self.init(store: .epic(accountId: epicAccount, namespace: namespace, appName: appName), phase: phase,
                  cloudPhase: cloudPhase, decisionID: decisionID)
    }

    private init(store: GameStore, phase: GamePhase, cloudPhase: CloudPhase, decisionID: UInt64) {
        current = GameSnapshot(id: sessionID, phase: phase, finished: false, failureReason: nil,
                               error: nil, revision: 1, reconnecting: false, rosetta: nil,
                               cloud: CloudStatus(phase: cloudPhase, decisionId: decisionID, error: nil,
                                                  allowedChoices: Self.choices(cloudPhase)),
                               store: store)
        current.finished = [.finished, .failed].contains(phase)
    }

    /// The choices the supervisor offers with a Cloud question.
    static func choices(_ phase: CloudPhase) -> [CloudChoice] {
        switch phase {
        case .conflict: [.local, .cloud, .cancel]
        case .failed: [.retry, .cancel]
        default: []
        }
    }

    private func locked<T>(_ body: () throws -> T) rethrows -> T {
        condition.lock()
        defer { condition.unlock() }
        return try body()
    }

    /// Changes the snapshot and wakes every waiting observer.
    private func change(_ body: () -> Void) {
        locked {
            body()
            current.revision += 1
            condition.broadcast()
        }
    }

    func begin() throws {
        try locked {
            _begins += 1
            if beginFailure { throw GameSessionError.Failure(reason: "backend.epic.cloud.session") }
        }
    }

    func cancelPreparation() { locked { _cancellations += 1 } }

    func choose(account: GameAccount, decisionId: UInt64, choice: CloudChoice) throws {
        locked { _cloudChoices.append(.init(account: account, decisionID: decisionId, choice: choice)) }
    }

    func revoke(account: GameAccount) throws { locked { _revocations.append(account) } }

    func resumeAfterRosetta(account: GameAccount, decisionId: UInt64) throws {
        try locked {
            _rosettaChecks.append(.init(account: account, decisionID: decisionId))
            if let rosettaError { throw GameSessionError.Failure(reason: rosettaError) }
            if rosettaResumeFailure || rosettaFailuresRemaining > 0 {
                rosettaFailuresRemaining = max(0, rosettaFailuresRemaining - 1)
                throw GameSessionError.Failure(reason: SteamGameSessions.rosettaUnavailableKey)
            }
            _rosettaResumes.append(.init(account: account, decisionID: decisionId))
        }
    }

    func failBegin(_ fail: Bool) { locked { beginFailure = fail } }
    func failRosettaResume(_ fail: Bool) { locked { rosettaResumeFailure = fail } }
    func failNextRosettaResumes(_ count: Int) { locked { rosettaFailuresRemaining = count } }
    func failRosettaWith(_ error: String) { locked { rosettaError = error } }

    func requireRosetta(decisionId: UInt64?) {
        change { current.rosetta = decisionId.map { RosettaRequest(decisionId: $0) } }
    }

    func snapshot(after: UInt64?) throws -> GameSnapshot {
        try locked {
            while after == current.revision && !current.finished && !observationFailure { condition.wait() }
            if observationFailure {
                observationFailure = false
                throw GameSessionError.Failure(reason: "backend.steam.runtime.unavailable")
            }
            return current
        }
    }

    func update(host: SteamHostState? = nil, reconnecting: Bool? = nil,
                phase: GamePhase? = nil, cloudPhase: CloudPhase? = nil, cloudError: String? = nil,
                decisionID: UInt64? = nil, allowedChoices: [CloudChoice]? = nil) {
        change {
            if let host, case let .steam(appId, accountId, _) = current.store {
                current.store = .steam(appId: appId, accountId: accountId, host: host)
            }
            if let reconnecting { current.reconnecting = reconnecting }
            if let phase { current.phase = phase }
            if let cloudPhase { current.cloud.phase = cloudPhase }
            if let cloudError { current.cloud.error = cloudError }
            if let decisionID { current.cloud.decisionId = decisionID }
            current.cloud.allowedChoices = allowedChoices ?? Self.choices(current.cloud.phase)
        }
    }

    func failObservation() { change { observationFailure = true } }

    /// The game exited; `owningProcess` keeps a process the supervisor still owns.
    func finish(exitCode: Int32 = 0, owningProcess: Bool = false) {
        change {
            current.phase = .finished
            current.reconnecting = false
            current.error = nil
            ownsProcess = owningProcess
            current.finished = !owningProcess
        }
    }

    /// The supervisor confirms that the last owned process is gone.
    func releaseProcess() {
        change {
            ownsProcess = false
            current.finished = [.finished, .failed].contains(current.phase)
        }
    }

    func fail(reason: String?, cancelled: Bool = false) {
        change {
            current.phase = .failed
            current.error = reason
            current.failureReason = cancelled ? .cancelled : .failed
            current.finished = !ownsProcess
        }
    }
}

/// Recovers through a test's closure, which runs on the main actor while the
/// Rust recovery waits for it. Without `recoverExisting`, startup finds no
/// session and only a lost handoff asks the closure.
private final class ClosureRecovery: GameRecovery, @unchecked Sendable {
    private let lock = NSLock()
    private var startup: Bool
    private let body: @MainActor () async throws -> (any GameSupervisor)?

    init(recoverExisting: Bool, _ body: @escaping @MainActor () async throws -> (any GameSupervisor)?) {
        startup = !recoverExisting
        self.body = body
    }

    func recover() throws -> (any GameSupervisor)? {
        let skip = lock.withLock {
            defer { startup = false }
            return startup
        }
        if skip { return nil }
        let result = RecoveryResult()
        let semaphore = DispatchSemaphore(value: 0)
        let body = body
        Task { @MainActor in
            do { result.set(.success(try await body())) }
            catch { result.set(.failure(error)) }
            semaphore.signal()
        }
        semaphore.wait()
        do { return try result.get() }
        catch let error as GameSessionError { throw error }
        catch { throw GameSessionError.Failure(reason: "\(error)") }
    }
}

private final class RecoveryResult: @unchecked Sendable {
    private let lock = NSLock()
    private var value: Result<(any GameSupervisor)?, Error> = .success(nil)
    func set(_ value: Result<(any GameSupervisor)?, Error>) { lock.withLock { self.value = value } }
    func get() throws -> (any GameSupervisor)? { try lock.withLock { value }.get() }
}

/// Runs `body` to its end for a caller on a Rust thread, which may block.
func awaitBlocking<T: Sendable>(_ body: @escaping @Sendable () async throws -> T) throws -> T {
    let box = BlockingResult<T>()
    let semaphore = DispatchSemaphore(value: 0)
    Task {
        let result: Result<T, Error>
        do { result = .success(try await body()) } catch { result = .failure(error) }
        box.lock.withLock { box.value = result }
        semaphore.signal()
    }
    semaphore.wait()
    do { return try box.lock.withLock { box.value! }.get() }
    catch { throw sessionError(error) }
}

private final class BlockingResult<T>: @unchecked Sendable {
    let lock = NSLock()
    var value: Result<T, Error>?
}

/// How tests name a storefront's game for an admission.
protocol TestGames: GameStorefront {
    static func identity(game: GameID, account: AccountID) -> GameIdentity
}
extension SteamGames: TestGames {
    static func identity(game: UInt32, account: UInt64) -> GameIdentity { .steam(appId: game, accountId: account) }
}
extension EpicGames: TestGames {
    /// Test library identities are `namespace:appName`.
    static func identity(game: String, account: String) -> GameIdentity {
        let parts = game.split(separator: ":", maxSplits: 1).map(String.init)
        return .epic(accountId: account, namespace: parts.first ?? game, appName: parts.count > 1 ? parts[1] : "")
    }
}

extension GameSessions where Store: TestGames {
    /// Rust's Game sessions recovering through `recoverSession`. Without
    /// `recoverExisting`, they are ready at once.
    convenience init(recoverExisting: Bool = false,
                     recoverSession: @escaping @MainActor () async throws -> (any GameSupervisor)? = { nil }) {
        let core = StorefrontSessions.withRecovery(
            storefront: Store.storefront,
            recovery: ClosureRecovery(recoverExisting: recoverExisting, recoverSession)
        )
        if !recoverExisting { while core.state().recovering { usleep(1000) } }
        self.init(core)
    }

    /// Launches `game` behind the supervisor `prepare` returns, which already
    /// accepted it.
    @discardableResult
    func launch(game: Store.GameID, account: Store.AccountID,
                prepare: @escaping @MainActor () async throws -> any GameSupervisor,
                onRunning: @escaping @MainActor () -> Void = {},
                onCompletion: @escaping @MainActor (Error?) -> Void = { _ in }) -> Task<Void, Never>? {
        launch(Store.identity(game: game, account: account), run: { admission in
            try await admission.launch(handoff: TestHandoff(prepare), beforeStart: nil)
        }, onRunning: onRunning, onCompletion: onCompletion)
    }
}

extension GameAdmission {
    /// Follows a supervisor that already accepted the game.
    func follow(supervisor: any GameSupervisor) async throws {
        try await launch(handoff: TestHandoff { supervisor }, beforeStart: nil)
    }
}

/// A handoff a test performs on the main actor while Rust waits for it.
final class TestHandoff: GameHandoff, @unchecked Sendable {
    private let body: @MainActor () async throws -> any GameSupervisor

    init(_ body: @escaping @MainActor () async throws -> any GameSupervisor) { self.body = body }

    func handOff() throws -> any GameSupervisor {
        let body = body
        return try awaitBlocking { try await body() }
    }
}

/// A test's error as the localization key Rust carries back.
func sessionError(_ error: Error) -> GameSessionError {
    switch error {
    case let error as GameSessionError: error
    case is CancellationError: .Cancelled
    case let SteamError.Failure(reason): .Failure(reason: reason)
    case let EpicError.Service(message): .Failure(reason: message)
    case EpicError.Network: .Failure(reason: "backend.epic.network")
    case EpicError.Unauthorized: .Failure(reason: "backend.epic.unauthorized")
    default: .Failure(reason: "\(error)")
    }
}

actor GameSessionGate<Value: Sendable> {
    private var result: Result<Value, Error>?
    private var waiters: [CheckedContinuation<Value, Error>] = []
    private(set) var requests = 0

    func wait() async throws -> Value {
        requests += 1
        if let result { return try result.get() }
        return try await withCheckedThrowingContinuation { waiters.append($0) }
    }

    func resolve(_ value: Value) { complete(.success(value)) }
    func fail(_ error: Error) { complete(.failure(error)) }

    private func complete(_ value: Result<Value, Error>) {
        result = value
        let pending = waiters
        waiters.removeAll()
        for waiter in pending { waiter.resume(with: value) }
    }
}

/// Waits until `predicate` holds, or fails the test after three seconds.
@MainActor
func eventually(_ predicate: @MainActor () async -> Bool, file: StaticString = #filePath,
                line: UInt = #line) async throws {
    let deadline = ContinuousClock.now + .seconds(3)
    while !(await predicate()) && ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(10)) }
    let satisfied = await predicate()
    if !satisfied {
        XCTFail("Fixture timed out", file: file, line: line)
        throw SteamError.Failure(reason: "Fixture timed out")
    }
}
