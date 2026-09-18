import Darwin
import Foundation
import XCTest
@testable import Forge

@MainActor
final class EpicDownloadTests: XCTestCase {
    /// Resolves production launch metadata and account authorization without
    /// updating installed files or executing a game process.
    func testLiveEpicLaunchPlanSmokeWhenRequested() async throws {
        executionTimeAllowance = 90
        guard ProcessInfo.processInfo.environment["FORGE_EPIC_LAUNCH_SMOKE"] == "1" else {
            throw XCTSkip("Set FORGE_EPIC_LAUNCH_SMOKE=1 to verify an installed game's launch plan")
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
        guard ProcessInfo.processInfo.environment["FORGE_EPIC_DOWNLOAD_SMOKE"] == "1" else {
            throw XCTSkip("Set FORGE_EPIC_DOWNLOAD_SMOKE=1 to verify content with the saved Epic session")
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

        fixture.model.downloadSelected()
        fixture.model.downloadSelected()
        XCTAssertTrue(fixture.model.isDownloading)
        XCTAssertEqual(fixture.model.activeTransferLibraryID, fixture.game.libraryID)
        XCTAssertFalse(fixture.model.primaryActionAvailable)
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

        try await eventually { fixture.model.transfer?.state == .downloading }
        XCTAssertEqual(fixture.model.transfer?.completedBytes, 40)
        await fixture.session.completeDownload(rotated: credentials("rotated-download"))
        try await eventually { !fixture.model.isDownloading }
        let stored = await fixture.store.value
        XCTAssertEqual(stored?.refreshToken, "rotated-download")
        XCTAssertEqual(fixture.model.selectedInstallation?.appName, "native")
        XCTAssertEqual(fixture.model.selectedInstallation?.name, "Game native")
        XCTAssertEqual(fixture.model.transfer?.state, .complete)
        XCTAssertEqual(fixture.model.primaryActionTitle, L10n.string("model.steam.launch"))
        XCTAssertFalse(fixture.model.canResumeDownload)
        XCTAssertNil(fixture.model.errorMessage)
        let receipt = try XCTUnwrap(receiptFiles(in: fixture.directory).first)
        let text = try String(contentsOf: receipt, encoding: .utf8)
        XCTAssertFalse(text.contains("rotated-download"))
        XCTAssertFalse(text.contains("prepared"))
    }

    func testWindowsDownloadDoesNotRequireOptionalDetailsAndUnsupportedGameIsDisabled() async throws {
        let fixture = try await fixture(game: game("windows"))
        defer { fixture.cleanUp() }
        XCTAssertTrue(fixture.model.primaryActionAvailable)
        fixture.model.downloadSelected()
        try await eventually { !fixture.model.isDownloading }
        let requests = await fixture.session.downloads
        XCTAssertEqual(requests.first?.platform, "Windows")
        XCTAssertNotNil(fixture.model.selectedInstallation)

        var unsupported = game("unsupported")
        unsupported.windows = false
        fixture.model.open(unsupported)
        XCTAssertFalse(fixture.model.primaryActionAvailable)
        fixture.model.downloadSelected()
        let afterUnsupported = await fixture.session.downloads
        XCTAssertEqual(afterUnsupported.count, 1)
    }

    func testCancellationRejectsLateSuccessAndResumesSameDestination() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        await fixture.session.holdNextDownload()
        fixture.model.downloadSelected()
        try await eventually { await fixture.session.waitingForDownload }
        fixture.model.cancelDownload()
        XCTAssertEqual(fixture.session.cancellations, 1)
        // Deliberately return success after cancellation to exercise the UI guard.
        await fixture.session.completeDownload()
        try await eventually { !fixture.model.isDownloading }
        XCTAssertTrue(fixture.model.installations.isEmpty)
        XCTAssertEqual(fixture.model.transfer?.state, .cancelled)
        XCTAssertTrue(fixture.model.canResumeDownload)
        XCTAssertEqual(fixture.model.primaryActionTitle, L10n.string("model.steam.resume"))
        XCTAssertNil(fixture.model.errorMessage)

        fixture.model.downloadSelected()
        try await eventually { !fixture.model.isDownloading }
        let requests = await fixture.session.downloads
        XCTAssertEqual(requests.count, 2)
        XCTAssertEqual(requests.first, requests.last)
        XCTAssertEqual(fixture.model.installations.count, 1)
        XCTAssertEqual(fixture.model.transfer?.state, .complete)
    }

    func testCancellationDuringFinalCredentialSaveRetainsCompletedInstallation() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        try await eventually { !fixture.model.isLoadingGameDetails && fixture.session.credentialRevisions.isWaiting }
        await fixture.session.holdNextDownload()
        fixture.model.downloadSelected()
        try await eventually { await fixture.session.waitingForDownload }
        await fixture.store.holdNextSave()
        await fixture.session.completeDownload()
        try await eventually { await fixture.store.waitingForSave }

        XCTAssertEqual(fixture.model.installations.count, 1)
        XCTAssertEqual(fixture.model.transfer?.state, .complete)
        fixture.model.cancelDownload()
        await fixture.store.completeSave()
        try await eventually { !fixture.model.isDownloading }
        XCTAssertEqual(fixture.model.installations.count, 1)
        XCTAssertEqual(fixture.model.transfer?.state, .complete)
        XCTAssertFalse(fixture.model.canResumeDownload)
        XCTAssertNil(fixture.model.errorMessage)
    }

