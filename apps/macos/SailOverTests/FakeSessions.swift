import Foundation
import XCTest
@testable import SailOver

/// A storefront's Game sessions as Rust would publish them, driven by the test.
/// Rust's own rules are tested in Rust; this only shows states and records the
/// calls Swift makes.
final class FakeSessions: StorefrontSessionsProtocol, @unchecked Sendable {
    struct RosettaCall: Equatable, Sendable {
        let session: String
        let account: GameAccount
        let decisionID: UInt64
    }

    private let lock = NSLock()
    private var current: GameSessionsState
    private var waiters: [(after: UInt64, continuation: CheckedContinuation<GameSessionsState, Never>)] = []
    private var _rosettaChecks: [RosettaCall] = []
    private var _rosettaResumes: [RosettaCall] = []
    private var _declines: [RosettaCall] = []
    private var _revocations: [GameAccount] = []
    private var _stops = 0
    /// Resuming after Rosetta fails with this key until the test clears it.
    private var rosettaFailure: String?
    private var rosettaFailuresRemaining = 0

    var rosettaChecks: [RosettaCall] { lock.withLock { _rosettaChecks } }
    var rosettaResumes: [RosettaCall] { lock.withLock { _rosettaResumes } }
    var declines: [RosettaCall] { lock.withLock { _declines } }
    var revocations: [GameAccount] { lock.withLock { _revocations } }
    var stops: Int { lock.withLock { _stops } }

    init(recovering: Bool = false) {
        current = GameSessionsState(revision: 1, active: nil, last: nil, recovering: recovering,
                                    recoveryFailed: false, revoked: [], rosetta: nil)
    }

    /// Publishes a change, as Rust does after every one.
    func publish(_ change: (inout GameSessionsState) -> Void) {
        let (state, woken) = lock.withLock {
            change(&current)
            current.revision += 1
            let woken = waiters.filter { $0.after < current.revision }
            waiters.removeAll { $0.after < current.revision }
            return (current, woken)
        }
        for waiter in woken { waiter.continuation.resume(returning: state) }
    }

    /// The session holding the Reservation, as its supervisor last showed it.
    func show(_ session: GameSessionState?) {
        publish { state in
            state.active = session
            if let session { state.last = session }
            state.recovering = false
        }
    }

    /// The active session's supervisor asks for Rosetta with `decisionID`, or
    /// no longer does.
    func requireRosetta(decisionID: UInt64?) {
        publish { state in
            state.rosetta = decisionID.flatMap { decision in
                state.active.map { RosettaPromptState(sessionId: $0.id, account: $0.game.account, decisionId: decision) }
            }
        }
    }

    /// Every resumption fails with `key` until it is nil again.
    func failRosetta(_ key: String?) { lock.withLock { rosettaFailure = key } }

    /// The next `count` resumptions find Rosetta still unavailable.
    func failNextRosettaResumes(_ count: Int) { lock.withLock { rosettaFailuresRemaining = count } }

    func state() -> GameSessionsState { lock.withLock { current } }

    func changed(after: UInt64) async -> GameSessionsState {
        await withCheckedContinuation { continuation in
            let ready: GameSessionsState? = lock.withLock {
                if current.revision > after { return current }
                waiters.append((after, continuation))
                return nil
            }
            if let ready { continuation.resume(returning: ready) }
        }
    }

    func declineRosetta(sessionId: String, account: GameAccount, decisionId: UInt64) {
        lock.withLock { _declines.append(.init(session: sessionId, account: account, decisionID: decisionId)) }
        publish { $0.rosetta = nil }
    }

    func resumeAfterRosetta(sessionId: String, account: GameAccount, decisionId: UInt64) async throws {
        let call = RosettaCall(session: sessionId, account: account, decisionID: decisionId)
        let failure: String? = lock.withLock {
            _rosettaChecks.append(call)
            if let rosettaFailure { return rosettaFailure }
            if rosettaFailuresRemaining > 0 {
                rosettaFailuresRemaining -= 1
                return SteamGameSessions.rosettaUnavailableKey
            }
            _rosettaResumes.append(call)
            return nil
        }
        if let failure { throw GameSessionError.Failure(reason: failure) }
    }

    func revoke(account: GameAccount) {
        lock.withLock { _revocations.append(account) }
        publish { if !$0.revoked.contains(account) { $0.revoked.append(account) } }
    }

    func stop() async throws { lock.withLock { _stops += 1 } }
}

extension GameIdentity {
    var account: GameAccount {
        switch self {
        case let .steam(_, accountId): .steam(steamId: accountId)
        case let .epic(accountId, _, _): .epic(accountId: accountId)
        }
    }
}

extension GameSessionState {
    /// A Steam game's session.
    static func steam(appID: UInt32 = 42, accountID: UInt64 = 42, id: String = UUID().uuidString,
                      phase: GamePhase = .running, cloud: CloudStatus = .idle, observed: Bool = true) -> Self {
        GameSessionState(id: id, game: .steam(appId: appID, accountId: accountID), phase: phase, reconnecting: false,
                         cloud: cloud, host: .online, observed: observed, runtimeInstall: nil)
    }

    /// An Epic game's session.
    static func epic(account: String, namespace: String = "namespace", appName: String = "game",
                     id: String = UUID().uuidString, phase: GamePhase = .running, cloud: CloudStatus = .idle,
                     observed: Bool = true) -> Self {
        GameSessionState(id: id, game: .epic(accountId: account, namespace: namespace, appName: appName),
                         phase: phase, reconnecting: false, cloud: cloud, host: nil, observed: observed,
                         runtimeInstall: nil)
    }
}

extension CloudStatus {
    static let idle = CloudStatus(phase: .idle, decisionId: 0, error: nil, allowedChoices: [])

    /// A question the sync waits on: a conflict, or a failed launch check.
    static func asking(_ phase: CloudPhase, decision: UInt64, error: String? = nil) -> CloudStatus {
        let choices: [CloudChoice] = switch phase {
        case .conflict: [.local, .cloud, .cancel]
        case .failed: [.retry, .cancel]
        default: []
        }
        return CloudStatus(phase: phase, decisionId: decision, error: error, allowedChoices: choices)
    }

    static func showing(_ phase: CloudPhase, error: String? = nil) -> CloudStatus {
        CloudStatus(phase: phase, decisionId: 0, error: error, allowedChoices: [])
    }
}

extension GameSessions {
    /// Game sessions whose state the test publishes through `fake`.
    convenience init() {
        self.init(FakeSessions())
    }

    var fake: FakeSessions { core as! FakeSessions }
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
