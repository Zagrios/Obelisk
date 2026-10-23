import Foundation
import Observation
import XCTest
@testable import Obelisk

/// The Game action of any game of an account, on its page and in the library,
/// over installations the test drives. Steam stands in for every storefront;
/// Rust's own suite covers the rules behind them.
@MainActor
final class GameActionTests: XCTestCase {
    func testThePageLaunchesTheInstallationAndCancelReachesItsPreparation() async throws {
        let fixture = InstallationsFixture()
        let installations = fixture.connect()

        installations.selectedAction?.perform()

        XCTAssertEqual(fixture.backend.actions, [.launch(fixture.receipt)])
        fixture.backend.publish {
            $0.launching = true
            $0.launchGame = 620
            $0.gate = .action(620)
        }
        try await eventually { installations.isLaunching }
        XCTAssertEqual(installations.selectedAction?.title, L10n.string("model.game.launching"))
        XCTAssertEqual(installations.selectedAction?.isAvailable, false)
        XCTAssertEqual(installations.selectedAction?.isCancellable, true)
        installations.selectedAction?.cancel()
        XCTAssertEqual(fixture.backend.actions.last, .cancel(620))
    }

    func testPlaySignsWindowsSteamInFirstWhenItKeepsNoLoginThenLaunches() async throws {
        let fixture = InstallationsFixture(installed: false)
        let installations = fixture.connect(platform: .windows)
        let tf2 = InstallationReceipt.steam(appId: 440, name: "Team Fortress 2",
                                            directory: "/fixture/Games/Steam/steam-440-windows", platform: .windows)
        fixture.backend.steamClient = .windows
        fixture.backend.publish { $0.receipts = [tf2] }
        try await eventually { installations.windowsSteamSignedOut.contains(tf2.id) }

        installations.launch(tf2)
        XCTAssertEqual(installations.windowsSteamSignIn?.receipt, tf2)
        XCTAssertEqual(fixture.backend.actions, [], "Its Windows Steam would wait for a sign-in no one sees")

        // Closed before signing in: nothing launches.
        installations.finishWindowsSteamSignIn(signedIn: false)
        XCTAssertNil(installations.windowsSteamSignIn)
        XCTAssertEqual(fixture.backend.actions, [])

        installations.launch(tf2)
        installations.finishWindowsSteamSignIn(signedIn: true)
        XCTAssertNil(installations.windowsSteamSignIn)
        XCTAssertEqual(fixture.backend.actions, [.launch(tf2)])

        // Obelisk's own Steam client needs no sign-in of its own.
        installations.checkWindowsSteam()
        try await eventually { installations.windowsSteamSignedOut.contains(tf2.id) }
        fixture.backend.steamClient = .obelisk
        installations.checkWindowsSteam()
        try await eventually { installations.windowsSteamSignedOut.isEmpty }
    }

    func testEveryPlaceNamesTheRuntimeALaunchWaitsForAndItsProgress() async throws {
        let fixture = InstallationsFixture()
        let installations = fixture.connect()
        installations.selectedAction?.perform()
        fixture.backend.publish {
            $0.launching = true
            $0.launchGame = 620
            $0.gate = .action(620)
        }
        try await eventually { installations.isLaunching }

        var session = GameSessionState.steam(appID: 620, phase: .preparing, observed: false)
        session.runtimeInstall = RuntimeInstallState(component: .steamComponents, fraction: 0.25)
        fixture.sessions.fake.show(session)
        try await eventually { installations.selectedAction?.stepFraction() == 0.25 }
        for action in try everyPlace(fixture) {
            XCTAssertEqual(action.kind, .step(.runtime(.steamComponents)))
            XCTAssertEqual(action.title, L10n.string("model.game.installingSteamComponents"))
            XCTAssertEqual(action.stepFraction(), 0.25)
        }

        session.runtimeInstall = RuntimeInstallState(component: .voile, fraction: 0.5)
        fixture.sessions.fake.show(session)
        try await eventually { installations.selectedAction?.stepFraction() == 0.5 }
        for action in try everyPlace(fixture) {
            XCTAssertEqual(action.kind, .step(.runtime(.voile)))
            XCTAssertEqual(action.title, L10n.string("model.game.installingVoile"))
            XCTAssertEqual(action.stepFraction(), 0.5)
        }

        session.runtimeInstall = nil
        fixture.sessions.fake.show(session)
        try await eventually { installations.selectedAction?.kind == .waiting(.launch) }
        for action in try everyPlace(fixture) {
            XCTAssertEqual(action.kind, .waiting(.launch))
            XCTAssertEqual(action.title, L10n.string("model.game.launching"))
            XCTAssertNil(action.stepFraction())
        }
    }

