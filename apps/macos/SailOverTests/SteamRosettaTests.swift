import Foundation
import SwiftUI
import XCTest
@testable import SailOver

final class SteamRosettaTests: XCTestCase {
    @MainActor
    func testInstalledRosettaResumesDespiteLaunchServicesFailure() async throws {
        let fixture = try await fixture(install: { throw NSError(domain: NSOSStatusErrorDomain, code: -10669) })
        fixture.model.install()
        try await eventually { fixture.model.prompt == nil || fixture.model.installationFailed }
        XCTAssertNil(fixture.model.prompt, "A failed helper callback must not keep the sheet open when Rust can run Rosetta")
        XCTAssertFalse(fixture.model.installationFailed)
        let resumed = await fixture.game.rosettaResumes
        XCTAssertEqual(resumed, [.init(accountID: 42, decisionID: 17)])
        await fixture.finish()
    }

    @MainActor
    func testLaunchServicesErrorBeforeReadinessStillResumesAutomatically() async throws {
        var openings = 0
        let fixture = try await fixture(install: {
            openings += 1
            throw NSError(domain: NSOSStatusErrorDomain, code: -10669)
        })
        await fixture.game.failNextRosettaResumes(2)
        fixture.model.install()
        try await eventually { fixture.model.prompt == nil || fixture.model.installationFailed }
        XCTAssertNil(fixture.model.prompt)
        XCTAssertFalse(fixture.model.installationFailed)
        XCTAssertEqual(openings, 1)
        let checks = await fixture.game.rosettaChecks
        let resumed = await fixture.game.rosettaResumes
        XCTAssertEqual(checks.count, 3)
        XCTAssertEqual(resumed.count, 1)
        await fixture.finish()
    }

    @MainActor
    func testPendingInstallerCallbackDoesNotBlockReadinessAndLateErrorCannotUndoResume() async throws {
        let installation = GameSessionGate<Void>()
        var installerReturned = false
        let fixture = try await fixture(install: {
            defer { installerReturned = true }
            try await installation.wait()
        })
        fixture.model.install()
        try await eventually { fixture.model.prompt == nil }
        XCTAssertFalse(installerReturned, "Readiness must not depend on the LaunchServices callback")
        await installation.fail(NSError(domain: NSOSStatusErrorDomain, code: -10669))
        try await eventually { installerReturned }
        XCTAssertFalse(fixture.model.installationFailed)
        XCTAssertNil(fixture.model.prompt)
        let resumed = await fixture.game.rosettaResumes
        XCTAssertEqual(resumed.count, 1)
        await fixture.finish()
    }

    @MainActor
    func testCancellationDuringReadinessWaitPreventsAnyLaterResume() async throws {
        let secondDelay = GameSessionGate<Void>()
        var delays = 0
        let fixture = try await fixture(install: {}, readinessDelay: {
            delays += 1
            if delays == 2 { try await secondDelay.wait() }
        })
        await fixture.game.failRosettaResume(true)
        fixture.model.install()
        try await eventually { await secondDelay.requests == 1 }
        fixture.model.cancel()
        await fixture.game.failRosettaResume(false)
        await secondDelay.resolve(())
        try await eventually { await fixture.game.cancellations == 1 }
        XCTAssertNil(fixture.model.prompt)
        let resumed = await fixture.game.rosettaResumes
        let checks = await fixture.game.rosettaChecks
        XCTAssertTrue(resumed.isEmpty)
        XCTAssertEqual(checks.count, 1)
        XCTAssertTrue(fixture.sessions.isBusy)
        await fixture.finish()
    }

    @MainActor
    func testReadinessTimeoutStopsCheckingAndAllowsRetry() async throws {
        let installation = GameSessionGate<Void>()
        var delays = 0
        let fixture = try await fixture(install: { try await installation.wait() }, readinessAttempts: 3,
                                        readinessDelay: { delays += 1; await Task.yield() })
        await fixture.game.failRosettaResume(true)
        fixture.model.install()
        try await eventually { fixture.model.installationFailed }
        XCTAssertEqual(delays, 3)
        XCTAssertFalse(fixture.model.isInstalling)
        XCTAssertNotNil(fixture.model.prompt)
        await fixture.game.failRosettaResume(false)
        fixture.model.install()
        try await eventually { fixture.model.prompt == nil }
        XCTAssertFalse(fixture.model.installationFailed)
        XCTAssertEqual(delays, 4)
        await installation.resolve(())
        let resumed = await fixture.game.rosettaResumes
        XCTAssertEqual(resumed.count, 1)
        await fixture.finish()
    }

