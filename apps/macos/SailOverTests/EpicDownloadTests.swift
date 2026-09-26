import Darwin
import Foundation
import XCTest
@testable import SailOver

@MainActor
final class EpicDownloadTests: XCTestCase {
    /// Resolves production launch metadata and account authorization without
    /// updating installed files or executing a game process.
    func testLiveEpicLaunchPlanSmokeWhenRequested() async throws {
        executionTimeAllowance = 90
        guard ProcessInfo.processInfo.environment["SAILOVER_EPIC_LAUNCH_SMOKE"] == "1" else {
            throw XCTSkip("Set SAILOVER_EPIC_LAUNCH_SMOKE=1 to verify an installed game's launch plan")
        }
        let keychain = EpicKeychain()
        guard let saved = try await keychain.load() else { throw XCTSkip("No saved Epic session is available") }
        let session = try await EpicSession.restore(credentials: saved)
        let restored = try await session.credentials()
        try await keychain.save(restored)
        do {
            let component = restored.accountId.utf8.map { String(format: "%02x", $0) }.joined()
            let receiptsURL = AppIdentity.libraryDirectory.appending(path: "epic-installations-\(component).json")
            guard FileManager.default.fileExists(atPath: receiptsURL.path) else {
                throw XCTSkip("The saved Epic account has no installation receipts")
            }
            let receipts = try JSONDecoder().decode([EpicInstallReceipt].self, from: Data(contentsOf: receiptsURL))
                .sorted { $0.id < $1.id }
            try await keychain.save(session.prepare())
            let games = try await session.games()
            try await keychain.save(session.credentials())
            guard let receipt = receipts.first(where: { receipt in
                FileManager.default.fileExists(atPath: receipt.directory)
                    && games.contains(where: { $0.namespace == receipt.namespace && $0.appName == receipt.appName })
            }), let game = games.first(where: { $0.namespace == receipt.namespace && $0.appName == receipt.appName }) else {
                throw XCTSkip("The saved Epic account has no installed game matching its library")
            }
            let plan = try await session.launchPlan(namespace: game.namespace, catalogItemId: game.catalogItemId,
                                                    appName: game.appName, platform: receipt.platform,
                                                    directory: receipt.directory, language: "en")
            try await keychain.save(session.credentials())
            XCTAssertTrue(FileManager.default.fileExists(atPath: plan.target.executable))
            XCTAssertTrue(plan.target.arguments.contains("-epicapp=\(game.appName)"))
            XCTAssertTrue(plan.target.arguments.contains("-epicuserid=\(restored.accountId)"))
            XCTAssertTrue(plan.target.arguments.contains("-AUTH_TYPE=exchangecode"))
            XCTAssertTrue(plan.target.arguments.contains { $0.hasPrefix("-AUTH_PASSWORD=") && $0.count > "-AUTH_PASSWORD=".count })
            print("Epic launch-plan smoke: \(game.title), platform=\(plan.platform), executable=\(URL(fileURLWithPath: plan.target.executable).lastPathComponent), argumentCount=\(plan.target.arguments.count)")
        } catch {
            try await keychain.save(session.credentials())
            throw error
        }
    }

    /// Opt-in only: validates a real manifest and verified game content, then
    /// cancels and removes this test's isolated installation directory.
    func testLiveEpicDownloadSmokeWhenRequested() async throws {
        executionTimeAllowance = 90
        guard ProcessInfo.processInfo.environment["SAILOVER_EPIC_DOWNLOAD_SMOKE"] == "1" else {
            throw XCTSkip("Set SAILOVER_EPIC_DOWNLOAD_SMOKE=1 to verify content with the saved Epic session")
        }
        let keychain = EpicKeychain()
        guard let credentials = try await keychain.load() else {
            throw XCTSkip("No saved Epic session is available")
        }
        let session = try await EpicSession.restore(credentials: credentials)
        // Restoration rotates the token. Save it before any other request.
        try await keychain.save(session.credentials())
        let directory = temporaryDirectory()
        var download: Task<EpicInstallation, Error>?
        defer { try? FileManager.default.removeItem(at: directory) }
        do {
            try await keychain.save(session.prepare())
            let games = try await session.games().sorted { $0.libraryID < $1.libraryID }
            try await keychain.save(session.credentials())
            guard let game = games.first(where: \.macos) ?? games.first(where: \.windows) else {
                throw XCTSkip("The saved Epic account has no supported Mac or Windows game")
            }
            let platform = game.preferredPlatform
            print("Epic download smoke: \(game.title), platform=\(platform)")
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let operation = Task {
                try await session.download(namespace: game.namespace, catalogItemId: game.catalogItemId,
                                           appName: game.appName, platform: platform, directory: directory.path)
            }
            download = operation
            let deadline = ContinuousClock.now + .seconds(55)
            var observedVerifiedContent = false
            var reachedDeadline = false
            while true {
                if let progress = session.downloadProgress() {
                    if progress.completedBytes > 0 && progress.contentBytes > 0 {
                        observedVerifiedContent = true
                        session.cancelDownload()
                        break
                    }
                    if progress.state == .complete || progress.state == .failed || progress.state == .cancelled { break }
                }
                if ContinuousClock.now >= deadline {
                    reachedDeadline = true
                    session.cancelDownload()
                    break
                }
                try await Task.sleep(for: .milliseconds(75))
            }
            let result = await operation.result
            download = nil
            try await keychain.save(session.credentials())
            let progress = session.downloadProgress()
            print("Epic download smoke: state=\(String(describing: progress?.state)), verifiedBytes=\(progress?.completedBytes ?? 0), contentBytes=\(progress?.contentBytes ?? 0), networkBytes=\(progress?.networkBytes ?? 0), deadline=\(reachedDeadline)")
            let completed: Bool
            switch result {
            case let .success(installation): completed = installation.totalBytes > 0
            case let .failure(error):
                completed = false
                if progress?.state != .cancelled {
                    XCTFail("The real Epic download failed: \(EpicModel.message(error))")
                    throw error
                }
            }
            XCTAssertTrue(observedVerifiedContent || completed,
                          "The real Epic download must verify downloaded game content before cancellation or complete a nonempty game")
            XCTAssertFalse(reachedDeadline, "The real Epic download did not verify content within 55 seconds")
        } catch {
            session.cancelDownload()
            if let download { _ = await download.result }
            // Failure may also have refreshed the account. Never revoke it here.
            try await keychain.save(session.credentials())
            throw error
        }
    }

