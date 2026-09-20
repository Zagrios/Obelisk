import Foundation
import XCTest
@testable import Forge

final class SteamCloudModelTests: XCTestCase {
    private let accountID: UInt64 = 0x0110_0001_0000_002a

    @MainActor
    func testCancelledLiveDecisionDoesNotHideSameDecisionInNextSession() async throws {
        let directory = try temporaryDirectory()
        let first = RecoveredGameFixture(accountID: accountID, phase: .preparing, cloudPhase: .conflict, decisionID: 2)
        let second = RecoveredGameFixture(accountID: accountID, phase: .preparing, cloudPhase: .conflict, decisionID: 2)
        var starts = 0
        let sessions = SteamGameSessions(startSession: { _, _ in
            starts += 1
            return starts == 1 ? first : second
        })
        let model = SteamCloudModel(root: directory.path, sessions: sessions)
        model.connect(credentials())
        let firstTask = try XCTUnwrap(sessions.launch(appId: 42, prepare: { gameLaunch() }))
        try await eventually { sessions.cloudPrompt?.cloud.decisionId == 2 }
        XCTAssertTrue(model.promptPresented)
        model.respond(.cancel)
        try await eventually { !model.promptBusy }
        XCTAssertFalse(model.promptPresented)
        await first.finish()
        await firstTask.value

        let secondTask = try XCTUnwrap(sessions.launch(appId: 42, prepare: { gameLaunch() }))
        try await eventually { sessions.cloudPrompt?.cloud.decisionId == 2 }
        XCTAssertTrue(model.promptPresented, "Decision IDs restart in each supervisor session")
        await second.finish()
        await secondTask.value
        model.signedOut()
    }

    @MainActor
    func testDismissedPostGameConflictDoesNotBlockOtherGamesMaintenance() async throws {
        let directory = try temporaryDirectory()
        let game = RecoveredGameFixture(accountID: accountID)
        let sessions = SteamGameSessions(startSession: { _, _ in game })
        let maintenance = CloudMaintenanceFixture(results: [
            SteamCloudPending(appId: 570, phase: .synced, error: nil, decisionToken: "done")
        ])
        let model = SteamCloudModel(root: directory.path, sessions: sessions, makeMaintenance: { _ in maintenance })
        model.connect(credentials())
        let task = try XCTUnwrap(sessions.launch(appId: 42, prepare: { gameLaunch() }))
        try await eventually { sessions.active?.running == true }
        await game.update(phase: .finalizing, cloudPhase: .conflict,
                          cloudError: "backend.steam.cloud.conflict", decisionToken: "kept-conflict")
        await game.finish()
        await task.value
        model.resume()
        XCTAssertTrue(model.promptPresented)
        model.respond(.cancel)
        try FileManager.default.createDirectory(at: directory.appending(path: String(accountID)),
                                                withIntermediateDirectories: true)

        model.resume()
        try await eventually { await maintenance.resumeCalls == 1 }
        try await eventually { !model.isBusy }
        XCTAssertFalse(model.promptPresented, "A dismissed conflict waits for an explicit retry")
        XCTAssertEqual(model.pending.map(\.appId), [42])
        let exclusions = await maintenance.excludedApps
        XCTAssertEqual(exclusions, [[42]])
        model.resume()
        try await eventually { await maintenance.resumeCalls == 2 }
        try await eventually { !model.isBusy }
        XCTAssertFalse(model.promptPresented)
        model.retryPending(appId: 42)
        XCTAssertTrue(model.promptPresented)
        model.signedOut()
    }

    @MainActor
    func testSecondLiveChoiceIsIgnoredWhileFirstResponseIsInFlight() async throws {
        let directory = try temporaryDirectory()
        let game = RecoveredGameFixture(accountID: accountID, phase: .preparing, cloudPhase: .conflict, decisionID: 2)
        let sessions = SteamGameSessions(startSession: { _, _ in game })
        let model = SteamCloudModel(root: directory.path, sessions: sessions)
        model.connect(credentials())
        let task = try XCTUnwrap(sessions.launch(appId: 42, prepare: { gameLaunch() }))
        try await eventually { sessions.cloudPrompt?.cloud.decisionId == 2 }

        model.respond(.local)
        model.respond(.cancel)
        try await eventually { !model.promptBusy }
        let choices = await game.cloudChoices
        XCTAssertEqual(choices.map(\.choice), [.local])
        XCTAssertTrue(model.promptPresented, "An ignored cancellation cannot dismiss the accepted choice")
        await game.finish()
        await task.value
        model.signedOut()
    }