    @MainActor
    func testSessionAndCloudErrorsAreNotRetriedAsRosettaUnavailability() async throws {
        for error in ["backend.steam.runtime.invalidSession", "backend.steam.cloud.network"] {
            let fixture = try await fixture(install: {})
            await fixture.game.failRosettaWith(error)
            fixture.model.install()
            try await eventually { fixture.model.installationFailed }
            let checks = await fixture.game.rosettaChecks
            XCTAssertEqual(checks.count, 1)
            XCTAssertNotNil(fixture.model.prompt)
            await fixture.finish()
        }
    }

    @MainActor
    func testElapsedDeadlineStopsReadinessBeforeAttemptLimit() async throws {
        var instant = ContinuousClock.now
        var delays = 0
        let fixture = try await fixture(install: {}, readinessAttempts: 600, readinessDelay: {
            delays += 1
            instant = instant.advanced(by: .seconds(1))
            await Task.yield()
        }, readinessTimeout: .seconds(2), now: { instant })
        await fixture.game.failRosettaResume(true)
        fixture.model.install()
        try await eventually { fixture.model.installationFailed }
        let checks = await fixture.game.rosettaChecks
        XCTAssertEqual(delays, 2)
        XCTAssertEqual(checks.count, 1)
        XCTAssertFalse(fixture.model.isInstalling)
        await fixture.finish()
    }

