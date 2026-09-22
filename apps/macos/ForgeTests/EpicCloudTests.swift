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
        let task = try XCTUnwrap(sessions.launch(libraryID: owned.libraryID, accountId: "account", prepare: { game }))
        try await eventually { model.isSynchronizingSaves }
        XCTAssertEqual(model.primaryActionTitle, L10n.string("ui.cloud.sync_short"))
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
            let task = try XCTUnwrap(sessions.launch(libraryID: "sandbox:game", accountId: "account",
                prepare: { game }, onCompletion: { failure = $0 }))
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
            let task = try XCTUnwrap(sessions.launch(libraryID: "sandbox:game", accountId: "account",
                prepare: { game }, onCompletion: { failure = $0 }))
            await task.value
            guard let failure, case let EpicError.Service(actual) = failure else {
                XCTFail("A genuine failure must remain visible")
                continue
            }
            XCTAssertEqual(actual, message ?? "backend.epic.cloud.failed")
            XCTAssertFalse(sessions.isBusy)
        }
    }

    func testNetworkRetryOccursOnlyWhenAnObservedOutageEnds() {
        var retries = 0
        let observation = NetworkReturnObservation(observeSystem: false) { retries += 1 }
        observation.receive(isSatisfied: true)
        observation.receive(isSatisfied: true)
        XCTAssertEqual(retries, 0)
        observation.receive(isSatisfied: false)
        observation.receive(isSatisfied: false)
        observation.receive(isSatisfied: true)
        observation.receive(isSatisfied: true)
        XCTAssertEqual(retries, 1)
        observation.receive(isSatisfied: false)
        observation.receive(isSatisfied: true)
        XCTAssertEqual(retries, 2)
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
        cloud.connect(accountId: "account", resume: { _ in game })
        let task = try XCTUnwrap(sessions.launch(libraryID: "sandbox:game", accountId: "account", prepare: {
            await gate.wait()
            return game
        }))
        XCTAssertTrue(sessions.protects("sandbox:game"))
        XCTAssertFalse(cloud.isSynchronizing(for: "sandbox:game"), "Verification precedes the supervisor's first Cloud snapshot")
        XCTAssertNil(sessions.launch(libraryID: "sandbox:other", accountId: "account", prepare: { game }))
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
        XCTAssertEqual(sessions.last?.cloudPhase, .synced)
    }

    func testObservationLossKeepsInstallationReserved() async throws {
        let game = EpicCloudGameFixture(phase: .running, cloudPhase: .ready)
        let sessions = EpicGameSessions(recoverSession: { nil })
        let task = try XCTUnwrap(sessions.launch(libraryID: "sandbox:game", accountId: "account", prepare: { game }))
        try await eventually { await game.waitingForSnapshot }
        await game.loseConnection()
        try await eventually { sessions.active?.reconnecting == true }
        XCTAssertTrue(sessions.protects("sandbox:game"))
        XCTAssertNotNil(sessions.recoveryError)
        await game.update(phase: .finished, cloudPhase: .pending)
        await task.value
        XCTAssertFalse(sessions.isBusy)
        XCTAssertEqual(sessions.last?.cloudPhase, .pending)
    }

    func testSignOutBeforeClientArrivalPreventsAdmission() async throws {
        let game = EpicCloudGameFixture(phase: .preparing, cloudPhase: .checking)
        let gate = EpicCloudPreparationGate()
        let sessions = EpicGameSessions(recoverSession: { nil })
        let cloud = makeCloud(sessions)
        cloud.connect(accountId: "account", resume: { _ in game })
        let task = try XCTUnwrap(sessions.launch(libraryID: "sandbox:game", accountId: "account", prepare: {
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
        let task = try XCTUnwrap(sessions.launch(libraryID: "sandbox:game", accountId: "account", prepare: { game }))
        try await eventually { await game.waitingForSnapshot }
        await game.rejectRevocation()
        do { try await sessions.revokeCloud(accountId: "account"); XCTFail("The fixture must lose its control socket") }
        catch {}
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
        let task = try XCTUnwrap(sessions.launch(libraryID: "sandbox:game", accountId: "account", prepare: { game }))
        try await eventually { sessions.active?.decisionId == 1 }
        let old = try XCTUnwrap(sessions.active)
        await game.update(phase: .finalizing, cloudPhase: .conflict, decisionId: 2)
        try await eventually { sessions.active?.decisionId == 2 }
        do {
            try await sessions.choose(.local, expected: old, accountId: "account")
            XCTFail("A stale decision must not overwrite newer saves")
        } catch {}
        let current = try XCTUnwrap(sessions.active)
        do {
            try await sessions.choose(.cloud, expected: current, accountId: "other-account")
            XCTFail("Another account must not resolve this conflict")
        } catch {}
        try await sessions.choose(.cloud, expected: current, accountId: "account")
        let choices = await game.choices
        XCTAssertEqual(choices, [.init(accountId: "account", decisionId: 2, choice: .cloud)])
        try await sessions.revokeCloud(accountId: "account")
        do {
            try await sessions.choose(.local, expected: current, accountId: "account")
            XCTFail("Sign-out must immediately prevent Cloud decisions")
        } catch {}
        XCTAssertTrue(sessions.isBusy)
        let revocations = await game.revocations
        XCTAssertEqual(revocations, ["account"])
        await game.update(phase: .finished, cloudPhase: .pending)
        await task.value
    }

    func testCloudPresentationFollowsCurrentAccountAndShowsOnlyConflictsAsSheets() async throws {
        let game = EpicCloudGameFixture(phase: .preparing, cloudPhase: .checking)
        let sessions = EpicGameSessions(recoverSession: { nil })
        let cloud = makeCloud(sessions)
        cloud.connect(accountId: "account", resume: { _ in game })
        try await eventually { !cloud.isLoadingPending }
        let task = try XCTUnwrap(sessions.launch(libraryID: "sandbox:game", accountId: "account", prepare: { game }))
        try await eventually { sessions.active?.decisionId == 1 }
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
        XCTAssertNil(cloud.message(for: "sandbox:game"))
        cloud.signedOut()
        XCTAssertFalse(cloud.promptPresented)
        XCTAssertNil(cloud.message(for: "sandbox:game"))
        XCTAssertTrue(sessions.isBusy)
        cloud.connect(accountId: "other-account", resume: { _ in game })
        XCTAssertFalse(cloud.promptPresented)
        XCTAssertFalse(cloud.isSynchronizing(for: "sandbox:game"))
        await game.update(phase: .finished, cloudPhase: .synced)
        await task.value
        XCTAssertNil(cloud.message(for: "sandbox:game"))
    }

    func testUnsupportedCloudNeverClaimsSynchronizationAndRetainsProcess() async throws {
        let game = EpicCloudGameFixture(phase: .running, cloudPhase: .unsupported)
        let sessions = EpicGameSessions(recoverSession: { nil })
        let cloud = makeCloud(sessions)
        cloud.connect(accountId: "account", resume: { _ in game })
        try await eventually { !cloud.isLoadingPending }
        let task = try XCTUnwrap(sessions.launch(libraryID: "sandbox:game", accountId: "account", prepare: { game }))
        try await eventually { sessions.active?.phase == .running }
        XCTAssertEqual(cloud.message(for: "sandbox:game"), L10n.string("ui.epic.cloud.unsupported"))
        XCTAssertFalse(cloud.hasPending(for: "sandbox:game"))
        XCTAssertFalse(cloud.isSynchronizing(for: "sandbox:game"))
        XCTAssertTrue(sessions.protects("sandbox:game"))
        await game.update(phase: .finished)
        await task.value
        XCTAssertNotEqual(cloud.message(for: "sandbox:game"), L10n.string("ui.cloud.synced"))
    }

    func testPendingResumeUsesCurrentHelperAndDoesNotLoopAfterFailure() async throws {
        let game = EpicCloudGameFixture(phase: .finalizing, cloudPhase: .checking)
        let sessions = EpicGameSessions(recoverSession: { nil })
        let configuration = EpicCloudConfiguration(root: "/old", home: "/fixture-home", install: "/fixture-game",
            sessionDirectory: "/old-session", helper: "/old-helper", namespace: "sandbox", catalogItemId: "catalog",
            appName: "game", platform: "Mac", language: "fr")
        let cloud = EpicCloudModel(root: "/fixture-cloud", sessionDirectory: "/fixture-sessions", sessions: sessions,
                                   loadPending: { _, _ in [configuration] })
        var resumed: [EpicCloudConfiguration] = []
        cloud.connect(accountId: "account", resume: {
            resumed.append($0)
            return game
        })
        try await eventually { sessions.active?.phase == .finalizing }
        XCTAssertEqual(resumed.count, 1)
        XCTAssertEqual(resumed.first?.root, "/fixture-cloud")
        XCTAssertEqual(resumed.first?.sessionDirectory, "/fixture-sessions")
        XCTAssertEqual(resumed.first?.helper, SteamGameLauncher.helper)
        XCTAssertEqual(resumed.first?.install, "/fixture-game")
        await game.update(phase: .finished, cloudPhase: .pending)
        try await eventually { !sessions.isBusy && !cloud.isLoadingPending }
        XCTAssertTrue(cloud.hasPending(for: "sandbox:game"))
        cloud.sessionFinished()
        try await eventually { !cloud.isLoadingPending }
        XCTAssertEqual(resumed.count, 1, "A failed automatic attempt must wait for a wake or explicit retry")
        cloud.retryPending(libraryID: "sandbox:game")
        try await eventually { resumed.count == 2 && !sessions.isBusy && !cloud.isLoadingPending }
    }

    func testTerminalConflictKeepsAnExplicitRetryAfterSupervisorCloses() async throws {
        let game = EpicCloudGameFixture(phase: .failed, cloudPhase: .conflict)
        let sessions = EpicGameSessions(recoverSession: { nil })
        let configuration = EpicCloudConfiguration(root: "/fixture-cloud", home: "/fixture-home", install: "/fixture-game",
            sessionDirectory: "/fixture-sessions", helper: "/fixture-helper", namespace: "sandbox", catalogItemId: "catalog",
            appName: "game", platform: "Mac", language: "fr")
        let cloud = EpicCloudModel(root: "/fixture-cloud", sessionDirectory: "/fixture-sessions", sessions: sessions,
                                   loadPending: { _, _ in [configuration] })
        var attempts = 0
        cloud.connect(accountId: "account", resume: { _ in attempts += 1; return game })
        try await eventually { attempts == 1 && !sessions.isBusy && !cloud.isLoadingPending }
        XCTAssertTrue(cloud.hasPending(for: "sandbox:game"))
        XCTAssertFalse(cloud.promptPresented, "A terminal supervisor cannot accept a choice")
        cloud.retryPending(libraryID: "sandbox:game")
        try await eventually { attempts == 2 && !sessions.isBusy && !cloud.isLoadingPending }
        XCTAssertTrue(cloud.hasPending(for: "sandbox:game"))
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
        XCTAssertTrue(model.primaryActionAvailable)
        let game = EpicCloudGameFixture(phase: .running, cloudPhase: .ready)
        let task = try XCTUnwrap(sessions.launch(libraryID: owned.libraryID, accountId: "account", prepare: { game }))
        try await eventually { sessions.active?.phase == .running }
        XCTAssertEqual(model.primaryActionTitle, L10n.string("model.steam.running"))
        XCTAssertFalse(model.primaryActionAvailable)
        model.downloadSelected()
        XCTAssertFalse(model.isDownloading)
        model.signOut()
        XCTAssertTrue(sessions.protects(owned.libraryID))
        try await eventually { await game.revocations == ["account"] }
        await game.update(phase: .finished, cloudPhase: .pending)
        await task.value
        XCTAssertFalse(sessions.isBusy)
        XCTAssertNil(model.cloud.message(for: owned.libraryID))
    }

    private func makeCloud(_ sessions: EpicGameSessions) -> EpicCloudModel {
        EpicCloudModel(root: "/fixture-cloud", sessionDirectory: "/fixture-sessions", sessions: sessions,
                       loadPending: { _, _ in [] })
    }

    private func eventually(file: StaticString = #filePath, line: UInt = #line,
                            _ condition: @MainActor () async -> Bool) async throws {
        for _ in 0..<400 {
            if await condition() { return }
            try await Task.sleep(for: .milliseconds(5))
        }
        XCTFail("Timed out waiting for Epic Cloud state", file: file, line: line)
        throw CocoaError(.coderInvalidValue)
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

private actor EpicCloudGameFixture: EpicGameSessionProtocol {
    struct Choice: Equatable {
        let accountId: String
        let decisionId: UInt64
        let choice: EpicCloudChoice
    }
    nonisolated let signals = EpicCloudSignals()
    private(set) var choices: [Choice] = []
    private(set) var revocations: [String] = []
    private(set) var beginnings = 0
    private var rejectsRevocation = false
    private var current: EpicGameSnapshot
    private var waiting: CheckedContinuation<EpicGameSnapshot, Error>?
    var waitingForSnapshot: Bool { waiting != nil }

    init(phase: EpicGamePhase, cloudPhase: EpicCloudPhase) {
        current = EpicGameSnapshot(id: UUID().uuidString, accountId: "account", namespace: "sandbox", appName: "game",
            phase: phase, cloudPhase: cloudPhase, decisionId: 1, allowedChoices: [.local, .cloud, .cancel],
            error: nil, failureReason: nil, exitCode: nil, revision: 1, reconnecting: false)
    }
    nonisolated func cancelPreparation() { signals.cancel() }
    func begin() async throws { beginnings += 1 }
    func rejectRevocation() { rejectsRevocation = true }
    func revokeCloud(accountId: String) async throws {
        if rejectsRevocation { throw EpicError.Network }
        revocations.append(accountId)
    }
    func choose(accountId: String, decisionId: UInt64, choice: EpicCloudChoice) async throws {
        choices.append(Choice(accountId: accountId, decisionId: decisionId, choice: choice))
    }
    func snapshot(afterRevision: UInt64?) async throws -> EpicGameSnapshot {
        if afterRevision != current.revision { return current }
        return try await withCheckedThrowingContinuation { waiting = $0 }
    }
    func update(phase: EpicGamePhase? = nil, cloudPhase: EpicCloudPhase? = nil,
                decisionId: UInt64? = nil, allowedChoices: [EpicCloudChoice]? = nil, error: String? = nil,
                failureReason: EpicGameFailureReason? = nil) {
        if let phase { current.phase = phase }
        if let cloudPhase { current.cloudPhase = cloudPhase }
        if let decisionId { current.decisionId = decisionId }
        if let allowedChoices { current.allowedChoices = allowedChoices }
        current.error = error
        current.failureReason = failureReason
        current.revision += 1
        waiting?.resume(returning: current)
        waiting = nil
    }
    func loseConnection() {
        waiting?.resume(throwing: EpicError.Network)
        waiting = nil
    }
}

private final class EpicCloudSignals: @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0
    var cancellations: Int { lock.withLock { count } }
    func cancel() { lock.withLock { count += 1 } }
}
