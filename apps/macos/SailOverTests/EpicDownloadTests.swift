import Foundation
import XCTest
@testable import SailOver

/// Epic's installation actions as the game page presents them, over
/// installations the test drives. Rust's own suite covers Transfers, launches,
/// uninstalls and the credentials each account call saves.
@MainActor
final class EpicDownloadTests: XCTestCase {
    func testDownloadPrefersMacAndShowsTheTransferUntilItsReceipt() async throws {
        let fixture = try await fixture(game: game("native", macos: true))

        fixture.model.installations.downloadSelected()

        XCTAssertEqual(fixture.backend.actions, [.install(fixture.offer("Mac"))])
        fixture.backend.publish {
            $0.downloading = true
            $0.gate = .action(fixture.game.libraryID)
            $0.transferGame = fixture.game.libraryID
            $0.transfer = GameTransfer(game: "native", state: .downloading, completedBytes: 40, totalBytes: 100,
                                       networkBytes: 20, contentBytes: 40, currentFile: "Content/game.pak", message: "")
        }
        try await eventually { fixture.model.installations.isDownloading }
        XCTAssertEqual(fixture.model.installations.activeTransferGame, fixture.game.libraryID)
        XCTAssertEqual(fixture.model.installations.transfer(for: fixture.game.libraryID)?.completedBytes, 40)
        XCTAssertFalse(fixture.model.installations.primaryActionAvailable)
        fixture.model.installations.downloadSelected()
        XCTAssertEqual(fixture.backend.actions.count, 1, "One Transfer at a time")

        fixture.backend.publish {
            $0.downloading = false
            $0.gate = .open
            $0.transfer?.state = .complete
            $0.receipts = [fixture.receipt("native", platform: "Mac")]
        }
        try await eventually { !fixture.model.installations.isDownloading }
        XCTAssertEqual(fixture.model.installations.selectedReceipt?.name, "Game native")
        XCTAssertEqual(fixture.model.installations.transfer?.state, .complete)
        XCTAssertEqual(fixture.model.installations.primaryActionTitle, L10n.string("model.game.launch"))
        XCTAssertFalse(fixture.model.installations.canResumeDownload)
        XCTAssertNil(fixture.model.errorMessage)
    }

    func testAWindowsBuildDownloadsAndAGameWithoutASupportedBuildIsDisabled() async throws {
        let fixture = try await fixture(game: game("windows"))
        XCTAssertTrue(fixture.model.installations.primaryActionAvailable)
        fixture.model.installations.downloadSelected()
        guard case let .install(offer) = fixture.backend.actions.first else { return XCTFail("No Transfer") }
        XCTAssertEqual(offer.platform, "Windows")

        var unsupported = game("unsupported")
        unsupported.windows = false
        fixture.model.open(unsupported)
        XCTAssertFalse(fixture.model.installations.primaryActionAvailable)
        fixture.model.installations.downloadSelected()
        XCTAssertEqual(fixture.backend.actions.count, 1)
    }

    func testAFailedTransferShowsTheStorefrontsErrorAndResumes() async throws {
        let fixture = try await fixture()
        fixture.backend.publish {
            $0.resumable = fixture.game.libraryID
            $0.transferGame = fixture.game.libraryID
            $0.report = .storefront(key: "backend.epic.network")
        }

        try await eventually { fixture.model.errorMessage != nil }
        XCTAssertEqual(fixture.model.errorMessage, L10n.string("model.epic.network"))
        XCTAssertTrue(fixture.model.installations.canResumeDownload)
        XCTAssertEqual(fixture.model.installations.primaryActionTitle, L10n.string("model.game.resume"))
        fixture.model.installations.downloadSelected()
        XCTAssertEqual(fixture.backend.actions, [.install(fixture.offer("Windows"))], "Rust resumes the Transfer")
        XCTAssertNil(fixture.model.errorMessage, "An action clears the last failure")
    }

    func testChangingSelectionKeepsDownloadAttachedToOriginalGame() async throws {
        let fixture = try await fixture()
        fixture.model.installations.downloadSelected()
        fixture.backend.publish {
            $0.downloading = true
            $0.gate = .action(fixture.game.libraryID)
            $0.transferGame = fixture.game.libraryID
        }
        try await eventually { fixture.model.installations.isDownloading }
        let other = game("other")
        fixture.model.open(other)
        XCTAssertEqual(fixture.model.installations.activeTransferGame, fixture.game.libraryID)
        XCTAssertNil(fixture.model.installations.selectedReceipt)
        XCTAssertFalse(fixture.model.installations.primaryActionAvailable)

        fixture.backend.publish {
            $0.downloading = false
            $0.gate = .open
            $0.receipts = [fixture.receipt("one", platform: "Windows")]
        }
        try await eventually { !fixture.model.installations.isDownloading }
        XCTAssertEqual(fixture.model.selectedGame?.libraryID, other.libraryID)
        XCTAssertNil(fixture.model.installations.selectedReceipt)
        XCTAssertEqual(fixture.model.installations.receipts.first?.gameID, fixture.game.libraryID)
        XCTAssertEqual(fixture.model.installations.primaryActionTitle, L10n.string("model.game.download"))
        XCTAssertTrue(fixture.model.installations.primaryActionAvailable)
    }