    @MainActor
    func testOnlyCurrentAccountsPublishedRequirementPresentsPrompt() async throws {
        let game = RecoveredGameFixture(phase: .preparing, cloudPhase: .checking)
        let sessions = SteamGameSessions()
        let model = SteamRosettaModel(sessions: sessions, install: { XCTFail("No installation without a requirement") })
        model.connect(accountId: 99)
        let task = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { game }))
        try await eventually { sessions.active?.cloud.phase == .checking }
        XCTAssertNil(model.prompt)
        model.install()
        await game.update(cloudPhase: .ready)
        await game.requireRosetta(decisionId: 17)
        try await eventually { sessions.rosettaPrompt(accountId: 42) != nil }
        XCTAssertNil(model.prompt)
        model.connect(accountId: 42)
        XCTAssertEqual(model.prompt?.decisionId, 17)
        XCTAssertFalse(model.isInstalling)
        sessions.signedOut(account: 42)
        XCTAssertNil(model.prompt)
        await game.finish()
        await task.value
    }

    @MainActor
    func testInstallationResumesSameSessionOnceAndKeepsAdmissionUntilGameFinishes() async throws {
        let installation = GameSessionGate<Void>()
        let fixture = try await fixture(install: { try await installation.wait() })
        await fixture.game.failRosettaResume(true)
        fixture.model.install()
        fixture.model.install()
        try await eventually { await installation.requests == 1 }
        XCTAssertTrue(fixture.model.isInstalling)
        XCTAssertTrue(fixture.sessions.protects(42))
        XCTAssertNil(fixture.sessions.launch(game: 570, account: 42, prepare: {
            XCTFail("Already reserved")
            return RecoveredGameFixture(appID: 570)
        }))
        let before = await fixture.game.rosettaResumes
        XCTAssertTrue(before.isEmpty)
        await fixture.game.failRosettaResume(false)
        await installation.resolve(())
        try await eventually { fixture.model.prompt == nil }
        let resumed = await fixture.game.rosettaResumes
        XCTAssertEqual(resumed, [.init(accountID: 42, decisionID: 17)])
        XCTAssertTrue(fixture.sessions.isBusy)
        await fixture.game.update(phase: .running)
        try await eventually { fixture.sessions.active?.running == true }
        await fixture.finish()
    }

    @MainActor
    func testInstallerFailureAllowsRetryWithoutPrematureResume() async throws {
        var attempts = 0
        let installation = GameSessionGate<Void>()
        let fixture = try await fixture(install: {
            attempts += 1
            if attempts == 1 { throw CocoaError(.executableNotLoadable) }
            try await installation.wait()
        })
        await fixture.game.failRosettaResume(true)
        fixture.model.install()
        try await eventually { fixture.model.installationFailed }
        XCTAssertNotNil(fixture.model.prompt)
        XCTAssertFalse(fixture.model.isInstalling)
        let before = await fixture.game.rosettaResumes
        XCTAssertTrue(before.isEmpty)
        fixture.model.install()
        try await eventually { await installation.requests == 1 }
        XCTAssertFalse(fixture.model.installationFailed)
        await fixture.game.failRosettaResume(false)
        await installation.resolve(())
        try await eventually { fixture.model.prompt == nil }
        XCTAssertEqual(attempts, 2)
        await fixture.finish()
    }

    @MainActor
    func testSuccessfulHelperCannotOverrideFailedRuntimeRecheck() async throws {
        let fixture = try await fixture(install: {}, readinessAttempts: 1)
        await fixture.game.failRosettaResume(true)
        fixture.model.install()
        try await eventually { fixture.model.installationFailed }
        XCTAssertNotNil(fixture.model.prompt)
        XCTAssertEqual(fixture.sessions.active?.phase, .preparing)
        await fixture.game.failRosettaResume(false)
        fixture.model.install()
        try await eventually { fixture.model.prompt == nil }
        let attempts = await fixture.game.rosettaChecks
        XCTAssertEqual(attempts.count, 2)
        await fixture.finish()
    }

    @MainActor
    func testCancelDuringAppleInstallationIgnoresLateCompletionAndWaitsForTerminalState() async throws {
        let installation = GameSessionGate<Void>()
        var installerReturned = false
        let fixture = try await fixture(install: { try await installation.wait(); installerReturned = true })
        await fixture.game.failRosettaResume(true)
        fixture.model.install()
        try await eventually { await installation.requests == 1 }
        fixture.model.cancel()
        try await eventually { await fixture.game.cancellations == 1 }
        XCTAssertNil(fixture.model.prompt)
        XCTAssertTrue(fixture.sessions.isBusy)
        XCTAssertTrue(fixture.sessions.protects(42))
        await installation.resolve(())
        try await eventually { installerReturned }
        await fixture.finish()
        let resumed = await fixture.game.rosettaResumes
        XCTAssertTrue(resumed.isEmpty)
        XCTAssertFalse(fixture.model.installationFailed)
    }

    @MainActor
    func testAccountChangeIgnoresLateInstallationAndDoesNotPresentOldRequirement() async throws {
        let installation = GameSessionGate<Void>()
        var installerReturned = false
        let fixture = try await fixture(install: { try await installation.wait(); installerReturned = true })
        await fixture.game.failRosettaResume(true)
        fixture.model.install()
        try await eventually { await installation.requests == 1 }
        fixture.model.connect(accountId: 99)
        try await eventually { await fixture.game.cancellations == 1 }
        XCTAssertNil(fixture.model.prompt)
        fixture.model.connect(accountId: 42)
        XCTAssertNil(fixture.model.prompt, "Signing back in must not reopen a cancelled attempt")
        await installation.resolve(())
        try await eventually { installerReturned }
        await fixture.finish()
        let resumed = await fixture.game.rosettaResumes
        XCTAssertTrue(resumed.isEmpty)
        XCTAssertFalse(fixture.model.isInstalling)
    }

    @MainActor
    func testSystemCancellationEndsAttemptWithoutInstallationError() async throws {
        for error in [NSError(domain: NSCocoaErrorDomain, code: NSUserCancelledError),
                      NSError(domain: NSOSStatusErrorDomain, code: -128)] {
            let fixture = try await fixture(install: { throw error })
            fixture.model.install()
            try await eventually { await fixture.game.cancellations == 1 }
            XCTAssertNil(fixture.model.prompt)
            XCTAssertFalse(fixture.model.installationFailed)
            let resumed = await fixture.game.rosettaResumes
            XCTAssertTrue(resumed.isEmpty)
            await fixture.finish()
        }
    }

    @MainActor
    func testLateInstallerCannotAnswerReplacementDecision() async throws {
        let installation = GameSessionGate<Void>()
        var installerReturned = false
        let fixture = try await fixture(install: { try await installation.wait(); installerReturned = true })
        await fixture.game.failRosettaResume(true)
        fixture.model.install()
        try await eventually { await installation.requests == 1 }
        await fixture.game.requireRosetta(decisionId: 18)
        try await eventually { fixture.model.prompt?.decisionId == 18 }
        await installation.resolve(())
        try await eventually { installerReturned }
        XCTAssertEqual(fixture.model.prompt?.decisionId, 18)
        XCTAssertFalse(fixture.model.isInstalling)
        let resumed = await fixture.game.rosettaResumes
        XCTAssertTrue(resumed.isEmpty)
        await fixture.finish()
    }

    @MainActor
    func testSessionRejectsStaleRosettaIdentityAccountAndDecisionBeforeBridgeCall() async throws {
        let fixture = try await fixture(install: {})
        let prompt = try XCTUnwrap(fixture.model.prompt)
        for obsolete in [SteamGameSessions.RosettaPrompt(id: UUID().uuidString, accountId: 42, decisionId: 17),
                         .init(id: prompt.id, accountId: 99, decisionId: 17),
                         .init(id: prompt.id, accountId: 42, decisionId: 16)] {
            do {
                try await fixture.sessions.resumeAfterRosetta(obsolete)
                XCTFail("An obsolete response must not reach the supervisor")
            } catch {}
            fixture.sessions.cancelRosetta(obsolete)
        }
        let resumed = await fixture.game.rosettaResumes
        let cancellations = await fixture.game.cancellations
        XCTAssertTrue(resumed.isEmpty)
        XCTAssertEqual(cancellations, 0)
        await fixture.finish()
    }

    @MainActor
    func testPreviousInstallerCompletionCannotResumeOrClearNewAttempt() async throws {
        let first = RecoveredGameFixture(phase: .preparing, cloudPhase: .ready)
        let second = RecoveredGameFixture(phase: .preparing, cloudPhase: .ready)
        await first.requireRosetta(decisionId: 17)
        await second.requireRosetta(decisionId: 17)
        let sessions = SteamGameSessions()
        let oldInstallation = GameSessionGate<Void>()
        let newInstallation = GameSessionGate<Void>()
        var installations = 0
        var oldInstallerReturned = false
        let model = SteamRosettaModel(sessions: sessions, install: {
            installations += 1
            if installations == 1 {
                try await oldInstallation.wait()
                oldInstallerReturned = true
            } else { try await newInstallation.wait() }
        })
        model.connect(accountId: 42)
        let oldTask = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { first }))
        try await eventually { model.prompt != nil }
        model.install()
        try await eventually { await oldInstallation.requests == 1 }
        model.cancel()
        await first.finish()
        await oldTask.value
        let newTask = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { second }))
        try await eventually { model.prompt?.id == second.sessionID }
        model.install()
        try await eventually { await newInstallation.requests == 1 }
        await oldInstallation.resolve(())
        try await eventually { oldInstallerReturned }
        XCTAssertTrue(model.isInstalling)
        let premature = await second.rosettaResumes
        XCTAssertTrue(premature.isEmpty)
        await newInstallation.resolve(())
        try await eventually { model.prompt == nil }
        let oldResumes = await first.rosettaResumes
        let newResumes = await second.rosettaResumes
        XCTAssertTrue(oldResumes.isEmpty)
        XCTAssertEqual(newResumes.count, 1)
        await second.finish()
        await newTask.value
    }

    @MainActor
    func testCancellingRecoveredRosettaAttemptDoesNotLeaveRecoveryError() async throws {
        let game = RecoveredGameFixture(phase: .preparing, cloudPhase: .ready)
        await game.requireRosetta(decisionId: 17)
        let sessions = SteamGameSessions(recoverExisting: true, recoverSession: { game })
        let model = SteamRosettaModel(sessions: sessions, install: { XCTFail("Cancellation does not install") })
        model.connect(accountId: 42)
        try await eventually { model.prompt != nil }
        model.cancel()
        try await eventually { await game.cancellations == 1 }
        XCTAssertTrue(sessions.isBusy)
        await game.fail(reason: "backend.steam.runtime.cancelled")
        try await eventually { !sessions.isBusy }
        XCTAssertNil(sessions.recoveryError)
    }

    @MainActor
    func testRenderRosettaPromptWhenRequested() async throws {
        guard let destination = ProcessInfo.processInfo.environment["SAILOVER_RENDER_PREVIEWS"], !destination.isEmpty else {
            throw XCTSkip("Set SAILOVER_RENDER_PREVIEWS to render the isolated Rosetta prompt")
        }
        let directory = URL(fileURLWithPath: destination, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let fixture = try await fixture(install: { throw CocoaError(.executableNotLoadable) })
        await fixture.game.failRosettaResume(true)
        try await render(fixture.model, to: directory.appending(path: "steam-rosetta-prompt.png"))
        fixture.model.install()
        try await eventually { fixture.model.installationFailed }
        try await render(fixture.model, to: directory.appending(path: "steam-rosetta-retry.png"))
        await fixture.finish()
    }

    @MainActor
    private func render(_ model: SteamRosettaModel, to file: URL) async throws {
        let view = RosettaPromptView(model: model)
            .environment(\.colorScheme, .dark)
            .background(SailOverPalette(colorScheme: .dark).background)
        let renderer = ImageRenderer(content: view)
        renderer.scale = 2
        let bitmap = NSBitmapImageRep(cgImage: try XCTUnwrap(renderer.cgImage))
        var luminances: [Double] = []
        for y in stride(from: 0, to: bitmap.pixelsHigh, by: 4) {
            for x in stride(from: 0, to: bitmap.pixelsWide, by: 4) {
                if let color = bitmap.colorAt(x: x, y: y)?.usingColorSpace(.deviceRGB) {
                    luminances.append(Double(0.2126 * color.redComponent + 0.7152 * color.greenComponent + 0.0722 * color.blueComponent))
                }
            }
        }
        let mean = luminances.reduce(0, +) / Double(max(luminances.count, 1))
        let variance = luminances.reduce(0) { $0 + ($1 - mean) * ($1 - mean) } / Double(max(luminances.count, 1))
        XCTAssertGreaterThan(variance, 0.002, "The preview must contain visible text and controls")
        guard variance > 0.002 else { throw CocoaError(.coderInvalidValue) }
        let png = try XCTUnwrap(bitmap.representation(using: .png, properties: [:]))
        try png.write(to: file)
    }

    @MainActor
    private func fixture(install: @escaping @MainActor () async throws -> Void, readinessAttempts: Int = 4,
                         readinessDelay: @escaping @MainActor () async throws -> Void = { try await Task.sleep(for: .milliseconds(10)) },
                         readinessTimeout: Duration = .seconds(600),
                         now: @escaping @MainActor () -> ContinuousClock.Instant = { .now }) async throws -> Fixture {
        let game = RecoveredGameFixture(phase: .preparing, cloudPhase: .ready)
        await game.requireRosetta(decisionId: 17)
        let sessions = SteamGameSessions()
        let model = SteamRosettaModel(sessions: sessions, install: install,
                                     readinessAttempts: readinessAttempts, readinessDelay: readinessDelay,
                                     readinessTimeout: readinessTimeout, now: now)
        model.connect(accountId: 42)
        let task = try XCTUnwrap(sessions.launch(game: 42, account: 42, prepare: { game }))
        try await eventually { model.prompt != nil }
        return Fixture(game: game, sessions: sessions, model: model, task: task)
    }

    @MainActor
    private struct Fixture {
        let game: RecoveredGameFixture
        let sessions: SteamGameSessions
        let model: SteamRosettaModel
        let task: Task<Void, Never>

        func finish() async {
            await game.finish()
            await task.value
        }
    }

}
