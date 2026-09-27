import Foundation
import XCTest
@testable import SailOver

/// The installation actions as the game on screen presents them, over
/// installations the test drives. Steam stands in for every storefront; Rust's
/// own suite covers the rules behind them.
@MainActor
final class GameInstallationsTests: XCTestCase {
    private let game = SteamGame(appId: 620, name: "Portal 2", iconHash: "", lastPlayed: 0, playtimeMinutes: 0)

    func testThePrimaryActionLaunchesTheInstallationAndCancelReachesItsPreparation() async throws {
        let fixture = Fixture(game: game)
        fixture.verifies = true
        let installations = fixture.connect()

        installations.primaryAction()

        XCTAssertEqual(fixture.backend.actions, [.launch(fixture.receipt, verification: fixture.update("macos"),
                                                         verify: true)])
        fixture.backend.publish {
            $0.launching = true
            $0.launchGame = 620
        }
        try await eventually { installations.isLaunching }
        XCTAssertEqual(installations.primaryActionTitle, L10n.string("model.game.launching"))
        installations.cancelDownload()
        XCTAssertEqual(fixture.backend.actions.last, .cancel)
    }

    func testAnInstallationLaunchesAndUpdatesBeforeTheStorefrontKnowsWhatItCanInstall() async throws {
        let fixture = Fixture(game: game)
        // The storefront would install the Windows build, once it knows it can.
        let installations = fixture.connect(platform: "windows", installable: false)

        XCTAssertEqual(installations.selectedReceipt?.platform, "macos")
        XCTAssertEqual(installations.primaryActionTitle, L10n.string("model.game.launch"))
        XCTAssertTrue(installations.primaryActionAvailable)
        installations.downloadSelected()
        XCTAssertEqual(fixture.backend.actions, [.download(fixture.update("macos"))], "An update keeps the installed build")
    }

    func testAFailedTransferIsReportedAndResumesWhereItStopped() async throws {
        let fixture = Fixture(game: game)
        let installations = fixture.connect()
        let request = fixture.update("macos")

        fixture.backend.publish {
            $0.lastDownload = request
            $0.transferGame = 620
            $0.report = .storefront(key: "backend.steam.content.network")
        }

        try await eventually { installations.canResumeDownload }
        XCTAssertEqual(fixture.reported, [L10n.message("backend.steam.content.network")])
        XCTAssertEqual(installations.primaryActionTitle, L10n.string("model.game.launch"))
        installations.downloadSelected()
        XCTAssertEqual(fixture.backend.actions, [.resume])
    }
}

/// One signed-in Steam account with Portal 2 installed for macOS.
@MainActor
private final class Fixture {
    let sessions = SteamGameSessions()
    let receipt: SteamInstallReceipt
    private(set) var backend = FakeInstallations<SteamGames>()
    var verifies = false
    private(set) var reported: [String] = []
    private let game: SteamGame
    private let installations: SteamInstallations

    init(game: SteamGame) {
        self.game = game
        receipt = SteamInstallReceipt(appId: 620, name: game.name, directory: "/fixture/Games/Steam/steam-620-macos",
                                      platform: "macos", language: "english", totalBytes: 100)
        installations = SteamInstallations(directory: URL(fileURLWithPath: "/fixture"), sessions: sessions,
                                           cloud: SteamCloudModel(sessions: sessions))
    }

    /// The Transfer that updates the installation of `platform` in place.
    func update(_ platform: String) -> SteamDownload {
        SteamDownload(game: game, platform: platform, language: "english",
                      directory: URL(fileURLWithPath: receipt.directory).deletingLastPathComponent())
    }

    func connect(platform: String = "macos", installable: Bool = true) -> SteamInstallations {
        let game = game
        installations.selection = {
            InstallationSelection(game: 620, platform: platform, installable: installable, request: { receipt, directory in
                SteamDownload(game: game, platform: receipt?.platform ?? platform, language: receipt?.language ?? "english",
                              directory: directory)
            })
        }
        backend = FakeInstallations(receipts: [receipt])
        installations.connect(account: 42, backend: backend, port: InstallationPort(
            verifiesBeforeLaunch: { [unowned self] in verifies },
            uninstallRequest: { _ in nil },
            report: { [unowned self] error in if let error { reported.append(SteamModel.message(error)) } },
            warn: { _ in }
        ))
        return installations
    }
}
