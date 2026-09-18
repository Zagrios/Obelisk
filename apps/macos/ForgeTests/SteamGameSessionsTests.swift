import Foundation
import XCTest
@testable import Forge

final class SteamGameSessionsTests: XCTestCase {
    @MainActor
    func testAdmissionProtectsPreparationSynchronouslyAndFailureNeverStartsOrRecovers() async throws {
        let preparation = GameSessionGate<SteamGameSessions.Launch>()
        var starts = 0
        var recoveries = 0
        var completion: Error?
        let sessions = SteamGameSessions(startSession: { _, _ in
            starts += 1
            return RecoveredGameFixture()
        }, recoverSession: { recoveries += 1; return nil })
        let task = try XCTUnwrap(sessions.launch(appId: 42, prepare: { try await preparation.wait() }, onCompletion: { completion = $0 }))
        XCTAssertTrue(sessions.protects(42))
        XCTAssertFalse(sessions.protects(570))
        assertLaunchRejected(sessions)
        try await eventually { await preparation.requests == 1 }
        assertLaunchRejected(sessions)
        await preparation.fail(SteamError.AuthenticationRequired)
        await task.value
        XCTAssertFalse(sessions.isBusy)
        XCTAssertNotNil(completion)
        XCTAssertEqual(starts, 0)
        XCTAssertEqual(recoveries, 0)
    }

    @MainActor
    func testCancellationBeforePreparationDoesNotCallItsBody() async throws {
        var prepared = false
        let sessions = SteamGameSessions(startSession: { _, _ in
            XCTFail("A cancelled preparation must not start a game")
            throw CancellationError()
        })
        let task = try XCTUnwrap(sessions.launch(appId: 42, prepare: {
            prepared = true
            return gameLaunch()
        }))
        task.cancel()
        await task.value
        XCTAssertFalse(prepared)
        XCTAssertFalse(sessions.isBusy)
    }

    @MainActor
    func testCancellationDuringPreparationRetainsGuardUntilWorkReturnsAndNeverLaunches() async throws {
        let preparation = GameSessionGate<SteamGameSessions.Launch>()
        var starts = 0
        let sessions = SteamGameSessions(startSession: { _, _ in starts += 1; return RecoveredGameFixture() })
        let task = try XCTUnwrap(sessions.launch(appId: 42, prepare: { try await preparation.wait() }))
        try await eventually { await preparation.requests == 1 }
        task.cancel()
        XCTAssertTrue(sessions.protects(42))
        assertLaunchRejected(sessions)
        await preparation.resolve(gameLaunch())
        await task.value
        XCTAssertFalse(sessions.isBusy)
        XCTAssertEqual(starts, 0)
    }

    @MainActor
    func testDefiniteStartFailureReleasesWithoutRecovery() async throws {
        var completion: Error?
        var recovered = false
        let sessions = SteamGameSessions(startSession: { _, _ in throw GameLaunchFailure.notExecutable }, recoverSession: {
            recovered = true
            return nil
        })
        let task = try XCTUnwrap(sessions.launch(appId: 42, prepare: { gameLaunch() }, onCompletion: { completion = $0 }))
        await task.value
        XCTAssertFalse(sessions.isBusy)
        XCTAssertFalse(recovered)
        XCTAssertTrue(completion is GameLaunchFailure)
    }

    @MainActor
    func testLostAcknowledgementAndDelayedRecoveryKeepContinuousGuardsEvenAfterCancellation() async throws {
        let game = RecoveredGameFixture()
        let recovery = GameSessionGate<RecoveredGameFixture?>()
        var starts = 0
        var recoveries = 0
        var completions = 0
        var completion: Error?
        let sessions = SteamGameSessions(startSession: { _, _ in
            starts += 1
            throw SteamGameHandoffFailure(cause: SteamError.Failure(reason: "backend.steam.runtime.unavailable"))
        }, recoverSession: {
            recoveries += 1
            if recoveries == 1 { throw SteamError.Failure(reason: "backend.steam.runtime.unavailable") }
            return try await recovery.wait()
        })
        let task = try XCTUnwrap(sessions.launch(appId: 42, prepare: { gameLaunch() }, onCompletion: {
            completions += 1
            completion = $0
        }))
        XCTAssertTrue(sessions.protects(42))
        assertLaunchRejected(sessions)
        try await eventually { await recovery.requests == 1 }
        XCTAssertTrue(sessions.isRecovering)
        XCTAssertNotNil(sessions.recoveryError)
        XCTAssertTrue(sessions.protects(42))
        XCTAssertTrue(sessions.protects(570))
        assertLaunchRejected(sessions)
        task.cancel()
        XCTAssertTrue(sessions.protects(42))
        await recovery.resolve(game)
        try await eventually { sessions.active?.running == true }
        try await eventually { await game.cancellations == 1 }
        XCTAssertFalse(sessions.isRecovering)
        XCTAssertNil(sessions.recoveryError)
        XCTAssertTrue(sessions.protects(42))
        XCTAssertFalse(sessions.protects(570))
        XCTAssertEqual(completions, 0)
        assertLaunchRejected(sessions)
        await game.finish()
        await task.value
        XCTAssertFalse(sessions.isBusy)
        XCTAssertNil(completion)
        XCTAssertEqual(completions, 1)
        XCTAssertEqual(starts, 1)
        XCTAssertEqual(recoveries, 2)
    }