    func testAnInstallationLaunchesAndUpdatesBeforeTheStorefrontKnowsWhatItCanInstall() async throws {
        let fixture = InstallationsFixture()
        // The storefront would install the Windows build, once it knows it can.
        let installations = fixture.connect(platform: .windows, installable: false)

        XCTAssertEqual(installations.selectedReceipt?.platform, .mac)
        XCTAssertEqual(installations.selectedAction?.title, L10n.string("model.game.launch"))
        XCTAssertEqual(installations.selectedAction?.isAvailable, true)

        var updated = fixture.receipt
        updated.updateAvailable = true
        fixture.backend.publish { $0.receipts = [updated] }
        try await eventually { installations.selectedReceipt?.updateAvailable == true }
        installations.selectedAction?.perform()
        XCTAssertEqual(fixture.backend.actions, [.update(updated)], "An update keeps the installed build")
    }

    func testANewInstallationIsWhatTheStorefrontOffers() async throws {
        let fixture = InstallationsFixture(installed: false)
        let installations = fixture.connect(platform: .windows)

        XCTAssertEqual(installations.selectedAction?.title, L10n.string("model.game.download"))
        installations.selectedAction?.perform()

        XCTAssertEqual(fixture.backend.actions, [.install(fixture.offer())])
    }

    func testALibraryDownloadNeedsAnOffer() async throws {
        let fixture = InstallationsFixture(installed: false)
        let actions = GameActions(fixture.connect(), in: .library)

        let unknown = actions(fixture.selection(620, installable: false))
        XCTAssertEqual(unknown.kind, .download(.install))
        XCTAssertFalse(unknown.isAvailable)
        unknown.perform()
        XCTAssertEqual(fixture.backend.actions, [], "Nothing is installed before the storefront knows what")

        let offered = actions(fixture.selection(620))
        XCTAssertTrue(offered.isAvailable)
        offered.perform()
        XCTAssertEqual(fixture.backend.actions, [.install(fixture.offer())])
    }

    func testAFailedTransferIsReportedAndResumesWhereItStopped() async throws {
        let fixture = InstallationsFixture(installed: false)
        let installations = fixture.connect()

        fixture.backend.publish {
            $0.resumable = 620
            $0.transferGame = 620
            $0.report = .storefront(key: "backend.steam.content.network")
        }

        try await eventually { installations.selectedAction?.title == L10n.string("model.game.resume") }
        XCTAssertEqual(fixture.reported, [L10n.message("backend.steam.content.network")])
        installations.selectedAction?.perform()
        XCTAssertEqual(fixture.backend.actions, [.install(fixture.offer())], "Rust resumes the Transfer")
    }

    func testAnInterruptedTransferResumesFromTheLibraryAsFromThePage() async throws {
        let fixture = InstallationsFixture(installed: false)
        let installations = fixture.connect()
        fixture.backend.publish {
            $0.resumable = 620
            $0.transferGame = 620
        }
        try await eventually { installations.resumableGame == 620 }

        for action in try everyPlace(fixture) {
            XCTAssertEqual(action.kind, .download(.resume))
            XCTAssertEqual(action.title, L10n.string("model.game.resume"))
            XCTAssertTrue(action.isAvailable)
        }
        GameActions(installations, in: .library)(fixture.selection(620)).perform()
        XCTAssertEqual(fixture.backend.actions, [.install(fixture.offer())], "Rust resumes the Transfer")
    }