    @MainActor
    func testCancellingFailedResolutionDoesNotImmediatelyRetry() async throws {
        let directory = try temporaryDirectory()
        try FileManager.default.createDirectory(at: directory.appending(path: String(accountID)),
                                                withIntermediateDirectories: true)
        let sessions = SteamGameSessions(recoverExisting: false)
        let maintenance = CloudMaintenanceFixture(results: [
            SteamCloudPending(appId: 42, phase: .conflict, error: nil, decisionToken: "unresolved")
        ])
        let model = SteamCloudModel(root: directory.path, sessions: sessions, makeMaintenance: { _ in maintenance })
        model.connect(credentials())
        try await eventually { !model.isBusy }
        model.respond(.local)
        try await eventually { !model.isBusy }
        XCTAssertTrue(model.failurePresented)

        model.respond(.cancel)
        XCTAssertFalse(model.isBusy, "Dismissing a failed request must not immediately issue it again")
        XCTAssertFalse(model.failurePresented)
        let calls = await maintenance.resumeCalls
        XCTAssertEqual(calls, 1)
        model.signedOut()
    }

    @MainActor
    func testSigningInAgainCanPresentSameUnresolvedPostGameConflict() async throws {
        let directory = try temporaryDirectory()
        let game = RecoveredGameFixture(accountID: accountID)
        let sessions = SteamGameSessions(startSession: { _, _ in game })
        let model = SteamCloudModel(root: directory.path, sessions: sessions)
        model.connect(credentials())
        let task = try XCTUnwrap(sessions.launch(appId: 42, prepare: { gameLaunch() }))
        try await eventually { sessions.active?.running == true }
        await game.update(phase: .finalizing, cloudPhase: .conflict,
                          cloudError: "backend.steam.cloud.conflict", decisionToken: "same-token")
        await game.finish()
        await task.value
        model.resume()
        model.respond(.cancel)
        XCTAssertFalse(model.promptPresented)

        model.connect(credentials())
        XCTAssertTrue(model.promptPresented, "Account reset must clear previously presented tokens")
        model.signedOut()
    }

    @MainActor
    func testConnectWithoutAccountDataDoesNotStartMaintenanceOrCreateFiles() throws {
        let directory = try temporaryDirectory()
        let sessions = SteamGameSessions(recoverExisting: false)
        let model = SteamCloudModel(root: directory.path, sessions: sessions)
        let before = try files(in: directory)

        model.connect(credentials())
        model.resume()

        XCTAssertFalse(model.isBusy)
        XCTAssertTrue(model.pending.isEmpty)
        XCTAssertFalse(model.promptPresented)
        XCTAssertEqual(try files(in: directory), before)
        model.signedOut()
    }

    @MainActor
    func testTerminalPendingIndicatorBelongsOnlyToItsAccount() async throws {
        let directory = try temporaryDirectory()
        let game = RecoveredGameFixture(accountID: accountID)
        let sessions = SteamGameSessions(startSession: { _, _ in game })
        let task = try XCTUnwrap(sessions.launch(appId: 42, prepare: { gameLaunch() }))
        try await eventually { sessions.active?.running == true }
        await game.update(cloudPhase: .pending, cloudError: "backend.steam.cloud.network")
        await game.finish()
        await task.value
        let model = SteamCloudModel(root: directory.path, sessions: sessions)

        model.connect(credentials())
        XCTAssertTrue(model.hasPending(for: 42))
        XCTAssertNotNil(model.message(for: 42))
        model.connect(credentials(accountID + 1))
        XCTAssertFalse(model.hasPending(for: 42))
        XCTAssertNil(model.message(for: 42))
        model.signedOut()
        XCTAssertFalse(model.hasPending(for: 42))
        XCTAssertNil(model.message(for: 42))
    }

