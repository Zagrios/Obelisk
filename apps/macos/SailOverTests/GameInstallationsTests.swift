import Foundation
import XCTest
@testable import SailOver

/// The installation actions as the game on screen presents them, over Rust's
/// installations. Steam's adapter stands in for every storefront; Rust's own
/// suite covers the rules behind them.
@MainActor
final class GameInstallationsTests: XCTestCase {
    private let game = SteamGame(appId: 620, name: "Portal 2", iconHash: "", lastPlayed: 0, playtimeMinutes: 0)

    func testCancellingALaunchStillPreparingNeverHandsTheGameOff() async throws {
        let fixture = try Fixture(game: game)
        let plan = GameSessionGate<Void>()
        fixture.content.launchPlan = { appId, platform, directory in
            try awaitBlocking { try await plan.wait() }
            return SteamLaunchPlan(appId: appId, platform: platform, installDirectory: directory,
                                   target: LaunchTarget(executable: directory, arguments: [], workingDirectory: directory,
                                                        isApplicationBundle: true))
        }
        let installations = fixture.connect()

        installations.primaryAction()
        XCTAssertTrue(installations.isLaunching)
        try await eventually { await plan.requests == 1 }
        installations.cancelDownload()
        await plan.resolve(())

        try await eventually { !installations.isLaunching && !fixture.sessions.isBusy }
        XCTAssertEqual(fixture.content.handOffs, 0, "A cancelled launch must not start its game")
        XCTAssertTrue(fixture.reported.isEmpty)
        XCTAssertTrue(installations.primaryActionAvailable)
    }

    func testAnInstallationLaunchesAndUpdatesBeforeTheStorefrontKnowsWhatItCanInstall() async throws {
        let fixture = try Fixture(game: game)
        // The storefront would install the Windows build, once it knows it can.
        let installations = fixture.connect(platform: "windows", installable: false)

        XCTAssertEqual(installations.selectedReceipt?.platform, "macos")
        XCTAssertEqual(installations.primaryActionTitle, L10n.string("model.game.launch"))
        XCTAssertTrue(installations.primaryActionAvailable)
        installations.downloadSelected()
        try await eventually { !fixture.content.downloads.isEmpty }
        XCTAssertEqual(fixture.content.downloads.first, "macos", "An update keeps the installed build")
    }

    func testAnInterruptedVerificationKeepsTheReceiptMarked() async throws {
        let fixture = try Fixture(game: game)
        fixture.verifies = true
        fixture.content.download = { _, _ in throw SteamError.Failure(reason: "backend.steam.content.network") }
        let installations = fixture.connect()

        installations.primaryAction()
        try await eventually { !installations.isLaunching }

        XCTAssertEqual(installations.selectedReceipt?.needsVerification, true)
        XCTAssertEqual(installations.transfer?.state, .failed)
        XCTAssertTrue(installations.canResumeDownload)
        XCTAssertEqual(fixture.reported, [L10n.message("backend.steam.content.network")])
        XCTAssertFalse(fixture.sessions.isBusy)
    }
}

/// A Steam session whose calls the test scripts.
private final class ClosureSteamContent: SteamContent, @unchecked Sendable {
    private let lock = NSLock()
    var download: @Sendable (UInt32, String) throws -> Void = { _, _ in throw SteamError.Cancelled }
    var launchPlan: @Sendable (UInt32, String, String) throws -> SteamLaunchPlan = { _, _, _ in
        throw SteamError.Failure(reason: "backend.steam.runtime.startFailed")
    }
    private var _downloads: [String] = []
    private var _handOffs = 0
    var downloads: [String] { lock.withLock { _downloads } }
    var handOffs: Int { lock.withLock { _handOffs } }

    func download(appId: UInt32, platform: String, language: String, directory: String) throws -> SteamInstallation {
        lock.withLock { _downloads.append(platform) }
        try download(appId, platform)
        throw SteamError.Cancelled
    }
    func downloadProgress() -> GameTransfer? { nil }
    func cancelDownload() {}
    func launchPlan(appId: UInt32, platform: String, directory: String) throws -> SteamLaunchPlan {
        try launchPlan(appId, platform, directory)
    }
    func handOff(plan: SteamLaunchPlan) throws -> any GameSupervisor {
        lock.withLock { _handOffs += 1 }
        return RecoveredGameFixture(appID: plan.appId)
    }
    func archiveCloud(config: SteamCloudConfiguration) throws {}
    func maintainCloud(appId: UInt32, config: SteamCloudConfiguration?, sync: CloudSync) throws {}
}

/// One signed-in Steam account with one installed game.
@MainActor
private final class Fixture {
    let root: URL
    let receiptsFile: URL
    let sessions = SteamGameSessions()
    let content = ClosureSteamContent()
    var verifies = false
    private(set) var reported: [String] = []
    private let game: SteamGame
    private let installations: SteamInstallations

    init(game: SteamGame) throws {
        self.game = game
        root = FileManager.default.temporaryDirectory.resolvingSymlinksInPath()
            .appending(path: "SailOverInstallationsTests-\(UUID().uuidString)")
        let directory = root.appending(path: "Games/Steam/steam-620-macos")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        receiptsFile = root.appending(path: "steam-installations-42.json")
        let receipt = SteamInstallReceipt(appId: 620, name: game.name, directory: directory.path, platform: "macos",
                                          language: "english", totalBytes: 100)
        try JSONEncoder().encode([receipt]).write(to: receiptsFile)
        let cloud = SteamCloudModel(sessions: sessions)
        installations = SteamInstallations(directory: root, sessions: sessions, cloud: cloud)
    }

    deinit { try? FileManager.default.removeItem(at: root) }

    func connect(platform: String = "macos", installable: Bool = true) -> SteamInstallations {
        let game = game
        installations.selection = {
            InstallationSelection(game: 620, platform: platform, installable: installable, request: { receipt, directory in
                SteamDownload(game: game, platform: receipt?.platform ?? platform, language: receipt?.language ?? "english",
                              directory: directory)
            })
        }
        let backend = SteamInstallationsBackend(try! SteamAccountInstallations.withContent(
            content: content, accountId: 42, sessions: sessions.core, receipts: receiptsFile.path,
            locations: .of(library: root)
        ))
        installations.connect(account: 42, backend: backend, port: InstallationPort(
            verifiesBeforeLaunch: { [unowned self] in verifies },
            uninstallRequest: { receipt in
                SteamDownload(appId: receipt.appId, name: receipt.name, platform: receipt.platform,
                              language: receipt.language,
                              directory: URL(fileURLWithPath: receipt.directory).deletingLastPathComponent())
            },
            report: { [unowned self] error in if let error { reported.append(SteamModel.message(error)) } },
            warn: { _ in }
        ))
        return installations
    }
}
