import Foundation
import XCTest
@testable import SailOver

final class SteamCloudModelTests: XCTestCase {
    private let accountID: UInt64 = 0x0110_0001_0000_002a

    @MainActor
    func testCancelledLiveDecisionDoesNotHideSameDecisionInNextSession() async throws {
        let fixture = try Fixture()
        let first = RecoveredGameFixture(accountID: accountID, phase: .preparing, cloudPhase: .conflict, decisionID: 2)
        let second = RecoveredGameFixture(accountID: accountID, phase: .preparing, cloudPhase: .conflict, decisionID: 2)
        let (sessions, model) = (fixture.sessions, fixture.model)
        fixture.connect()
        let firstTask = try XCTUnwrap(sessions.launch(game: 42, account: accountID, prepare: { first }))
        try await eventually { model.prompt?.cloud.decisionId == 2 }
        XCTAssertTrue(model.promptPresented)
        model.respond(.cancel)
        try await eventually { !model.promptBusy }
        XCTAssertFalse(model.promptPresented)
        await first.finish()
        await firstTask.value

        let secondTask = try XCTUnwrap(sessions.launch(game: 42, account: accountID, prepare: { second }))
        try await eventually { model.prompt?.cloud.decisionId == 2 }
        XCTAssertTrue(model.promptPresented, "Decision IDs restart in each supervisor session")
        await second.finish()
        await secondTask.value
        fixture.signOut()
    }

    @MainActor
    func testSecondLiveChoiceIsIgnoredWhileFirstResponseIsInFlight() async throws {
        let fixture = try Fixture()
        let game = RecoveredGameFixture(accountID: accountID, phase: .preparing, cloudPhase: .conflict, decisionID: 2)
        let (sessions, model) = (fixture.sessions, fixture.model)
        fixture.connect()
        let task = try XCTUnwrap(sessions.launch(game: 42, account: accountID, prepare: { game }))
        try await eventually { model.prompt?.cloud.decisionId == 2 }

        model.respond(.local)
        model.respond(.cancel)
        try await eventually { !model.promptBusy }
        let choices = await game.cloudChoices
        XCTAssertEqual(choices.map(\.choice), [.local])
        XCTAssertTrue(model.promptPresented, "An ignored cancellation cannot dismiss the accepted choice")
        await game.finish()
        await task.value
        fixture.signOut()
    }

    @MainActor
    func testConnectingStartsNoCloudWorkAndCreatesNoFiles() throws {
        let fixture = try Fixture()
        let before = try files(in: fixture.root)

        fixture.connect()

        XCTAssertFalse(fixture.model.promptPresented)
        XCTAssertEqual(try files(in: fixture.root), before)
        fixture.signOut()
    }

    @MainActor
    func testIndicatorsBelongOnlyToTheirAccount() async throws {
        let fixture = try Fixture()
        fixture.connect()

        fixture.model.refreshIndicators(for: [42])

        try await eventually { fixture.model.indicator(for: 42)?.state == .neverSynced }
        fixture.connect(accountID + 1)
        XCTAssertNil(fixture.model.indicator(for: 42))
        fixture.signOut()
        fixture.model.refreshIndicators(for: [42])
        try await Task.sleep(for: .milliseconds(50))
        XCTAssertNil(fixture.model.indicator(for: 42), "A signed-out model reads nothing")
    }

    @MainActor
    func testMaintenanceAsksLiveAndRefreshesTheIndicatorWhenItEnds() async throws {
        let fixture = try Fixture(installed: "macos")
        let (model, maintenance) = (fixture.model, fixture.maintenance)
        fixture.connect()

        fixture.installations.syncCloud()

        XCTAssertTrue(model.isMaintaining, "Rust holds the Reservation from the call")
        try await eventually { maintenance.starts.count == 1 }
        XCTAssertEqual(maintenance.starts.last?.steam?.install, fixture.installation("macos").path)
        XCTAssertNil(maintenance.starts.last?.steam?.prefix)
        XCTAssertEqual(maintenance.starts.last?.steam?.root, SailOverLocations.of(library: fixture.root).cloudArchive)
        maintenance.show(.syncing)
        try await eventually { model.isSynchronizing(for: 42) }
        maintenance.askConflict(error: "backend.steam.cloud.conflict")
        try await eventually { model.promptPresented }
        XCTAssertEqual(model.promptGame, 42)
        XCTAssertEqual(model.promptMessage, "backend.steam.cloud.conflict")

        model.respond(.local)

        try await eventually { maintenance.answers == [.local] }
        maintenance.finish()
        try await eventually { !model.isMaintaining }
        XCTAssertFalse(model.promptPresented)
        XCTAssertFalse(model.failurePresented)
        try await eventually { model.indicator(for: 42)?.state == .neverSynced }
    }