    func testDownloadPrefersMacReservesTransferAndSavesRotatedCredentials() async throws {
        let fixture = try await fixture(game: game("native", macos: true))
        defer { fixture.cleanUp() }
        await fixture.session.holdNextDownload()
        await fixture.session.rotateOnPrepare(credentials("prepared"))

        fixture.model.installations.downloadSelected()
        fixture.model.installations.downloadSelected()
        XCTAssertTrue(fixture.model.installations.isDownloading)
        XCTAssertEqual(fixture.model.installations.activeTransferGame, fixture.game.libraryID)
        XCTAssertFalse(fixture.model.installations.primaryActionAvailable)
        try await eventually { await fixture.session.waitingForDownload }
        let requests = await fixture.session.downloads
        XCTAssertEqual(requests.count, 1)
        XCTAssertEqual(requests.first?.namespace, fixture.game.namespace)
        XCTAssertEqual(requests.first?.catalogItemId, fixture.game.catalogItemId)
        XCTAssertEqual(requests.first?.appName, "native")
        XCTAssertEqual(requests.first?.platform, "Mac")
        XCTAssertEqual(requests.first?.directory, fixture.directory.appending(path: "Games/Epic").path)
        let prepared = await fixture.store.value
        XCTAssertEqual(prepared?.refreshToken, "prepared")

        try await eventually { fixture.model.installations.transfer?.state == .downloading }
        XCTAssertEqual(fixture.model.installations.transfer?.completedBytes, 40)
        await fixture.session.completeDownload(rotated: credentials("rotated-download"))
        try await eventually { !fixture.model.installations.isDownloading }
        let stored = await fixture.store.value
        XCTAssertEqual(stored?.refreshToken, "rotated-download")
        XCTAssertEqual(fixture.model.installations.selectedReceipt?.appName, "native")
        XCTAssertEqual(fixture.model.installations.selectedReceipt?.name, "Game native")
        XCTAssertEqual(fixture.model.installations.transfer?.state, .complete)
        XCTAssertEqual(fixture.model.installations.primaryActionTitle, L10n.string("model.game.launch"))
        XCTAssertFalse(fixture.model.installations.canResumeDownload)
        XCTAssertNil(fixture.model.errorMessage)
        let receipt = try XCTUnwrap(receiptFiles(in: fixture.directory).first)
        let text = try String(contentsOf: receipt, encoding: .utf8)
        XCTAssertFalse(text.contains("rotated-download"))
        XCTAssertFalse(text.contains("prepared"))
    }

    func testWindowsDownloadDoesNotRequireOptionalDetailsAndUnsupportedGameIsDisabled() async throws {
        let fixture = try await fixture(game: game("windows"))
        defer { fixture.cleanUp() }
        XCTAssertTrue(fixture.model.installations.primaryActionAvailable)
        fixture.model.installations.downloadSelected()
        try await eventually { !fixture.model.installations.isDownloading }
        let requests = await fixture.session.downloads
        XCTAssertEqual(requests.first?.platform, "Windows")
        XCTAssertNotNil(fixture.model.installations.selectedReceipt)

        var unsupported = game("unsupported")
        unsupported.windows = false
        fixture.model.open(unsupported)
        XCTAssertFalse(fixture.model.installations.primaryActionAvailable)
        fixture.model.installations.downloadSelected()
        let afterUnsupported = await fixture.session.downloads
        XCTAssertEqual(afterUnsupported.count, 1)
    }

    func testCancellationRejectsLateSuccessAndResumesSameDestination() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        await fixture.session.holdNextDownload()
        fixture.model.installations.downloadSelected()
        try await eventually { await fixture.session.waitingForDownload }
        fixture.model.installations.cancelDownload()
        XCTAssertEqual(fixture.session.cancellations, 1)
        // Deliberately return success after cancellation to exercise the UI guard.
        await fixture.session.completeDownload()
        try await eventually { !fixture.model.installations.isDownloading }
        XCTAssertTrue(fixture.model.installations.receipts.isEmpty)
        XCTAssertEqual(fixture.model.installations.transfer?.state, .cancelled)
        XCTAssertTrue(fixture.model.installations.canResumeDownload)
        XCTAssertEqual(fixture.model.installations.primaryActionTitle, L10n.string("model.game.resume"))
        XCTAssertNil(fixture.model.errorMessage)

