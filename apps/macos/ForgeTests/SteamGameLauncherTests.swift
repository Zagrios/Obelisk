import Foundation
import XCTest
@testable import Forge

final class SteamGameLauncherTests: XCTestCase {
    @MainActor
    func testMacBundleRepairsDeclaredMachOExecutableWithoutChangingResources() async throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let app = try nonExecutableBundle(root: root, binarySource: URL(fileURLWithPath: "/usr/bin/true"))
        let binary = app.appending(path: "Contents/MacOS/game")
        let resources = app.appending(path: "Contents/Resources")
        try FileManager.default.createDirectory(at: resources, withIntermediateDirectories: true)
        let resource = resources.appending(path: "resource.txt")
        try "resource".write(to: resource, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o644], ofItemAtPath: resource.path)
        // The copied Apple executable is not an Apple-signed application bundle.
        // Sign only this disposable fixture so Gatekeeper does not prompt or kill
        // it after Process.run; the test must also observe its successful exit.
        for arguments in [["--force", "--sign", "-", "--identifier", "com.zagrios.forge.fixture", app.path],
                          ["--verify", "--deep", "--strict", app.path]] {
            let codesign = Process()
            codesign.executableURL = URL(fileURLWithPath: "/usr/bin/codesign")
            codesign.arguments = arguments
            try codesign.run()
            codesign.waitUntilExit()
            guard codesign.terminationStatus == 0 else {
                throw NSError(domain: "ForgeFixtureSigning", code: Int(codesign.terminationStatus))
            }
        }
        XCTAssertFalse(FileManager.default.isExecutableFile(atPath: binary.path))
        let plan = SteamLaunchPlan(appId: 42, platform: "macos", target: LaunchTarget(executable: app.path,
                                   arguments: [], workingDirectory: root.path, isApplicationBundle: true))
        try await SteamGameLauncher.launchForTesting(plan)
        XCTAssertTrue(FileManager.default.isExecutableFile(atPath: binary.path))
        XCTAssertEqual(try FileManager.default.attributesOfItem(atPath: binary.path)[.posixPermissions] as? Int, 0o744)
        XCTAssertEqual(try FileManager.default.attributesOfItem(atPath: resource.path)[.posixPermissions] as? Int, 0o644)
        // Verification may restore the manifest's non-executable permissions.
        try FileManager.default.setAttributes([.posixPermissions: 0o644], ofItemAtPath: binary.path)
        try await EpicGameLauncher.launch(EpicLaunchPlan(appName: "fixture", platform: "Mac", target: LaunchTarget(executable: app.path,
            arguments: [], workingDirectory: root.path, isApplicationBundle: true), ownershipToken: nil))
        XCTAssertTrue(FileManager.default.isExecutableFile(atPath: binary.path))
    }

    @MainActor
    func testMacBundleDoesNotMakeInvalidDeclaredExecutableRunnable() async throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appending(path: "invalid")
        try "not a Mach-O executable".write(to: source, atomically: true, encoding: .utf8)
        let app = try nonExecutableBundle(root: root, binarySource: source)
        let binary = app.appending(path: "Contents/MacOS/game")
        do {
            try await SteamGameLauncher.launchForTesting(SteamLaunchPlan(appId: 42, platform: "macos", target: LaunchTarget(executable: app.path,
                arguments: [], workingDirectory: root.path, isApplicationBundle: true)))
            XCTFail("Invalid content must not be made executable")
        } catch GameLaunchFailure.notExecutable {
            XCTAssertFalse(FileManager.default.isExecutableFile(atPath: binary.path))
        }
    }

    @MainActor
    func testCancellationAfterSpawnWaitsForActualProcessExit() async throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let executable = root.appending(path: "held-game")
        let release = root.appending(path: "release")
        try "#!/bin/sh\nfor n in $(seq 1 300); do [ -e \"$1\" ] && exit 0; sleep 0.01; done\nexit 9\n"
            .write(to: executable, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: executable.path)
        let plan = SteamLaunchPlan(appId: 42, platform: "macos", target: LaunchTarget(executable: executable.path,
            arguments: [release.path], workingDirectory: root.path, isApplicationBundle: false))
        var running = false
        var finished = false
        let operation = Task {
            try await SteamGameLauncher.launchForTesting(plan) { event in
                if case .running = event { running = true }
            }
            finished = true
        }
        let deadline = ContinuousClock.now + .seconds(2)
        while !running && ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertTrue(running)
        operation.cancel()
        try await Task.sleep(for: .milliseconds(50))
        XCTAssertFalse(finished, "Cancellation must not release a live game")
        try Data().write(to: release)
        try await operation.value
        XCTAssertTrue(finished)
    }

    @MainActor
    func testImmediateExitCompletesAndFailedStartNeverReportsRunning() async throws {
        var starts = 0
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let executables = ["/usr/bin/true", "/usr/bin/false", root.appending(path: "missing").path]
        for executable in (0..<10).flatMap({ _ in executables }) {
            let plan = SteamLaunchPlan(appId: 42, platform: "macos", target: LaunchTarget(executable: executable,
                arguments: [], workingDirectory: root.path, isApplicationBundle: false))
            do {
                try await SteamGameLauncher.launchForTesting(plan) { event in
                    if case .running = event { starts += 1 }
                }
                XCTAssertEqual(executable, "/usr/bin/true")
            } catch {
                XCTAssertNotEqual(executable, "/usr/bin/true")
            }
        }
        XCTAssertEqual(starts, 20)
    }

    @MainActor
    func testSessionRejectsStaleEventsAndOnlyItsOwnerCanReleaseIt() throws {
        let sessions = SteamGameSessions()
        let first = try XCTUnwrap(sessions.reserve(appId: 42))
        XCTAssertNil(sessions.reserve(appId: 43))
        sessions.release(UUID())
        XCTAssertTrue(sessions.isBusy)
        sessions.release(first)
        let second = try XCTUnwrap(sessions.reserve(appId: 43))
        sessions.receive(.running, for: first)
        sessions.receive(.host(.lost), for: first)
        sessions.release(first)
        XCTAssertEqual(sessions.active?.id, second)
        XCTAssertEqual(sessions.active?.running, false)
        XCTAssertEqual(sessions.active?.host, .online)
        sessions.release(second)
    }

    private func nonExecutableBundle(root: URL, binarySource: URL) throws -> URL {
        let app = root.appending(path: "Repair.app")
        let contents = app.appending(path: "Contents")
        let binaries = contents.appending(path: "MacOS")
        try FileManager.default.createDirectory(at: binaries, withIntermediateDirectories: true)
        let info = ["CFBundleExecutable": "game", "CFBundlePackageType": "APPL",
                    "CFBundleIdentifier": "com.zagrios.forge.repair.\(UUID().uuidString)"]
        try PropertyListSerialization.data(fromPropertyList: info, format: .xml, options: 0)
            .write(to: contents.appending(path: "Info.plist"))
        let binary = binaries.appending(path: "game")
        try FileManager.default.copyItem(at: binarySource, to: binary)
        try FileManager.default.setAttributes([.posixPermissions: 0o644], ofItemAtPath: binary.path)
        return app
    }

    @MainActor
    func testNativeExecutableReceivesWorkingDirectorySteamIdentityAndLiteralArguments() async throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let executable = root.appending(path: "launch fixture")
        try writeFixture(executable)
        try await verifyLaunch(executable: executable, root: root, isBundle: false)
    }

    @MainActor
    func testMacApplicationRunsItsDeclaredExecutableWithGameWorkingDirectory() async throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let app = root.appending(path: "Fixture.app")
        let contents = app.appending(path: "Contents")
        let binaries = contents.appending(path: "MacOS")
        try FileManager.default.createDirectory(at: binaries, withIntermediateDirectories: true)
        let info: [String: String] = [
            "CFBundleExecutable": "launch-fixture",
            "CFBundleIdentifier": "com.zagrios.forge.fixture.\(UUID().uuidString)",
            "CFBundlePackageType": "APPL",
            "CFBundleVersion": "1",
        ]
        try PropertyListSerialization.data(fromPropertyList: info, format: .xml, options: 0)
            .write(to: contents.appending(path: "Info.plist"))
        try writeFixture(binaries.appending(path: "launch-fixture"))
        try await verifyLaunch(executable: app, root: root, isBundle: true)
    }

    @MainActor
    func testWindowsLaunchExplainsMissingRuntime() async throws {
        let plan = SteamLaunchPlan(appId: 42, platform: "windows", target: LaunchTarget(executable: "/unavailable/game.exe", arguments: [], workingDirectory: "/unavailable", isApplicationBundle: false))
        do {
            try await SteamGameLauncher.launchForTesting(plan)
            XCTFail("A Windows executable requires a compatibility runtime")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("Wine"))
        }
    }

    @MainActor
    func testEpicNativeExecutableUsesLiteralArgumentsWithoutSteamIdentity() async throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let executable = root.appending(path: "epic fixture")
        try writeFixture(executable)
        let captured = root.appending(path: "captured.txt")
        let literal = "spaces ; $(touch unexpected) & 'quotes'"
        let plan = EpicLaunchPlan(appName: "fixture", platform: "Mac", target: LaunchTarget(executable: executable.path,
                                  arguments: [captured.path, literal], workingDirectory: root.path,
                                  isApplicationBundle: false), ownershipToken: nil)
        try await EpicGameLauncher.launch(plan)
        try await waitForFile(captured)
        let lines = try String(contentsOf: captured, encoding: .utf8).components(separatedBy: "\n")
        XCTAssertEqual(URL(fileURLWithPath: lines[0]).resolvingSymlinksInPath(), root)
        XCTAssertEqual(Array(lines.dropFirst()), ["", "", literal, ""])
        XCTAssertFalse(FileManager.default.fileExists(atPath: root.appending(path: "unexpected").path))
    }

    @MainActor
    func testEpicOwnershipTokenIsPrivateReadableByGameAndRemovedAfterExit() async throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let executable = root.appending(path: "ownership fixture")
        let script = """
        #!/bin/sh
        token="${2#-epicovt=}"
        printf '%s\\n' "$token" "$(stat -f %Lp "$token")" "$(stat -f %Lp "$(dirname "$token")")" > "$1"
        cat "$token" >> "$1"
        printf '\\n' >> "$1"
        """
        try script.write(to: executable, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: executable.path)
        let captured = root.appending(path: "captured.txt")
        let token = Data("fixture-ownership-proof".utf8)
        let plan = EpicLaunchPlan(appName: "fixture", platform: "Mac", target: LaunchTarget(executable: executable.path,
                                  arguments: [captured.path], workingDirectory: root.path,
                                  isApplicationBundle: false), ownershipToken: token)
        try await EpicGameLauncher.launch(plan)
        let deadline = ContinuousClock.now + .seconds(3)
        while (try? String(contentsOf: captured, encoding: .utf8).hasSuffix("fixture-ownership-proof\n")) != true {
            guard ContinuousClock.now < deadline else { XCTFail("The fixture did not read its ownership token"); return }
            try await Task.sleep(for: .milliseconds(10))
        }
        let lines = try String(contentsOf: captured, encoding: .utf8).components(separatedBy: "\n")
        XCTAssertEqual(lines[1], "600")
        XCTAssertEqual(lines[2], "700")
        XCTAssertEqual(lines[3], "fixture-ownership-proof")
        let tokenDirectory = URL(fileURLWithPath: lines[0]).deletingLastPathComponent()
        XCTAssertTrue(tokenDirectory.lastPathComponent.hasPrefix("Forge-Epic-"))
        while FileManager.default.fileExists(atPath: tokenDirectory.path) && ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTAssertFalse(FileManager.default.fileExists(atPath: tokenDirectory.path))
    }

    @MainActor
    func testEpicWindowsLaunchExplainsMissingRuntimeBeforeExecuting() async throws {
        let plan = EpicLaunchPlan(appName: "fixture", platform: "Windows", target: LaunchTarget(executable: "/unavailable/game.exe",
                                  arguments: [], workingDirectory: "/unavailable", isApplicationBundle: false),
                                  ownershipToken: Data("fixture-proof".utf8))
        do {
            try await EpicGameLauncher.launch(plan)
            XCTFail("A Windows executable requires a compatibility runtime")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("Wine"))
        }
    }

    @MainActor
    func testEpicProcessFailureRemovesTemporaryOwnershipFiles() async throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let executable = root.appending(path: "invalid interpreter")
        try "#!/definitely-not-installed/forge-fixture-interpreter\n".write(to: executable, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: executable.path)
        let before = try ownershipDirectories()
        let plan = EpicLaunchPlan(appName: "fixture", platform: "Mac", target: LaunchTarget(executable: executable.path,
                                  arguments: [], workingDirectory: root.path, isApplicationBundle: false),
                                  ownershipToken: Data("fixture-proof".utf8))
        do {
            try await EpicGameLauncher.launch(plan)
            XCTFail("An executable with a missing interpreter must fail to start")
        } catch {
            XCTAssertTrue(try ownershipDirectories().subtracting(before).isEmpty)
        }
    }

    @MainActor
    func testCancelledEpicLaunchDoesNotCreateOwnershipFilesOrRunProcess() async throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let executable = root.appending(path: "cancelled fixture")
        try writeFixture(executable)
        let output = root.appending(path: "must-not-exist")
        let before = try ownershipDirectories()
        let plan = EpicLaunchPlan(appName: "fixture", platform: "Mac", target: LaunchTarget(executable: executable.path,
                                  arguments: [output.path, "fixture"], workingDirectory: root.path,
                                  isApplicationBundle: false), ownershipToken: Data("fixture-proof".utf8))
        let operation = Task { try await EpicGameLauncher.launch(plan) }
        operation.cancel()
        do {
            try await operation.value
            XCTFail("A cancelled launch must not start its process")
        } catch is CancellationError {
            XCTAssertFalse(FileManager.default.fileExists(atPath: output.path))
            XCTAssertTrue(try ownershipDirectories().subtracting(before).isEmpty)
        }
    }

    private func ownershipDirectories() throws -> Set<String> {
        Set(try FileManager.default.contentsOfDirectory(atPath: FileManager.default.temporaryDirectory.path)
            .filter { $0.hasPrefix("Forge-Epic-") })
    }

    @MainActor
    private func waitForFile(_ file: URL) async throws {
        let deadline = ContinuousClock.now + .seconds(3)
        while (try? String(contentsOf: file, encoding: .utf8).hasSuffix("\n")) != true {
            guard ContinuousClock.now < deadline else { XCTFail("The launched fixture did not write its process context"); return }
            try await Task.sleep(for: .milliseconds(10))
        }
    }

    @MainActor
    private func verifyLaunch(executable: URL, root: URL, isBundle: Bool) async throws {
        let captured = root.appending(path: "captured.txt")
        let literal = "spaces ; $(touch unexpected) & 'quotes'"
        let plan = SteamLaunchPlan(appId: 42, platform: "macos", target: LaunchTarget(executable: executable.path, arguments: [captured.path, literal], workingDirectory: root.path, isApplicationBundle: isBundle))
        try await SteamGameLauncher.launchForTesting(plan)
        let deadline = ContinuousClock.now + .seconds(3)
        while (try? String(contentsOf: captured, encoding: .utf8).hasSuffix("\n")) != true {
            guard ContinuousClock.now < deadline else {
                XCTFail("The launched fixture did not write its process context")
                return
            }
            try await Task.sleep(for: .milliseconds(10))
        }
        let lines = try String(contentsOf: captured, encoding: .utf8).components(separatedBy: "\n")
        // macOS may expose the same directory as /var/... or /private/var/....
        let observed = try FileManager.default.attributesOfItem(atPath: lines[0])
        let expected = try FileManager.default.attributesOfItem(atPath: root.path)
        XCTAssertEqual(observed[.systemFileNumber] as? NSNumber, expected[.systemFileNumber] as? NSNumber)
        XCTAssertEqual(observed[.systemNumber] as? NSNumber, expected[.systemNumber] as? NSNumber)
        XCTAssertEqual(Array(lines.dropFirst()), ["42", "42", literal, ""])
        XCTAssertFalse(FileManager.default.fileExists(atPath: root.appending(path: "unexpected").path))
    }

    private func temporaryDirectory() throws -> URL {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        return root.resolvingSymlinksInPath()
    }

    private func writeFixture(_ path: URL) throws {
        try "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$SteamAppId\" \"$SteamGameId\" \"$2\" > \"$1\"\n"
            .write(to: path, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: path.path)
    }
}