    func testThePageOffersOnlyWhatRustsGateLetsStart() async throws {
        let fixture = InstallationsFixture()
        let installations = fixture.connect()
        XCTAssertEqual(installations.selectedAction?.isAvailable, true)
        XCTAssertTrue(installations.canUninstallSelectedGame)
        XCTAssertTrue(installations.canSyncCloud)

        // The game runs: nothing touches it.
        fixture.backend.publish { $0.gate = .session(620) }
        try await eventually { installations.selectedAction?.isAvailable == false }
        XCTAssertFalse(installations.canUninstallSelectedGame)
        XCTAssertFalse(installations.canSyncCloud)

        // Another game runs: this one may go, but not launch or sync.
        fixture.backend.publish { $0.gate = .session(730) }
        try await eventually { installations.canUninstallSelectedGame }
        XCTAssertEqual(installations.selectedAction?.isAvailable, false)
        XCTAssertFalse(installations.canSyncCloud)
        installations.selectedAction?.perform()

        // Another game's Transfer runs: only this game's Cloud may sync.
        fixture.backend.publish { $0.gate = .action(730) }
        try await eventually { installations.canSyncCloud }
        XCTAssertFalse(installations.canUninstallSelectedGame)
        installations.verify(fixture.receipt)
        installations.uninstall(fixture.receipt)
        installations.selectedAction?.perform()
        XCTAssertEqual(fixture.backend.actions, [], "Swift forwards nothing the gate refuses")
    }

    func testANewerBuildTurnsThePagesActionIntoAnUpdate() async throws {
        let fixture = InstallationsFixture()
        let installations = fixture.connect()
        XCTAssertEqual(fixture.backend.calls.filter { $0 == .checkUpdates }.count, 1, "Connecting asks for newer builds")
        XCTAssertEqual(installations.selectedAction?.launches, true)

        var updated = fixture.receipt
        updated.updateAvailable = true
        fixture.backend.publish { $0.receipts = [updated] }
        try await eventually { installations.selectedReceipt?.updateAvailable == true }

        XCTAssertEqual(installations.selectedAction?.title, L10n.string("model.game.update"))
        XCTAssertEqual(installations.selectedAction?.launches, false)
        XCTAssertEqual(installations.selectedAction?.isAvailable, true)
        installations.selectedAction?.perform()
        XCTAssertEqual(fixture.backend.actions, [.update(updated)], "The update replaces the launch")

        // The game runs: its files stay as they are.
        fixture.backend.publish { $0.gate = .session(620) }
        try await eventually { installations.selectedAction?.isAvailable == false }
    }

    func testTheLibraryLaunchesAnInstalledGameWhoseUpdateItsPageOffersAlone() async throws {
        let fixture = InstallationsFixture()
        let installations = fixture.connect()
        var updated = fixture.receipt
        updated.updateAvailable = true
        fixture.backend.publish { $0.receipts = [updated] }
        try await eventually { installations.selectedReceipt?.updateAvailable == true }

        let page = try XCTUnwrap(installations.selectedAction)
        XCTAssertEqual(page.kind, .download(.update))
        XCTAssertEqual(page.title, L10n.string("model.game.update"))
        XCTAssertFalse(page.launches)
        let library = GameActions(installations, in: .library)(fixture.selection(620))
        XCTAssertEqual(library.kind, .launch)
        XCTAssertEqual(library.title, L10n.string("ui.game.play"))
        XCTAssertTrue(library.launches)
        XCTAssertTrue(library.isAvailable)

        page.perform()
        XCTAssertEqual(fixture.backend.actions, [.update(updated)])
        library.perform()
        XCTAssertEqual(fixture.backend.actions.last, .launch(updated), "Its launch installs the update first")
    }