    func testTransferProgressIsClamped() {
        let transfer = GameTransfer(game: "one", state: .verifying, completedBytes: 120, totalBytes: 100,
                                    networkBytes: 0, contentBytes: 0, currentFile: "Content/game.pak", message: "")
        XCTAssertEqual(transfer.progressFraction, 1)
        XCTAssertEqual(transfer.downloadedGameDataText, L10n.string("ui.transfer.no_downloaded_data"))
    }

    func testAnInstalledBuildAlwaysVerifiesBeforeItLaunchesAndShowsEachStep() async throws {
        let fixture = try await fixture(game: game("native", macos: true), installed: "Mac")
        let receipt = try XCTUnwrap(fixture.model.installations.selectedReceipt)

        fixture.model.installations.primaryAction()

        XCTAssertEqual(fixture.backend.actions, [.launch(receipt, verify: true)])
        fixture.backend.publish {
            $0.launching = true
            $0.gate = .action(fixture.game.libraryID)
            $0.verifyingBeforeLaunch = true
            $0.launchGame = fixture.game.libraryID
        }
        try await eventually { fixture.model.installations.isVerifyingBeforeLaunch }
        XCTAssertEqual(fixture.model.installations.primaryActionTitle, L10n.string("model.game.verifying"))
        fixture.model.installations.primaryAction()
        XCTAssertEqual(fixture.backend.actions.count, 1, "A launch still preparing is not repeated")
        fixture.backend.publish { $0.verifyingBeforeLaunch = false }
        try await eventually { !fixture.model.installations.isVerifyingBeforeLaunch }
        XCTAssertEqual(fixture.model.installations.primaryActionTitle, L10n.string("model.game.launching"))

        fixture.sessions.fake.show(.epic(account: Self.account, namespace: fixture.game.namespace,
                                         appName: fixture.game.appName))
        fixture.backend.publish {
            $0.launching = false
            $0.launchGame = nil
            $0.gate = .session(fixture.game.libraryID)
        }
        try await eventually { fixture.model.installations.selectedSession?.running == true }
        XCTAssertEqual(fixture.model.installations.primaryActionTitle, L10n.string("model.game.running"))
        XCTAssertFalse(fixture.model.installations.primaryActionAvailable)
    }

    func testChangingSelectionDoesNotRetargetAPendingLaunch() async throws {
        let fixture = try await fixture(installed: "Windows")
        fixture.model.installations.primaryAction()
        fixture.backend.publish {
            $0.launching = true
            $0.launchGame = fixture.game.libraryID
            $0.gate = .action(fixture.game.libraryID)
        }
        try await eventually { fixture.model.installations.isLaunching }

        fixture.model.open(game("other"))

        XCTAssertFalse(fixture.model.installations.primaryActionAvailable)
        fixture.backend.publish {
            $0.launching = false
            $0.launchGame = nil
            $0.gate = .open
        }
        try await eventually { !fixture.model.installations.isLaunching }
        XCTAssertEqual(fixture.model.selectedGame?.appName, "other")
        XCTAssertNil(fixture.model.installations.selectedReceipt)
        XCTAssertTrue(fixture.model.installations.primaryActionAvailable)
    }

    func testUninstallRemovesTheInstalledBuildAndRestoresTheDownloadAction() async throws {
        let fixture = try await fixture(installed: "Windows")
        let receipt = try XCTUnwrap(fixture.model.installations.selectedReceipt)
        XCTAssertTrue(fixture.model.installations.canUninstallSelectedGame)

        fixture.model.installations.uninstall(receipt)

        XCTAssertEqual(fixture.backend.actions, [.uninstall(receipt)])
        fixture.backend.publish {
            $0.uninstalling = true
            $0.uninstallGame = fixture.game.libraryID
            $0.gate = .action(fixture.game.libraryID)
        }
        try await eventually { fixture.model.installations.isUninstalling }
        XCTAssertEqual(fixture.model.installations.primaryActionTitle, L10n.string("model.game.uninstalling"))
        XCTAssertFalse(fixture.model.installations.primaryActionAvailable)
        XCTAssertFalse(fixture.model.installations.canUninstallSelectedGame)
        fixture.model.installations.uninstall(receipt)
        XCTAssertEqual(fixture.backend.actions.count, 1)

        fixture.backend.publish {
            $0.uninstalling = false
            $0.uninstallGame = nil
            $0.receipts = []
            $0.gate = .open
        }
        try await eventually { fixture.model.installations.receipts.isEmpty }
        XCTAssertEqual(fixture.model.installations.primaryActionTitle, L10n.string("model.game.download"))
        XCTAssertTrue(fixture.model.installations.primaryActionAvailable)
    }