    @MainActor
    func testAWindowsInstallationSyncsTheSavesOfItsWinePrefix() async throws {
        let fixture = try Fixture(installed: "windows")
        fixture.connect()

        fixture.installations.syncCloud()

        try await eventually { fixture.maintenance.starts.count == 1 }
        XCTAssertEqual(fixture.maintenance.starts.last?.steam?.install, fixture.installation("windows").path)
        XCTAssertEqual(fixture.maintenance.starts.last?.steam?.prefix,
                       SailOverLocations.of(library: fixture.root).winePrefixes + "/steam-42-windows")
        fixture.maintenance.finish()
        try await eventually { !fixture.model.isMaintaining }
    }

    @MainActor
    func testAFailedMaintenanceShowsNoBannerAndSyncNowStartsTheNextAttempt() async throws {
        let fixture = try Fixture()
        let (model, maintenance) = (fixture.model, fixture.maintenance)
        fixture.connect()
        fixture.installations.syncCloud()
        try await eventually { maintenance.starts.count == 1 }

        maintenance.finish(SteamError.Failure(reason: "backend.steam.cloud.network"))

        try await eventually { !model.isMaintaining }
        XCTAssertFalse(model.failurePresented, "Only a launch check shows a banner")
        XCTAssertEqual(model.promptMessage, L10n.string("ui.cloud.working"))
        fixture.installations.syncCloud()
        try await eventually { maintenance.starts.count == 2 }
        XCTAssertNil(maintenance.starts.last?.steam, "An uninstalled game syncs from its saved context")
        maintenance.finish()
    }

    @MainActor
    func testARejectedMaintenanceAnswerDoesNotOutliveItsAttempt() async throws {
        let fixture = try Fixture()
        let (model, maintenance) = (fixture.model, fixture.maintenance)
        fixture.connect()
        fixture.installations.syncCloud()
        try await eventually { maintenance.starts.count == 1 }
        let question = maintenance.askConflict(error: "backend.steam.cloud.conflict")
        try await eventually { model.promptPresented }

        model.respond(.retry)

        try await eventually { model.promptMessage == L10n.message("backend.steam.runtime.invalidSession") }
        model.respond(.cancel)
        _ = await question.value
        maintenance.finish()
        try await eventually { !model.isMaintaining }
        XCTAssertEqual(maintenance.answers, [nil])
        XCTAssertEqual(model.promptMessage, L10n.string("ui.cloud.working"), "A later question starts without the old error")
    }

    @MainActor
    func testSigningOutCancelsMaintenanceAndItsQuestion() async throws {
        let fixture = try Fixture()
        let (model, maintenance) = (fixture.model, fixture.maintenance)
        fixture.connect()
        fixture.installations.syncCloud()
        try await eventually { maintenance.starts.count == 1 }
        let question = maintenance.askConflict()
        try await eventually { model.promptPresented }

        fixture.signOut()

        XCTAssertFalse(model.isMaintaining)
        XCTAssertFalse(model.promptPresented)
        let answer = await question.value
        XCTAssertNil(answer, "Signing out cancels the sync's question")
        maintenance.finish()
    }

    @MainActor
    func testExitConflictIsPresentedLiveAndNeverReopensByItself() async throws {
        let fixture = try Fixture()
        let game = RecoveredGameFixture(accountID: accountID)
        let (sessions, model) = (fixture.sessions, fixture.model)
        fixture.connect()
        let task = try XCTUnwrap(sessions.launch(game: 42, account: accountID, prepare: { game }))
        try await eventually { sessions.active?.running == true }
        await game.update(phase: .finalizing, cloudPhase: .conflict, cloudError: "backend.steam.cloud.conflict",
                          decisionID: 3, allowedChoices: [.local, .cloud, .cancel])
        try await eventually { model.promptPresented }
        XCTAssertEqual(model.promptGame, 42)
        XCTAssertFalse(model.promptWorking)

        model.respond(.local)
        try await eventually { !model.promptBusy }

        let choices = await game.cloudChoices
        XCTAssertEqual(choices.map(\.choice), [.local])
        await game.update(cloudPhase: .pending, cloudError: "backend.steam.cloud.conflict")
        await game.finish()
        await task.value
        XCTAssertFalse(model.promptPresented, "An unanswered exit conflict waits for the next launch or maintenance")
        fixture.connect()
        XCTAssertFalse(model.promptPresented)
        fixture.signOut()
    }