    func testAnotherGamesTransferLeavesThisGameLaunchableAndItsCancelAlone() async throws {
        let fixture = InstallationsFixture()
        let installations = fixture.connect()
        fixture.backend.publish {
            $0.downloading = true
            $0.transferGame = 730
            $0.gate = .transfer(730)
        }
        try await eventually { installations.isDownloading }

        XCTAssertEqual(installations.selectedAction?.isAvailable, true)
        XCTAssertEqual(installations.selectedAction?.title, L10n.string("model.game.launch"))
        XCTAssertEqual(installations.selectedAction?.isCancellable, false)
        installations.selectedAction?.perform()
        XCTAssertEqual(fixture.backend.actions, [.launch(fixture.receipt)])
        installations.cancel(620)
        XCTAssertEqual(fixture.backend.actions.count, 1, "this game has nothing to stop")
        let other = GameActions(installations, in: .library)(fixture.selection(730))
        XCTAssertEqual(other.kind, .transferring(.files))
        other.cancel()
        XCTAssertEqual(fixture.backend.actions.last, .cancel(730))
    }

    func testAQueuedGameWaitsAndItsCancelTakesItOutOfTheQueue() async throws {
        let fixture = InstallationsFixture(installed: false)
        let installations = fixture.connect()
        fixture.backend.publish {
            $0.downloading = true
            $0.transferGame = 730
            $0.queued = [620]
            $0.gate = .transfer(730)
        }
        try await eventually { installations.isQueued(620) }

        XCTAssertEqual(installations.selectedAction?.kind, .queued)
        XCTAssertEqual(installations.selectedAction?.title, L10n.string("model.game.queued"))
        XCTAssertEqual(installations.selectedAction?.isAvailable, false)
        installations.selectedAction?.perform()
        XCTAssertEqual(fixture.backend.actions, [], "a queued game waits")
        installations.selectedAction?.cancel()
        XCTAssertEqual(fixture.backend.actions, [.cancel(620)])
    }

    func testAQueuedTransferAndAPreparingLaunchCanBeCancelledFromTheLibraryAsFromThePage() async throws {
        let fixture = InstallationsFixture()
        let installations = fixture.connect()
        // An update of the game waits in the queue.
        fixture.backend.publish {
            $0.downloading = true
            $0.transferGame = 730
            $0.queued = [620]
            $0.gate = .transfer(730)
        }
        try await eventually { installations.isQueued(620) }
        for action in try everyPlace(fixture) {
            XCTAssertEqual(action.kind, .queued)
            XCTAssertTrue(action.isCancellable)
        }
        GameActions(installations, in: .library)(fixture.selection(620)).cancel()
        XCTAssertEqual(fixture.backend.actions, [.cancel(620)])

        fixture.backend.publish {
            $0.downloading = false
            $0.transferGame = nil
            $0.queued = []
            $0.launching = true
            $0.launchGame = 620
            $0.gate = .action(620)
        }
        try await eventually { installations.isLaunching }
        for action in try everyPlace(fixture) {
            XCTAssertEqual(action.kind, .waiting(.launch))
            XCTAssertTrue(action.isCancellable)
        }
        GameActions(installations, in: .library)(fixture.selection(620)).cancel()
        XCTAssertEqual(fixture.backend.actions, [.cancel(620), .cancel(620)])
    }

