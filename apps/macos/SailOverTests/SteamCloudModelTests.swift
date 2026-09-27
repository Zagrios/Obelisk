import Foundation
import XCTest
@testable import SailOver

final class SteamCloudModelTests: XCTestCase {
    private let accountID: UInt64 = 0x0110_0001_0000_002a

    @MainActor
    func testCancelledLiveDecisionDoesNotHideSameDecisionInNextSession() async throws {
        let fixture = Fixture()
        let model = fixture.model
        fixture.connect()
        fixture.sessions.fake.show(.steam(accountID: accountID, phase: .preparing,
                                          cloud: .asking(.conflict, decision: 2)))
        try await eventually { model.prompt?.cloud.decisionId == 2 }
        XCTAssertTrue(model.promptPresented)

        model.respond(.cancel)
        try await eventually { !model.promptBusy }
        XCTAssertFalse(model.promptPresented)
        XCTAssertEqual(fixture.backend.actions, [.answer(42, decision: 2, .cancel)])

        fixture.sessions.fake.show(.steam(accountID: accountID, phase: .preparing,
                                          cloud: .asking(.conflict, decision: 2)))
        try await eventually { model.promptPresented }
        XCTAssertEqual(model.prompt?.cloud.decisionId, 2, "Decision IDs restart in each supervisor session")
        fixture.signOut()
    }

    @MainActor
    func testSecondLiveChoiceIsIgnoredWhileFirstResponseIsInFlight() async throws {
        let fixture = Fixture()
        let model = fixture.model
        let answer = GameSessionGate<Void>()
        fixture.connect()
        fixture.backend.answering = { _, _, _ in try await answer.wait() }
        fixture.sessions.fake.show(.steam(accountID: accountID, phase: .preparing,
                                          cloud: .asking(.conflict, decision: 2)))
        try await eventually { model.prompt?.cloud.decisionId == 2 }

        model.respond(.local)
        model.respond(.cancel)
        await answer.resolve(())
        try await eventually { !model.promptBusy }

        XCTAssertEqual(fixture.backend.actions, [.answer(42, decision: 2, .local)])
        XCTAssertTrue(model.promptPresented, "An ignored cancellation cannot dismiss the accepted choice")
        fixture.signOut()
    }

    @MainActor
    func testIndicatorsBelongOnlyToTheirAccount() async throws {
        let fixture = Fixture()
        fixture.connect()
        fixture.backend.indicators = [(42, .neverSynced)]

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
        let fixture = Fixture(installed: true)
        let model = fixture.model
        fixture.connect()
        let backend = fixture.backend
        backend.onCall = { call in
            if case .maintain = call { backend.publish { $0.maintenance = .init(game: 42, cloud: .showing(.checking)) } }
        }

        fixture.installations.syncCloud()

        XCTAssertTrue(model.isMaintaining, "Rust holds the Reservation from the call")
        XCTAssertEqual(backend.actions, [.maintain(42, installed: fixture.receipt)])
        backend.publish { $0.maintenance?.cloud = .showing(.syncing) }
        try await eventually { model.isSynchronizing(for: 42) }
        backend.publish {
            $0.maintenance?.cloud = .asking(.conflict, decision: 1, error: "backend.steam.cloud.conflict")
        }
        try await eventually { model.promptPresented }
        XCTAssertEqual(model.promptGame, 42)
        XCTAssertEqual(model.promptMessage, "backend.steam.cloud.conflict")

        model.respond(.local)

        try await eventually { backend.actions.last == .answer(42, decision: 1, .local) }
        backend.indicators = [(42, .neverSynced)]
        backend.publish { $0.maintenance = nil }
        try await eventually { !model.isMaintaining }
        XCTAssertFalse(model.promptPresented)
        XCTAssertFalse(model.failurePresented)
        try await eventually { model.indicator(for: 42)?.state == .neverSynced }
    }

    @MainActor
    func testAFailedMaintenanceShowsNoBannerAndSyncNowStartsTheNextAttempt() async throws {
        let fixture = Fixture()
        fixture.connect()
        let (model, backend) = (fixture.model, fixture.backend)
        fixture.installations.syncCloud()
        backend.publish { $0.maintenance = .init(game: 42, cloud: .showing(.syncing)) }
        try await eventually { model.isMaintaining }

        backend.publish { $0.maintenance = nil }

        try await eventually { !model.isMaintaining }
        XCTAssertFalse(model.failurePresented, "Only a launch check shows a banner")
        XCTAssertEqual(model.promptMessage, L10n.string("ui.cloud.working"))
        fixture.installations.syncCloud()
        XCTAssertEqual(backend.actions, [.maintain(42, installed: nil), .maintain(42, installed: nil)],
                       "An uninstalled game syncs from its saved context")
    }

