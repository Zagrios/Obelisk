import AppKit
import Foundation
import SwiftUI
import XCTest
@testable import Forge

@MainActor
final class EpicCloudTests: XCTestCase {
    func testCloudPresentationPreviewWhenRequested() async throws {
        guard let path = ProcessInfo.processInfo.environment["FORGE_EPIC_CLOUD_PREVIEW_DIR"] else {
            throw XCTSkip("Set FORGE_EPIC_CLOUD_PREVIEW_DIR to render disposable Epic Cloud fixtures")
        }
        let output = URL(fileURLWithPath: path, isDirectory: true)
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        let directory = FileManager.default.temporaryDirectory.appending(path: "ForgeEpicCloudPreview-\(UUID().uuidString)")
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
            .tint(ForgeStyle.accent)
            .background(ForgePalette(colorScheme: .dark).background)
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
        NSColor(ForgePalette(colorScheme: .dark).background).setFill()
        bounds.fill()
        context.cgContext.setBlendMode(.normal)
        context.cgContext.draw(try XCTUnwrap(bitmap.cgImage), in: bounds)
        context.flushGraphics()
        NSGraphicsContext.restoreGraphicsState()
        XCTAssertEqual(opaque.colorAt(x: 0, y: 0)?.alphaComponent, 1)
        try XCTUnwrap(opaque.representation(using: .png, properties: [:])).write(to: file)
    }

    func testHandoffForwardsCancellationBeforeAndAfterClientArrival() async throws {
        let late = EpicCloudGameFixture(phase: .preparing, cloudPhase: .checking)
        let before = EpicGameHandoff()
        before.cancel()
        before.receive(late)
        before.cancel()
        try await eventually { late.signals.cancellations == 1 }

        let received = EpicCloudGameFixture(phase: .preparing, cloudPhase: .checking)
        let after = EpicGameHandoff()
        after.receive(received)
        after.cancel()
        try await eventually { received.signals.cancellations == 1 }
        try await after.revoke(accountId: "account")
        let revocations = await received.revocations
        XCTAssertEqual(revocations, ["account"])
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
        cloud.connect(account: "account")
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
        cloud.connect(account: "account")
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

    func testChoicesAreBoundToAccountSessionAndDecisionIncludingFinalization() async throws {
        let game = EpicCloudGameFixture(phase: .preparing, cloudPhase: .conflict)
        let sessions = EpicGameSessions(recoverSession: { nil })
        let task = try XCTUnwrap(sessions.launch(game: "sandbox:game", account: "account", prepare: { game }))
        try await eventually { sessions.active?.cloud.decisionId == 1 }
        let old = try XCTUnwrap(sessions.active)
        await game.update(phase: .finalizing, cloudPhase: .conflict, decisionId: 2)
        try await eventually { sessions.active?.cloud.decisionId == 2 }
        do {
            try await sessions.choose(.local, expected: old)
            XCTFail("A stale decision must not overwrite newer saves")
        } catch {}
        let current = try XCTUnwrap(sessions.active)
        let other = makeCloud(sessions)
        other.connect(account: "other-account")
        XCTAssertNil(other.prompt, "Another account must not resolve this conflict")
        try await sessions.choose(.cloud, expected: current)
        let choices = await game.choices
        XCTAssertEqual(choices, [.init(accountId: "account", decisionId: 2, choice: .cloud)])
        sessions.signedOut(account: "account")
        do {
            try await sessions.choose(.local, expected: current)
            XCTFail("Sign-out must immediately prevent Cloud decisions")
        } catch {}
        XCTAssertTrue(sessions.isBusy)
        try await eventually { await game.revocations == ["account"] }
        await game.update(phase: .finished, cloudPhase: .pending)
        await task.value
    }

    func testCloudPresentationFollowsCurrentAccountAndShowsOnlyConflictsAsSheets() async throws {
        let game = EpicCloudGameFixture(phase: .preparing, cloudPhase: .checking)
        let sessions = EpicGameSessions(recoverSession: { nil })
        let cloud = makeCloud(sessions)
        cloud.connect(account: "account")
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
        cloud.connect(account: "other-account")
        XCTAssertFalse(cloud.promptPresented)
        XCTAssertFalse(cloud.isSynchronizing(for: "sandbox:game"))
        await game.update(phase: .finished, cloudPhase: .synced)
        await task.value
    }

    func testUnsupportedCloudNeverClaimsSynchronizationAndRetainsProcess() async throws {
        let game = EpicCloudGameFixture(phase: .running, cloudPhase: .unsupported)
        let sessions = EpicGameSessions(recoverSession: { nil })
        let cloud = makeCloud(sessions)
        cloud.connect(account: "account")
        let task = try XCTUnwrap(sessions.launch(game: "sandbox:game", account: "account", prepare: { game }))
        try await eventually { sessions.active?.phase == .running }
        XCTAssertFalse(cloud.isSynchronizing(for: "sandbox:game"))
        XCTAssertTrue(sessions.protects("sandbox:game"))
        await game.update(phase: .finished)
        await task.value
    }

    func testEpicModelDisablesFileMutationsThroughRunningGameAndSignOut() async throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "ForgeEpicCloudTests-\(UUID().uuidString)")
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
        let sessions = EpicGameSessions(recoverSession: { nil })
        let services = CloudServicesFixture()
        let cloud = EpicCloudModel(root: "/fixture-cloud", sessionDirectory: "/fixture-sessions", sessions: sessions,
                                   indicators: services.epic(CloudIndicator(state: .pending, gameSession: false, syncedAt: nil,
                                                                            pendingUpload: true, installationRemoved: false)).indicators)
        cloud.connect(account: "account") { services.startEpicMaintenance($0) }
        let configuration = EpicCloudConfiguration(root: "/fixture-cloud", home: "/fixture-home", install: "/fixture-game",
            sessionDirectory: "/fixture-sessions", helper: "/fixture-helper", namespace: "sandbox", catalogItemId: "catalog",
            appName: "game", platform: "Mac", language: "fr")