    func testALaunchVerifiesItsFilesOnceItsTransfersTurnComesWhereverItShows() async throws {
        let fixture = InstallationsFixture()
        let installations = fixture.connect()
        // Another game downloads: the launch's Transfer waits its turn.
        fixture.backend.publish {
            $0.downloading = true
            $0.transferGame = 730
            $0.queued = [620]
            $0.launching = true
            $0.verifyingBeforeLaunch = true
            $0.launchGame = 620
            $0.gate = .transfer(730)
        }
        try await eventually { installations.isVerifyingBeforeLaunch }
        for action in try everyPlace(fixture) {
            XCTAssertEqual(action.kind, .waiting(.verification))
            XCTAssertEqual(action.title, L10n.string("model.game.verifying"))
            XCTAssertTrue(action.isCancellable)
        }

        fixture.backend.publish {
            $0.transferGame = 620
            $0.queued = []
            $0.gate = .action(620)
        }
        try await eventually { installations.activeTransferGame == 620 }
        for action in try everyPlace(fixture) {
            XCTAssertEqual(action.kind, .transferring(.launch))
            XCTAssertEqual(action.title, L10n.string("model.game.verifying"))
            XCTAssertTrue(action.isCancellable)
        }
    }

    func testAnUpdateQueuedWhileTheGamesSessionEndsShowsAsQueuedWhereverItShows() async throws {
        let fixture = InstallationsFixture()
        let installations = fixture.connect()
        // The game exited: its session settles while its update waits in the queue.
        fixture.sessions.fake.show(.steam(appID: 620, phase: .finalizing))
        fixture.backend.publish {
            $0.queued = [620]
            $0.gate = .session(620)
        }
        try await eventually { installations.isQueued(620) && installations.accountSession?.game == 620 }

        for action in try everyPlace(fixture) {
            XCTAssertEqual(action.kind, .queued)
            XCTAssertEqual(action.title, L10n.string("model.game.queued"))
            XCTAssertFalse(action.isAvailable)
            XCTAssertTrue(action.isCancellable)
        }
    }

    func testCancelStopsAGamesCancellableCloudSyncRatherThanItsLaunch() async throws {
        let fixture = InstallationsFixture()
        let installations = fixture.connect()
        fixture.backend.publish {
            $0.launching = true
            $0.launchGame = 620
            $0.gate = .action(620)
            $0.cloudSync = .launch(620, decision: 17, working: true, cancellable: true)
        }
        try await eventually { installations.cloud.canCancelSynchronization(for: 620) }

        for action in try everyPlace(fixture) {
            XCTAssertEqual(action.kind, .step(.cloudSync))
            XCTAssertEqual(action.title, L10n.string("ui.cloud.sync_short"))
            XCTAssertTrue(action.isCancellable)
        }
        GameActions(installations, in: .library)(fixture.selection(620)).cancel()
        try await eventually { fixture.backend.actions.last == .answer(620, decision: 17, .cancel) }
        XCTAssertFalse(fixture.backend.actions.contains(.cancel(620)), "The launch waits for its sync's answer")
    }

    func testCloudMaintenanceShowsAsAStepInTheLibrary() async throws {
        let fixture = InstallationsFixture()
        let installations = fixture.connect()
        fixture.backend.publish {
            $0.cloudSync = .maintenance(620)
            $0.gate = .closed
        }
        try await eventually { installations.cloud.isMaintaining }

        let maintaining = GameActions(installations, in: .library)(fixture.selection(620))
        XCTAssertEqual(maintaining.kind, .step(.cloudSync))
        XCTAssertEqual(maintaining.title, L10n.string("ui.cloud.sync_short"))
        XCTAssertFalse(maintaining.isAvailable)
        XCTAssertFalse(maintaining.isCancellable)

        fixture.backend.publish {
            $0.cloudSync = nil
            $0.gate = .open
        }
        try await eventually { !installations.cloud.isMaintaining }
        XCTAssertEqual(GameActions(installations, in: .library)(fixture.selection(620)).kind, .launch)
    }

    func testAGameSessionOfAnotherAccountIsNotThisAccountsRunningGame() async throws {
        let fixture = InstallationsFixture()
        let installations = fixture.connect()
        fixture.sessions.fake.show(.steam(appID: 620, accountID: 43))
        fixture.backend.publish { $0.gate = .session(620) }
        try await eventually { fixture.sessions.active?.running == true && installations.gate == .session(620) }

        for action in try everyPlace(fixture) {
            XCTAssertEqual(action.kind, .launch)
            XCTAssertFalse(action.isAvailable)
            action.perform()
        }
        await drainMainQueue(times: 3)
        XCTAssertEqual(fixture.sessions.fake.quits, [], "Another account's game is not this one's to quit")
        XCTAssertEqual(fixture.backend.actions, [])
    }

