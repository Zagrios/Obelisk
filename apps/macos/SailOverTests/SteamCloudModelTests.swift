import Foundation
import XCTest
@testable import SailOver

final class SteamCloudModelTests: XCTestCase {
    private let accountID: UInt64 = 0x0110_0001_0000_002a

    @MainActor
    func testAnAnswerReachesRustAndTheQuestionFollowsWhatRustShows() async throws {
        let fixture = Fixture()
        let model = fixture.model
        fixture.connect()
        fixture.backend.publish { $0.cloudSync = .launch(42, question: .conflict(2)) }
        try await eventually { model.promptPresented }
        XCTAssertEqual(model.promptGame, 42)

        model.respond(.cancel)
        try await eventually { !model.promptBusy }
        XCTAssertEqual(fixture.backend.actions, [.answer(42, decision: 2, .cancel)])
        XCTAssertTrue(model.promptPresented, "Rust closes an answered question, not Swift")

        fixture.backend.publish { $0.cloudSync = .launch(42) }
        try await eventually { !model.promptPresented }
        fixture.signOut()
    }

    @MainActor
    func testSecondLiveChoiceIsIgnoredWhileFirstResponseIsInFlight() async throws {
        let fixture = Fixture()
        let model = fixture.model
        let answer = GameSessionGate<Void>()
        fixture.connect()
        fixture.backend.answering = { _, _, _ in try await answer.wait() }
        fixture.backend.publish { $0.cloudSync = .launch(42, question: .conflict(2)) }
        try await eventually { model.promptPresented }

        model.respond(.local)
        model.respond(.cancel)
        XCTAssertTrue(model.promptWorking)
        await answer.resolve(())
        try await eventually { !model.promptBusy }

        XCTAssertEqual(fixture.backend.actions, [.answer(42, decision: 2, .local)])
        fixture.signOut()
    }

    @MainActor
    func testALaunchCheckCanBeCancelledWhileItWorks() async throws {
        let fixture = Fixture()
        let model = fixture.model
        fixture.connect()
        fixture.backend.publish { $0.cloudSync = .launch(42, decision: 6, working: true, cancellable: true) }
        try await eventually { model.isSynchronizing(for: 42) }
        XCTAssertTrue(model.canCancelSynchronization(for: 42))
        XCTAssertFalse(model.canCancelSynchronization(for: 570))
        XCTAssertFalse(model.promptPresented)

        model.cancelSynchronization(for: 42)

        try await eventually { !model.promptBusy }
        XCTAssertEqual(fixture.backend.actions, [.answer(42, decision: 6, .cancel)])
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
            if case .maintain = call { backend.publish { $0.cloudSync = .maintenance(42) } }
        }

        fixture.installations.syncCloud()

        XCTAssertTrue(model.isMaintaining, "Rust holds the Reservation from the call")
        XCTAssertEqual(backend.actions, [.maintain(42, installed: fixture.receipt)])
        XCTAssertTrue(model.isSynchronizing(for: 42))
        backend.publish { $0.cloudSync = .maintenance(42, question: .conflict(1, error: "backend.steam.cloud.conflict")) }
        try await eventually { model.promptPresented }
        XCTAssertEqual(model.promptGame, 42)
        XCTAssertEqual(model.promptMessage, "backend.steam.cloud.conflict")

        model.respond(.local)

        try await eventually { backend.actions.last == .answer(42, decision: 1, .local) }
        backend.indicators = [(42, .neverSynced)]
        backend.publish { $0.cloudSync = nil }
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
        backend.publish { $0.cloudSync = .maintenance(42) }
        try await eventually { model.isMaintaining }

        backend.publish { $0.cloudSync = nil }