    @MainActor
    func testEmptyRecoveryReleasesOnlyAfterAbsenceIsConfirmedAndReportsOriginalFailure() async throws {
        let recovery = GameSessionGate<RecoveredGameFixture?>()
        var completion: Error?
        let sessions = SteamGameSessions(startSession: { _, _ in
            throw SteamGameHandoffFailure(cause: GameLaunchFailure.notExecutable)
        }, recoverSession: { try await recovery.wait() })
        let task = try XCTUnwrap(sessions.launch(appId: 42, prepare: { gameLaunch() }, onCompletion: { completion = $0 }))
        try await eventually { await recovery.requests == 1 }
        XCTAssertTrue(sessions.protects(42))
        XCTAssertTrue(sessions.isRecovering)
        await recovery.resolve(nil)
        await task.value
        XCTAssertFalse(sessions.isBusy)
        XCTAssertFalse(sessions.protects(42))
        XCTAssertTrue(completion is GameLaunchFailure)
    }

    @MainActor
    func testStartupRecoveryRetriesAndProtectsBeforeFirstAwaitUntilRecoveredGameExits() async throws {
        let game = RecoveredGameFixture()
        let recovery = GameSessionGate<RecoveredGameFixture?>()
        var attempts = 0
        let sessions = SteamGameSessions(recoverExisting: true, recoverSession: {
            attempts += 1
            if attempts == 1 { throw SteamError.Failure(reason: "backend.steam.runtime.unavailable") }
            return try await recovery.wait()
        })
        XCTAssertTrue(sessions.isRecovering)
        XCTAssertTrue(sessions.protects(42))
        XCTAssertTrue(sessions.protects(570))
        assertLaunchRejected(sessions)
        try await eventually { await recovery.requests == 1 }
        XCTAssertTrue(sessions.protects(42))
        await recovery.resolve(game)
        try await eventually { sessions.active?.running == true }
        XCTAssertEqual(attempts, 2)
        XCTAssertNil(sessions.recoveryError)
        XCTAssertFalse(sessions.isRecovering)
        XCTAssertTrue(sessions.protects(42))
        XCTAssertFalse(sessions.protects(570))
        await game.finish()
        try await eventually { !sessions.isBusy }
    }

    @MainActor
    func testControlFailureAndCancellationPreserveLastStateUntilConfirmedNonzeroExit() async throws {
        let game = RecoveredGameFixture()
        var completion: Error?
        var completed = false
        let sessions = SteamGameSessions(startSession: { _, _ in game })
        let task = try XCTUnwrap(sessions.launch(appId: 42, prepare: { gameLaunch() }, onCompletion: {
            completed = true
            completion = $0
        }))
        try await eventually { sessions.active?.running == true }
        await game.failObservation()
        try await eventually { sessions.active?.connection == .reconnecting }
        XCTAssertEqual(sessions.active?.phase, .running)
        XCTAssertEqual(sessions.active?.host, .online)
        XCTAssertTrue(sessions.protects(42))
        task.cancel()
        try await eventually { await game.cancellations == 1 }
        XCTAssertFalse(completed)
        assertLaunchRejected(sessions)
        await game.update(host: .lost, connection: .connected)
        try await eventually { sessions.active?.host == .lost }
        XCTAssertTrue(sessions.protects(42))
        await game.finish(exitCode: 7)
        await task.value
        XCTAssertTrue(completed)
        let error = try XCTUnwrap(completion)
        guard case let SteamError.Failure(reason) = error else { return XCTFail("Expected the terminal game error") }
        XCTAssertEqual(reason, "model.steam.gameExited")
        XCTAssertFalse(sessions.isBusy)
    }

