import Foundation
import XCTest
@testable import Forge

/// The installation actions through their own interface. Steam's adapter
/// stands in for every storefront; the storefront models' suites cover the rest.
@MainActor
final class GameInstallationsTests: XCTestCase {
    private let game = SteamGame(appId: 620, name: "Portal 2", iconHash: "", lastPlayed: 0, playtimeMinutes: 0)

    func testCancellingALaunchStillPreparingNeverHandsTheGameOff() async throws {
        let fixture = try Fixture(game: game)
        let handoff = GameSessionGate<Void>()
        var handedOff = false
        fixture.port.start = { _, proceed in
            try await handoff.wait()
            try proceed()
            handedOff = true
            return RecoveredGameFixture(appID: 620)
        }
        let installations = fixture.connect()

        installations.primaryAction()
        XCTAssertTrue(installations.isLaunching)
        try await eventually { await handoff.requests == 1 }
        installations.cancelDownload()
        await handoff.resolve(())

        try await eventually { !installations.isLaunching && !fixture.sessions.isBusy }
        XCTAssertFalse(handedOff, "A cancelled launch must not start its game")
        XCTAssertEqual(fixture.port.cancellations.values.count, 1)
        XCTAssertTrue(fixture.port.reported.isEmpty)
        XCTAssertTrue(installations.primaryActionAvailable)
    }

    func testAnUninstallRecordsTheRemovalEvenWhenTheReceiptsCannotBeSaved() async throws {
        let fixture = try Fixture(game: game)
        let installations = fixture.connect()
        let receipt = try XCTUnwrap(installations.selectedReceipt)
        // The receipt file can no longer be replaced once the folder is gone.
        try FileManager.default.removeItem(at: fixture.receiptsFile)
        try FileManager.default.createDirectory(at: fixture.receiptsFile, withIntermediateDirectories: false)

        installations.uninstall(receipt)
        XCTAssertTrue(installations.isUninstalling)
        XCTAssertFalse(installations.primaryActionAvailable)

        try await eventually { !installations.isUninstalling }
        XCTAssertFalse(FileManager.default.fileExists(atPath: receipt.directory))
        XCTAssertEqual(fixture.port.archives, 1)
        XCTAssertEqual(fixture.port.removals.values, ["steam-620-macos"])
        XCTAssertEqual(fixture.port.reported.count, 1, "The failed receipt write is still reported")
        XCTAssertTrue(installations.receipts.isEmpty, "A removed folder leaves no receipt")
    }

    func testAnInstallationLaunchesAndUpdatesBeforeTheStorefrontKnowsWhatItCanInstall() async throws {
        let fixture = try Fixture(game: game)
        var updated: SteamDownload?
        fixture.port.download = { request, _ in
            updated = request
            throw CancellationError()
        }
        // The storefront would install the Windows build, once it knows it can.
        let installations = fixture.connect(platform: "windows", installable: false)

        XCTAssertEqual(installations.selectedReceipt?.platform, "macos")
        XCTAssertEqual(installations.primaryActionTitle, L10n.string("model.game.launch"))
        XCTAssertTrue(installations.primaryActionAvailable)
        installations.downloadSelected()
        try await eventually { updated != nil }
        XCTAssertEqual(updated?.platform, "macos", "An update keeps the installed build")
    }

    func testAnInterruptedVerificationKeepsTheReceiptMarked() async throws {
        let fixture = try Fixture(game: game)
        fixture.port.verifies = true
        fixture.port.download = { _, events in
            try events.started()
            throw SteamError.Failure(reason: "backend.steam.content.network")
        }
        let installations = fixture.connect()

        installations.primaryAction()
        try await eventually { !installations.isLaunching }

        XCTAssertEqual(installations.selectedReceipt?.needsVerification, true)
        XCTAssertEqual(installations.transfer?.state, .failed)
        XCTAssertTrue(installations.canResumeDownload)
        XCTAssertEqual(fixture.port.reported, [L10n.message("backend.steam.content.network")])
        XCTAssertFalse(fixture.sessions.isBusy)
    }
}

/// One signed-in Steam account with one installed game.
@MainActor
private final class Fixture {
    let root: URL
    let receiptsFile: URL
    let sessions = SteamGameSessions()
    let port = PortFixture()
    private let game: SteamGame
    private let installations: SteamInstallations

    init(game: SteamGame) throws {
        self.game = game
        root = FileManager.default.temporaryDirectory.resolvingSymlinksInPath()
            .appending(path: "ForgeInstallationsTests-\(UUID().uuidString)")
        let directory = root.appending(path: "Games/Steam/steam-620-macos")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        receiptsFile = root.appending(path: "steam-installations-42.json")
        let receipt = SteamInstallReceipt(appId: 620, name: game.name, directory: directory.path, platform: "macos",
                                          language: "english", totalBytes: 100)
        try InstallationReceipts.save([receipt], to: receiptsFile)
        let cloud = SteamCloudModel(locations: .of(library: root), sessions: sessions, indicators: { _, _, _ in [] })
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
        installations.connect(account: 42, receipts: receiptsFile, port: port.port)
        return installations
    }
}

@MainActor
private final class PortFixture {
    var verifies = false
    var download: @MainActor (SteamDownload, TransferEvents<SteamInstallReceipt>) async throws -> Void = { _, _ in
        throw CancellationError()
    }
    var start: @MainActor (SteamInstallReceipt, @MainActor () throws -> Void) async throws -> any GameSessionProtocol = {
        _, _ in throw CancellationError()
    }
    let cancellations = LockedList<Int>()
    let removals = LockedList<String>()
    private(set) var archives = 0
    private(set) var reported: [String] = []

    var port: InstallationPort<SteamGames> {
        let cancellations = cancellations
        let removals = removals
        return InstallationPort(
            session: { [unowned self] in
                InstallationSession(
                    download: { [unowned self] in try await download($0, $1) },
                    progress: { nil },
                    cancel: { cancellations.append(1) },
                    start: { [unowned self] in try await start($0, $1) }
                )
            },
            verifiesBeforeLaunch: { [unowned self] in verifies },
            uninstall: { [unowned self] receipt in
                GameUninstall(directoryName: "steam-\(receipt.appId)-\(receipt.platform)",
                              archive: { [unowned self] in archives += 1 },
                              removed: { removals.append("steam-\(receipt.appId)-\(receipt.platform)") })
            },
            report: { [unowned self] error in if let error { reported.append(SteamModel.message(error)) } },
            warn: { _ in }
        )
    }
}