        try await eventually { !model.isMaintaining }
        XCTAssertFalse(model.failurePresented, "Only a launch check shows a banner")
        XCTAssertEqual(model.promptMessage, L10n.string("ui.cloud.working"))
        fixture.installations.syncCloud()
        XCTAssertEqual(backend.actions, [.maintain(42, installed: nil), .maintain(42, installed: nil)],
                       "An uninstalled game syncs from its saved context")
    }

    @MainActor
    func testARejectedAnswerShowsItsFailureUntilTheMaintenanceEnds() async throws {
        let fixture = Fixture()
        fixture.connect()
        let (model, backend) = (fixture.model, fixture.backend)
        fixture.installations.syncCloud()
        backend.publish { $0.cloudSync = .maintenance(42, question: .conflict(1, error: "backend.steam.cloud.conflict")) }
        try await eventually { model.promptPresented }
        backend.answering = { _, _, _ in throw SteamError.Failure(reason: "backend.steam.runtime.invalidSession") }

        model.respond(.local)

        try await eventually { model.promptMessage == L10n.message("backend.steam.runtime.invalidSession") }
        backend.publish { $0.cloudSync = nil }
        try await eventually { !model.isMaintaining }
        XCTAssertEqual(model.promptMessage, L10n.string("ui.cloud.working"), "A later question starts without the old error")
    }

    @MainActor
    func testSigningOutCancelsMaintenanceAndItsQuestion() async throws {
        let fixture = Fixture()
        fixture.connect()
        let (model, backend) = (fixture.model, fixture.backend)
        fixture.installations.syncCloud()
        backend.publish { $0.cloudSync = .maintenance(42, question: .conflict(1)) }
        try await eventually { model.promptPresented }

        fixture.signOut()

        XCTAssertEqual(backend.actions.last, .signedOut, "Rust cancels the sync and its question")
        XCTAssertFalse(model.isMaintaining)
        XCTAssertFalse(model.promptPresented)
    }

    @MainActor
    func testALaunchCheckThatFailedAsksWhetherToLaunchAnyway() async throws {
        let fixture = Fixture()
        let model = fixture.model
        fixture.connect()
        fixture.backend.publish {
            $0.cloudSync = .launch(42, question: .failed(4, error: "backend.steam.cloud.network", launchAnyway: true))
        }
        try await eventually { model.failurePresented }
        XCTAssertFalse(model.promptPresented, "A failed check is a banner, not a sheet")
        XCTAssertTrue(model.canLaunchAnyway)
        XCTAssertEqual(model.promptMessage, "backend.steam.cloud.network")
        fixture.backend.publish { $0.cloudSync = .launch(42, question: .failed(4, launchAnyway: false)) }
        try await eventually { !model.canLaunchAnyway }

        model.respond(.retry)

        try await eventually { !model.promptBusy }
        XCTAssertEqual(fixture.backend.actions, [.answer(42, decision: 4, .retry)])
    }
}

/// One Steam account's installations as Rust would show them, with game 42
/// on screen, installed for macOS or not at all.
@MainActor
private final class Fixture {
    let sessions = SteamGameSessions()
    let installations: SteamInstallations
    let receipt: InstallationReceipt<UInt32>?
    private(set) var backend = FakeInstallations<SteamGames>()
    var model: SteamCloudModel { installations.cloud }

    init(installed: Bool = false) {
        installations = SteamInstallations(sessions: sessions, cloud: SteamCloudModel(sessions: sessions))
        receipt = installed ? .steam(appId: 42, name: "Fixture", directory: "/fixture/Games/Steam/steam-42-macos",
                                     platform: "macos", totalBytes: 0) : nil
    }

    /// Connects the account as SteamModel does.
    func connect(_ account: UInt64 = 0x0110_0001_0000_002a) {
        installations.selection = {
            InstallationSelection(game: 42, platform: "macos",
                                  offer: GameOffer.steam(appId: 42, name: "Fixture", platform: "macos", language: "english"))
        }
        backend = FakeInstallations(receipts: receipt.map { [$0] } ?? [])
        installations.connect(account: account, backend: backend, port: InstallationPort(
            verifiesBeforeLaunch: { false }, report: { _ in }, warn: { _ in }
        ))
    }

    /// Signs out as SteamModel does.
    func signOut() {
        installations.signedOut()
        model.signedOut()
    }
}