        cloud.startMaintenance(configuration)

        XCTAssertTrue(cloud.isMaintaining, "The Reservation starts before the Rust attempt exists")
        try await eventually { services.epicMaintenance.count == 1 }
        let attempt = try XCTUnwrap(services.epicMaintenance.last?.attempt)
        attempt.publish(.conflict, decisionId: 3, choices: [.local, .cloud, .cancel])
        try await eventually { cloud.promptPresented }
        XCTAssertEqual(cloud.promptGame, "sandbox:game")
        cloud.respond(.cloud)
        XCTAssertEqual(attempt.answers.map(\.1), [.cloud])
        attempt.finish(.failed, error: "backend.cloud.sharedSavesInUse")
        try await eventually { !cloud.isMaintaining }
        XCTAssertFalse(cloud.failurePresented, "Only a launch check shows a banner")
        try await eventually { cloud.indicator(for: "sandbox:game")?.state == .pending }

        cloud.startMaintenance(configuration)
        try await eventually { services.epicMaintenance.count == 2 }
        XCTAssertEqual(services.epicMaintenance.last?.configuration, configuration, "Sync now is the retry")
        cloud.signedOut()
        XCTAssertNil(cloud.indicator(for: "sandbox:game"))
    }

    func testEpicMaintenanceOfAnUninstalledGameUsesItsSavedConfiguration() async throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "ForgeEpicCloudTests-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let credentials = EpicCredentials(accountId: "account", displayName: "Fixture", refreshToken: "fixture")
        let owned = EpicGame(appName: "game", namespace: "sandbox", catalogItemId: "catalog", title: "Fixture",
                             description: "", imageUrl: nil, headerImageUrl: nil, developer: "", windows: false, macos: true)
        let saved = EpicCloudConfiguration(root: "/old-cloud", home: "/fixture-home", install: "/removed-game",
            sessionDirectory: "/old-sessions", helper: "/old-helper", namespace: "sandbox", catalogItemId: "catalog",
            appName: "game", platform: "Mac", language: "fr")
        let services = CloudServicesFixture()
        let sessions = EpicGameSessions(recoverSession: { nil })
        let model = EpicModel(directory: directory, keychain: EpicMemoryStore(credentials),
                              restore: { _ in EpicTestSession(credentials: credentials, games: [owned]) },
                              gameSessions: sessions,
                              cloudServices: services.epic(CloudIndicator(state: .pending, gameSession: false, syncedAt: nil,
                                                                          pendingUpload: true, installationRemoved: true),
                                                           configuration: saved),
                              maintainCloud: { services.startEpicMaintenance($1) })
        await model.start()
        model.open(owned)
        try await eventually { model.installations.cloudIndicator != nil }
        XCTAssertNil(model.installations.selectedReceipt)
        XCTAssertTrue(model.installations.canSyncCloud)

        model.syncCloud()

        XCTAssertTrue(model.cloud.isMaintaining)
        XCTAssertFalse(model.installations.canModify(owned.libraryID), "Maintenance holds the Reservation")
        XCTAssertFalse(model.installations.canSyncCloud)
        try await eventually { services.epicMaintenance.count == 1 }
        XCTAssertEqual(services.epicMaintenance.last?.configuration.install, "/removed-game")
        XCTAssertNotEqual(services.epicMaintenance.last?.configuration.root, "/old-cloud", "This Forge's paths replace saved ones")
        model.signOut()
        XCTAssertFalse(model.cloud.isMaintaining)
    }

    func testAnEpicCloudQuestionAppearsWhileTheSteamTabIsOpen() async throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "ForgeEpicCloudTests-\(UUID().uuidString)")
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
        let directory = FileManager.default.temporaryDirectory.appending(path: "ForgeEpicRosetta-\(UUID().uuidString)")
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
        EpicCloudModel(root: "/fixture-cloud", sessionDirectory: "/fixture-sessions", sessions: sessions,
                       indicators: { _, _, _ in [] })
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

