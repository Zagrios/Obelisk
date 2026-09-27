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
        let resumed = fixture.sessions.fake.rosettaResumes
        XCTAssertEqual(resumed.map(\.decisionID), [17])
        XCTAssertEqual(resumed.map(\.account), [.steam(steamId: 42)])
    }

    @MainActor
    func testLaunchServicesErrorBeforeReadinessStillResumesAutomatically() async throws {
        var openings = 0
        let fixture = try await fixture(install: {
            openings += 1
            throw NSError(domain: NSOSStatusErrorDomain, code: -10669)
        })
        fixture.sessions.fake.failNextRosettaResumes(2)
        fixture.model.install()
        try await eventually { fixture.model.prompt == nil || fixture.model.installationFailed }
        XCTAssertNil(fixture.model.prompt)
        XCTAssertFalse(fixture.model.installationFailed)
        XCTAssertEqual(openings, 1)
        let checks = fixture.sessions.fake.rosettaChecks
        let resumed = fixture.sessions.fake.rosettaResumes
        XCTAssertEqual(checks.count, 3)
        XCTAssertEqual(resumed.count, 1)
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
        let resumed = fixture.sessions.fake.rosettaResumes
        XCTAssertEqual(resumed.count, 1)
    }

    @MainActor
    func testCancellationDuringReadinessWaitPreventsAnyLaterResume() async throws {
        let secondDelay = GameSessionGate<Void>()
        var delays = 0
        let fixture = try await fixture(install: {}, readinessDelay: {
            delays += 1
            if delays == 2 { try await secondDelay.wait() }
        })
        fixture.sessions.fake.failRosetta(SteamGameSessions.rosettaUnavailableKey)
        fixture.model.install()
        try await eventually { await secondDelay.requests == 1 }
        fixture.model.cancel()
        fixture.sessions.fake.failRosetta(nil)
        await secondDelay.resolve(())
        try await eventually { fixture.sessions.fake.declines.count == 1 }
        XCTAssertNil(fixture.model.prompt)
        let resumed = fixture.sessions.fake.rosettaResumes
        let checks = fixture.sessions.fake.rosettaChecks
        XCTAssertTrue(resumed.isEmpty)
        XCTAssertEqual(checks.count, 1)
        XCTAssertTrue(fixture.sessions.isBusy)
    }

    @MainActor
    func testReadinessTimeoutStopsCheckingAndAllowsRetry() async throws {
        let installation = GameSessionGate<Void>()
        var delays = 0
        let fixture = try await fixture(install: { try await installation.wait() }, readinessAttempts: 3,
                                        readinessDelay: { delays += 1; await Task.yield() })
        fixture.sessions.fake.failRosetta(SteamGameSessions.rosettaUnavailableKey)
        fixture.model.install()
        try await eventually { fixture.model.installationFailed }
        XCTAssertEqual(delays, 3)
        XCTAssertFalse(fixture.model.isInstalling)
        XCTAssertNotNil(fixture.model.prompt)
        fixture.sessions.fake.failRosetta(nil)
        fixture.model.install()
        try await eventually { fixture.model.prompt == nil }
        XCTAssertFalse(fixture.model.installationFailed)
        XCTAssertEqual(delays, 4)
        await installation.resolve(())
        let resumed = fixture.sessions.fake.rosettaResumes
        XCTAssertEqual(resumed.count, 1)
    }

    @MainActor
    func testSessionAndCloudErrorsAreNotRetriedAsRosettaUnavailability() async throws {
        for error in ["backend.steam.runtime.invalidSession", "backend.steam.cloud.network"] {
            let fixture = try await fixture(install: {})
            fixture.sessions.fake.failRosetta(error)
            fixture.model.install()
            try await eventually { fixture.model.installationFailed }
            let checks = fixture.sessions.fake.rosettaChecks
            XCTAssertEqual(checks.count, 1)
            XCTAssertNotNil(fixture.model.prompt)
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
        fixture.sessions.fake.failRosetta(SteamGameSessions.rosettaUnavailableKey)
        fixture.model.install()
        try await eventually { fixture.model.installationFailed }
        let checks = fixture.sessions.fake.rosettaChecks
        XCTAssertEqual(delays, 2)
        XCTAssertEqual(checks.count, 1)
        XCTAssertFalse(fixture.model.isInstalling)
    }

    @MainActor
    func testOnlyCurrentAccountsPublishedRequirementPresentsPrompt() async throws {
        let sessions = SteamGameSessions()
        let model = SteamRosettaModel(sessions: sessions, install: { XCTFail("No installation without a requirement") })
        model.connect(accountId: 99)
        sessions.fake.show(.steam(phase: .preparing, cloud: .showing(.checking)))
        try await eventually { sessions.active?.cloud.phase == .checking }
        XCTAssertNil(model.prompt)
        model.install()
        sessions.fake.requireRosetta(decisionID: 17)
        try await eventually { sessions.rosettaPrompt(accountId: 42) != nil }
        XCTAssertNil(model.prompt)
        model.connect(accountId: 42)
        XCTAssertEqual(model.prompt?.decisionId, 17)
        XCTAssertFalse(model.isInstalling)
    }

    @MainActor
    func testInstallationResumesSameSessionOnceAndKeepsAdmissionUntilGameFinishes() async throws {
        let installation = GameSessionGate<Void>()
        let fixture = try await fixture(install: { try await installation.wait() })
        fixture.sessions.fake.failRosetta(SteamGameSessions.rosettaUnavailableKey)
        fixture.model.install()
        fixture.model.install()
        try await eventually { await installation.requests == 1 }
        XCTAssertTrue(fixture.model.isInstalling)
        XCTAssertTrue(fixture.sessions.protects(42))
        let before = fixture.sessions.fake.rosettaResumes
        XCTAssertTrue(before.isEmpty)
        fixture.sessions.fake.failRosetta(nil)
        await installation.resolve(())
        try await eventually { fixture.model.prompt == nil }
        let resumed = fixture.sessions.fake.rosettaResumes
        XCTAssertEqual(resumed.map(\.decisionID), [17])
        XCTAssertEqual(resumed.map(\.account), [.steam(steamId: 42)])
        XCTAssertTrue(fixture.sessions.isBusy, "The launch keeps its Reservation until its game exits")
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
        fixture.sessions.fake.failRosetta(SteamGameSessions.rosettaUnavailableKey)
        fixture.model.install()
        try await eventually { fixture.model.installationFailed }
        XCTAssertNotNil(fixture.model.prompt)
        XCTAssertFalse(fixture.model.isInstalling)
        let before = fixture.sessions.fake.rosettaResumes
        XCTAssertTrue(before.isEmpty)
        fixture.model.install()
        try await eventually { await installation.requests == 1 }
        XCTAssertFalse(fixture.model.installationFailed)
        fixture.sessions.fake.failRosetta(nil)
        await installation.resolve(())
        try await eventually { fixture.model.prompt == nil }
        XCTAssertEqual(attempts, 2)
    }

    @MainActor
    func testSuccessfulHelperCannotOverrideFailedRuntimeRecheck() async throws {
        let fixture = try await fixture(install: {}, readinessAttempts: 1)
        fixture.sessions.fake.failRosetta(SteamGameSessions.rosettaUnavailableKey)
        fixture.model.install()
        try await eventually { fixture.model.installationFailed }
        XCTAssertNotNil(fixture.model.prompt)
        XCTAssertEqual(fixture.sessions.active?.phase, .preparing)
        fixture.sessions.fake.failRosetta(nil)
        fixture.model.install()
        try await eventually { fixture.model.prompt == nil }
        let attempts = fixture.sessions.fake.rosettaChecks
        XCTAssertEqual(attempts.count, 2)
    }

    @MainActor
    func testCancelDuringAppleInstallationIgnoresLateCompletionAndWaitsForTerminalState() async throws {
        let installation = GameSessionGate<Void>()
        var installerReturned = false
        let fixture = try await fixture(install: { try await installation.wait(); installerReturned = true })
        fixture.sessions.fake.failRosetta(SteamGameSessions.rosettaUnavailableKey)
        fixture.model.install()
        try await eventually { await installation.requests == 1 }
        fixture.model.cancel()
        try await eventually { fixture.sessions.fake.declines.count == 1 }
        XCTAssertNil(fixture.model.prompt)
        XCTAssertTrue(fixture.sessions.isBusy)
        XCTAssertTrue(fixture.sessions.protects(42))
        await installation.resolve(())
        try await eventually { installerReturned }
        let resumed = fixture.sessions.fake.rosettaResumes
        XCTAssertTrue(resumed.isEmpty)
        XCTAssertFalse(fixture.model.installationFailed)
    }

    @MainActor
    func testAccountChangeIgnoresLateInstallationAndDoesNotPresentOldRequirement() async throws {
        let installation = GameSessionGate<Void>()
        var installerReturned = false
        let fixture = try await fixture(install: { try await installation.wait(); installerReturned = true })
        fixture.sessions.fake.failRosetta(SteamGameSessions.rosettaUnavailableKey)
        fixture.model.install()
        try await eventually { await installation.requests == 1 }
        fixture.model.connect(accountId: 99)
        try await eventually { fixture.sessions.fake.declines.count == 1 }
        XCTAssertNil(fixture.model.prompt)
        fixture.model.connect(accountId: 42)
        XCTAssertNil(fixture.model.prompt, "Signing back in must not reopen a cancelled attempt")
        await installation.resolve(())
        try await eventually { installerReturned }
        let resumed = fixture.sessions.fake.rosettaResumes
        XCTAssertTrue(resumed.isEmpty)
        XCTAssertFalse(fixture.model.isInstalling)
    }

    @MainActor
    func testSystemCancellationEndsAttemptWithoutInstallationError() async throws {
        for error in [NSError(domain: NSCocoaErrorDomain, code: NSUserCancelledError),
                      NSError(domain: NSOSStatusErrorDomain, code: -128)] {
            let fixture = try await fixture(install: { throw error })
            fixture.model.install()
            try await eventually { fixture.sessions.fake.declines.count == 1 }
            XCTAssertNil(fixture.model.prompt)
            XCTAssertFalse(fixture.model.installationFailed)
            let resumed = fixture.sessions.fake.rosettaResumes
            XCTAssertTrue(resumed.isEmpty)
            }
    }

    @MainActor
    func testLateInstallerCannotAnswerReplacementDecision() async throws {
        let installation = GameSessionGate<Void>()
        var installerReturned = false
        let fixture = try await fixture(install: { try await installation.wait(); installerReturned = true })
        fixture.sessions.fake.failRosetta(SteamGameSessions.rosettaUnavailableKey)
        fixture.model.install()
        try await eventually { await installation.requests == 1 }
        fixture.sessions.fake.requireRosetta(decisionID: 18)
        try await eventually { fixture.model.prompt?.decisionId == 18 }
        await installation.resolve(())
        try await eventually { installerReturned }
        XCTAssertEqual(fixture.model.prompt?.decisionId, 18)
        XCTAssertFalse(fixture.model.isInstalling)
        let resumed = fixture.sessions.fake.rosettaResumes
        XCTAssertTrue(resumed.isEmpty)
    }

    @MainActor
    func testAStaleRosettaPromptCannotCancelTheCurrentRequirement() async throws {
        let fixture = try await fixture(install: {})
        let prompt = try XCTUnwrap(fixture.model.prompt)
        for obsolete in [SteamGameSessions.RosettaPrompt(id: UUID().uuidString, accountId: 42, decisionId: 17),
                         .init(id: prompt.id, accountId: 99, decisionId: 17),
                         .init(id: prompt.id, accountId: 42, decisionId: 16)] {
            fixture.sessions.cancelRosetta(obsolete)
        }
        XCTAssertTrue(fixture.sessions.fake.declines.isEmpty)
        XCTAssertEqual(fixture.model.prompt, prompt)
    }

    @MainActor
    func testPreviousInstallerCompletionCannotResumeOrClearNewAttempt() async throws {
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
        let first = GameSessionState.steam(phase: .preparing, cloud: .showing(.ready))
        sessions.fake.show(first)
        sessions.fake.requireRosetta(decisionID: 17)
        try await eventually { model.prompt != nil }
        model.install()
        try await eventually { await oldInstallation.requests == 1 }
        model.cancel()
        sessions.fake.show(nil)
        let second = GameSessionState.steam(phase: .preparing, cloud: .showing(.ready))
        sessions.fake.show(second)
        sessions.fake.requireRosetta(decisionID: 17)
        try await eventually { model.prompt?.id == second.id }
        model.install()
        try await eventually { await newInstallation.requests == 1 }
        await oldInstallation.resolve(())
        try await eventually { oldInstallerReturned }
        XCTAssertTrue(model.isInstalling)
        XCTAssertTrue(sessions.fake.rosettaResumes.isEmpty)
        await newInstallation.resolve(())
        try await eventually { model.prompt == nil }
        XCTAssertEqual(sessions.fake.rosettaResumes.map(\.session), [second.id])
    }

    @MainActor
    func testRenderRosettaPromptWhenRequested() async throws {
        guard let destination = ProcessInfo.processInfo.environment["SAILOVER_RENDER_PREVIEWS"], !destination.isEmpty else {
            throw XCTSkip("Set SAILOVER_RENDER_PREVIEWS to render the isolated Rosetta prompt")
        }
        let directory = URL(fileURLWithPath: destination, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let fixture = try await fixture(install: { throw CocoaError(.executableNotLoadable) })
        fixture.sessions.fake.failRosetta(SteamGameSessions.rosettaUnavailableKey)
        try await render(fixture.model, to: directory.appending(path: "steam-rosetta-prompt.png"))
        fixture.model.install()
        try await eventually { fixture.model.installationFailed }
        try await render(fixture.model, to: directory.appending(path: "steam-rosetta-retry.png"))
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
        let sessions = SteamGameSessions()
        let model = SteamRosettaModel(sessions: sessions, install: install,
                                     readinessAttempts: readinessAttempts, readinessDelay: readinessDelay,
                                     readinessTimeout: readinessTimeout, now: now)
        model.connect(accountId: 42)
        sessions.fake.show(.steam(phase: .preparing, cloud: .showing(.ready)))
        sessions.fake.requireRosetta(decisionID: 17)
        try await eventually { model.prompt != nil }
        return Fixture(sessions: sessions, model: model)
    }

    /// A Steam launch of game 42 waiting for Rosetta, decision 17.
    @MainActor
    private struct Fixture {
        let sessions: SteamGameSessions
        let model: SteamRosettaModel
    }
}