    @MainActor
    func testPostGameConflictCanBeDismissedBeforeHandoffAndRemainsAccountScoped() async throws {
        let directory = try temporaryDirectory()
        let game = RecoveredGameFixture(accountID: accountID)
        let sessions = SteamGameSessions(startSession: { _, _ in game })
        let model = SteamCloudModel(root: directory.path, sessions: sessions)
        model.connect(credentials())
        let task = try XCTUnwrap(sessions.launch(appId: 42, prepare: { gameLaunch() }))
        try await eventually { sessions.active?.running == true }
        await game.update(phase: .finalizing, cloudPhase: .conflict,
                          cloudError: "backend.steam.cloud.conflict", decisionToken: "finalizing-token")
        try await eventually { model.promptPresented }
        XCTAssertEqual(model.promptAppId, 42)
        XCTAssertTrue(model.promptWorking)
        model.respond(.local)
        XCTAssertFalse(model.isBusy, "A choice must not access the archive before the supervisor releases it")
        model.respond(.cancel)
        XCTAssertFalse(model.promptPresented)
        model.retryPending(appId: 42)
        XCTAssertTrue(model.promptPresented)
        XCTAssertTrue(model.promptWorking)
        model.respond(.local)
        XCTAssertFalse(model.isBusy)
        model.respond(.cancel)
        await game.finish()
        await task.value
        model.resume()
        XCTAssertFalse(model.promptPresented)
        XCTAssertFalse(model.isBusy)
        XCTAssertTrue(model.hasPending(for: 42))
        model.retryPending(appId: 42)
        XCTAssertTrue(model.promptPresented)
        XCTAssertFalse(model.promptWorking)
        model.connect(credentials(accountID + 1))
        XCTAssertFalse(model.promptPresented)
        XCTAssertNil(model.promptAppId)
        model.signedOut()
    }

    @MainActor
    func testFailedCapturedConflictChoiceCanRetryMaintenance() async throws {
        let directory = try temporaryDirectory()
        let game = RecoveredGameFixture(accountID: accountID)
        let sessions = SteamGameSessions(startSession: { _, _ in game })
        let model = SteamCloudModel(root: directory.path, sessions: sessions)
        model.connect(credentials())
        let task = try XCTUnwrap(sessions.launch(appId: 42, prepare: { gameLaunch() }))
        try await eventually { sessions.active?.running == true }
        await game.update(phase: .finalizing, cloudPhase: .conflict,
                          cloudError: "backend.steam.cloud.conflict", decisionToken: "missing-context-token")
        await game.finish()
        await task.value
        model.resume()
        model.respond(.local)
        try await eventually { !model.isBusy }
        XCTAssertTrue(model.failurePresented, "The fixture has no durable context to resolve")

        try FileManager.default.createDirectory(at: directory.appending(path: String(accountID)),
                                                withIntermediateDirectories: true)
        model.respond(.retry)
        XCTAssertTrue(model.isBusy, "The captured conflict must not suppress an explicit retry after failure")
        try await eventually { !model.isBusy }
        XCTAssertFalse(model.promptPresented)
        XCTAssertFalse(model.failurePresented)
        model.signedOut()
    }

    private func credentials(_ id: UInt64? = nil) -> SteamCredentials {
        SteamCredentials(accountName: "Local fixture", steamId: id ?? accountID, refreshToken: "fixture-only-never-sent")
    }

    private func temporaryDirectory() throws -> URL {
        let directory = FileManager.default.temporaryDirectory.resolvingSymlinksInPath()
            .appending(path: "ForgeCloudTests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: directory) }
        return directory
    }

    private func files(in root: URL) throws -> [String: Data] {
        let enumerator = try XCTUnwrap(FileManager.default.enumerator(at: root, includingPropertiesForKeys: [.isRegularFileKey]))
        var result: [String: Data] = [:]
        for case let file as URL in enumerator where try file.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile == true {
            result[file.path] = try Data(contentsOf: file)
        }
        return result
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

private actor CloudMaintenanceFixture: SteamCloudMaintenanceProtocol {
    private let results: [SteamCloudPending]
    private(set) var resumeCalls = 0
    private(set) var excludedApps: [[UInt32]] = []

    init(results: [SteamCloudPending]) { self.results = results }
    nonisolated func cancel() {}

    func resume(root: String, sessionDirectory: String, helper: String, excludedAppIds: [UInt32]) async throws -> [SteamCloudPending] {
        resumeCalls += 1
        excludedApps.append(excludedAppIds)
        return results
    }

    func resolve(root: String, appId: UInt32, choice: SteamCloudChoice, decisionToken: String) async throws -> SteamCloudPending {
        throw SteamError.Failure(reason: "Unexpected fixture resolution")
    }
}
