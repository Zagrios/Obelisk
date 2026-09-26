import Foundation
import XCTest
@testable import SailOver

/// One Game session observer for every storefront, through the production
/// session interface. Steam fixtures stand in; Epic differs only in names.
final class GameSessionsTests: XCTestCase {
    @MainActor
    func testAdmissionProtectsPreparationSynchronouslyAndFailureNeverRecovers() async throws {
        let preparation = GameSessionGate<RecoveredGameFixture>()
        var recoveries = 0
        var completion: Error?
        let sessions = SteamGameSessions(recoverSession: { recoveries += 1; return nil })
        let task = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { try await preparation.wait() },
                                                 onCompletion: { completion = $0 }))
        XCTAssertTrue(sessions.protects(42))
        XCTAssertFalse(sessions.protects(570))
        assertLaunchRejected(sessions)
        try await eventually { await preparation.requests == 1 }
        assertLaunchRejected(sessions)
        await preparation.fail(SteamError.AuthenticationRequired)
        await task.value
        XCTAssertFalse(sessions.isBusy)
        XCTAssertNotNil(completion)
        XCTAssertEqual(recoveries, 0, "Only a failed handoff may have left a game to recover")
    }

    @MainActor
    func testCancellationBeforePreparationDoesNotCallItsBody() async throws {
        var prepared = false
        let sessions = SteamGameSessions()
        let task = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: {
            prepared = true
            return RecoveredGameFixture()
        }))
        task.cancel()
        await task.value
        XCTAssertFalse(prepared)
        XCTAssertFalse(sessions.isBusy)
    }

    @MainActor
    func testDefiniteStartFailureReleasesWithoutRecovery() async throws {
        var completion: Error?
        var recovered = false
        let sessions = SteamGameSessions(recoverSession: {
            recovered = true
            return nil
        })
        let task = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { throw GameLaunchFailure.windowsRuntimeUnavailable },
                                                 onCompletion: { completion = $0 }))
        await task.value
        XCTAssertFalse(sessions.isBusy)
        XCTAssertFalse(recovered)
        XCTAssertTrue(completion is GameLaunchFailure)
    }

    @MainActor
    func testLostAcknowledgementAndDelayedRecoveryKeepContinuousGuardsEvenAfterCancellation() async throws {
        let game = RecoveredGameFixture()
        let recovery = GameSessionGate<RecoveredGameFixture?>()
        var recoveries = 0
        var completions = 0
        var completion: Error?
        let sessions = SteamGameSessions(recoverSession: {
            recoveries += 1
            if recoveries == 1 { throw SteamError.Failure(reason: "backend.steam.runtime.unavailable") }
            return try await recovery.wait()
        })
        let task = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: {
            throw GameHandoffFailure(cause: SteamError.Failure(reason: "backend.steam.runtime.unavailable"))
        }, onCompletion: {
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
        XCTAssertEqual(recoveries, 2)
    }

    @MainActor
    func testEmptyRecoveryReleasesOnlyAfterAbsenceIsConfirmedAndReportsOriginalFailure() async throws {
        let recovery = GameSessionGate<RecoveredGameFixture?>()
        var completion: Error?
        let sessions = SteamGameSessions(recoverSession: { try await recovery.wait() })
        let task = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: {
            throw GameHandoffFailure(cause: GameLaunchFailure.windowsRuntimeUnavailable)
        }, onCompletion: { completion = $0 }))
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
    func testARecoveredSessionThatFailedLeavesNoLastingError() async throws {
        let game = RecoveredGameFixture()
        let sessions = SteamGameSessions(recoverExisting: true, recoverSession: { game })
        try await eventually { sessions.active?.running == true }

        await game.fail(reason: "backend.steam.runtime.startFailed")

        try await eventually { !sessions.isBusy }
        XCTAssertNil(sessions.recoveryError, "A recovered game's end is not a lasting message")
    }

    @MainActor
    func testControlFailureAndCancellationPreserveLastStateUntilConfirmedNonzeroExit() async throws {
        let game = RecoveredGameFixture()
        var completion: Error?
        var completed = false
        let sessions = SteamGameSessions()
        let task = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { game }, onCompletion: {
            completed = true
            completion = $0
        }))
        try await eventually { sessions.active?.running == true }
        await game.failObservation()
        try await eventually { sessions.active?.reconnecting == true }
        XCTAssertEqual(sessions.active?.phase, .running)
        XCTAssertEqual(sessions.active?.host, .online)
        XCTAssertNil(sessions.recoveryError, "Losing the control socket is only uncertainty")
        XCTAssertTrue(sessions.protects(42))
        task.cancel()
        try await eventually { await game.cancellations == 1 }
        XCTAssertFalse(completed)
        assertLaunchRejected(sessions)
        await game.update(host: .lost, reconnecting: false)
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
    func testTheReservationHoldsUntilTheSupervisorOwnsNoProcess() async throws {
        let game = RecoveredGameFixture()
        let sessions = SteamGameSessions()
        let task = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { game }))
        try await eventually { sessions.active?.running == true }

        await game.finish(owningProcess: true)
        try await eventually { sessions.active?.phase == .finished }

        XCTAssertTrue(sessions.protects(42), "A final phase alone does not end the session")
        assertLaunchRejected(sessions)
        await game.releaseProcess()
        await task.value
        XCTAssertFalse(sessions.isBusy)
    }

    @MainActor
    func testCompletedLaunchHandleAndOldSessionCannotAffectNextGame() async throws {
        let first = RecoveredGameFixture()
        let second = RecoveredGameFixture(appID: 570)
        let sessions = SteamGameSessions()
        let oldTask = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { first }))
        try await eventually { sessions.active?.running == true }
        await first.finish()
        await oldTask.value
        let newTask = try XCTUnwrap(sessions.launch(game: 570, account: 42, prepare: { second }))
        try await eventually { sessions.active?.running == true }
        oldTask.cancel()
        await first.update(host: .lost)
        XCTAssertEqual(sessions.active?.game, 570)
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
        let sessions = SteamGameSessions()
        let task = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { try await handoff.wait() }))
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
    func testEveryLaunchOpensItsSupervisorsStartGateUnlessItsAccountSignedOut() async throws {
        let game = RecoveredGameFixture(phase: .preparing)
        let sessions = SteamGameSessions()
        let task = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { game }))
        try await eventually { await game.begins == 1 }
        await game.finish()
        await task.value

        let handoff = GameSessionGate<RecoveredGameFixture>()
        let revoked = RecoveredGameFixture(phase: .preparing)
        let second = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { try await handoff.wait() }))
        try await eventually { await handoff.requests == 1 }
        sessions.signedOut(account: 42)
        await handoff.resolve(revoked)
        try await eventually { await revoked.cancellations == 1 }
        let begins = await revoked.begins
        XCTAssertEqual(begins, 0, "A signed-out account's launch never begins")
        await revoked.fail(reason: nil, cancelled: true)
        await second.value
        XCTAssertFalse(sessions.isBusy)
    }

    @MainActor
    func testALostBeginReplyKeepsTheReservationUntilATerminalSnapshot() async throws {
        let game = RecoveredGameFixture(phase: .preparing)
        await game.failBegin(true)
        let sessions = SteamGameSessions()
        let task = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { game }))
        try await eventually { await game.begins == 1 }
        try await eventually { sessions.active?.observed == true }
        XCTAssertTrue(sessions.protects(42))
        await game.finish()
        await task.value
        XCTAssertFalse(sessions.isBusy)
    }

    @MainActor
    func testSignOutDuringHandoffRevokesBeforeExposingCloudDecision() async throws {
        let handoff = GameSessionGate<RecoveredGameFixture>()
        let game = RecoveredGameFixture(phase: .preparing, cloudPhase: .conflict, decisionID: 17)
        let sessions = SteamGameSessions()
        let task = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { try await handoff.wait() }))
        try await eventually { await handoff.requests == 1 }

        sessions.signedOut(account: 42)
        task.cancel()
        XCTAssertTrue(sessions.protects(42))
        await handoff.resolve(game)
        try await eventually { await game.revocations == [.steam(steamId: 42)] }
        try await eventually { sessions.active?.cloud.phase == .conflict }
        XCTAssertTrue(sessions.revoked(42))
        XCTAssertNil(sessions.rosettaPrompt(accountId: 42))
        XCTAssertTrue(sessions.protects(42))

        await game.finish()
        await task.value
        XCTAssertFalse(sessions.isBusy)
        let revocations = await game.revocations
        XCTAssertEqual(revocations, [.steam(steamId: 42)])
    }

    @MainActor
    func testAFailedRevocationStillObservesTheRealExit() async throws {
        final class FailingRevocation: GameSessionProtocol, @unchecked Sendable {
            let game: RecoveredGameFixture
            init(_ game: RecoveredGameFixture) { self.game = game }
            func begin() async throws { try await game.begin() }
            func cancelPreparation() { game.cancelPreparation() }
            func choose(account: GameAccount, decisionId: UInt64, choice: CloudChoice) async throws {}
            func resumeAfterRosetta(account: GameAccount, decisionId: UInt64) async throws {}
            func revoke(account: GameAccount) async throws {
                throw GameSessionError.Failure(reason: "backend.steam.runtime.unavailable")
            }
            func snapshot(after: UInt64?) async throws -> GameSnapshot { try await game.snapshot(after: after) }
        }
        let game = RecoveredGameFixture()
        let sessions = SteamGameSessions()
        let task = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { FailingRevocation(game) }))
        try await eventually { sessions.active?.running == true }

        sessions.signedOut(account: 42)
        await game.update(host: .online)
        try await eventually { sessions.active?.reconnecting == true }
        XCTAssertEqual(sessions.active?.phase, .running, "A failed revocation never ends the game")

        await game.finish()
        await task.value
        XCTAssertFalse(sessions.isBusy)
    }

    @MainActor
    func testCloudChoiceAnswersOnlyTheQuestionItWasShownAndOnlyForItsAccount() async throws {
        let game = RecoveredGameFixture(phase: .preparing, cloudPhase: .conflict, decisionID: 17)
        let sessions = SteamGameSessions()
        let task = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { game }))
        try await eventually { sessions.active?.cloud.decisionId == 17 && sessions.active?.observed == true }
        let shown = try XCTUnwrap(sessions.active)
        var stale = shown
        stale.cloud.decisionId = 16

        do {
            try await sessions.choose(.local, expected: stale)
            XCTFail("An answer to an older question must be refused")
        } catch {}
        do {
            try await sessions.choose(.retry, expected: shown)
            XCTFail("A choice the question does not offer must be refused")
        } catch {}
        let rejectedChoices = await game.cloudChoices
        XCTAssertTrue(rejectedChoices.isEmpty)

        try await sessions.choose(.cloud, expected: shown)
        let choices = await game.cloudChoices
        XCTAssertEqual(choices, [.init(accountID: 42, decisionID: 17, choice: .cloud)])
        sessions.signedOut(account: 42)
        do {
            try await sessions.choose(.local, expected: shown)
            XCTFail("A revoked account must not answer the Cloud decision")
        } catch {}
        let choicesAfterSignOut = await game.cloudChoices
        XCTAssertEqual(choicesAfterSignOut, choices)

        await game.finish()
        await task.value
    }

    @MainActor
    func testExitConflictIsAskedWhileFinalizingAndAnsweredToTheSupervisor() async throws {
        let game = RecoveredGameFixture()
        let sessions = SteamGameSessions()
        let task = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { game }))
        try await eventually { sessions.active?.running == true }
        await game.update(phase: .finalizing, cloudPhase: .conflict, cloudError: "backend.steam.cloud.conflict",
                          decisionID: 5)
        try await eventually { sessions.active?.cloud.decisionId == 5 }

        try await sessions.choose(.cancel, expected: try XCTUnwrap(sessions.active))

        let choices = await game.cloudChoices
        XCTAssertEqual(choices, [.init(accountID: 42, decisionID: 5, choice: .cancel)])
        let cancellations = await game.cancellations
        XCTAssertEqual(cancellations, 0, "An exit question never cancels the finished game's session")
        await game.finish()
        await task.value
    }

    @MainActor
    func testCancellingACloudCheckOrSyncCancelsPreparationWithoutInventingDecision() async throws {
        for phase in [CloudPhase.checking, .syncing] {
            let game = RecoveredGameFixture(phase: .preparing, cloudPhase: phase)
            let sessions = SteamGameSessions()
            let task = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { game }))
            try await eventually { sessions.active?.cloud.phase == phase }

            try await sessions.choose(.cancel, expected: try XCTUnwrap(sessions.active))
            try await eventually { await game.cancellations == 1 }
            let choices = await game.cloudChoices
            XCTAssertTrue(choices.isEmpty)
            XCTAssertTrue(sessions.protects(42))
            await game.finish()
            await task.value
        }
    }

    @MainActor
    func testFinalizingProtectsInstallationUntilFinishedAndRetainsPendingCloudStatus() async throws {
        let game = RecoveredGameFixture()
        let sessions = SteamGameSessions()
        let task = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { game }))
        try await eventually { sessions.active?.running == true }
        await game.update(phase: .finalizing, cloudPhase: .syncing)
        try await eventually { sessions.active?.phase == .finalizing }

        XCTAssertTrue(sessions.isBusy)
        XCTAssertTrue(sessions.protects(42))
        XCTAssertFalse(sessions.active?.running ?? true)
        assertLaunchRejected(sessions)

        await game.update(cloudPhase: .pending, cloudError: "backend.steam.cloud.network")
        await game.finish()
        await task.value
        XCTAssertFalse(sessions.isBusy)
        XCTAssertFalse(sessions.protects(42))
        XCTAssertEqual(sessions.last?.game, 42)
        XCTAssertEqual(sessions.last?.cloud.phase, .pending)
        XCTAssertEqual(sessions.last?.cloud.error, "backend.steam.cloud.network")
    }

    @MainActor
    func testEpicSessionsNameGamesByLibraryIdentityAndReportEpicErrors() async throws {
        let game = RecoveredGameFixture(epicAccount: "account", namespace: "ns", appName: "app")
        let sessions = EpicGameSessions()
        let libraryID = epicLibraryID(namespace: "ns", appName: "app")
        var completion: Error?
        let task = try XCTUnwrap(sessions.launch(game: libraryID, account: "account", prepare: { game },
                                                 onCompletion: { completion = $0 }))
        try await eventually { sessions.active?.running == true }
        XCTAssertTrue(sessions.protects(libraryID))
        XCTAssertFalse(sessions.protects(epicLibraryID(namespace: "ns", appName: "other")))
        XCTAssertNil(sessions.active?.host)

        await game.fail(reason: nil)
        await task.value
        guard case let EpicError.Service(message) = try XCTUnwrap(completion) else {
            return XCTFail("Expected an Epic error")
        }
        XCTAssertEqual(message, "backend.epic.cloud.failed")
    }

    @MainActor
    private func assertLaunchRejected(_ sessions: SteamGameSessions, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertNil(sessions.launch(game: 570, account: 42, prepare: {
            XCTFail("A rejected launch must not prepare", file: file, line: line)
            throw CancellationError()
        }), file: file, line: line)
    }
}