    func testAFailedUninstallShowsItsReason() async throws {
        let fixture = try await fixture(installed: "Windows")
        fixture.backend.publish { $0.report = .uninstall(reason: "model.game.installationBusy") }
        try await eventually { fixture.model.errorMessage != nil }
        XCTAssertNotNil(fixture.model.installations.selectedReceipt)
    }

    func testUninstallRejectsAStaleReceiptAndAnActiveDownload() async throws {
        let fixture = try await fixture(installed: "Windows")
        let receipt = try XCTUnwrap(fixture.model.installations.selectedReceipt)
        let forged = InstallationReceipt(id: receipt.id, gameID: receipt.gameID, name: receipt.name,
                                         directory: fixture.directory.path, platform: receipt.platform,
                                         totalBytes: receipt.totalBytes)
        fixture.model.installations.uninstall(forged)
        XCTAssertEqual(fixture.backend.actions, [])

        fixture.backend.publish {
            $0.downloading = true
            $0.transferGame = fixture.game.libraryID
            $0.gate = .action(fixture.game.libraryID)
        }
        try await eventually { fixture.model.installations.isDownloading }
        fixture.model.installations.uninstall(receipt)
        XCTAssertEqual(fixture.backend.actions, [])
        XCTAssertFalse(fixture.model.installations.canUninstallSelectedGame)
    }

    /// An account with `owned` on screen, installed for `installed`.
    private func fixture(game owned: EpicGame? = nil, installed: String? = nil) async throws -> EpicDownloadFixture {
        let owned = owned ?? game("one")
        let directory = FileManager.default.temporaryDirectory
            .appending(path: "SailOverEpicDownloadTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        let credentials = EpicCredentials(accountId: Self.account, displayName: Self.account, refreshToken: "current")
        let receipts = installed.map { [EpicDownloadFixture.receipt(owned, directory: directory, platform: $0)] } ?? []
        let backend = FakeInstallations<EpicGames>(receipts: receipts)
        let sessions = EpicGameSessions()
        let model = EpicModel(directory: directory, keychain: EpicMemoryStore(credentials),
                              restore: { _ in EpicTestSession(credentials: credentials, games: [owned]) },
                              language: "fr-FR", gameSessions: sessions, installations: EpicModel.installations(backend))
        await model.start()
        model.open(owned)
        try await eventually { !model.isLoadingGameDetails }
        return EpicDownloadFixture(directory: directory, model: model, backend: backend, sessions: sessions, game: owned)
    }

    private func game(_ app: String, macos: Bool = false) -> EpicGame {
        EpicGame(appName: app, namespace: "download-fixture", catalogItemId: "catalog-\(app)", title: "Game \(app)",
                 description: "", imageUrl: nil, headerImageUrl: nil, developer: "", windows: true, macos: macos)
    }

    private static let account = "0123456789abcdef0123456789abcdef"
}

@MainActor
private struct EpicDownloadFixture {
    let directory: URL
    let model: EpicModel
    let backend: FakeInstallations<EpicGames>
    let sessions: EpicGameSessions
    let game: EpicGame

    /// What Epic offers to install of the game on screen, for `platform`.
    func offer(_ platform: String) -> GameOffer {
        GameOffer.epic(namespace: game.namespace, catalogItemId: game.catalogItemId, appName: game.appName,
                  title: game.title, platform: platform)
    }

    func receipt(_ app: String, platform: String) -> InstallationReceipt<String> {
        var owned = game
        owned.appName = app
        return Self.receipt(owned, directory: directory, platform: platform)
    }

    static func receipt(_ game: EpicGame, directory: URL, platform: String) -> InstallationReceipt<String> {
        InstallationReceipt.epic(namespace: game.namespace, appName: game.appName, name: game.title,
                           directory: directory.appending(path: "Games/Epic/epic-\(game.appName)-\(platform)").path,
                           platform: platform, totalBytes: 100)
    }
}