private actor EpicCloudGameFixture: GameSessionProtocol {
    struct Choice: Equatable {
        let accountId: String
        let decisionId: UInt64
        let choice: CloudChoice
    }
    nonisolated let signals = EpicCloudSignals()
    private(set) var choices: [Choice] = []
    private(set) var revocations: [String] = []
    private(set) var beginnings = 0
    private(set) var rosettaChecks = 0
    private(set) var rosettaResumes: [UInt64] = []
    private var unavailableAttempts = 0
    private var rejectsRevocation = false
    private var current: GameSnapshot
    private var waiting: CheckedContinuation<GameSnapshot, Error>?
    var waitingForSnapshot: Bool { waiting != nil }

    init(phase: GamePhase, cloudPhase: CloudPhase) {
        current = GameSnapshot(id: UUID().uuidString, phase: phase, finished: [.finished, .failed].contains(phase),
            failureReason: nil, exitCode: nil, error: nil, revision: 1, reconnecting: false, rosetta: nil,
            cloud: CloudStatus(phase: cloudPhase, decisionId: 1, error: nil, allowedChoices: [.local, .cloud, .cancel]),
            store: .epic(accountId: "account", namespace: "sandbox", appName: "game"))
    }
    nonisolated func cancelPreparation() { signals.cancel() }
    func begin() async throws { beginnings += 1 }
    func rejectRevocation() { rejectsRevocation = true }
    func revoke(account: GameAccount) async throws {
        if rejectsRevocation { throw GameSessionError.Failure(reason: "backend.epic.cloud.session") }
        guard case let .epic(accountId) = account else { throw GameSessionError.Failure(reason: "wrong store") }
        revocations.append(accountId)
    }
    func choose(account: GameAccount, decisionId: UInt64, choice: CloudChoice) async throws {
        guard case let .epic(accountId) = account else { throw GameSessionError.Failure(reason: "wrong store") }
        choices.append(Choice(accountId: accountId, decisionId: decisionId, choice: choice))
    }
    func requireRosetta(decisionId: UInt64) {
        current.rosetta = RosettaRequest(decisionId: decisionId)
        update()
    }
    func failNextRosettaResumes(_ attempts: Int) { unavailableAttempts = attempts }
    func resumeAfterRosetta(account: GameAccount, decisionId: UInt64) async throws {
        rosettaChecks += 1
        if unavailableAttempts > 0 {
            unavailableAttempts -= 1
            throw GameSessionError.Failure(reason: EpicGameSessions.rosettaUnavailableKey)
        }
        guard account == .epic(accountId: "account"), current.rosetta?.decisionId == decisionId else {
            throw GameSessionError.Failure(reason: "backend.epic.cloud.invalidSession")
        }
        rosettaResumes.append(decisionId)
    }
    func snapshot(after: UInt64?) async throws -> GameSnapshot {
        if after != current.revision { return current }
        return try await withCheckedThrowingContinuation { waiting = $0 }
    }
    func update(phase: GamePhase? = nil, cloudPhase: CloudPhase? = nil,
                decisionId: UInt64? = nil, allowedChoices: [CloudChoice]? = nil, error: String? = nil,
                failureReason: GameFailureReason? = nil) {
        if let phase { current.phase = phase }
        if let cloudPhase { current.cloud.phase = cloudPhase }
        if let decisionId { current.cloud.decisionId = decisionId }
        if let allowedChoices { current.cloud.allowedChoices = allowedChoices }
        current.error = error
        current.failureReason = failureReason
        current.finished = [.finished, .failed].contains(current.phase)
        current.revision += 1
        waiting?.resume(returning: current)
        waiting = nil
    }
    func loseConnection() {
        waiting?.resume(throwing: GameSessionError.Failure(reason: "backend.epic.cloud.session"))
        waiting = nil
    }
}

private final class EpicCloudSignals: @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0
    var cancellations: Int { lock.withLock { count } }
    func cancel() { lock.withLock { count += 1 } }
}