    private func files(in root: URL) throws -> [String: Data] {
        let enumerator = try XCTUnwrap(FileManager.default.enumerator(at: root, includingPropertiesForKeys: [.isRegularFileKey]))
        var result: [String: Data] = [:]
        for case let file as URL in enumerator where try file.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile == true {
            result[file.path] = try Data(contentsOf: file)
        }
        return result
    }
}

/// One Steam account's installations in Rust, whose Cloud maintenance the
/// test runs, with game 42 installed for `installed` or not at all.
@MainActor
private final class Fixture {
    let root: URL
    let sessions = SteamGameSessions()
    let maintenance = CloudMaintenanceFixture()
    let installations: SteamInstallations
    var model: SteamCloudModel { installations.cloud }
    private let platform: String

    init(installed: String? = nil) throws {
        root = FileManager.default.temporaryDirectory.resolvingSymlinksInPath()
            .appending(path: "SailOverCloudTests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        platform = installed ?? "macos"
        installations = SteamInstallations(directory: root, sessions: sessions, cloud: SteamCloudModel(sessions: sessions))
        if let installed {
            try FileManager.default.createDirectory(at: installation(installed), withIntermediateDirectories: true)
            let receipt = SteamInstallReceipt(appId: 42, name: "Fixture", directory: installation(installed).path,
                                              platform: installed, language: "english", totalBytes: 0)
            for account in [UInt64(0x0110_0001_0000_002a), 0x0110_0001_0000_002b] {
                try JSONEncoder().encode([receipt]).write(to: receipts(account))
            }
        }
    }

    deinit { try? FileManager.default.removeItem(at: root) }

    func installation(_ platform: String) -> URL { root.appending(path: "Games/Steam/steam-42-\(platform)") }
    private func receipts(_ account: UInt64) -> URL { root.appending(path: "steam-installations-\(account).json") }

    /// Connects the account as SteamModel does, game 42 on screen.
    func connect(_ account: UInt64 = 0x0110_0001_0000_002a) {
        let platform = platform
        installations.selection = {
            InstallationSelection(game: 42, platform: platform, installable: true, request: { receipt, directory in
                SteamDownload(appId: 42, name: "Fixture", platform: receipt?.platform ?? platform,
                              language: "english", directory: directory)
            })
        }
        let backend = SteamInstallationsBackend(try! SteamAccountInstallations.withContent(
            content: CloudContent(maintenance),
            accountId: account, sessions: sessions.core, receipts: receipts(account).path,
            locations: .of(library: root)
        ))
        installations.connect(account: account, backend: backend, port: InstallationPort(
            verifiesBeforeLaunch: { false }, uninstallRequest: { _ in nil }, report: { _ in }, warn: { _ in }
        ))
    }

    /// Signs out as SteamModel does.
    func signOut() {
        installations.signedOut()
        model.signedOut()
    }
}

/// A Steam session that only syncs Cloud saves, through the test's fixture.
private final class CloudContent: SteamContent, @unchecked Sendable {
    private let maintenance: CloudMaintenanceFixture
    init(_ maintenance: CloudMaintenanceFixture) { self.maintenance = maintenance }

    func download(appId: UInt32, platform: String, language: String, directory: String) throws -> SteamInstallation {
        throw SteamError.Cancelled
    }
    func downloadProgress() -> GameTransfer? { nil }
    func cancelDownload() {}
    func launchPlan(appId: UInt32, platform: String, directory: String) throws -> SteamLaunchPlan {
        throw SteamError.Failure(reason: "backend.steam.runtime.startFailed")
    }
    func handOff(plan: SteamLaunchPlan) throws -> any GameSupervisor {
        throw GameSessionError.Cancelled
    }
    func archiveCloud(config: SteamCloudConfiguration) throws {}
    func maintainCloud(appId: UInt32, config: SteamCloudConfiguration?, sync: CloudSync) throws {
        let maintain = maintenance.steam
        let done = DispatchSemaphore(value: 0)
        let failure = LockedFailure()
        Task {
            do { try await maintain(appId, config, sync) } catch { failure.set(error) }
            done.signal()
        }
        done.wait()
        if let error = failure.get() { throw error }
    }
}

private final class LockedFailure: @unchecked Sendable {
    private let lock = NSLock()
    private var error: Error?
    func set(_ error: Error) { lock.withLock { self.error = error } }
    func get() -> Error? { lock.withLock { error } }
}
