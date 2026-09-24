import Foundation
import XCTest
@testable import Forge

/// A Game session supervisor driven by the test, through the production
/// session interface: tests cannot mutate the session owner directly.
actor RecoveredGameFixture: GameSessionProtocol {
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

    nonisolated let sessionID = UUID().uuidString
    private var current: GameSnapshot
    /// A final phase with a process still owned: not finished yet.
    private var ownsProcess = false
    private var observationFailure = false
    private var beginFailure = false
    private var waiters: [CheckedContinuation<Void, Never>] = []
    private(set) var begins = 0
    private(set) var cancellations = 0
    private(set) var revocations: [GameAccount] = []
    private(set) var cloudChoices: [CloudDecision] = []
    private(set) var rosettaResumes: [RosettaDecision] = []
    private(set) var rosettaChecks: [RosettaDecision] = []
    private var rosettaResumeFailure = false
    private var rosettaFailuresRemaining = 0
    private var rosettaError: String?

    /// A Steam game.
    init(appID: UInt32 = 42, accountID: UInt64 = 42, phase: GamePhase = .running,
         cloudPhase: CloudPhase = .idle, decisionID: UInt64 = 0) {
        self.init(store: .steam(appId: appID, accountId: accountID, host: .online), phase: phase,
                  cloudPhase: cloudPhase, decisionID: decisionID)
    }

    /// An Epic game.
    init(epicAccount: String, namespace: String = "namespace", appName: String = "game",
         phase: GamePhase = .running, cloudPhase: CloudPhase = .idle, decisionID: UInt64 = 0) {
        self.init(store: .epic(accountId: epicAccount, namespace: namespace, appName: appName), phase: phase,
                  cloudPhase: cloudPhase, decisionID: decisionID)
    }

    private init(store: GameStore, phase: GamePhase, cloudPhase: CloudPhase, decisionID: UInt64) {
        current = GameSnapshot(id: sessionID, phase: phase, finished: false, failureReason: nil, exitCode: nil,
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

    func begin() async throws {
        begins += 1
        if beginFailure { throw GameSessionError.Failure(reason: "backend.epic.cloud.session") }
    }

    nonisolated func cancelPreparation() { Task { await recordCancellation() } }
    private func recordCancellation() { cancellations += 1 }

    func choose(account: GameAccount, decisionId: UInt64, choice: CloudChoice) async throws {
        cloudChoices.append(.init(account: account, decisionID: decisionId, choice: choice))
    }

    func revoke(account: GameAccount) async throws { revocations.append(account) }

    func resumeAfterRosetta(account: GameAccount, decisionId: UInt64) async throws {
        rosettaChecks.append(.init(account: account, decisionID: decisionId))
        if let rosettaError { throw GameSessionError.Failure(reason: rosettaError) }
        if rosettaResumeFailure || rosettaFailuresRemaining > 0 {
            rosettaFailuresRemaining = max(0, rosettaFailuresRemaining - 1)
            throw GameSessionError.Failure(reason: SteamGameSessions.rosettaUnavailableKey)
        }
        rosettaResumes.append(.init(account: account, decisionID: decisionId))
    }

    func failBegin(_ fail: Bool) { beginFailure = fail }
    func failRosettaResume(_ fail: Bool) { rosettaResumeFailure = fail }
    func failNextRosettaResumes(_ count: Int) { rosettaFailuresRemaining = count }
    func failRosettaWith(_ error: String) { rosettaError = error }

    func requireRosetta(decisionId: UInt64?) {
        current.rosetta = decisionId.map { RosettaRequest(decisionId: $0) }
        current.revision += 1
        wake()
    }

    func snapshot(after: UInt64?) async throws -> GameSnapshot {
        try Task.checkCancellation()
        while after == current.revision && !current.finished && !observationFailure {
            await withCheckedContinuation { waiters.append($0) }
        }
        if observationFailure {
            observationFailure = false
            throw GameSessionError.Failure(reason: "backend.steam.runtime.unavailable")
        }
        return current
    }

    func update(host: SteamHostState? = nil, reconnecting: Bool? = nil,
                phase: GamePhase? = nil, cloudPhase: CloudPhase? = nil, cloudError: String? = nil,
                decisionID: UInt64? = nil, allowedChoices: [CloudChoice]? = nil) {
        if let host, case let .steam(appId, accountId, _) = current.store {
            current.store = .steam(appId: appId, accountId: accountId, host: host)
        }
        if let reconnecting { current.reconnecting = reconnecting }
        if let phase { current.phase = phase }
        if let cloudPhase { current.cloud.phase = cloudPhase }
        if let cloudError { current.cloud.error = cloudError }
        if let decisionID { current.cloud.decisionId = decisionID }
        current.cloud.allowedChoices = allowedChoices ?? Self.choices(current.cloud.phase)
        current.revision += 1
        wake()
    }

    func failObservation() { observationFailure = true; wake() }

    /// The game exited; `owningProcess` keeps a process the supervisor still owns.
    func finish(exitCode: Int32 = 0, owningProcess: Bool = false) {
        current.phase = .finished
        current.reconnecting = false
        current.exitCode = exitCode
        current.error = nil
        ownsProcess = owningProcess
        current.finished = !owningProcess
        current.revision += 1
        wake()
    }

    /// The supervisor confirms that the last owned process is gone.
    func releaseProcess() {
        ownsProcess = false
        current.finished = [.finished, .failed].contains(current.phase)
        current.revision += 1
        wake()
    }

    func fail(reason: String?, cancelled: Bool = false) {
        current.phase = .failed
        current.error = reason
        current.failureReason = cancelled ? .cancelled : .failed
        current.exitCode = nil
        current.finished = !ownsProcess
        current.revision += 1
        wake()
    }

    private func wake() {
        let pending = waiters
        waiters.removeAll()
        for waiter in pending { waiter.resume() }
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