    // MARK: Quitting the running game

    func testARunningGameIsAskedToQuitThenMayBeForcedOnceItHadTimeToClose() async throws {
        let quitting = FakeQuitting()
        let fixture = InstallationsFixture(quitting: quitting)
        let installations = fixture.connect()
        let running = GameSessionState.steam(appID: 620)
        fixture.sessions.fake.show(running)
        fixture.backend.publish { $0.gate = .session(620) }
        try await eventually { installations.selectedAction?.kind == .running(.quit) }
        XCTAssertEqual(installations.selectedAction?.title, L10n.string("model.game.quit"))
        XCTAssertEqual(installations.selectedAction?.isAvailable, true)

        installations.selectedAction?.perform()
        try await eventually { fixture.sessions.fake.quits == [running.id] && quitting.waiting == 1 }
        XCTAssertEqual(quitting.delays, [.seconds(10)], "The game gets ten seconds to close")
        XCTAssertEqual(quitting.macApps, [[fixture.receipt.directory]], "Its Mac build is asked first")
        XCTAssertEqual(installations.selectedAction?.kind, .running(.closing))
        XCTAssertEqual(installations.selectedAction?.title, L10n.string("model.game.closing"))
        XCTAssertEqual(installations.selectedAction?.isAvailable, false)
        installations.selectedAction?.perform()

        quitting.wake()
        try await eventually { installations.selectedAction?.kind == .running(.forceQuit) }
        let forcing = try XCTUnwrap(installations.selectedAction)
        XCTAssertEqual(forcing.title, L10n.string("model.game.forceQuit"))
        XCTAssertTrue(forcing.isAvailable)
        XCTAssertTrue(forcing.needsConfirmation)
        // Its player confirmed.
        forcing.perform()
        try await eventually { fixture.sessions.fake.forceQuits == [running.id] }
        XCTAssertEqual(fixture.sessions.fake.quits, [running.id], "Quit is asked once")
        XCTAssertEqual(fixture.backend.actions, [], "Ending the game is no installation action")

        // The game exits: its session settles its Cloud saves, then ends.
        fixture.sessions.fake.show(.steam(appID: 620, id: running.id, phase: .finalizing))
        try await eventually { fixture.sessions.quitRequest == nil }
        XCTAssertEqual(installations.selectedAction?.kind, .waiting(.closing))
        XCTAssertEqual(installations.selectedAction?.title, L10n.string("model.game.closing"))
    }

    func testAMacGameIsAskedToQuitAsItsOwnQuitMenuWould() async throws {
        let quitting = FakeQuitting()
        quitting.macAppRuns = true
        let fixture = InstallationsFixture(quitting: quitting)
        let installations = fixture.connect()
        fixture.sessions.fake.show(.steam(appID: 620))
        fixture.backend.publish { $0.gate = .session(620) }
        try await eventually { installations.selectedAction?.kind == .running(.quit) }

        installations.selectedAction?.perform()
        try await eventually { quitting.macApps == [[fixture.receipt.directory]] && quitting.waiting == 1 }
        XCTAssertEqual(installations.selectedAction?.kind, .running(.closing))
        XCTAssertEqual(fixture.sessions.fake.quits, [], "No signal ends it before it could save")
    }

