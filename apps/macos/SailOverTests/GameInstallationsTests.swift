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

        XCTAssertEqual(fixture.backend.actions, [.launch(fixture.receipt, verify: true)])
        fixture.backend.publish {
            $0.launching = true
            $0.launchGame = 620
            $0.gate = .action(620)
        }
        try await eventually { installations.isLaunching }
        XCTAssertEqual(installations.primaryActionTitle, L10n.string("model.game.launching"))
        XCTAssertFalse(installations.primaryActionAvailable)
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
        XCTAssertEqual(fixture.backend.actions, [.update(fixture.receipt)], "An update keeps the installed build")
    }

    func testANewInstallationIsWhatTheStorefrontOffers() async throws {
        let fixture = Fixture(game: game, installed: false)
        let installations = fixture.connect(platform: "windows")

        XCTAssertEqual(installations.primaryActionTitle, L10n.string("model.game.download"))
        installations.primaryAction()

        XCTAssertEqual(fixture.backend.actions, [.install(fixture.offer("windows"))])
    }

    func testAFailedTransferIsReportedAndResumesWhereItStopped() async throws {
        let fixture = Fixture(game: game, installed: false)
        let installations = fixture.connect()

        fixture.backend.publish {
            $0.resumable = 620
            $0.transferGame = 620
            $0.report = .storefront(key: "backend.steam.content.network")
        }

        try await eventually { installations.canResumeDownload }
        XCTAssertEqual(fixture.reported, [L10n.message("backend.steam.content.network")])
        XCTAssertEqual(installations.primaryActionTitle, L10n.string("model.game.resume"))
        installations.primaryAction()
        XCTAssertEqual(fixture.backend.actions, [.install(fixture.offer("macos"))], "Rust resumes the Transfer")
    }

    func testThePageOffersOnlyWhatRustsGateLetsStart() async throws {
        let fixture = Fixture(game: game)
        let installations = fixture.connect()
        XCTAssertTrue(installations.primaryActionAvailable)
        XCTAssertTrue(installations.canUninstallSelectedGame)
        XCTAssertTrue(installations.canSyncCloud)

        // The game runs: nothing touches it.
        fixture.backend.publish { $0.gate = .session(620) }
        try await eventually { !installations.primaryActionAvailable }
        XCTAssertFalse(installations.canUninstallSelectedGame)
        XCTAssertFalse(installations.canSyncCloud)

        // Another game runs: this one may go, but not launch or sync.
        fixture.backend.publish { $0.gate = .session(730) }
        try await eventually { installations.canUninstallSelectedGame }
        XCTAssertFalse(installations.primaryActionAvailable)
        XCTAssertFalse(installations.canSyncCloud)
        installations.primaryAction()

        // Another game's Transfer runs: only this game's Cloud may sync.
        fixture.backend.publish { $0.gate = .action(730) }
        try await eventually { installations.canSyncCloud }
        XCTAssertFalse(installations.canUninstallSelectedGame)
        installations.uninstall(fixture.receipt)
        installations.downloadSelected()
        XCTAssertEqual(fixture.backend.actions, [], "Swift forwards nothing the gate refuses")
    }
}

/// One signed-in Steam account with Portal 2 on screen, installed for macOS.
@MainActor
private final class Fixture {
    let sessions = SteamGameSessions()
    let receipt: InstallationReceipt<UInt32>
    private(set) var backend = FakeInstallations<SteamGames>()
    var verifies = false
    private(set) var reported: [String] = []
    private let game: SteamGame
    private let installed: Bool
    private let installations: SteamInstallations

    init(game: SteamGame, installed: Bool = true) {
        self.game = game
        self.installed = installed
        receipt = .steam(appId: 620, name: game.name, directory: "/fixture/Games/Steam/steam-620-macos",
                         platform: "macos", totalBytes: 100)
        installations = SteamInstallations(sessions: sessions, cloud: SteamCloudModel(sessions: sessions))
    }

    /// What Steam offers to install, for `platform`.
    func offer(_ platform: String) -> GameOffer {
        GameOffer.steam(appId: 620, name: game.name, platform: platform, language: "english")
    }

    func connect(platform: String = "macos", installable: Bool = true) -> SteamInstallations {
        let offer = installable ? offer(platform) : nil
        installations.selection = { InstallationSelection(game: 620, platform: platform, offer: offer) }
        backend = FakeInstallations(receipts: installed ? [receipt] : [])
        installations.connect(account: 42, backend: backend, port: InstallationPort(
            verifiesBeforeLaunch: { [unowned self] in verifies },
            report: { [unowned self] error in if let error { reported.append(SteamModel.message(error)) } },
            warn: { _ in }
        ))
        return installations
    }
}