        fixture.model.installations.downloadSelected()
        try await eventually { !fixture.model.installations.isDownloading }
        let requests = await fixture.session.downloads
        XCTAssertEqual(requests.count, 2)
        XCTAssertEqual(requests.first, requests.last)
        XCTAssertEqual(fixture.model.installations.receipts.count, 1)
        XCTAssertEqual(fixture.model.installations.transfer?.state, .complete)
    }

    func testCancellationDuringFinalCredentialSaveRetainsCompletedInstallation() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        try await eventually { !fixture.model.isLoadingGameDetails && fixture.session.credentialRevisions.isWaiting }
        await fixture.session.holdNextDownload()
        fixture.model.installations.downloadSelected()
        try await eventually { await fixture.session.waitingForDownload }
        await fixture.store.holdNextSave()
        await fixture.session.completeDownload()
        try await eventually { await fixture.store.waitingForSave }

        XCTAssertEqual(fixture.model.installations.receipts.count, 1)
        XCTAssertEqual(fixture.model.installations.transfer?.state, .complete)
        fixture.model.installations.cancelDownload()
        await fixture.store.completeSave()
        try await eventually { !fixture.model.installations.isDownloading }
        XCTAssertEqual(fixture.model.installations.receipts.count, 1)
        XCTAssertEqual(fixture.model.installations.transfer?.state, .complete)
        XCTAssertFalse(fixture.model.installations.canResumeDownload)
        XCTAssertNil(fixture.model.errorMessage)
    }

    func testCancellationDuringPreparationSavesRotationWithoutStartingDownload() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        await fixture.session.holdNextPrepare()
        fixture.model.installations.downloadSelected()
        try await eventually { await fixture.session.waitingForPrepare }
        fixture.model.installations.cancelDownload()
        await fixture.session.completePrepare(rotated: credentials("cancelled-prepare"))
        try await eventually { !fixture.model.installations.isDownloading }
        let requests = await fixture.session.downloads
        let saved = await fixture.store.value
        XCTAssertTrue(requests.isEmpty)
        XCTAssertEqual(saved?.refreshToken, "cancelled-prepare")
        XCTAssertEqual(fixture.model.installations.transfer?.state, .cancelled)
        XCTAssertTrue(fixture.model.installations.canResumeDownload)
        XCTAssertNil(fixture.model.errorMessage)
    }

    func testFailureSavesRotatedTokenAndRetrySucceeds() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        await fixture.session.failNextDownload(rotated: credentials("failed-download"))
        fixture.model.installations.downloadSelected()
        try await eventually { !fixture.model.installations.isDownloading }
        let saved = await fixture.store.value
        XCTAssertEqual(saved?.refreshToken, "failed-download")
        XCTAssertEqual(fixture.model.installations.transfer?.state, .failed)
        XCTAssertEqual(fixture.model.errorMessage, L10n.string("model.epic.network"))
        XCTAssertTrue(fixture.model.installations.canResumeDownload)
        XCTAssertTrue(fixture.model.installations.receipts.isEmpty)

        fixture.model.installations.resumeDownload()
        try await eventually { !fixture.model.installations.isDownloading }
        XCTAssertNil(fixture.model.errorMessage)
        XCTAssertEqual(fixture.model.installations.receipts.count, 1)
        XCTAssertFalse(fixture.model.installations.canResumeDownload)
    }

    func testRetryPreparationFailureDoesNotInheritPreviousCancellation() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        await fixture.session.holdNextDownload()
        fixture.model.installations.downloadSelected()
        try await eventually { await fixture.session.waitingForDownload }
        fixture.model.installations.cancelDownload()
        await fixture.session.completeDownload(cancelled: true)
        try await eventually { !fixture.model.installations.isDownloading }
        XCTAssertEqual(fixture.session.downloadProgress()?.state, .cancelled)

        await fixture.session.failNextPrepare()
        fixture.model.installations.resumeDownload()
        try await eventually { !fixture.model.installations.isDownloading }
        XCTAssertEqual(fixture.model.installations.transfer?.state, .failed)
        XCTAssertEqual(fixture.model.errorMessage, L10n.string("model.epic.network"))
        XCTAssertTrue(fixture.model.installations.canResumeDownload)

        fixture.model.installations.resumeDownload()
        try await eventually { !fixture.model.installations.isDownloading }
        XCTAssertEqual(fixture.model.installations.transfer?.state, .complete)
        XCTAssertNil(fixture.model.errorMessage)
    }

    func testChangingSelectionKeepsDownloadAttachedToOriginalGame() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        await fixture.session.holdNextDownload()
        fixture.model.installations.downloadSelected()
        try await eventually { await fixture.session.waitingForDownload }
        let other = game("other")
        fixture.model.open(other)
        XCTAssertEqual(fixture.model.installations.activeTransferGame, fixture.game.libraryID)
        XCTAssertNil(fixture.model.installations.selectedReceipt)
        XCTAssertFalse(fixture.model.installations.primaryActionAvailable)

        await fixture.session.completeDownload()
        try await eventually { !fixture.model.installations.isDownloading }
        XCTAssertEqual(fixture.model.selectedGame?.libraryID, other.libraryID)
        XCTAssertNil(fixture.model.installations.selectedReceipt)
        XCTAssertEqual(fixture.model.installations.receipts.first?.libraryID, fixture.game.libraryID)
        XCTAssertEqual(fixture.model.installations.transferGame, fixture.game.libraryID)
        XCTAssertEqual(fixture.model.installations.primaryActionTitle, L10n.string("model.game.download"))
        XCTAssertTrue(fixture.model.installations.primaryActionAvailable)
    }

    func testSignOutAndNewAccountIgnoreLateDownloadAndCredentials() async throws {
        let directory = temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: directory) }
        let oldCredentials = credentials("old")
        let newCredentials = credentials("new", account: "second-account")
        let store = EpicMemoryStore(oldCredentials)
        let oldSession = EpicDownloadSession(credentials: oldCredentials, game: game("one"))
        let newSession = EpicDownloadSession(credentials: newCredentials, game: game("two"))
        let model = EpicModel(directory: directory, keychain: store, authorize: { "fixture-code" },
                              signIn: { _ in newSession }, restore: { _ in oldSession })
        await model.start()
        model.open(game("one"))
        try await eventually { !model.isLoadingGameDetails }
        await oldSession.holdNextDownload()
        model.installations.downloadSelected()
        try await eventually { await oldSession.waitingForDownload }
        model.signOut()
        XCTAssertEqual(oldSession.cancellations, 1)
        try await eventually { !model.isSigningOut }
        model.signIn()
        try await eventually { model.isConnected && !model.isLoadingGames }
        model.open(game("two"))
        await oldSession.completeDownload(rotated: credentials("late-private-token"))
        try await eventually { await oldSession.returnedDownloads == 1 }
        await Task.yield()

        let saved = await store.value
        XCTAssertEqual(saved, newCredentials)
        XCTAssertEqual(model.selectedGame?.appName, "two")
        XCTAssertTrue(model.installations.receipts.isEmpty)
        XCTAssertNil(model.installations.transfer)
        XCTAssertFalse(model.installations.isDownloading)
        XCTAssertTrue(try receiptFiles(in: directory).isEmpty)
    }

    func testRestartRestoresOnlyCurrentAccountReceiptsAndVerifiesExistingDirectory() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.installations.downloadSelected()
        try await eventually { !fixture.model.installations.isDownloading }
        let installed = try XCTUnwrap(fixture.model.installations.selectedReceipt)

        let restored = EpicModel(directory: fixture.directory, keychain: fixture.store, restore: { _ in fixture.session })
        await restored.start()
        restored.open(fixture.game)
        XCTAssertEqual(restored.installations.selectedReceipt?.directory, installed.directory)
        await fixture.session.holdNextDownload()
        restored.installations.downloadSelected()
        XCTAssertEqual(restored.installations.selectedReceipt?.needsVerification, true)
        try await eventually { await fixture.session.waitingForDownload }
        let request = await fixture.session.downloads.last
        XCTAssertEqual(request?.directory, URL(fileURLWithPath: installed.directory).deletingLastPathComponent().path)
        restored.installations.cancelDownload()
        await fixture.session.completeDownload()
        try await eventually { !restored.installations.isDownloading }

        let interrupted = EpicModel(directory: fixture.directory, keychain: fixture.store, restore: { _ in fixture.session })
        await interrupted.start()
        interrupted.open(fixture.game)
        XCTAssertEqual(interrupted.installations.selectedReceipt?.needsVerification, true)
        interrupted.installations.downloadSelected()
        try await eventually { !interrupted.installations.isDownloading }
        XCTAssertEqual(interrupted.installations.selectedReceipt?.needsVerification, false)

        let different = credentials("different", account: "different-account")
        let otherSession = EpicDownloadSession(credentials: different, game: fixture.game)
        let other = EpicModel(directory: fixture.directory, keychain: EpicMemoryStore(different), restore: { _ in otherSession })
        await other.start()
        XCTAssertTrue(other.installations.receipts.isEmpty)

        try FileManager.default.removeItem(atPath: installed.directory)
        let missing = EpicModel(directory: fixture.directory, keychain: fixture.store, restore: { _ in fixture.session })
        await missing.start()
        XCTAssertTrue(missing.installations.receipts.isEmpty)
    }

    func testTransferProgressIsClamped() {
        let transfer = GameTransfer(game: "one", state: .verifying, completedBytes: 120, totalBytes: 100,
                                    networkBytes: 0, contentBytes: 0, currentFile: "Content/game.pak", message: "")
        XCTAssertEqual(transfer.progressFraction, 1)
        XCTAssertEqual(transfer.downloadedGameDataText, L10n.string("ui.transfer.no_downloaded_data"))
    }

    func testInstalledPrimaryActionAlwaysVerifiesThenAuthorizesAndLaunchesWithoutDuplicates() async throws {
        let fixture = try await fixture(game: game("native", macos: true))
        defer { fixture.cleanUp() }
        fixture.model.installations.primaryAction()
        try await eventually { !fixture.model.installations.isDownloading }
        let installedLaunches = await fixture.session.launchRequests
        XCTAssertTrue(installedLaunches.isEmpty, "Installing a game must not launch it")
        XCTAssertEqual(fixture.model.installations.primaryActionTitle, L10n.string("model.game.launch"))

        await fixture.session.holdNextDownload()
        await fixture.session.holdNextLaunch()
        await fixture.session.holdNextRunningGame()
        fixture.model.installations.primaryAction()
        fixture.model.installations.primaryAction()
        XCTAssertTrue(fixture.model.installations.isLaunching)
        XCTAssertTrue(fixture.model.installations.isVerifyingBeforeLaunch)
        XCTAssertTrue(fixture.model.gameSessions.isBusy)
        try await eventually { await fixture.session.waitingForDownload }
        let verifyingLaunches = await fixture.session.launchRequests
        XCTAssertTrue(verifyingLaunches.isEmpty)
        await fixture.session.completeDownload()
        try await eventually { await fixture.session.waitingForLaunch }
        XCTAssertFalse(fixture.model.installations.isDownloading)
        XCTAssertTrue(fixture.model.installations.isLaunching)
        XCTAssertFalse(fixture.model.installations.isVerifyingBeforeLaunch)
        XCTAssertEqual(fixture.model.installations.primaryActionTitle, L10n.string("model.game.launching"))
        fixture.model.installations.primaryAction()
        await fixture.session.completeLaunch(rotated: credentials("launch-rotation"))
        try await eventually { fixture.model.gameSessions.active?.phase == .running }
        let game = try await launchedGame(fixture)
        let storedAtBegin = await game.credentialsAtBegin
        XCTAssertEqual(storedAtBegin?.refreshToken, "launch-rotation")
        XCTAssertFalse(fixture.model.installations.isLaunching)
        XCTAssertTrue(fixture.model.gameSessions.isBusy)
        XCTAssertFalse(fixture.model.installations.primaryActionAvailable)
        fixture.model.installations.primaryAction()
        let requests = await fixture.session.downloads
        let launches = await fixture.session.launchRequests
        XCTAssertEqual(requests.count, 2)
        XCTAssertEqual(launches.count, 1)
        XCTAssertEqual(launches.first?.language, "fr-FR")
        XCTAssertEqual(launches.first?.install, fixture.model.installations.selectedReceipt?.directory)
        XCTAssertEqual(launches.first?.namespace, fixture.game.namespace)
        XCTAssertEqual(launches.first?.appName, fixture.game.appName)
        await game.finish()
        try await eventually { !fixture.model.gameSessions.isBusy }
        let events = await fixture.session.events
        XCTAssertEqual(events, ["download", "download", "launchGame", "begin"])

        fixture.model.installations.primaryAction()
        try await eventually { !fixture.model.gameSessions.isBusy }
        let repeated = await fixture.session.events
        let repeatedLaunches = await fixture.session.launchRequests
        XCTAssertEqual(Array(repeated.suffix(3)), ["download", "launchGame", "begin"])
        XCTAssertEqual(repeatedLaunches.count, 2)
    }

    func testCancelledLaunchVerificationNeverHandsOffOrBeginsAndKeepsLaunchAction() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.installations.downloadSelected()
        try await eventually { !fixture.model.installations.isDownloading }
        await fixture.session.holdNextDownload()
        fixture.model.installations.primaryAction()
        try await eventually { await fixture.session.waitingForDownload }
        fixture.model.installations.cancelDownload()
        await fixture.session.completeDownload()
        try await eventually { !fixture.model.gameSessions.isBusy }
        let launches = await fixture.session.launchRequests
        let games = await fixture.session.launchedGames
        XCTAssertTrue(launches.isEmpty)
        XCTAssertTrue(games.isEmpty)
        XCTAssertEqual(fixture.model.installations.selectedReceipt?.needsVerification, true)
        XCTAssertEqual(fixture.model.installations.primaryActionTitle, L10n.string("model.game.launch"))
        XCTAssertNil(fixture.model.errorMessage)
    }

    func testCancelledLaunchHandoffPersistsRotationAndCancelsLateClientWithoutBeginning() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.installations.downloadSelected()
        try await eventually { !fixture.model.installations.isDownloading }
        await fixture.session.holdNextLaunch()
        fixture.model.installations.primaryAction()
        try await eventually { await fixture.session.waitingForLaunch }
        fixture.model.installations.cancelDownload()
        await fixture.session.completeLaunch(rotated: credentials("cancelled-launch-rotation"))
        try await eventually { !fixture.model.gameSessions.isBusy }
        let game = try await launchedGame(fixture)
        try await eventually { await game.cancellations > 0 }
        let saved = await fixture.store.value
        let beginnings = await game.beginnings
        XCTAssertEqual(saved?.refreshToken, "cancelled-launch-rotation")
        XCTAssertEqual(beginnings, 0)
        XCTAssertNil(fixture.model.errorMessage)
        XCTAssertEqual(fixture.model.installations.primaryActionTitle, L10n.string("model.game.launch"))
    }

    func testCancelledLaunchDuringFinalCredentialSaveCancelsClientBeforeSaveCompletes() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.installations.downloadSelected()
        try await eventually { !fixture.model.installations.isDownloading }
        await fixture.session.holdNextLaunch()
        fixture.model.installations.primaryAction()
        try await eventually { await fixture.session.waitingForLaunch }
        await fixture.store.holdNextSave()
        await fixture.session.completeLaunch(rotated: credentials("final-launch-rotation"))
        try await eventually { await fixture.store.waitingForSave }
        try await eventually { !(await fixture.session.launchedGames.isEmpty) }
        let game = try await launchedGame(fixture)
        let beginningsBeforeCancel = await game.beginnings
        XCTAssertEqual(beginningsBeforeCancel, 0)
        fixture.model.installations.cancelDownload()
        try await eventually { await game.cancellations > 0 }
        let stillSaving = await fixture.store.waitingForSave
        XCTAssertTrue(stillSaving, "Cancellation must reach the returned client while credential storage is blocked")
        await fixture.store.completeSave()
        try await eventually { !fixture.model.gameSessions.isBusy }
        let saved = await fixture.store.value
        let beginnings = await game.beginnings
        XCTAssertEqual(saved?.refreshToken, "final-launch-rotation")
        XCTAssertEqual(beginnings, 0)
        XCTAssertNotNil(fixture.model.installations.selectedReceipt)
        XCTAssertNil(fixture.model.errorMessage)
    }

    func testSignOutRevokesLateLaunchClientWithoutBeginning() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.installations.downloadSelected()
        try await eventually { !fixture.model.installations.isDownloading }
        await fixture.session.holdNextLaunch()
        fixture.model.installations.primaryAction()
        try await eventually { await fixture.session.waitingForLaunch }
        fixture.model.signOut()
        await fixture.session.completeLaunch(rotated: credentials("abandoned-launch"))
        try await eventually { !fixture.model.isSigningOut && !fixture.model.gameSessions.isBusy }
        let game = try await launchedGame(fixture)
        try await eventually { await game.cancellations > 0 }
        let saved = await fixture.store.value
        let beginnings = await game.beginnings
        let revocations = await game.revocations
        XCTAssertNil(saved)
        XCTAssertEqual(beginnings, 0)
        XCTAssertEqual(revocations, [Self.account])
        XCTAssertFalse(fixture.model.installations.isLaunching)
        XCTAssertFalse(fixture.model.installations.isDownloading)
        XCTAssertTrue(fixture.model.installations.receipts.isEmpty)
    }

    func testChangingSelectionDoesNotRetargetPendingLaunch() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.installations.downloadSelected()
        try await eventually { !fixture.model.installations.isDownloading }
        await fixture.session.holdNextLaunch()
        fixture.model.installations.primaryAction()
        try await eventually { await fixture.session.waitingForLaunch }
        fixture.model.open(game("other"))
        XCTAssertFalse(fixture.model.installations.primaryActionAvailable)
        await fixture.session.completeLaunch()
        try await eventually { !fixture.model.gameSessions.isBusy }
        let launched = try await launchedGame(fixture)
        let beginnings = await launched.beginnings
        let launch = await fixture.session.launchRequests.first
        XCTAssertEqual(beginnings, 1)
        XCTAssertEqual(launch?.appName, fixture.game.appName)
        XCTAssertEqual(launch?.namespace, fixture.game.namespace)
        XCTAssertEqual(fixture.model.selectedGame?.appName, "other")
        XCTAssertNil(fixture.model.installations.selectedReceipt)
        XCTAssertTrue(fixture.model.installations.primaryActionAvailable)
    }

    func testVerificationAndLaunchHandoffFailuresNeverBegin() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.installations.downloadSelected()
        try await eventually { !fixture.model.installations.isDownloading }
        await fixture.session.failNextDownload(rotated: credentials("verify-failure"))
        fixture.model.installations.primaryAction()
        try await eventually { !fixture.model.gameSessions.isBusy }
        let firstLaunches = await fixture.session.launchRequests
        XCTAssertTrue(firstLaunches.isEmpty)
        XCTAssertNotNil(fixture.model.errorMessage)
        XCTAssertEqual(fixture.model.installations.primaryActionTitle, L10n.string("model.game.launch"))

        await fixture.session.failNextLaunch(rotated: credentials("handoff-failure"))
        fixture.model.installations.primaryAction()
        try await eventually { !fixture.model.gameSessions.isBusy }
        let saved = await fixture.store.value
        let games = await fixture.session.launchedGames
        let events = await fixture.session.events
        XCTAssertEqual(saved?.refreshToken, "handoff-failure")
        XCTAssertTrue(games.isEmpty)
        XCTAssertFalse(events.contains("begin"))
        XCTAssertEqual(fixture.model.errorMessage, L10n.string("model.epic.network"))
    }

    func testUninstallDeletesCapturedEpicInstallationAndRestoresDownloadAction() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.installations.downloadSelected()
        try await eventually { !fixture.model.installations.isDownloading }
        let receipt = try XCTUnwrap(fixture.model.installations.selectedReceipt)
        XCTAssertTrue(fixture.model.installations.canUninstallSelectedGame)
        fixture.model.installations.uninstall(receipt)
        XCTAssertTrue(fixture.model.installations.isUninstalling)
        XCTAssertFalse(fixture.model.installations.primaryActionAvailable)
        XCTAssertFalse(fixture.model.installations.canUninstallSelectedGame)
        fixture.model.installations.uninstall(receipt)
        fixture.model.open(game("other"))
        try await eventually { !fixture.model.installations.isUninstalling }
        XCTAssertFalse(FileManager.default.fileExists(atPath: receipt.directory))
        XCTAssertTrue(fixture.model.installations.receipts.isEmpty)
        XCTAssertEqual(fixture.model.selectedGame?.appName, "other")
        XCTAssertNil(fixture.model.errorMessage)
        XCTAssertNil(fixture.model.installations.transfer)
        let file = try XCTUnwrap(receiptFiles(in: fixture.directory).first)
        XCTAssertTrue(try JSONDecoder().decode([EpicInstallReceipt].self, from: Data(contentsOf: file)).isEmpty)
        fixture.model.open(fixture.game)
        XCTAssertEqual(fixture.model.installations.primaryActionTitle, L10n.string("model.game.download"))
        XCTAssertTrue(fixture.model.installations.primaryActionAvailable)
    }

    func testUninstallKeepsEpicReceiptAfterFilesystemLockFailure() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.installations.downloadSelected()
        try await eventually { !fixture.model.installations.isDownloading }
        let receipt = try XCTUnwrap(fixture.model.installations.selectedReceipt)
        let descriptor = Darwin.open(URL(fileURLWithPath: receipt.directory).appending(path: ".sailover-install.lock").path,
                                     O_RDWR | O_CREAT, S_IRUSR | S_IWUSR)
        XCTAssertGreaterThanOrEqual(descriptor, 0)
        defer { Darwin.close(descriptor) }
        XCTAssertEqual(flock(descriptor, LOCK_EX | LOCK_NB), 0)
        fixture.model.installations.uninstall(receipt)
        try await eventually { !fixture.model.installations.isUninstalling }
        XCTAssertEqual(fixture.model.installations.selectedReceipt, receipt)
        XCTAssertNotNil(fixture.model.errorMessage)
        XCTAssertTrue(FileManager.default.fileExists(atPath: receipt.directory))
    }

    func testUninstallArchivesSavesBeforeRemovingFilesAndStopsOnArchiveFailure() async throws {
        let fixture = try await fixture(game: game("native", macos: true))
        defer { fixture.cleanUp() }
        fixture.model.installations.downloadSelected()
        try await eventually { !fixture.model.installations.isDownloading }
        let receipt = try XCTUnwrap(fixture.model.installations.selectedReceipt)
        await fixture.session.failNextArchive()
        fixture.model.installations.uninstall(receipt)
        try await eventually { !fixture.model.installations.isUninstalling }
        XCTAssertTrue(FileManager.default.fileExists(atPath: receipt.directory))
        XCTAssertEqual(fixture.model.installations.selectedReceipt, receipt)
        XCTAssertNotNil(fixture.model.errorMessage)
        let requests = await fixture.session.archives
        XCTAssertEqual(requests.count, 1)
        XCTAssertEqual(requests.first?.install, receipt.directory)
        XCTAssertEqual(requests.first?.namespace, fixture.game.namespace)
        XCTAssertEqual(requests.first?.appName, fixture.game.appName)
        XCTAssertEqual(requests.first?.root, fixture.directory.appending(path: "SteamCloud").path)
        let observedExisting = await fixture.session.archivedExistingInstallation
        XCTAssertTrue(observedExisting)
    }

    func testUninstallRejectsEpicStaleReceiptAndActiveDownload() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.installations.downloadSelected()
        try await eventually { !fixture.model.installations.isDownloading }
        let receipt = try XCTUnwrap(fixture.model.installations.selectedReceipt)
        let forged = EpicInstallReceipt(namespace: receipt.namespace, appName: receipt.appName, name: receipt.name,
                                        directory: fixture.directory.path, platform: receipt.platform, totalBytes: receipt.totalBytes)
        fixture.model.installations.uninstall(forged)
        XCTAssertFalse(fixture.model.installations.isUninstalling)
        await fixture.session.holdNextDownload()
        fixture.model.installations.primaryAction()
        fixture.model.installations.uninstall(receipt)
        XCTAssertFalse(fixture.model.installations.isUninstalling)
        XCTAssertFalse(fixture.model.installations.canUninstallSelectedGame)
        try await eventually { await fixture.session.waitingForDownload }
        fixture.model.installations.cancelDownload()
        await fixture.session.completeDownload(cancelled: true)
        try await eventually { !fixture.model.installations.isLaunching }
        XCTAssertNotNil(fixture.model.installations.selectedReceipt)
        XCTAssertTrue(FileManager.default.fileExists(atPath: receipt.directory))
    }

    func testSignOutBeforeEpicUninstallStartsPreservesGameAndReceipt() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.installations.downloadSelected()
        try await eventually { !fixture.model.installations.isDownloading }
        let receipt = try XCTUnwrap(fixture.model.installations.selectedReceipt)
        fixture.model.installations.uninstall(receipt)
        fixture.model.signOut()
        try await eventually { !fixture.model.installations.isUninstalling && !fixture.model.isSigningOut }
        XCTAssertFalse(fixture.model.isConnected)
        XCTAssertTrue(fixture.model.installations.receipts.isEmpty)
        XCTAssertTrue(FileManager.default.fileExists(atPath: receipt.directory))
        let file = try XCTUnwrap(receiptFiles(in: fixture.directory).first)
        XCTAssertEqual(try JSONDecoder().decode([EpicInstallReceipt].self, from: Data(contentsOf: file)), [receipt])
    }

    private func fixture(game owned: EpicGame? = nil) async throws -> EpicDownloadFixture {
        let owned = owned ?? game("one")
        let directory = temporaryDirectory()
        let credential = credentials("current")
        let store = EpicMemoryStore(credential)
        let session = EpicDownloadSession(credentials: credential, game: owned, store: store)
        let model = EpicModel(directory: directory, keychain: store, restore: { _ in session }, language: "fr-FR",
                              gameSessions: EpicGameSessions(recoverSession: { nil }),
                              launchGame: { _, admission, configuration, beforeStart in
                                  try await admission.launch(handoff: TestHandoff {
                                      try await session.startGame(configuration: configuration)
                                  }, beforeStart: beforeStart)
                              })
        await model.start()
        model.open(owned)
        try await eventually { !model.isLoadingGameDetails }
        return EpicDownloadFixture(directory: directory, model: model, session: session, store: store, game: owned)
    }

    private func launchedGame(_ fixture: EpicDownloadFixture) async throws -> EpicDownloadGameState {
        let game = await fixture.session.launchedGames.last
        return try XCTUnwrap(game)
    }

    private func temporaryDirectory() -> URL {
        FileManager.default.temporaryDirectory.appending(path: "SailOverEpicDownloadTests-\(UUID().uuidString)", directoryHint: .isDirectory)
    }

    private func receiptFiles(in directory: URL) throws -> [URL] {
        guard FileManager.default.fileExists(atPath: directory.path) else { return [] }
        return try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)
            .filter { $0.lastPathComponent.hasPrefix("epic-installations-") }
    }

    private func game(_ app: String, macos: Bool = false) -> EpicGame {
        EpicGame(appName: app, namespace: "download-fixture", catalogItemId: "catalog-\(app)", title: "Game \(app)",
                 description: "", imageUrl: nil, headerImageUrl: nil, developer: "", windows: true, macos: macos)
    }

    /// Shaped like an Epic account id, which Cloud storage checks on uninstall.
    private static let account = "0123456789abcdef0123456789abcdef"

    private func credentials(_ token: String, account: String = EpicDownloadTests.account) -> EpicCredentials {
        EpicCredentials(accountId: account, displayName: account, refreshToken: token)
    }

    private func eventually(file: StaticString = #filePath, line: UInt = #line, _ condition: @MainActor () async -> Bool) async throws {
        for _ in 0..<400 {
            if await condition() { return }
            try await Task.sleep(for: .milliseconds(5))
        }
        XCTFail("Timed out waiting for Epic download state", file: file, line: line)
        throw EpicDownloadTestFailure.timeout
    }
}