    @MainActor
    func testCompletedLaunchHandleAndOldSessionCannotAffectNextGame() async throws {
        let first = RecoveredGameFixture()
        let second = RecoveredGameFixture(appID: 570)
        let sessions = SteamGameSessions(startSession: { plan, _ in plan.appId == 42 ? first : second })
        let oldTask = try XCTUnwrap(sessions.launch(appId: 42, prepare: { gameLaunch() }))
        try await eventually { sessions.active?.running == true }
        await first.finish()
        await oldTask.value
        let newTask = try XCTUnwrap(sessions.launch(appId: 570, prepare: { gameLaunch(570) }))
        try await eventually { sessions.active?.running == true }
        oldTask.cancel()
        await first.update(host: .lost)
        XCTAssertEqual(sessions.active?.appId, 570)
        XCTAssertEqual(sessions.active?.host, .online)
        XCTAssertTrue(sessions.protects(570))
        assertLaunchRejected(sessions)
        await second.finish()
        await newTask.value
        let cancellations = await second.cancellations
        XCTAssertEqual(cancellations, 0)
        XCTAssertFalse(sessions.isBusy)
    }

    @MainActor
    func testCancellationWhileHandoffReturnsSessionStillObservesItUntilExit() async throws {
        let handoff = GameSessionGate<RecoveredGameFixture>()
        let game = RecoveredGameFixture()
        let sessions = SteamGameSessions(startSession: { _, _ in try await handoff.wait() })
        let task = try XCTUnwrap(sessions.launch(appId: 42, prepare: { gameLaunch() }))
        try await eventually { await handoff.requests == 1 }
        task.cancel()
        XCTAssertTrue(sessions.protects(42))
        await handoff.resolve(game)
        try await eventually { sessions.active?.running == true }
        try await eventually { await game.cancellations == 1 }
        XCTAssertTrue(sessions.protects(42))
        assertLaunchRejected(sessions)
        await game.finish()
        await task.value
        XCTAssertFalse(sessions.isBusy)
    }

    @MainActor
    private func assertLaunchRejected(_ sessions: SteamGameSessions, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertNil(sessions.launch(appId: 570, prepare: {
            XCTFail("A rejected launch must not prepare", file: file, line: line)
            throw CancellationError()
        }), file: file, line: line)
    }

    @MainActor
    private func eventually(_ predicate: @MainActor () async -> Bool) async throws {
        let deadline = ContinuousClock.now + .seconds(3)
        while !(await predicate()) && ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(10)) }
        let satisfied = await predicate()
        XCTAssertTrue(satisfied)
        if !satisfied { throw SteamError.Failure(reason: "Fixture timed out") }
    }
}

func gameLaunch(_ appId: UInt32 = 42) -> SteamGameSessions.Launch {
    SteamGameSessions.Launch(
        plan: SteamLaunchPlan(appId: appId, platform: "macos", target: LaunchTarget(executable: "/fixture/game", arguments: [], workingDirectory: "/fixture", isApplicationBundle: false)),
        credentials: SteamCredentials(accountName: "Fixture", steamId: 42, refreshToken: "fixture-only-never-sent")
    )
}

/// Uses the production snapshot interface; tests cannot mutate the session owner.
actor RecoveredGameFixture: SteamGameSessionProtocol {
    nonisolated let sessionID = UUID().uuidString
    private var current: SteamGameSnapshot
    private var observationFailure = false
    private var waiters: [CheckedContinuation<Void, Never>] = []
    private(set) var cancellations = 0

    init(appID: UInt32 = 42) {
        current = SteamGameSnapshot(id: sessionID, appId: appID, phase: .running, connection: .connected,
                                    host: .online, exitCode: nil, error: nil, revision: 1)
    }

    nonisolated func cancelPreparation() { Task { await recordCancellation() } }
    private func recordCancellation() { cancellations += 1 }

    func snapshot(after: UInt64?) async throws -> SteamGameSnapshot {
        try Task.checkCancellation()
        while after == current.revision && !current.finished && !observationFailure {
            await withCheckedContinuation { waiters.append($0) }
        }
        if observationFailure {
            observationFailure = false
            throw SteamError.Failure(reason: "backend.steam.runtime.unavailable")
        }
        return current
    }

    func update(host: SteamHostState? = nil, connection: SteamSupervisorConnection? = nil) {
        current = SteamGameSnapshot(id: current.id, appId: current.appId, phase: current.phase,
                                    connection: connection ?? current.connection, host: host ?? current.host,
                                    exitCode: current.exitCode, error: current.error, revision: current.revision + 1)
        wake()
    }

    func failObservation() { observationFailure = true; wake() }

    func finish(exitCode: Int32 = 0) {
        current = SteamGameSnapshot(id: current.id, appId: current.appId, phase: .finished, connection: .connected,
                                    host: current.host, exitCode: exitCode, error: nil, revision: current.revision + 1)
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
