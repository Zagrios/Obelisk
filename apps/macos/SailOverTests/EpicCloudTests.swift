import AppKit
import Foundation
import SwiftUI
import XCTest
@testable import SailOver

@MainActor
final class EpicCloudTests: XCTestCase {
    func testCloudPresentationPreviewWhenRequested() async throws {
        guard let path = ProcessInfo.processInfo.environment["SAILOVER_EPIC_CLOUD_PREVIEW_DIR"] else {
            throw XCTSkip("Set SAILOVER_EPIC_CLOUD_PREVIEW_DIR to render disposable Epic Cloud fixtures")
        }
        let output = URL(fileURLWithPath: path, isDirectory: true)
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        let directory = FileManager.default.temporaryDirectory.appending(path: "SailOverEpicCloudPreview-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let credentials = EpicCredentials(accountId: "account", displayName: "Fixture", refreshToken: "fixture")
        let owned = EpicGame(appName: "game", namespace: "sandbox", catalogItemId: "catalog", title: "Cloud fixture",
                             description: "", imageUrl: nil, headerImageUrl: nil, developer: "", windows: false, macos: true)
        let account = EpicTestSession(credentials: credentials, games: [owned])
        let sessions = EpicGameSessions()
        let model = EpicModel(directory: directory, keychain: EpicMemoryStore(credentials), restore: { _ in account },
                              gameSessions: sessions, installations: EpicModel.installations(FakeInstallations()))
        await model.start()
        model.open(owned)
        var session = GameSessionState.epic(account: "account", namespace: "sandbox", appName: "game",
                                            phase: .preparing, cloud: .showing(.checking))
        sessions.fake.show(session)
        try await eventually { model.installations.isSynchronizingSaves }
        XCTAssertEqual(model.installations.primaryActionTitle, L10n.string("ui.cloud.sync_short"))
        let view = EpicView(model: model)
            .environment(\.colorScheme, .dark)
            .tint(SailOverStyle.accent)
            .background(SailOverPalette(colorScheme: .dark).background)
        let fixture = EpicPresentationWindow(view, size: NSSize(width: 1060, height: 720))
        defer { fixture.close() }
        fixture.layout()
        try await Task.sleep(for: .milliseconds(250))
        fixture.layout()
        try savePreview(fixture.host, to: output.appending(path: "epic-cloud-synchronization.png"))
        session.cloud = .asking(.conflict, decision: 2, error: "backend.epic.cloud_conflict")
        sessions.fake.show(session)
        try await eventually { fixture.layout(); return model.cloud.promptPresented && fixture.sheetContent != nil }
        try await Task.sleep(for: .milliseconds(250))
        fixture.layout()
        let sheet = try XCTUnwrap(fixture.sheetContent)
        try savePreview(sheet, to: output.appending(path: "epic-cloud-conflict.png"))
        sessions.fake.show(nil)
        try await eventually { fixture.layout(); return fixture.sheetContent == nil }
    }

    private func savePreview(_ view: NSView, to file: URL) throws {
        view.layoutSubtreeIfNeeded()
        let bitmap = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: bitmap)
        XCTAssertGreaterThan(bitmap.pixelsWide, 300)
        XCTAssertGreaterThan(bitmap.pixelsHigh, 150)
        // A sheet's AppKit backdrop is outside its content view. Preserve the
        // captured alpha when compositing onto the fixture's dark background.
        let opaque = try XCTUnwrap(NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: bitmap.pixelsWide,
            pixelsHigh: bitmap.pixelsHigh, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
            isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0))
        let context = try XCTUnwrap(NSGraphicsContext(bitmapImageRep: opaque))
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = context
        let bounds = NSRect(x: 0, y: 0, width: bitmap.pixelsWide, height: bitmap.pixelsHigh)
        NSColor(SailOverPalette(colorScheme: .dark).background).setFill()
        bounds.fill()
        context.cgContext.setBlendMode(.normal)
        context.cgContext.draw(try XCTUnwrap(bitmap.cgImage), in: bounds)
        context.flushGraphics()
        NSGraphicsContext.restoreGraphicsState()
        XCTAssertEqual(opaque.colorAt(x: 0, y: 0)?.alphaComponent, 1)
        try XCTUnwrap(opaque.representation(using: .png, properties: [:])).write(to: file)
    }


    func testRecoveryReservesEveryGameUntilItFindsTheSession() async throws {
        let sessions = EpicGameSessions(FakeSessions(recovering: true))
        XCTAssertTrue(sessions.isRecovering)
        XCTAssertTrue(sessions.protects("sandbox:unknown"))

        sessions.fake.show(.epic(account: "account", namespace: "sandbox", appName: "game"))

        try await eventually { !sessions.isRecovering }
        XCTAssertTrue(sessions.protects("sandbox:game"))
        XCTAssertFalse(sessions.protects("sandbox:unknown"))
        sessions.fake.show(nil)
        try await eventually { !sessions.isBusy }
    }

    func testCloudPresentationFollowsCurrentAccountAndShowsOnlyConflictsAsSheets() async throws {
        let sessions = EpicGameSessions()
        let cloud = EpicCloudModel(sessions: sessions)
        cloud.connect(account: "account", backend: FakeInstallations())
        var session = GameSessionState.epic(account: "account", namespace: "sandbox", appName: "game",
                                            phase: .preparing, cloud: .showing(.checking))
        sessions.fake.show(session)
        try await eventually { sessions.active?.cloud.phase == .checking }
        XCTAssertTrue(cloud.isSynchronizing(for: "sandbox:game"))
        XCTAssertTrue(cloud.canCancelSynchronization(for: "sandbox:game"))
        XCTAssertFalse(cloud.promptPresented)
        XCTAssertFalse(cloud.failurePresented)
        session.cloud = CloudStatus(phase: .failed, decisionId: 1, error: nil, allowedChoices: [.retry, .cancel, .launchAnyway])
        sessions.fake.show(session)
        try await eventually { cloud.failurePresented }
        XCTAssertTrue(cloud.canLaunchAnyway)
        XCTAssertFalse(cloud.promptPresented)
        session.phase = .finalizing
        session.cloud = .asking(.conflict, decision: 2)
        sessions.fake.show(session)
        try await eventually { cloud.promptPresented }
        XCTAssertFalse(cloud.failurePresented)
        XCTAssertFalse(cloud.canCancelSynchronization(for: "sandbox:game"))
        cloud.signedOut()
        XCTAssertFalse(cloud.promptPresented)
        XCTAssertEqual(sessions.fake.revocations, [.epic(accountId: "account")])
        cloud.connect(account: "other-account", backend: FakeInstallations())
        XCTAssertFalse(cloud.promptPresented)
        XCTAssertFalse(cloud.isSynchronizing(for: "sandbox:game"))
    }

    func testUnsupportedCloudNeverClaimsSynchronization() async throws {
        let sessions = EpicGameSessions()
        let cloud = EpicCloudModel(sessions: sessions)
        cloud.connect(account: "account", backend: FakeInstallations())
        sessions.fake.show(.epic(account: "account", namespace: "sandbox", appName: "game",
                                 cloud: .showing(.unsupported)))
        try await eventually { sessions.active?.phase == .running }
        XCTAssertFalse(cloud.isSynchronizing(for: "sandbox:game"))
        XCTAssertTrue(sessions.protects("sandbox:game"))
    }

    func testEpicModelDisablesFileMutationsThroughRunningGameAndSignOut() async throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "SailOverEpicCloudTests-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let credentials = EpicCredentials(accountId: "account", displayName: "Fixture", refreshToken: "fixture")
        let owned = EpicGame(appName: "game", namespace: "sandbox", catalogItemId: "catalog", title: "Fixture",
                             description: "", imageUrl: nil, headerImageUrl: nil, developer: "", windows: false, macos: true)
        let account = EpicTestSession(credentials: credentials, games: [owned])
        let sessions = EpicGameSessions()
        let backend = FakeInstallations<EpicGames>()
        let model = EpicModel(directory: directory, keychain: EpicMemoryStore(credentials), restore: { _ in account },
                              gameSessions: sessions, installations: EpicModel.installations(backend))
        await model.start()
        model.open(owned)
        XCTAssertTrue(model.installations.primaryActionAvailable)
        sessions.fake.show(.epic(account: "account", namespace: "sandbox", appName: "game"))
        try await eventually { sessions.active?.phase == .running }
        XCTAssertEqual(model.installations.primaryActionTitle, L10n.string("model.game.running"))
        XCTAssertFalse(model.installations.primaryActionAvailable)
        model.installations.downloadSelected()
        XCTAssertEqual(backend.actions, [], "A running game's files stay untouched")
        model.signOut()
        XCTAssertTrue(sessions.protects(owned.libraryID))
        XCTAssertEqual(sessions.fake.revocations, [.epic(accountId: "account")])
    }

    func testEpicMaintenanceAsksLiveAndShowsNoBannerAfterAFailure() async throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "SailOverEpicCloudTests-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let credentials = EpicCredentials(accountId: "account", displayName: "Fixture", refreshToken: "fixture")
        let owned = EpicGame(appName: "game", namespace: "sandbox", catalogItemId: "catalog", title: "Fixture",
                             description: "", imageUrl: nil, headerImageUrl: nil, developer: "", windows: false, macos: true)
        let receipt = EpicInstallReceipt(namespace: "sandbox", appName: "game", name: "Fixture",
                                         directory: directory.appending(path: "Games/Epic/epic-game-Mac").path,
                                         platform: "Mac", totalBytes: 0)
        let backend = FakeInstallations<EpicGames>(receipts: [receipt])
        backend.onCall = { call in
            if case .maintain = call {
                backend.publish { $0.maintenance = .init(game: "sandbox:game", cloud: .showing(.checking)) }
            }
        }
        let model = EpicModel(directory: directory, keychain: EpicMemoryStore(credentials),
                              restore: { _ in EpicTestSession(credentials: credentials, games: [owned]) },
                              gameSessions: EpicGameSessions(), installations: EpicModel.installations(backend))
        await model.start()
        model.open(owned)
        try await eventually { model.installations.canSyncCloud }

        model.syncCloud()

        XCTAssertTrue(model.cloud.isMaintaining, "Rust holds the Reservation from the call")
        XCTAssertFalse(model.installations.canModify(owned.libraryID))
        XCTAssertEqual(backend.actions, [.maintain("sandbox:game", installed: receipt)])
        backend.publish { $0.maintenance?.cloud = .asking(.conflict, decision: 1) }
        try await eventually { model.cloud.promptPresented }
        XCTAssertEqual(model.cloud.promptGame, "sandbox:game")
        model.cloud.respond(.cloud)
        try await eventually { backend.actions.last == .answer("sandbox:game", decision: 1, .cloud) }
        backend.indicators = [("sandbox:game", .neverSynced)]
        backend.publish { $0.maintenance = nil }
        try await eventually { !model.cloud.isMaintaining }
        XCTAssertFalse(model.cloud.failurePresented, "Only a launch check shows a banner")
        try await eventually { model.cloud.indicator(for: "sandbox:game") != nil }

        model.syncCloud()
        XCTAssertTrue(model.cloud.isMaintaining)
        model.signOut()
        XCTAssertFalse(model.cloud.isMaintaining)
        XCTAssertNil(model.cloud.indicator(for: "sandbox:game"))
        XCTAssertEqual(backend.actions.last, .signedOut)
    }

    func testAnEpicCloudQuestionAppearsWhileTheSteamTabIsOpen() async throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "SailOverEpicCloudTests-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let credentials = EpicCredentials(accountId: "account", displayName: "Fixture", refreshToken: "fixture")
        let owned = EpicGame(appName: "game", namespace: "sandbox", catalogItemId: "catalog", title: "Fixture",
                             description: "", imageUrl: nil, headerImageUrl: nil, developer: "", windows: false, macos: true)
        let sessions = EpicGameSessions()
        let epic = EpicModel(directory: directory, keychain: EpicMemoryStore(credentials),
                             restore: { _ in EpicTestSession(credentials: credentials, games: [owned]) },
                             gameSessions: sessions, installations: EpicModel.installations(FakeInstallations()))
        await epic.start()
        let app = AppModel(directory: directory, steam: SteamModel(directory: directory, gameSessions: SteamGameSessions()),
                           epic: epic)
        app.selection = .steam

        sessions.fake.show(.epic(account: "account", namespace: "sandbox", appName: "game", phase: .finalizing,
                                 cloud: .asking(.conflict, decision: 1)))

        try await eventually { app.presentedPrompt == .epicCloud }
        sessions.fake.show(nil)
        try await eventually { app.presentedPrompt == nil }
    }

    func testEpicRosettaUsesSharedInstallationAndWaitsForSupervisorReadiness() async throws {
        let sessions = EpicGameSessions()
        sessions.fake.failNextRosettaResumes(2)
        var installations = 0
        let model = EpicRosettaModel(sessions: sessions, install: { installations += 1 },
            readinessAttempts: 4, readinessDelay: { try await Task.sleep(for: .milliseconds(5)) })
        model.connect(accountId: "account")
        waitForRosetta(sessions)
        try await eventually { model.prompt != nil }
        model.install()
        try await eventually { model.prompt == nil }
        XCTAssertEqual(installations, 1)
        XCTAssertEqual(sessions.fake.rosettaChecks.count, 3)
        XCTAssertEqual(sessions.fake.rosettaResumes.map(\.decisionID), [17])
        XCTAssertEqual(sessions.fake.rosettaResumes.map(\.account), [.epic(accountId: "account")])
    }

    func testAStaleEpicRosettaPromptCannotCancelTheCurrentRequirement() async throws {
        let sessions = EpicGameSessions()
        waitForRosetta(sessions)
        try await eventually { sessions.rosettaPrompt(accountId: "account") != nil }
        let request = try XCTUnwrap(sessions.rosettaPrompt(accountId: "account"))
        for obsolete in [EpicGameSessions.RosettaPrompt(id: "old-session", accountId: "account", decisionId: 17),
                         .init(id: request.id, accountId: "other-account", decisionId: 17),
                         .init(id: request.id, accountId: "account", decisionId: 16)] {
            sessions.cancelRosetta(obsolete)
        }
        XCTAssertTrue(sessions.fake.declines.isEmpty)
        XCTAssertEqual(sessions.rosettaPrompt(accountId: "account"), request)
    }

    func testEpicRosettaCancellationHidesPromptUntilTheSessionEnds() async throws {
        let sessions = EpicGameSessions()
        let model = EpicRosettaModel(sessions: sessions, install: { XCTFail("Cancellation must not install") })
        model.connect(accountId: "account")
        waitForRosetta(sessions)
        try await eventually { model.prompt != nil }
        model.cancel()
        XCTAssertEqual(sessions.fake.declines.map(\.decisionID), [17])
        XCTAssertNil(model.prompt)
        XCTAssertTrue(sessions.isBusy)
        sessions.fake.show(nil)
        try await eventually { !sessions.isBusy }
    }

    func testEpicRosettaAccountChangeCancelsInstallationAndPreventsResume() async throws {
        let sessions = EpicGameSessions()
        let gate = EpicCloudPreparationGate()
        let model = EpicRosettaModel(sessions: sessions, install: {}, readinessDelay: { await gate.wait() })
        model.connect(accountId: "account")
        waitForRosetta(sessions)
        try await eventually { model.prompt != nil }
        model.install()
        try await eventually { await gate.waiting }
        model.connect(accountId: "other-account")
        await gate.release()
        XCTAssertEqual(sessions.fake.declines.count, 1)
        try await Task.sleep(for: .milliseconds(20))
        XCTAssertTrue(sessions.fake.rosettaChecks.isEmpty)
        XCTAssertNil(model.prompt)
    }

    func testEpicModelConnectsAndDisconnectsSharedRosettaPresentation() async throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "SailOverEpicRosetta-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let credentials = EpicCredentials(accountId: "account", displayName: "Fixture", refreshToken: "fixture")
        let account = EpicTestSession(credentials: credentials, games: [])
        let sessions = EpicGameSessions()
        let model = EpicModel(directory: directory, keychain: EpicMemoryStore(credentials), restore: { _ in account },
                              gameSessions: sessions, installations: EpicModel.installations(FakeInstallations()))
        await model.start()
        waitForRosetta(sessions)
        try await eventually { model.rosetta.prompt != nil }
        model.signOut()
        XCTAssertNil(model.rosetta.prompt)
        XCTAssertEqual(sessions.fake.declines.count, 1)
        XCTAssertTrue(sessions.isBusy)
    }

    /// An Epic launch of `sandbox:game` waiting for Rosetta, decision 17.
    private func waitForRosetta(_ sessions: EpicGameSessions) {
        sessions.fake.show(.epic(account: "account", namespace: "sandbox", appName: "game", phase: .preparing,
                                 cloud: .showing(.ready)))
        sessions.fake.requireRosetta(decisionID: 17)
    }
}

private actor EpicCloudPreparationGate {
    private var continuation: CheckedContinuation<Void, Never>?
    private(set) var waiting = false
    func wait() async {
        waiting = true
        await withCheckedContinuation { continuation = $0 }
    }
    func release() { continuation?.resume(); continuation = nil }
}