    func testForceQuitWaitsForItsPlayersConfirmationWhereverItShows() async throws {
        let quitting = FakeQuitting()
        let fixture = InstallationsFixture(quitting: quitting)
        let installations = fixture.connect()
        let running = GameSessionState.steam(appID: 620)
        fixture.sessions.fake.show(running)
        fixture.backend.publish { $0.gate = .session(620) }
        try await eventually { installations.selectedAction?.kind == .running(.quit) }
        XCTAssertFalse(try everyPlace(fixture).contains { $0.needsConfirmation })

        installations.selectedAction?.perform()
        try await eventually { quitting.waiting == 1 }
        for action in try everyPlace(fixture) {
            XCTAssertEqual(action.kind, .running(.closing))
            XCTAssertFalse(action.needsConfirmation)
        }

        quitting.wake()
        try await eventually { installations.selectedAction?.kind == .running(.forceQuit) }
        let actions = try everyPlace(fixture)
        for action in actions {
            XCTAssertEqual(action.kind, .running(.forceQuit))
            XCTAssertTrue(action.needsConfirmation)
        }
        XCTAssertEqual(fixture.sessions.fake.forceQuits, [])
        // Its player confirmed in the library.
        actions[1].perform()
        try await eventually { fixture.sessions.fake.forceQuits == [running.id] }
    }

    // MARK: Observation

    func testTheLibrarysActionsChangeWithAnOperationNotWithItsProgress() async throws {
        let fixture = InstallationsFixture()
        let installations = fixture.connect()
        // Another game downloads while Portal 2's launch waits for Voile.
        fixture.backend.publish {
            $0.downloading = true
            $0.transferGame = 730
            $0.transfer = GameTransfer(game: "730", state: .downloading, completedBytes: 10, totalBytes: 100,
                                       networkBytes: 10, contentBytes: 10, currentFile: "", message: "")
            $0.launching = true
            $0.launchGame = 620
            $0.gate = .transfer(730)
        }
        var session = GameSessionState.steam(appID: 620, phase: .preparing, observed: false)
        session.runtimeInstall = RuntimeInstallState(component: .voile, fraction: 0.25)
        fixture.sessions.fake.show(session)
        try await eventually {
            installations.transfer?.completedBytes == 10
                && installations.sessions.active?.runtimeInstall?.fraction == 0.25
        }

        let progress = expectation(description: "No progress tick redraws the library")
        progress.isInverted = true
        readLibrary(fixture) { progress.fulfill() }
        fixture.backend.publish { $0.transfer?.completedBytes = 50 }
        try await eventually { installations.transfer?.completedBytes == 50 }
        session.runtimeInstall = RuntimeInstallState(component: .voile, fraction: 0.5)
        fixture.sessions.fake.show(session)
        try await eventually { installations.sessions.active?.runtimeInstall?.fraction == 0.5 }
        await fulfillment(of: [progress], timeout: 0.2)

        let operation = expectation(description: "A game joining the queue redraws the library")
        readLibrary(fixture) { operation.fulfill() }
        fixture.backend.publish { $0.queued = [440] }
        await fulfillment(of: [operation], timeout: 2)
    }

    /// Reads two games' actions as the library does, and runs `changed` once
    /// what it read changes.
    private func readLibrary(_ fixture: InstallationsFixture, changed: @escaping @Sendable () -> Void) {
        withObservationTracking {
            let actions = GameActions(fixture.installations, in: .library)
            _ = actions(fixture.selection(620))
            _ = actions(fixture.selection(730))
        } onChange: {
            changed()
        }
    }

    /// Portal 2's action on its page, then in the library.
    private func everyPlace(_ fixture: InstallationsFixture) throws -> [GameAction] {
        [try XCTUnwrap(fixture.installations.selectedAction),
         GameActions(fixture.installations, in: .library)(fixture.selection(620))]
    }

    // MARK: Transfer text

    func testLocalVerificationDoesNotPresentManifestBytesAsDownloadedGameData() {
        // A second pass may receive a small manifest, then validate hundreds of MB
        // already on disk without requesting a single game-content chunk.
        let transfer = GameTransfer(game: "620", state: .verifying, completedBytes: 400_000_000, totalBytes: 522_734_781, networkBytes: 253, contentBytes: 0, currentFile: "Game_Data/resources.assets", message: "")
        XCTAssertEqual(transfer.activityTitle, L10n.string("ui.transfer.verifying_files"))
        XCTAssertEqual(transfer.verifiedProgressText, L10n.string("ui.transfer.verified_files", GameFormatting.bytes(400_000_000), GameFormatting.bytes(522_734_781)))
        XCTAssertEqual(transfer.downloadedGameDataText, L10n.string("ui.transfer.no_downloaded_data"))
    }

