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
        let sessions = EpicGameSessions(recoverSession: { nil })
        let model = EpicModel(directory: directory, keychain: EpicMemoryStore(credentials), restore: { _ in account },
                              gameSessions: sessions)
        await model.start()
        model.open(owned)
        let game = EpicCloudGameFixture(phase: .preparing, cloudPhase: .checking)
        let task = try XCTUnwrap(sessions.launch(game: owned.libraryID, account: "account", prepare: { game }))
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
        await game.update(cloudPhase: .conflict, decisionId: 2, error: "backend.epic.cloud_conflict")
        try await eventually { fixture.layout(); return model.cloud.promptPresented && fixture.sheetContent != nil }
        try await Task.sleep(for: .milliseconds(250))
        fixture.layout()
        let sheet = try XCTUnwrap(fixture.sheetContent)
        try savePreview(sheet, to: output.appending(path: "epic-cloud-conflict.png"))
        await game.update(phase: .finished, cloudPhase: .pending)
        await task.value
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

    func testSupervisorCancellationDoesNotBecomeAServiceFailure() async throws {
        for message in ["backend.epic.cloud.cancelled", "backend.epic.cancelled", "renamed cancellation", nil] {
            let game = EpicCloudGameFixture(phase: .failed, cloudPhase: .failed)
            await game.update(error: message, failureReason: .cancelled)
            let sessions = EpicGameSessions(recoverSession: { nil })
            var failure: Error?
            let task = try XCTUnwrap(sessions.launch(game: "sandbox:game", account: "account", prepare: { game },
                onCompletion: { failure = $0 }))
            await task.value
            XCTAssertTrue(failure is CancellationError, "Cancellation was presented as a service failure: \(message ?? "nil")")
            XCTAssertFalse(sessions.isBusy)
        }
    }

    func testTerminalServiceFailureCannotBeDisguisedAsCancellationByItsMessage() async throws {
        for message in ["backend.epic.cloud.cancelled", "backend.epic.cancelled", nil] {
            let game = EpicCloudGameFixture(phase: .failed, cloudPhase: .failed)
            await game.update(error: message, failureReason: .failed)
            let sessions = EpicGameSessions(recoverSession: { nil })
            var failure: Error?
            let task = try XCTUnwrap(sessions.launch(game: "sandbox:game", account: "account", prepare: { game },
                onCompletion: { failure = $0 }))
            await task.value
            guard let failure, case let EpicError.Service(actual) = failure else {
                XCTFail("A genuine failure must remain visible")
                continue
            }
            XCTAssertEqual(actual, message ?? "backend.epic.cloud.failed")
            XCTAssertFalse(sessions.isBusy)
        }
    }

    func testStartupRecoveryReservesFilesBeforeTheFirstSnapshot() async throws {
        let game = EpicCloudGameFixture(phase: .running, cloudPhase: .ready)
        let gate = EpicCloudPreparationGate()
        let sessions = EpicGameSessions(recoverExisting: true, recoverSession: {
            await gate.wait()
            return game
        })
        XCTAssertTrue(sessions.isRecovering)
        XCTAssertTrue(sessions.protects("sandbox:unknown"))
        try await eventually { await gate.waiting }
        await gate.release()
        try await eventually { sessions.active?.phase == .running }
        let starts = await game.beginnings
        XCTAssertEqual(starts, 0, "Recovery must never admit a launch that a former process left waiting")
        XCTAssertFalse(sessions.isRecovering)
        XCTAssertTrue(sessions.protects("sandbox:game"))
        await game.update(phase: .finished, cloudPhase: .synced)
        try await eventually { !sessions.isBusy }
    }

    func testReservationSurvivesCancellationUntilSupervisorConfirmsExit() async throws {
        let game = EpicCloudGameFixture(phase: .running, cloudPhase: .ready)
        let gate = EpicCloudPreparationGate()
        let sessions = EpicGameSessions(recoverSession: { nil })
        let cloud = makeCloud(sessions)
        cloud.connect(account: "account", backend: backend(sessions, "account"))
        let task = try XCTUnwrap(sessions.launch(game: "sandbox:game", account: "account", prepare: {
            await gate.wait()
            return game
        }))
        XCTAssertTrue(sessions.protects("sandbox:game"))
        XCTAssertFalse(cloud.isSynchronizing(for: "sandbox:game"), "Verification precedes the supervisor's first Cloud snapshot")
        XCTAssertNil(sessions.launch(game: "sandbox:other", account: "account", prepare: { game }))
        try await eventually { await gate.waiting }
        task.cancel()
        await gate.release()
        try await eventually { sessions.active?.phase == .running }
        try await eventually { game.signals.cancellations == 1 }
        let starts = await game.beginnings
        XCTAssertEqual(starts, 0, "Cancellation before client arrival must prevent Begin")
        XCTAssertTrue(sessions.isBusy)
        await game.update(phase: .finalizing, cloudPhase: .syncing,
                          error: "renamed cancellation", failureReason: .cancelled)
        try await eventually { sessions.active?.phase == .finalizing }
        XCTAssertTrue(sessions.protects("sandbox:game"))
        await game.update(phase: .finished, cloudPhase: .synced)
        await task.value
        XCTAssertFalse(sessions.isBusy)
        XCTAssertEqual(sessions.last?.cloud.phase, .synced)
    }

    func testObservationLossKeepsInstallationReserved() async throws {
        let game = EpicCloudGameFixture(phase: .running, cloudPhase: .ready)
        let sessions = EpicGameSessions(recoverSession: { nil })
        let task = try XCTUnwrap(sessions.launch(game: "sandbox:game", account: "account", prepare: { game }))
        try await eventually { await game.waitingForSnapshot }
        await game.loseConnection()
        try await eventually { sessions.active?.reconnecting == true }
        XCTAssertTrue(sessions.protects("sandbox:game"))
        XCTAssertNil(sessions.recoveryError, "Losing the control socket is only uncertainty")
        await game.update(phase: .finished, cloudPhase: .pending)
        await task.value
        XCTAssertFalse(sessions.isBusy)
        XCTAssertEqual(sessions.last?.cloud.phase, .pending)
    }

    func testSignOutBeforeClientArrivalPreventsAdmission() async throws {
        let game = EpicCloudGameFixture(phase: .preparing, cloudPhase: .checking)
        let gate = EpicCloudPreparationGate()
        let sessions = EpicGameSessions(recoverSession: { nil })
        let cloud = makeCloud(sessions)
        cloud.connect(account: "account", backend: backend(sessions, "account"))
        let task = try XCTUnwrap(sessions.launch(game: "sandbox:game", account: "account", prepare: {
            await gate.wait()
            return game
        }))
        try await eventually { await gate.waiting }
        cloud.signedOut()
        await gate.release()
        try await eventually { await game.revocations == ["account"] }
        let starts = await game.beginnings
        XCTAssertEqual(starts, 0)
        XCTAssertTrue(sessions.isBusy)
        await game.update(phase: .finished, cloudPhase: .pending)
        await task.value
    }

    func testFailedRevocationDoesNotPreventObservingActualExit() async throws {
        let game = EpicCloudGameFixture(phase: .running, cloudPhase: .ready)
        let sessions = EpicGameSessions(recoverSession: { nil })
        let task = try XCTUnwrap(sessions.launch(game: "sandbox:game", account: "account", prepare: { game }))
        try await eventually { await game.waitingForSnapshot }
        await game.rejectRevocation()
        sessions.signedOut(account: "account")
        await game.update(phase: .finalizing, cloudPhase: .pending)
        try await eventually { sessions.active?.reconnecting == true }
        XCTAssertTrue(sessions.isBusy)
        await game.update(phase: .finished, cloudPhase: .pending)
        await task.value
        XCTAssertFalse(sessions.isBusy)
    }

    func testCloudPresentationFollowsCurrentAccountAndShowsOnlyConflictsAsSheets() async throws {
        let game = EpicCloudGameFixture(phase: .preparing, cloudPhase: .checking)
        let sessions = EpicGameSessions(recoverSession: { nil })
        let cloud = makeCloud(sessions)
        cloud.connect(account: "account", backend: backend(sessions, "account"))
        let task = try XCTUnwrap(sessions.launch(game: "sandbox:game", account: "account", prepare: { game }))
        try await eventually { sessions.active?.cloud.decisionId == 1 }
        XCTAssertTrue(cloud.isSynchronizing(for: "sandbox:game"))
        XCTAssertTrue(cloud.canCancelSynchronization(for: "sandbox:game"))
        XCTAssertFalse(cloud.promptPresented)
        XCTAssertFalse(cloud.failurePresented)
        await game.update(cloudPhase: .failed, allowedChoices: [.retry, .cancel, .launchAnyway])
        try await eventually { cloud.failurePresented }
        XCTAssertTrue(cloud.canLaunchAnyway)
        XCTAssertFalse(cloud.promptPresented)
        await game.update(phase: .finalizing, cloudPhase: .conflict, allowedChoices: [.local, .cloud, .cancel])
        try await eventually { cloud.promptPresented }
        XCTAssertFalse(cloud.failurePresented)
        XCTAssertFalse(cloud.canCancelSynchronization(for: "sandbox:game"))
        cloud.signedOut()
        XCTAssertFalse(cloud.promptPresented)
        XCTAssertTrue(sessions.isBusy)
        cloud.connect(account: "other-account", backend: backend(sessions, "other-account"))
        XCTAssertFalse(cloud.promptPresented)
        XCTAssertFalse(cloud.isSynchronizing(for: "sandbox:game"))
        await game.update(phase: .finished, cloudPhase: .synced)
        await task.value
    }

    func testUnsupportedCloudNeverClaimsSynchronizationAndRetainsProcess() async throws {
        let game = EpicCloudGameFixture(phase: .running, cloudPhase: .unsupported)
        let sessions = EpicGameSessions(recoverSession: { nil })
        let cloud = makeCloud(sessions)
        cloud.connect(account: "account", backend: backend(sessions, "account"))
        let task = try XCTUnwrap(sessions.launch(game: "sandbox:game", account: "account", prepare: { game }))
        try await eventually { sessions.active?.phase == .running }
        XCTAssertFalse(cloud.isSynchronizing(for: "sandbox:game"))
        XCTAssertTrue(sessions.protects("sandbox:game"))
        await game.update(phase: .finished)
        await task.value
    }

    func testEpicModelDisablesFileMutationsThroughRunningGameAndSignOut() async throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "SailOverEpicCloudTests-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let credentials = EpicCredentials(accountId: "account", displayName: "Fixture", refreshToken: "fixture")
        let owned = EpicGame(appName: "game", namespace: "sandbox", catalogItemId: "catalog", title: "Fixture",
                             description: "", imageUrl: nil, headerImageUrl: nil, developer: "", windows: false, macos: true)
        let account = EpicTestSession(credentials: credentials, games: [owned])
        let sessions = EpicGameSessions(recoverSession: { nil })
        let model = EpicModel(directory: directory, keychain: EpicMemoryStore(credentials), restore: { _ in account },
                              gameSessions: sessions)
        await model.start()
        model.open(owned)
        XCTAssertTrue(model.installations.primaryActionAvailable)
        let game = EpicCloudGameFixture(phase: .running, cloudPhase: .ready)
        let task = try XCTUnwrap(sessions.launch(game: owned.libraryID, account: "account", prepare: { game }))
        try await eventually { sessions.active?.phase == .running }
        XCTAssertEqual(model.installations.primaryActionTitle, L10n.string("model.game.running"))
        XCTAssertFalse(model.installations.primaryActionAvailable)
        model.installations.downloadSelected()
        XCTAssertFalse(model.installations.isDownloading)
        model.signOut()
        XCTAssertTrue(sessions.protects(owned.libraryID))
        try await eventually { await game.revocations == ["account"] }
        await game.update(phase: .finished, cloudPhase: .pending)
        await task.value
        XCTAssertFalse(sessions.isBusy)
    }

    func testEpicMaintenanceAsksLiveAndShowsNoBannerAfterAFailure() async throws {
        let directory = FileManager.default.temporaryDirectory.resolvingSymlinksInPath()
            .appending(path: "SailOverEpicCloudTests-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        // Epic's journals take real account identifiers.
        let account = "0123456789abcdef0123456789abcdef"
        let credentials = EpicCredentials(accountId: account, displayName: "Fixture", refreshToken: "fixture")
        let owned = EpicGame(appName: "game", namespace: "sandbox", catalogItemId: "catalog", title: "Fixture",
                             description: "", imageUrl: nil, headerImageUrl: nil, developer: "", windows: false, macos: true)
        let installed = directory.appending(path: "Games/Epic/epic-game-Mac")
        try FileManager.default.createDirectory(at: installed, withIntermediateDirectories: true)
        let receipt = EpicInstallReceipt(namespace: "sandbox", appName: "game", name: "Fixture", directory: installed.path,
                                         platform: "Mac", totalBytes: 0)
        let component = account.utf8.map { String(format: "%02x", $0) }.joined()
        try JSONEncoder().encode([receipt]).write(to: directory.appending(path: "epic-installations-\(component).json"))
        let maintenance = CloudMaintenanceFixture()
        let model = EpicModel(directory: directory, keychain: EpicMemoryStore(credentials),
                              restore: { _ in EpicTestSession(credentials: credentials, games: [owned]) },
                              language: "fr", gameSessions: EpicGameSessions(recoverSession: { nil }),
                              installations: EpicModel.installations(library: directory, maintenance: maintenance))
        await model.start()
        model.open(owned)
        try await eventually { model.installations.canSyncCloud }

        model.syncCloud()

        XCTAssertTrue(model.cloud.isMaintaining, "Rust holds the Reservation from the call")
        XCTAssertFalse(model.installations.canModify(owned.libraryID))
        try await eventually { maintenance.starts.count == 1 }
        let configuration = try XCTUnwrap(maintenance.starts.last?.epic)
        XCTAssertEqual(configuration.install, installed.path)
        XCTAssertEqual(configuration.catalogItemId, "catalog")
        XCTAssertEqual(configuration.language, "fr")
        XCTAssertNil(configuration.prefix, "A Mac build keeps its saves outside Wine")
        maintenance.askConflict()
        try await eventually { model.cloud.promptPresented }
        XCTAssertEqual(model.cloud.promptGame, "sandbox:game")
        model.cloud.respond(.cloud)
        try await eventually { maintenance.answers == [.cloud] }
        maintenance.finish(EpicError.Service(message: "backend.cloud.sharedSavesInUse"))
        try await eventually { !model.cloud.isMaintaining }
        XCTAssertFalse(model.cloud.failurePresented, "Only a launch check shows a banner")
        try await eventually { model.cloud.indicator(for: "sandbox:game") != nil }

        model.syncCloud()
        try await eventually { maintenance.starts.count == 2 }
        model.signOut()
        XCTAssertFalse(model.cloud.isMaintaining)
        XCTAssertNil(model.cloud.indicator(for: "sandbox:game"))
        maintenance.finish()
    }

    func testAnEpicCloudQuestionAppearsWhileTheSteamTabIsOpen() async throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "SailOverEpicCloudTests-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let credentials = EpicCredentials(accountId: "account", displayName: "Fixture", refreshToken: "fixture")
        let owned = EpicGame(appName: "game", namespace: "sandbox", catalogItemId: "catalog", title: "Fixture",
                             description: "", imageUrl: nil, headerImageUrl: nil, developer: "", windows: false, macos: true)
        let sessions = EpicGameSessions(recoverSession: { nil })
        let epic = EpicModel(directory: directory, keychain: EpicMemoryStore(credentials),
                             restore: { _ in EpicTestSession(credentials: credentials, games: [owned]) }, gameSessions: sessions)
        await epic.start()
        let app = AppModel(directory: directory, steam: SteamModel(directory: directory, gameSessions: SteamGameSessions()),
                           epic: epic)
        app.selection = .steam
        let game = EpicCloudGameFixture(phase: .finalizing, cloudPhase: .conflict)

        let task = try XCTUnwrap(sessions.launch(game: owned.libraryID, account: "account", prepare: { game }))

        try await eventually { app.presentedPrompt == .epicCloud }
        await game.update(phase: .finished, cloudPhase: .pending)
        await task.value
        try await eventually { app.presentedPrompt == nil }
    }

    func testEpicRosettaUsesSharedInstallationAndWaitsForSupervisorReadiness() async throws {
        let game = EpicCloudGameFixture(phase: .preparing, cloudPhase: .ready)
        await game.requireRosetta(decisionId: 17)
        await game.failNextRosettaResumes(2)
        let sessions = EpicGameSessions(recoverSession: { nil })
        var installations = 0
        let model = EpicRosettaModel(sessions: sessions, install: { installations += 1 },
            readinessAttempts: 4, readinessDelay: { try await Task.sleep(for: .milliseconds(5)) })
        model.connect(accountId: "account")
        let task = try XCTUnwrap(sessions.launch(game: "sandbox:game", account: "account", prepare: { game }))
        try await eventually { model.prompt != nil }
        model.install()
        try await eventually { model.prompt == nil }
        let resumes = await game.rosettaResumes
        let checks = await game.rosettaChecks
        XCTAssertEqual(installations, 1)
        XCTAssertEqual(checks, 3)
        XCTAssertEqual(resumes, [17])
        await game.update(phase: .finished)
        await task.value
    }

    func testEpicRosettaRejectsStaleSessionAccountAndDecision() async throws {
        let game = EpicCloudGameFixture(phase: .preparing, cloudPhase: .ready)
        await game.requireRosetta(decisionId: 17)
        let sessions = EpicGameSessions(recoverSession: { nil })
        let task = try XCTUnwrap(sessions.launch(game: "sandbox:game", account: "account", prepare: { game }))
        try await eventually { sessions.rosettaPrompt(accountId: "account") != nil }
        let request = try XCTUnwrap(sessions.rosettaPrompt(accountId: "account"))
        for obsolete in [EpicGameSessions.RosettaPrompt(id: "old-session", accountId: "account", decisionId: 17),
                         .init(id: request.id, accountId: "other-account", decisionId: 17),
                         .init(id: request.id, accountId: "account", decisionId: 16)] {
            do {
                try await sessions.resumeAfterRosetta(obsolete)
                XCTFail("An obsolete Rosetta decision must not reach the supervisor")
            } catch { }
            sessions.cancelRosetta(obsolete)
        }
        let checks = await game.rosettaChecks
        XCTAssertEqual(checks, 0)
        XCTAssertEqual(game.signals.cancellations, 0)
        XCTAssertEqual(sessions.rosettaPrompt(accountId: "account"), request)
        await game.update(phase: .finished)
        await task.value
    }

    func testEpicRosettaCancellationHidesPromptUntilSupervisorReleasesSession() async throws {
        let game = EpicCloudGameFixture(phase: .preparing, cloudPhase: .ready)
        await game.requireRosetta(decisionId: 17)
        let sessions = EpicGameSessions(recoverSession: { nil })
        let model = EpicRosettaModel(sessions: sessions, install: { XCTFail("Cancellation must not install") })
        model.connect(accountId: "account")
        let task = try XCTUnwrap(sessions.launch(game: "sandbox:game", account: "account", prepare: { game }))
        try await eventually { model.prompt != nil }
        model.cancel()
        try await eventually { game.signals.cancellations == 1 }
        XCTAssertNil(model.prompt)
        XCTAssertTrue(sessions.isBusy)
        await game.update(phase: .failed, error: "cancelled", failureReason: .cancelled)
        await task.value
        XCTAssertFalse(sessions.isBusy)
        XCTAssertNil(sessions.recoveryError)
    }

    func testEpicRosettaAccountChangeCancelsInstallationAndPreventsResume() async throws {
        let game = EpicCloudGameFixture(phase: .preparing, cloudPhase: .ready)
        await game.requireRosetta(decisionId: 17)
        let sessions = EpicGameSessions(recoverSession: { nil })
        let gate = EpicCloudPreparationGate()
        let model = EpicRosettaModel(sessions: sessions, install: {}, readinessDelay: { await gate.wait() })
        model.connect(accountId: "account")
        let task = try XCTUnwrap(sessions.launch(game: "sandbox:game", account: "account", prepare: { game }))
        try await eventually { model.prompt != nil }
        model.install()
        try await eventually { await gate.waiting }
        model.connect(accountId: "other-account")
        await gate.release()
        try await eventually { game.signals.cancellations == 1 }
        let checks = await game.rosettaChecks
        XCTAssertEqual(checks, 0)
        XCTAssertNil(model.prompt)
        await game.update(phase: .failed, error: "cancelled", failureReason: .cancelled)
        await task.value
    }

    func testEpicModelConnectsAndDisconnectsSharedRosettaPresentation() async throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "SailOverEpicRosetta-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let credentials = EpicCredentials(accountId: "account", displayName: "Fixture", refreshToken: "fixture")
        let account = EpicTestSession(credentials: credentials, games: [])
        let game = EpicCloudGameFixture(phase: .preparing, cloudPhase: .ready)
        await game.requireRosetta(decisionId: 17)
        let sessions = EpicGameSessions(recoverSession: { nil })
        let model = EpicModel(directory: directory, keychain: EpicMemoryStore(credentials), restore: { _ in account },
                              gameSessions: sessions)
        await model.start()
        let task = try XCTUnwrap(sessions.launch(game: "sandbox:game", account: "account", prepare: { game }))
        try await eventually { model.rosetta.prompt != nil }
        model.signOut()
        XCTAssertNil(model.rosetta.prompt)
        try await eventually { game.signals.cancellations >= 1 }
        XCTAssertTrue(sessions.isBusy)
        await game.update(phase: .failed, error: "cancelled", failureReason: .cancelled)
        await task.value
    }

    private func makeCloud(_ sessions: EpicGameSessions) -> EpicCloudModel {
        EpicCloudModel(sessions: sessions)
    }

    /// Rust's installations of `account`, which answer its Cloud questions.
    private func backend(_ sessions: EpicGameSessions, _ account: String) -> any InstallationsBackend<EpicGames> {
        let directory = FileManager.default.temporaryDirectory.appending(path: "SailOverEpicCloudTests-\(UUID().uuidString)")
        addTeardownBlock { try? FileManager.default.removeItem(at: directory) }
        let session = EpicTestSession(credentials: EpicCredentials(accountId: account, displayName: "Fixture",
                                                                   refreshToken: "fixture"), games: [])
        return EpicModel.installations(library: directory)(session, account, "fr", NoCredentialSave(), sessions.core,
                                                           directory.appending(path: "epic-installations.json"))
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

private final class EpicCloudGameFixture: GameSupervisor, @unchecked Sendable {
    struct Choice: Equatable {
        let accountId: String
        let decisionId: UInt64
        let choice: CloudChoice
    }
    let signals = EpicCloudSignals()
    private let condition = NSCondition()
    private var _choices: [Choice] = []
    private var _revocations: [String] = []
    private var _beginnings = 0
    private var _rosettaChecks = 0
    private var _rosettaResumes: [UInt64] = []
    private var unavailableAttempts = 0
    private var rejectsRevocation = false
    private var lostConnection = false
    private var waiters = 0
    private var current: GameSnapshot
    var choices: [Choice] { locked { _choices } }
    var revocations: [String] { locked { _revocations } }
    var beginnings: Int { locked { _beginnings } }
    var rosettaChecks: Int { locked { _rosettaChecks } }
    var rosettaResumes: [UInt64] { locked { _rosettaResumes } }
    var waitingForSnapshot: Bool { locked { waiters > 0 } }

    init(phase: GamePhase, cloudPhase: CloudPhase) {
        current = GameSnapshot(id: UUID().uuidString, phase: phase, finished: [.finished, .failed].contains(phase),
            failureReason: nil, error: nil, revision: 1, reconnecting: false, rosetta: nil,
            cloud: CloudStatus(phase: cloudPhase, decisionId: 1, error: nil, allowedChoices: [.local, .cloud, .cancel]),
            store: .epic(accountId: "account", namespace: "sandbox", appName: "game"))
    }
    private func locked<T>(_ body: () throws -> T) rethrows -> T {
        condition.lock()
        defer { condition.unlock() }
        return try body()
    }
    func cancelPreparation() { signals.cancel() }
    func stop() throws { update(phase: .finished) }
    func begin() throws { locked { _beginnings += 1 } }
    func rejectRevocation() { locked { rejectsRevocation = true } }
    func revoke(account: GameAccount) throws {
        try locked {
            if rejectsRevocation { throw GameSessionError.Failure(reason: "backend.epic.cloud.session") }
            guard case let .epic(accountId) = account else { throw GameSessionError.Failure(reason: "wrong store") }
            _revocations.append(accountId)
        }
    }
    func choose(account: GameAccount, decisionId: UInt64, choice: CloudChoice) throws {
        try locked {
            guard case let .epic(accountId) = account else { throw GameSessionError.Failure(reason: "wrong store") }
            _choices.append(Choice(accountId: accountId, decisionId: decisionId, choice: choice))
        }
    }
    func requireRosetta(decisionId: UInt64) {
        locked { current.rosetta = RosettaRequest(decisionId: decisionId) }
        update(keeping: true)
    }
    func failNextRosettaResumes(_ attempts: Int) { locked { unavailableAttempts = attempts } }
    func resumeAfterRosetta(account: GameAccount, decisionId: UInt64) throws {
        try locked {
            _rosettaChecks += 1
            if unavailableAttempts > 0 {
                unavailableAttempts -= 1
                throw GameSessionError.Failure(reason: EpicGameSessions.rosettaUnavailableKey)
            }
            guard account == .epic(accountId: "account"), current.rosetta?.decisionId == decisionId else {
                throw GameSessionError.Failure(reason: "backend.epic.cloud.invalidSession")
            }
            _rosettaResumes.append(decisionId)
        }
    }
    func snapshot(after: UInt64?) throws -> GameSnapshot {
        try locked {
            waiters += 1
            defer { waiters -= 1 }
            while after == current.revision && !lostConnection { condition.wait() }
            if lostConnection {
                lostConnection = false
                throw GameSessionError.Failure(reason: "backend.epic.cloud.session")
            }
            return current
        }
    }
    func update(phase: GamePhase? = nil, cloudPhase: CloudPhase? = nil,
                decisionId: UInt64? = nil, allowedChoices: [CloudChoice]? = nil, error: String? = nil,
                failureReason: GameFailureReason? = nil) {
        locked {
            if let phase { current.phase = phase }
            if let cloudPhase { current.cloud.phase = cloudPhase }
            if let decisionId { current.cloud.decisionId = decisionId }
            if let allowedChoices { current.cloud.allowedChoices = allowedChoices }
            current.error = error
            current.failureReason = failureReason
        }
        update(keeping: true)
    }
    private func update(keeping: Bool) {
        locked {
            current.finished = [.finished, .failed].contains(current.phase)
            current.revision += 1
            condition.broadcast()
        }
    }
    func loseConnection() {
        locked {
            guard waiters > 0 else { return }
            lostConnection = true
            condition.broadcast()
        }
    }
}

private final class EpicCloudSignals: @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0
    var cancellations: Int { lock.withLock { count } }
    func cancel() { lock.withLock { count += 1 } }
}

private final class NoCredentialSave: EpicCredentialSave {
    func save() {}
}