private struct EpicDownloadFixture {
    let directory: URL
    let model: EpicModel
    let session: EpicDownloadSession
    let store: EpicMemoryStore
    let game: EpicGame

    func cleanUp() { try? FileManager.default.removeItem(at: directory) }
}

private struct EpicDownloadRequest: Equatable, Sendable {
    let namespace: String
    let catalogItemId: String
    let appName: String
    let platform: String
    let directory: String
}

private actor EpicDownloadSession: EpicSessionProtocol {
    private(set) var archives: [EpicCloudConfiguration] = []
    private(set) var archivedExistingInstallation = false
    private var archiveFailure = false
    func failNextArchive() { archiveFailure = true }
    func archiveCloud(configuration: EpicCloudConfiguration) async throws {
        archives.append(configuration)
        archivedExistingInstallation = FileManager.default.fileExists(atPath: configuration.install)
        if archiveFailure { archiveFailure = false; throw EpicError.Network }
    }

    func startFriends() async {}
    func reconnectFriends() async {}
    nonisolated func stopFriends() {}
    nonisolated func shutdownObservers() { credentialRevisions.stop() }
    nonisolated func friends() -> EpicFriendsSnapshot {
        EpicFriendsSnapshot(accountId: "unused", revision: 1, connection: .stopped, friends: [], message: nil)
    }
    func nextFriends(afterRevision: UInt64) async -> EpicFriendsSnapshot { friends() }
    nonisolated let credentialRevisions = EpicCredentialRevisionFeed()
    func nextCredentialsRevision(afterRevision: UInt64) async -> UInt64? {
        await credentialRevisions.next(after: afterRevision)
    }

    private var credential: EpicCredentials {
        didSet { credentialRevisions.publish() }
    }
    private let owned: EpicGame
    private var prepared: EpicCredentials?
    private var failure: EpicCredentials?
    private var shouldHoldDownload = false
    private var downloadContinuation: CheckedContinuation<Void, Never>?
    private var shouldHoldPrepare = false
    private var shouldFailPrepare = false
    private var shouldCancelDownload = false
    private var shouldHoldLaunch = false
    private var shouldHoldRunningGame = false
    private var launchFailure: EpicCredentials?
    private var launchContinuation: CheckedContinuation<Void, Never>?
    private(set) var waitingForLaunch = false
    private(set) var launchRequests: [EpicCloudConfiguration] = []
    private(set) var launchedGames: [EpicDownloadGameState] = []
    private let store: EpicMemoryStore
    private(set) var events: [String] = []
    private var prepareContinuation: CheckedContinuation<Void, Never>?
    private(set) var waitingForDownload = false
    private(set) var waitingForPrepare = false
    private(set) var downloads: [EpicDownloadRequest] = []
    private(set) var returnedDownloads = 0
    nonisolated private let progress = EpicDownloadProgress()
    nonisolated var cancellations: Int { progress.cancelCount }

    init(credentials: EpicCredentials, game: EpicGame, store: EpicMemoryStore? = nil) {
        credential = credentials
        owned = game
        self.store = store ?? EpicMemoryStore(credentials)
    }
    func credentials() -> EpicCredentials { credential }
    func prepare() async throws -> EpicCredentials {
        if shouldHoldPrepare {
            shouldHoldPrepare = false
            waitingForPrepare = true
            await withCheckedContinuation { prepareContinuation = $0 }
            waitingForPrepare = false
        }
        if let prepared { credential = prepared; self.prepared = nil }
        if shouldFailPrepare { shouldFailPrepare = false; throw EpicError.Network }
        return credential
    }
    func games() -> [EpicGame] { [owned] }
    func gameDetails(namespace: String, catalogItemId: String, appName: String, platform: String) -> EpicGameDetails {
        EpicGameDetails(playtimeSeconds: nil, installationBytes: nil)
    }
    func download(namespace: String, catalogItemId: String, appName: String, platform: String, directory: String) async throws -> EpicInstallation {
        events.append("download")
        downloads.append(EpicDownloadRequest(namespace: namespace, catalogItemId: catalogItemId, appName: appName, platform: platform, directory: directory))
        progress.set(GameTransfer(game: appName, state: .downloading, completedBytes: 40, totalBytes: 100,
                                 networkBytes: 20, contentBytes: 40, currentFile: "Content/game.pak", message: ""))
        defer { returnedDownloads += 1 }
        if shouldHoldDownload {
            shouldHoldDownload = false
            waitingForDownload = true
            await withCheckedContinuation { downloadContinuation = $0 }
            waitingForDownload = false
        }
        if shouldCancelDownload {
            shouldCancelDownload = false
            progress.finish(.cancelled)
            throw CancellationError()
        }
        if let failure {
            credential = failure
            self.failure = nil
            progress.finish(.failed)
            throw EpicError.Network
        }
        let component = epicInstallationDirectoryName(namespace: namespace, catalogItemId: catalogItemId,
                                                      appName: appName, platform: platform)
        let destination = URL(fileURLWithPath: directory).appending(path: component, directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
        progress.finish(.complete)
        return EpicInstallation(appName: appName, directory: destination.path, platform: platform, totalBytes: 100)
    }
    func launchPlan(namespace: String, catalogItemId: String, appName: String, platform: String, directory: String, language: String) async throws -> EpicLaunchPlan {
        throw CocoaError(.featureUnsupported)
    }
    func startGame(configuration: EpicCloudConfiguration) async throws -> any GameSupervisor {
        events.append("launchGame")
        launchRequests.append(configuration)
        if shouldHoldLaunch {
            shouldHoldLaunch = false
            waitingForLaunch = true
            await withCheckedContinuation { launchContinuation = $0 }
            waitingForLaunch = false
        }
        if let failure = launchFailure {
            launchFailure = nil
            credential = failure
            throw EpicError.Network
        }
        let game = EpicDownloadGameState(configuration: configuration, accountId: credential.accountId,
                                         store: store, holdRunning: shouldHoldRunningGame) { [weak self] in
            await self?.recordBegin()
        }
        shouldHoldRunningGame = false
        launchedGames.append(game)
        return EpicDownloadGameSession(state: game)
    }
    func holdNextLaunch() { shouldHoldLaunch = true }
    func holdNextRunningGame() { shouldHoldRunningGame = true }
    func failNextLaunch(rotated: EpicCredentials) { launchFailure = rotated }
    func completeLaunch(rotated: EpicCredentials? = nil) {
        if let rotated { credential = rotated }
        launchContinuation?.resume()
        launchContinuation = nil
    }
    private func recordBegin() { events.append("begin") }
    nonisolated func downloadProgress() -> GameTransfer? { progress.snapshot }
    nonisolated func cancelDownload() { progress.cancel() }
    func disconnect() {}
    func holdNextDownload() { shouldHoldDownload = true }
    func completeDownload(rotated: EpicCredentials? = nil, cancelled: Bool = false) {
        if let rotated { credential = rotated }
        shouldCancelDownload = cancelled
        downloadContinuation?.resume()
        downloadContinuation = nil
    }
    func failNextDownload(rotated: EpicCredentials) { failure = rotated }
    func rotateOnPrepare(_ credentials: EpicCredentials) { prepared = credentials }
    func holdNextPrepare() { shouldHoldPrepare = true }
    func failNextPrepare() { shouldFailPrepare = true }
    func completePrepare(rotated: EpicCredentials) {
        prepared = rotated
        prepareContinuation?.resume()
        prepareContinuation = nil
    }
}

private final class EpicDownloadProgress: @unchecked Sendable {
    private let lock = NSLock()
    private var value: GameTransfer?
    private var cancelled = 0
    var snapshot: GameTransfer? { lock.withLock { value } }
    var cancelCount: Int { lock.withLock { cancelled } }
    func set(_ transfer: GameTransfer) { lock.withLock { value = transfer } }
    func finish(_ state: TransferState) { lock.withLock { value?.state = state } }
    func cancel() { lock.withLock { cancelled += 1; value?.state = .cancelled } }
}

private enum EpicDownloadTestFailure: Error { case timeout }

/// A Game session fake behind the production supervisor interface.
private final class EpicDownloadGameSession: GameSupervisor, @unchecked Sendable {
    private let state: EpicDownloadGameState

    init(state: EpicDownloadGameState) { self.state = state }
    func begin() throws { let state = state; try awaitBlocking { await state.begin() } }
    func cancelPreparation() { let state = state; _ = try? awaitBlocking { await state.cancelPreparation() } }
    func choose(account: GameAccount, decisionId: UInt64, choice: CloudChoice) throws {
        let state = state
        try awaitBlocking { await state.choose(choice) }
    }
    func resumeAfterRosetta(account: GameAccount, decisionId: UInt64) throws {
        throw GameSessionError.Failure(reason: "backend.epic.cloud.invalidSession")
    }
    func revoke(account: GameAccount) throws {
        guard case let .epic(accountId) = account else { return }
        let state = state
        try awaitBlocking { await state.revoke(accountId: accountId) }
    }
    func snapshot(after: UInt64?) throws -> GameSnapshot {
        let state = state
        return try awaitBlocking { await state.snapshot(after: after) }
    }
}

private actor EpicDownloadGameState {
    private let store: EpicMemoryStore
    private let holdRunning: Bool
    private let onBegin: @Sendable () async -> Void
    private var current: GameSnapshot
    private var waiting: CheckedContinuation<GameSnapshot, Never>?
    private(set) var beginnings = 0
    private(set) var cancellations = 0
    private(set) var revocations: [String] = []
    private(set) var credentialsAtBegin: EpicCredentials?

    init(configuration: EpicCloudConfiguration, accountId: String, store: EpicMemoryStore,
         holdRunning: Bool, onBegin: @escaping @Sendable () async -> Void) {
        self.store = store
        self.holdRunning = holdRunning
        self.onBegin = onBegin
        current = GameSnapshot(id: UUID().uuidString, phase: .preparing, finished: false, failureReason: nil,
            error: nil, revision: 1, reconnecting: false, rosetta: nil,
            cloud: CloudStatus(phase: .checking, decisionId: 0, error: nil, allowedChoices: []),
            store: .epic(accountId: accountId, namespace: configuration.namespace, appName: configuration.appName))
    }
    func begin() async {
        beginnings += 1
        credentialsAtBegin = await store.value
        await onBegin()
        guard cancellations == 0 else { return }
        current.phase = holdRunning ? .running : .finished
        current.cloud.phase = .ready
        publish()
    }
    func cancelPreparation() {
        cancellations += 1
        current.phase = .failed
        current.error = "backend.epic.cloud.cancelled"
        publish()
    }
    func revoke(accountId: String) { revocations.append(accountId) }
    func choose(_ choice: CloudChoice) {
        if choice == .cancel { cancelPreparation() }
    }
    func snapshot(after: UInt64?) async -> GameSnapshot {
        if after != current.revision { return current }
        return await withCheckedContinuation { waiting = $0 }
    }
    func finish() {
        current.phase = .finished
        publish()
    }
    private func publish() {
        current.finished = [.finished, .failed].contains(current.phase)
        current.revision += 1
        waiting?.resume(returning: current)
        waiting = nil
    }
}