    func testActualChunkDownloadReportsReceivedGameDataSeparatelyFromVerifiedFiles() {
        let downloading = GameTransfer(game: "620", state: .downloading, completedBytes: 400_000_000, totalBytes: 522_734_781, networkBytes: 12_345 + 253, contentBytes: 12_345, currentFile: "Game_Data/resources.assets", message: "")
        XCTAssertEqual(downloading.activityTitle, L10n.string("ui.transfer.downloading"))
        XCTAssertEqual(downloading.downloadedGameDataText, L10n.string("ui.transfer.downloaded_data", GameFormatting.bytes(12_345)))
        XCTAssertEqual(downloading.verifiedProgressText, L10n.string("ui.transfer.verified_files", GameFormatting.bytes(400_000_000), GameFormatting.bytes(522_734_781)))

        // Returning to disk verification must update the activity while preserving
        // the number of game bytes actually received earlier in this operation.
        var verifying = downloading
        verifying.state = .verifying
        XCTAssertEqual(verifying.activityTitle, L10n.string("ui.transfer.verifying_files"))
        XCTAssertEqual(verifying.downloadedGameDataText, downloading.downloadedGameDataText)
    }

    func testARuntimeDownloadBeforeTheGameNamesTheRuntimeAndCountsNoGameFiles() {
        var transfer = GameTransfer(game: "620", state: .downloading, completedBytes: 5_000_000, totalBytes: 20_000_000, networkBytes: 5_000_000, contentBytes: 0, currentFile: "", message: "model.game.installingVoile")
        XCTAssertEqual(transfer.activityTitle, L10n.string("model.game.installingVoile"))
        XCTAssertEqual(transfer.buttonTitle, L10n.string("model.game.installingVoile"))
        XCTAssertEqual(transfer.progressFraction, 0.25)
        XCTAssertNil(transfer.countedFilesText)
        XCTAssertNil(transfer.downloadedGameDataText)
        XCTAssertEqual(transfer.progressSummary, L10n.string("model.game.installingVoile"))

        // A failed runtime download shows its own failure, as any Transfer's.
        transfer.state = .failed
        transfer.message = "backend.voile.downloadFailed"
        XCTAssertNil(transfer.runtimeTitle)
        XCTAssertEqual(transfer.activityTitle, L10n.string("ui.transfer.failed"))
    }

    func testManifestPreparationAndLocalCompletionDoNotClaimAFileDownload() {
        var transfer = GameTransfer(game: "620", state: .preparing, completedBytes: 0, totalBytes: 0, networkBytes: 253, contentBytes: 0, currentFile: "", message: "")
        XCTAssertEqual(transfer.activityTitle, L10n.string("ui.transfer.checking_updates"))
        XCTAssertNil(transfer.downloadedGameDataText)

        transfer.state = .complete
        transfer.completedBytes = 522_734_781
        transfer.totalBytes = transfer.completedBytes
        XCTAssertEqual(transfer.activityTitle, L10n.string("ui.transfer.up_to_date"))
        XCTAssertNil(transfer.downloadedGameDataText)
    }

    func testTransferProgressIsClamped() {
        let transfer = GameTransfer(game: "one", state: .verifying, completedBytes: 120, totalBytes: 100,
                                    networkBytes: 0, contentBytes: 0, currentFile: "Content/game.pak", message: "")
        XCTAssertEqual(transfer.progressFraction, 1)
        XCTAssertEqual(transfer.downloadedGameDataText, L10n.string("ui.transfer.no_downloaded_data"))
    }
}