    @MainActor
    func testARejectedMaintenanceAnswerDoesNotOutliveItsAttempt() async throws {
        let fixture = Fixture()
        fixture.connect()
        let (model, backend) = (fixture.model, fixture.backend)
        fixture.installations.syncCloud()
        backend.publish {
            $0.maintenance = .init(game: 42, cloud: .asking(.conflict, decision: 1, error: "backend.steam.cloud.conflict"))
        }
        try await eventually { model.promptPresented }
        backend.answering = { _, _, _ in throw SteamError.Failure(reason: "backend.steam.runtime.invalidSession") }

        model.respond(.retry)

        try await eventually { model.promptMessage == L10n.message("backend.steam.runtime.invalidSession") }
        backend.publish { $0.maintenance = nil }
        try await eventually { !model.isMaintaining }
        XCTAssertEqual(model.promptMessage, L10n.string("ui.cloud.working"), "A later question starts without the old error")
    }

    @MainActor
    func testSigningOutCancelsMaintenanceAndItsQuestion() async throws {
        let fixture = Fixture()
        fixture.connect()
        let (model, backend) = (fixture.model, fixture.backend)
        fixture.installations.syncCloud()
        backend.publish { $0.maintenance = .init(game: 42, cloud: .asking(.conflict, decision: 1)) }
        try await eventually { model.promptPresented }

        fixture.signOut()

        XCTAssertEqual(backend.actions.last, .signedOut, "Rust cancels the sync and its question")
        XCTAssertFalse(model.isMaintaining)
        XCTAssertFalse(model.promptPresented)
    }

    @MainActor
    func testExitConflictIsPresentedLiveAndNeverReopensByItself() async throws {
        let fixture = Fixture()
        let (sessions, model) = (fixture.sessions, fixture.model)
        fixture.connect()
        let running = GameSessionState.steam(accountID: accountID)
        sessions.fake.show(running)
        try await eventually { sessions.active?.running == true }
        var finalizing = running
        finalizing.phase = .finalizing
        finalizing.cloud = .asking(.conflict, decision: 3, error: "backend.steam.cloud.conflict")
        sessions.fake.show(finalizing)
        try await eventually { model.promptPresented }
        XCTAssertEqual(model.promptGame, 42)
        XCTAssertFalse(model.promptWorking)

        model.respond(.local)
        try await eventually { !model.promptBusy }

        XCTAssertEqual(fixture.backend.actions, [.answer(42, decision: 3, .local)])
        finalizing.cloud = .showing(.pending, error: "backend.steam.cloud.conflict")
        sessions.fake.show(finalizing)
        sessions.fake.show(nil)
        try await eventually { sessions.active == nil }
        XCTAssertFalse(model.promptPresented, "An unanswered exit conflict waits for the next launch or maintenance")
        fixture.connect()
        XCTAssertFalse(model.promptPresented)
        fixture.signOut()
    }
}

/// One Steam account's installations as Rust would show them, with game 42
/// on screen, installed for macOS or not at all.
@MainActor
private final class Fixture {
    let sessions = SteamGameSessions()
    let installations: SteamInstallations
    let receipt: SteamInstallReceipt?
    private(set) var backend = FakeInstallations<SteamGames>()
    var model: SteamCloudModel { installations.cloud }

    init(installed: Bool = false) {
        let root = URL(fileURLWithPath: "/fixture")
        installations = SteamInstallations(directory: root, sessions: sessions, cloud: SteamCloudModel(sessions: sessions))
        receipt = installed ? SteamInstallReceipt(appId: 42, name: "Fixture", directory: "/fixture/Games/Steam/steam-42-macos",
                                                  platform: "macos", language: "english", totalBytes: 0) : nil
    }

    /// Connects the account as SteamModel does.
    func connect(_ account: UInt64 = 0x0110_0001_0000_002a) {
        installations.selection = {
            InstallationSelection(game: 42, platform: "macos", installable: true, request: { receipt, directory in
                SteamDownload(appId: 42, name: "Fixture", platform: receipt?.platform ?? "macos",
                              language: "english", directory: directory)
            })
        }
        backend = FakeInstallations(receipts: receipt.map { [$0] } ?? [])
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
