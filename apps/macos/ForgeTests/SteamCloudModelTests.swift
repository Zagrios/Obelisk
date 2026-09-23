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
    func testConnectingStartsNoCloudWorkAndCreatesNoFiles() throws {
        let directory = try temporaryDirectory()
        let sessions = SteamGameSessions(recoverExisting: false)
        let model = SteamCloudModel(root: directory.path, sessions: sessions)
        let before = try files(in: directory)

        model.connect(credentials())

        XCTAssertFalse(model.promptPresented)
        XCTAssertEqual(try files(in: directory), before)
        model.signedOut()
    }

    @MainActor
    func testIndicatorsBelongOnlyToTheirAccount() async throws {
        let directory = try temporaryDirectory()
        let services = CloudServicesFixture()
        let model = SteamCloudModel(root: directory.path, sessions: SteamGameSessions(recoverExisting: false),
                                    services: services.steam)
        model.connect(credentials())

        model.refreshIndicators(for: [42])

        try await eventually { model.indicator(for: 42)?.state == .synced }
        XCTAssertEqual(services.steamRequests.last?.accountId, accountID)
        XCTAssertEqual(services.steamRequests.last?.root, directory.path)
        model.connect(credentials(accountID + 1))
        XCTAssertNil(model.indicator(for: 42))
        let reads = services.steamRequests.count
        model.signedOut()
        model.refreshIndicators(for: [42])
        XCTAssertNil(model.indicator(for: 42))
        XCTAssertEqual(services.steamRequests.count, reads, "A signed-out model reads nothing")
    }

    @MainActor
    func testMaintenanceAsksLiveAndRefreshesTheIndicatorWhenItEnds() async throws {
        let directory = try temporaryDirectory()
        let services = CloudServicesFixture()
        let model = SteamCloudModel(root: directory.path, sessions: SteamGameSessions(recoverExisting: false),
                                    services: services.steam)
        model.connect(credentials())

        model.startMaintenance(appId: 42, install: "/fixture/steam-42-macos")

        XCTAssertTrue(model.isMaintaining)
        XCTAssertEqual(services.steamMaintenance.last?.request.install, "/fixture/steam-42-macos")
        XCTAssertEqual(services.steamMaintenance.last?.request.root, directory.path)
        XCTAssertEqual(services.steamMaintenance.last?.credentials.steamId, accountID)
        let attempt = try XCTUnwrap(services.steamMaintenance.last?.attempt)
        attempt.publish(.syncing)
        try await eventually { model.isSynchronizing(for: 42) }
        attempt.publish(.conflict, decisionId: 7, choices: [.local, .cloud, .cancel], error: "backend.steam.cloud.conflict")
        try await eventually { model.promptPresented }
        XCTAssertEqual(model.promptAppId, 42)
        XCTAssertEqual(model.promptMessage, "backend.steam.cloud.conflict")

        model.respond(.local)

        XCTAssertEqual(attempt.answers.map(\.0), [7])
        XCTAssertEqual(attempt.answers.map(\.1), [.local])
        attempt.finish(.synced)
        try await eventually { !model.isMaintaining }
        XCTAssertFalse(model.promptPresented)
        XCTAssertFalse(model.failurePresented)
        try await eventually { model.indicator(for: 42)?.state == .synced }
    }

    @MainActor
    func testAFailedMaintenanceShowsNoBannerAndSyncNowStartsTheNextAttempt() async throws {
        let directory = try temporaryDirectory()
        let services = CloudServicesFixture()
        let model = SteamCloudModel(root: directory.path, sessions: SteamGameSessions(recoverExisting: false),
                                    services: services.steam)
        model.connect(credentials())
        model.startMaintenance(appId: 42, install: nil)
        let first = try XCTUnwrap(services.steamMaintenance.last?.attempt)

        first.finish(.failed, error: "backend.steam.cloud.network")

        try await eventually { !model.isMaintaining }
        XCTAssertFalse(model.failurePresented, "Only a launch check shows a banner")
        XCTAssertEqual(model.promptMessage, "ui.cloud.working")
        model.startMaintenance(appId: 42, install: nil)
        XCTAssertEqual(services.steamMaintenance.count, 2, "Sync now is the retry")
        XCTAssertNil(services.steamMaintenance.last?.request.install)
    }

    @MainActor
    func testARejectedMaintenanceAnswerDoesNotOutliveItsAttempt() async throws {
        let directory = try temporaryDirectory()
        let services = CloudServicesFixture()
        let model = SteamCloudModel(root: directory.path, sessions: SteamGameSessions(recoverExisting: false),
                                    services: services.steam)
        model.connect(credentials())
        model.startMaintenance(appId: 42, install: nil)
        let attempt = try XCTUnwrap(services.steamMaintenance.last?.attempt)
        attempt.publish(.conflict, decisionId: 4, choices: [.local, .cancel], error: "backend.steam.cloud.conflict")
        try await eventually { model.promptPresented }

        model.respond(.cloud)

        XCTAssertEqual(model.promptMessage, "backend.steam.runtime.invalidSession")
        attempt.finish(.cancelled)
        try await eventually { !model.isMaintaining }
        XCTAssertEqual(model.promptMessage, "ui.cloud.working", "A later question starts without the old error")
    }

    @MainActor
    func testSigningOutCancelsMaintenanceAndItsQuestion() async throws {
        let directory = try temporaryDirectory()
        let services = CloudServicesFixture()
        let model = SteamCloudModel(root: directory.path, sessions: SteamGameSessions(recoverExisting: false),
                                    services: services.steam)
        model.connect(credentials())
        model.startMaintenance(appId: 42, install: nil)
        let attempt = try XCTUnwrap(services.steamMaintenance.last?.attempt)
        attempt.publish(.conflict, decisionId: 2, choices: [.local, .cloud, .cancel])
        try await eventually { model.promptPresented }

        model.signedOut()

        XCTAssertTrue(attempt.cancelled)
        XCTAssertFalse(model.isMaintaining)
        XCTAssertFalse(model.promptPresented)
    }

    @MainActor
    func testExitConflictIsPresentedLiveAndNeverReopensByItself() async throws {
        let directory = try temporaryDirectory()
        let game = RecoveredGameFixture(accountID: accountID)
        let sessions = SteamGameSessions(startSession: { _, _ in game })
        let model = SteamCloudModel(root: directory.path, sessions: sessions)
        model.connect(credentials())
        let task = try XCTUnwrap(sessions.launch(appId: 42, prepare: { gameLaunch() }))
        try await eventually { sessions.active?.running == true }
        await game.update(phase: .finalizing, cloudPhase: .conflict, cloudError: "backend.steam.cloud.conflict",
                          decisionID: 3, allowedChoices: [.local, .cloud, .cancel])
        try await eventually { model.promptPresented }
        XCTAssertEqual(model.promptAppId, 42)
        XCTAssertFalse(model.promptWorking)

        model.respond(.local)
        try await eventually { !model.promptBusy }

        let choices = await game.cloudChoices
        XCTAssertEqual(choices.map(\.choice), [.local])
        await game.update(cloudPhase: .pending, cloudError: "backend.steam.cloud.conflict")
        await game.finish()
        await task.value
        XCTAssertFalse(model.promptPresented, "An unanswered exit conflict waits for the next launch or maintenance")
        model.connect(credentials())
        XCTAssertFalse(model.promptPresented)
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