    func testCancellationDuringPreparationSavesRotationWithoutStartingDownload() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        await fixture.session.holdNextPrepare()
        fixture.model.downloadSelected()
        try await eventually { await fixture.session.waitingForPrepare }
        fixture.model.cancelDownload()
        await fixture.session.completePrepare(rotated: credentials("cancelled-prepare"))
        try await eventually { !fixture.model.isDownloading }
        let requests = await fixture.session.downloads
        let saved = await fixture.store.value
        XCTAssertTrue(requests.isEmpty)
        XCTAssertEqual(saved?.refreshToken, "cancelled-prepare")
        XCTAssertEqual(fixture.model.transfer?.state, .cancelled)
        XCTAssertTrue(fixture.model.canResumeDownload)
        XCTAssertNil(fixture.model.errorMessage)
    }

    func testFailureSavesRotatedTokenAndRetrySucceeds() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        await fixture.session.failNextDownload(rotated: credentials("failed-download"))
        fixture.model.downloadSelected()
        try await eventually { !fixture.model.isDownloading }
        let saved = await fixture.store.value
        XCTAssertEqual(saved?.refreshToken, "failed-download")
        XCTAssertEqual(fixture.model.transfer?.state, .failed)
        XCTAssertEqual(fixture.model.errorMessage, L10n.string("model.epic.network"))
        XCTAssertTrue(fixture.model.canResumeDownload)
        XCTAssertTrue(fixture.model.installations.isEmpty)

        fixture.model.resumeDownload()
        try await eventually { !fixture.model.isDownloading }
        XCTAssertNil(fixture.model.errorMessage)
        XCTAssertEqual(fixture.model.installations.count, 1)
        XCTAssertFalse(fixture.model.canResumeDownload)
    }

    func testRetryPreparationFailureDoesNotInheritPreviousCancellation() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        await fixture.session.holdNextDownload()
        fixture.model.downloadSelected()
        try await eventually { await fixture.session.waitingForDownload }
        fixture.model.cancelDownload()
        await fixture.session.completeDownload(cancelled: true)
        try await eventually { !fixture.model.isDownloading }
        XCTAssertEqual(fixture.session.downloadProgress()?.state, .cancelled)

        await fixture.session.failNextPrepare()
        fixture.model.resumeDownload()
        try await eventually { !fixture.model.isDownloading }
        XCTAssertEqual(fixture.model.transfer?.state, .failed)
        XCTAssertEqual(fixture.model.errorMessage, L10n.string("model.epic.network"))
        XCTAssertTrue(fixture.model.canResumeDownload)

        fixture.model.resumeDownload()
        try await eventually { !fixture.model.isDownloading }
        XCTAssertEqual(fixture.model.transfer?.state, .complete)
        XCTAssertNil(fixture.model.errorMessage)
    }

    func testChangingSelectionKeepsDownloadAttachedToOriginalGame() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        await fixture.session.holdNextDownload()
        fixture.model.downloadSelected()
        try await eventually { await fixture.session.waitingForDownload }
        let other = game("other")
        fixture.model.open(other)
        XCTAssertEqual(fixture.model.activeTransferLibraryID, fixture.game.libraryID)
        XCTAssertNil(fixture.model.selectedInstallation)
        XCTAssertFalse(fixture.model.primaryActionAvailable)

        await fixture.session.completeDownload()
        try await eventually { !fixture.model.isDownloading }
        XCTAssertEqual(fixture.model.selectedGame?.libraryID, other.libraryID)
        XCTAssertNil(fixture.model.selectedInstallation)
        XCTAssertEqual(fixture.model.installations.first?.libraryID, fixture.game.libraryID)
        XCTAssertEqual(fixture.model.transferLibraryID, fixture.game.libraryID)
        XCTAssertEqual(fixture.model.primaryActionTitle, L10n.string("ui.game.download"))
        XCTAssertTrue(fixture.model.primaryActionAvailable)
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
        model.downloadSelected()
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
        XCTAssertTrue(model.installations.isEmpty)
        XCTAssertNil(model.transfer)
        XCTAssertFalse(model.isDownloading)
        XCTAssertTrue(try receiptFiles(in: directory).isEmpty)
    }

    func testRestartRestoresOnlyCurrentAccountReceiptsAndVerifiesExistingDirectory() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.downloadSelected()
        try await eventually { !fixture.model.isDownloading }
        let installed = try XCTUnwrap(fixture.model.selectedInstallation)

        let restored = EpicModel(directory: fixture.directory, keychain: fixture.store, restore: { _ in fixture.session })
        await restored.start()
        restored.open(fixture.game)
        XCTAssertEqual(restored.selectedInstallation?.directory, installed.directory)
        await fixture.session.holdNextDownload()
        restored.downloadSelected()
        XCTAssertEqual(restored.selectedInstallation?.needsVerification, true)
        try await eventually { await fixture.session.waitingForDownload }
        let request = await fixture.session.downloads.last
        XCTAssertEqual(request?.directory, URL(fileURLWithPath: installed.directory).deletingLastPathComponent().path)
        restored.cancelDownload()
        await fixture.session.completeDownload()
        try await eventually { !restored.isDownloading }

        let interrupted = EpicModel(directory: fixture.directory, keychain: fixture.store, restore: { _ in fixture.session })
        await interrupted.start()
        interrupted.open(fixture.game)
        XCTAssertEqual(interrupted.selectedInstallation?.needsVerification, true)
        interrupted.downloadSelected()
        try await eventually { !interrupted.isDownloading }
        XCTAssertEqual(interrupted.selectedInstallation?.needsVerification, false)

        let different = credentials("different", account: "different-account")
        let otherSession = EpicDownloadSession(credentials: different, game: fixture.game)
        let other = EpicModel(directory: fixture.directory, keychain: EpicMemoryStore(different), restore: { _ in otherSession })
        await other.start()
        XCTAssertTrue(other.installations.isEmpty)

        try FileManager.default.removeItem(atPath: installed.directory)
        let missing = EpicModel(directory: fixture.directory, keychain: fixture.store, restore: { _ in fixture.session })
        await missing.start()
        XCTAssertTrue(missing.installations.isEmpty)
    }

    func testEpicTransfersReuseSteamFormattingAndClampProgress() {
        let epic = EpicTransfer(appName: "one", state: .verifying, completedBytes: 120, totalBytes: 100,
                                networkBytes: 0, contentBytes: 0, currentFile: "Content/game.pak", message: "")
        let steam = SteamTransfer(appId: 1, state: .verifying, completedBytes: 120, totalBytes: 100,
                                  networkBytes: 0, contentBytes: 0, currentFile: "Content/game.pak", message: "")
        XCTAssertEqual(epic.progressSummary, steam.progressSummary)
        XCTAssertEqual(epic.buttonTitle, steam.buttonTitle)
        XCTAssertEqual(epic.progressFraction, 1)
        XCTAssertEqual(epic.downloadedGameDataText, L10n.string("ui.transfer.no_downloaded_data"))
    }

    func testInstalledPrimaryActionAlwaysVerifiesThenAuthorizesAndLaunchesWithoutDuplicates() async throws {
        let fixture = try await fixture(game: game("native", macos: true))
        defer { fixture.cleanUp() }
        fixture.model.primaryAction()
        try await eventually { !fixture.model.isDownloading }
        XCTAssertTrue(fixture.launcher.plans.isEmpty, "Installing a game must not launch it")
        XCTAssertEqual(fixture.model.primaryActionTitle, L10n.string("model.steam.launch"))

        await fixture.session.holdNextDownload()
        await fixture.session.holdNextLaunchPlan()
        fixture.launcher.shouldHold = true
        fixture.model.primaryAction()
        fixture.model.primaryAction()
        XCTAssertTrue(fixture.model.isLaunching)
        XCTAssertTrue(fixture.model.isVerifyingBeforeLaunch)
        try await eventually { await fixture.session.waitingForDownload }
        XCTAssertTrue(fixture.launcher.plans.isEmpty)
        await fixture.session.completeDownload()
        try await eventually { await fixture.session.waitingForLaunchPlan }
        XCTAssertFalse(fixture.model.isDownloading)
        XCTAssertTrue(fixture.model.isLaunching)
        XCTAssertFalse(fixture.model.isVerifyingBeforeLaunch)
        XCTAssertEqual(fixture.model.primaryActionTitle, L10n.string("model.steam.launching"))
        fixture.model.primaryAction()
        await fixture.session.completeLaunchPlan(rotated: credentials("launch-rotation"))
        try await eventually { fixture.launcher.isWaiting }
        let stored = await fixture.store.value
        XCTAssertEqual(stored?.refreshToken, "launch-rotation")
        fixture.model.primaryAction()
        let requests = await fixture.session.downloads
        XCTAssertEqual(requests.count, 2)
        XCTAssertEqual(fixture.launcher.plans.count, 1)
        fixture.launcher.complete()
        try await eventually { !fixture.model.isLaunching }
        let events = await fixture.session.events
        XCTAssertEqual(events, ["download", "download", "launchPlan", "execute"])
        let planRequest = await fixture.session.launchRequests.first
        XCTAssertEqual(planRequest?.language, "fr-FR")
        XCTAssertEqual(planRequest?.directory, fixture.model.selectedInstallation?.directory)

        fixture.model.primaryAction()
        try await eventually { !fixture.model.isLaunching }
        let repeated = await fixture.session.events
        XCTAssertEqual(Array(repeated.suffix(3)), ["download", "launchPlan", "execute"])
        XCTAssertEqual(fixture.launcher.plans.count, 2)
    }

    func testCancelledLaunchVerificationNeverAuthorizesOrExecutesAndKeepsLaunchAction() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.downloadSelected()
        try await eventually { !fixture.model.isDownloading }
        await fixture.session.holdNextDownload()
        fixture.model.primaryAction()
        try await eventually { await fixture.session.waitingForDownload }
        fixture.model.cancelDownload()
        await fixture.session.completeDownload()
        try await eventually { !fixture.model.isLaunching }
        let plans = await fixture.session.launchRequests
        XCTAssertTrue(plans.isEmpty)
        XCTAssertTrue(fixture.launcher.plans.isEmpty)
        XCTAssertEqual(fixture.model.selectedInstallation?.needsVerification, true)
        XCTAssertEqual(fixture.model.primaryActionTitle, L10n.string("model.steam.launch"))
        XCTAssertNil(fixture.model.errorMessage)
    }

    func testCancelledLaunchAuthorizationPersistsRotationWithoutExecuting() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.downloadSelected()
        try await eventually { !fixture.model.isDownloading }
        await fixture.session.holdNextLaunchPlan()
        fixture.model.primaryAction()
        try await eventually { await fixture.session.waitingForLaunchPlan }
        fixture.model.cancelDownload()
        await fixture.session.completeLaunchPlan(rotated: credentials("cancelled-launch-rotation"))
        try await eventually { !fixture.model.isLaunching }
        let saved = await fixture.store.value
        XCTAssertEqual(saved?.refreshToken, "cancelled-launch-rotation")
        XCTAssertTrue(fixture.launcher.plans.isEmpty)
        XCTAssertNil(fixture.model.errorMessage)
        XCTAssertEqual(fixture.model.primaryActionTitle, L10n.string("model.steam.launch"))
    }

    func testSignOutDiscardsLateLaunchPlanAndDoesNotExecute() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.downloadSelected()
        try await eventually { !fixture.model.isDownloading }
        await fixture.session.holdNextLaunchPlan()
        fixture.model.primaryAction()
        try await eventually { await fixture.session.waitingForLaunchPlan }
        fixture.model.signOut()
        await fixture.session.completeLaunchPlan(rotated: credentials("abandoned-launch"))
        try await eventually {
            let returned = await fixture.session.returnedLaunchPlans
            return !fixture.model.isSigningOut && returned == 1
        }
        let saved = await fixture.store.value
        XCTAssertNil(saved)
        XCTAssertTrue(fixture.launcher.plans.isEmpty)
        XCTAssertFalse(fixture.model.isLaunching)
        XCTAssertFalse(fixture.model.isDownloading)
        XCTAssertTrue(fixture.model.installations.isEmpty)
    }

    func testChangingSelectionDoesNotRetargetPendingLaunch() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.downloadSelected()
        try await eventually { !fixture.model.isDownloading }
        await fixture.session.holdNextLaunchPlan()
        fixture.model.primaryAction()
        try await eventually { await fixture.session.waitingForLaunchPlan }
        fixture.model.open(game("other"))
        XCTAssertFalse(fixture.model.primaryActionAvailable)
        await fixture.session.completeLaunchPlan()
        try await eventually { !fixture.model.isLaunching }
        XCTAssertEqual(fixture.launcher.plans.first?.appName, fixture.game.appName)
        XCTAssertEqual(fixture.model.selectedGame?.appName, "other")
        XCTAssertNil(fixture.model.selectedInstallation)
        XCTAssertTrue(fixture.model.primaryActionAvailable)
    }

    func testVerificationAndLaunchAuthorizationFailuresNeverExecute() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.downloadSelected()
        try await eventually { !fixture.model.isDownloading }
        await fixture.session.failNextDownload(rotated: credentials("verify-failure"))
        fixture.model.primaryAction()
        try await eventually { !fixture.model.isLaunching }
        let firstPlans = await fixture.session.launchRequests
        XCTAssertTrue(firstPlans.isEmpty)
        XCTAssertTrue(fixture.launcher.plans.isEmpty)
        XCTAssertNotNil(fixture.model.errorMessage)
        XCTAssertEqual(fixture.model.primaryActionTitle, L10n.string("model.steam.launch"))

        await fixture.session.failNextLaunchPlan(rotated: credentials("plan-failure"))
        fixture.model.primaryAction()
        try await eventually { !fixture.model.isLaunching }
        let saved = await fixture.store.value
        XCTAssertEqual(saved?.refreshToken, "plan-failure")
        XCTAssertTrue(fixture.launcher.plans.isEmpty)
        XCTAssertEqual(fixture.model.errorMessage, L10n.string("model.epic.network"))
    }

    func testUninstallDeletesCapturedEpicInstallationAndRestoresDownloadAction() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.downloadSelected()
        try await eventually { !fixture.model.isDownloading }
        let receipt = try XCTUnwrap(fixture.model.selectedInstallation)
        XCTAssertTrue(fixture.model.canUninstallSelectedGame)
        fixture.model.uninstall(receipt)
        XCTAssertTrue(fixture.model.isUninstalling)
        XCTAssertFalse(fixture.model.primaryActionAvailable)
        XCTAssertFalse(fixture.model.canUninstallSelectedGame)
        fixture.model.uninstall(receipt)
        fixture.model.open(game("other"))
        try await eventually { !fixture.model.isUninstalling }
        XCTAssertFalse(FileManager.default.fileExists(atPath: receipt.directory))
        XCTAssertTrue(fixture.model.installations.isEmpty)
        XCTAssertEqual(fixture.model.selectedGame?.appName, "other")
        XCTAssertNil(fixture.model.errorMessage)
        XCTAssertNil(fixture.model.transfer)
        let file = try XCTUnwrap(receiptFiles(in: fixture.directory).first)
        XCTAssertTrue(try JSONDecoder().decode([EpicInstallReceipt].self, from: Data(contentsOf: file)).isEmpty)
        fixture.model.open(fixture.game)
        XCTAssertEqual(fixture.model.primaryActionTitle, L10n.string("ui.game.download"))
        XCTAssertTrue(fixture.model.primaryActionAvailable)
    }

    func testUninstallKeepsEpicReceiptAfterFilesystemLockFailure() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.downloadSelected()
        try await eventually { !fixture.model.isDownloading }
        let receipt = try XCTUnwrap(fixture.model.selectedInstallation)
        let descriptor = Darwin.open(URL(fileURLWithPath: receipt.directory).appending(path: ".forge-install.lock").path,
                                     O_RDWR | O_CREAT, S_IRUSR | S_IWUSR)
        XCTAssertGreaterThanOrEqual(descriptor, 0)
        defer { Darwin.close(descriptor) }
        XCTAssertEqual(flock(descriptor, LOCK_EX | LOCK_NB), 0)
        fixture.model.uninstall(receipt)
        try await eventually { !fixture.model.isUninstalling }
        XCTAssertEqual(fixture.model.selectedInstallation, receipt)
        XCTAssertNotNil(fixture.model.errorMessage)
        XCTAssertTrue(FileManager.default.fileExists(atPath: receipt.directory))
    }

    func testUninstallRejectsEpicStaleReceiptAndActiveDownload() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.downloadSelected()
        try await eventually { !fixture.model.isDownloading }
        let receipt = try XCTUnwrap(fixture.model.selectedInstallation)
        let forged = EpicInstallReceipt(namespace: receipt.namespace, appName: receipt.appName, name: receipt.name,
                                        directory: fixture.directory.path, platform: receipt.platform, totalBytes: receipt.totalBytes)
        fixture.model.uninstall(forged)
        XCTAssertFalse(fixture.model.isUninstalling)
        await fixture.session.holdNextDownload()
        fixture.model.primaryAction()
        fixture.model.uninstall(receipt)
        XCTAssertFalse(fixture.model.isUninstalling)
        XCTAssertFalse(fixture.model.canUninstallSelectedGame)
        try await eventually { await fixture.session.waitingForDownload }
        fixture.model.cancelDownload()
        await fixture.session.completeDownload(cancelled: true)
        try await eventually { !fixture.model.isLaunching }
        XCTAssertNotNil(fixture.model.selectedInstallation)
        XCTAssertTrue(FileManager.default.fileExists(atPath: receipt.directory))
    }

    func testSignOutBeforeEpicUninstallStartsPreservesGameAndReceipt() async throws {
        let fixture = try await fixture()
        defer { fixture.cleanUp() }
        fixture.model.downloadSelected()
        try await eventually { !fixture.model.isDownloading }
        let receipt = try XCTUnwrap(fixture.model.selectedInstallation)
        fixture.model.uninstall(receipt)
        fixture.model.signOut()
        try await eventually { !fixture.model.isUninstalling && !fixture.model.isSigningOut }
        XCTAssertFalse(fixture.model.isConnected)
        XCTAssertTrue(fixture.model.installations.isEmpty)
        XCTAssertTrue(FileManager.default.fileExists(atPath: receipt.directory))
        let file = try XCTUnwrap(receiptFiles(in: fixture.directory).first)
        XCTAssertEqual(try JSONDecoder().decode([EpicInstallReceipt].self, from: Data(contentsOf: file)), [receipt])
    }

    private func fixture(game owned: EpicGame? = nil) async throws -> EpicDownloadFixture {
        let owned = owned ?? game("one")
        let directory = temporaryDirectory()
        let credential = credentials("current")
        let store = EpicMemoryStore(credential)
        let session = EpicDownloadSession(credentials: credential, game: owned)
        let launcher = EpicLaunchRecorder()
        let model = EpicModel(directory: directory, keychain: store, restore: { _ in session }, language: "fr-FR", launch: { plan in
            await session.recordExecution()
            await launcher.execute(plan)
        })
        await model.start()
        model.open(owned)
        try await eventually { !model.isLoadingGameDetails }
        return EpicDownloadFixture(directory: directory, model: model, session: session, store: store, game: owned, launcher: launcher)
    }

    private func temporaryDirectory() -> URL {
        FileManager.default.temporaryDirectory.appending(path: "ForgeEpicDownloadTests-\(UUID().uuidString)", directoryHint: .isDirectory)
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

    private func credentials(_ token: String, account: String = "download-account") -> EpicCredentials {
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
    let launcher: EpicLaunchRecorder

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
    private var shouldHoldLaunchPlan = false
    private var launchFailure: EpicCredentials?
    private var launchContinuation: CheckedContinuation<Void, Never>?
    private(set) var waitingForLaunchPlan = false
    private(set) var returnedLaunchPlans = 0
    private(set) var launchRequests: [EpicLaunchRequest] = []
    private(set) var events: [String] = []
    private var prepareContinuation: CheckedContinuation<Void, Never>?
    private(set) var waitingForDownload = false
    private(set) var waitingForPrepare = false
    private(set) var downloads: [EpicDownloadRequest] = []
    private(set) var returnedDownloads = 0
    nonisolated private let progress = EpicDownloadProgress()
    nonisolated var cancellations: Int { progress.cancelCount }

    init(credentials: EpicCredentials, game: EpicGame) { credential = credentials; owned = game }
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
        progress.set(EpicTransfer(appName: appName, state: .downloading, completedBytes: 40, totalBytes: 100,
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
        events.append("launchPlan")
        launchRequests.append(EpicLaunchRequest(appName: appName, directory: directory, language: language))
        defer { returnedLaunchPlans += 1 }
        if shouldHoldLaunchPlan {
            shouldHoldLaunchPlan = false
            waitingForLaunchPlan = true
            await withCheckedContinuation { launchContinuation = $0 }
            waitingForLaunchPlan = false
        }
        if let failure = launchFailure {
            launchFailure = nil
            credential = failure
            throw EpicError.Network
        }
        return EpicLaunchPlan(appName: appName, platform: platform, target: LaunchTarget(executable: directory + "/fixture",
                              arguments: [], workingDirectory: directory, isApplicationBundle: false), ownershipToken: nil)
    }
    func holdNextLaunchPlan() { shouldHoldLaunchPlan = true }
    func failNextLaunchPlan(rotated: EpicCredentials) { launchFailure = rotated }
    func completeLaunchPlan(rotated: EpicCredentials? = nil) {
        if let rotated { credential = rotated }
        launchContinuation?.resume()
        launchContinuation = nil
    }
    func recordExecution() { events.append("execute") }
    nonisolated func downloadProgress() -> EpicTransfer? { progress.snapshot }
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
    private var value: EpicTransfer?
    private var cancelled = 0
    var snapshot: EpicTransfer? { lock.withLock { value } }
    var cancelCount: Int { lock.withLock { cancelled } }
    func set(_ transfer: EpicTransfer) { lock.withLock { value = transfer } }
    func finish(_ state: TransferState) { lock.withLock { value?.state = state } }
    func cancel() { lock.withLock { cancelled += 1; value?.state = .cancelled } }
}

private enum EpicDownloadTestFailure: Error { case timeout }

private struct EpicLaunchRequest: Sendable {
    let appName: String
    let directory: String
    let language: String
}

@MainActor
private final class EpicLaunchRecorder {
    private(set) var plans: [EpicLaunchPlan] = []
    var shouldHold = false
    private(set) var isWaiting = false
    private var continuation: CheckedContinuation<Void, Never>?

    func execute(_ plan: EpicLaunchPlan) async {
        plans.append(plan)
        if shouldHold {
            shouldHold = false
            isWaiting = true
            await withCheckedContinuation { continuation = $0 }
            isWaiting = false
        }
    }
    func complete() { continuation?.resume(); continuation = nil }
}
