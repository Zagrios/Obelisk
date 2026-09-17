import AppKit
import Darwin
import SwiftUI
import XCTest
@testable import Forge

final class SteamPresentationTests: XCTestCase {
    @MainActor
    func testLocalVerificationDoesNotPresentManifestBytesAsDownloadedGameData() {
        // A second pass may receive a small manifest, then validate hundreds of MB
        // already on disk without requesting a single game-content chunk.
        let transfer = SteamTransfer(appId: 620, state: .verifying, completedBytes: 400_000_000, totalBytes: 522_734_781, networkBytes: 253, contentBytes: 0, currentFile: "Game_Data/resources.assets", message: "")
        XCTAssertEqual(transfer.activityTitle, L10n.string("ui.transfer.verifying_files"))
        XCTAssertEqual(transfer.verifiedProgressText, L10n.string("ui.transfer.verified_files", SteamFormatting.bytes(400_000_000), SteamFormatting.bytes(522_734_781)))
        XCTAssertEqual(transfer.downloadedGameDataText, L10n.string("ui.transfer.no_downloaded_data"))
    }

    @MainActor
    func testActualChunkDownloadReportsReceivedGameDataSeparatelyFromVerifiedFiles() {
        let downloading = SteamTransfer(appId: 620, state: .downloading, completedBytes: 400_000_000, totalBytes: 522_734_781, networkBytes: 12_345 + 253, contentBytes: 12_345, currentFile: "Game_Data/resources.assets", message: "")
        XCTAssertEqual(downloading.activityTitle, L10n.string("ui.transfer.downloading"))
        XCTAssertEqual(downloading.downloadedGameDataText, L10n.string("ui.transfer.downloaded_data", SteamFormatting.bytes(12_345)))
        XCTAssertEqual(downloading.verifiedProgressText, L10n.string("ui.transfer.verified_files", SteamFormatting.bytes(400_000_000), SteamFormatting.bytes(522_734_781)))

        // Returning to disk verification must update the activity while preserving
        // the number of game bytes actually received earlier in this operation.
        var verifying = downloading
        verifying.state = .verifying
        XCTAssertEqual(verifying.activityTitle, L10n.string("ui.transfer.verifying_files"))
        XCTAssertEqual(verifying.downloadedGameDataText, downloading.downloadedGameDataText)
    }

    @MainActor
    func testManifestPreparationAndLocalCompletionDoNotClaimAFileDownload() {
        var transfer = SteamTransfer(appId: 620, state: .preparing, completedBytes: 0, totalBytes: 0, networkBytes: 253, contentBytes: 0, currentFile: "", message: "")
        XCTAssertEqual(transfer.activityTitle, L10n.string("ui.transfer.checking_updates"))
        XCTAssertNil(transfer.downloadedGameDataText)

        transfer.state = .complete
        transfer.completedBytes = 522_734_781
        transfer.totalBytes = transfer.completedBytes
        XCTAssertEqual(transfer.activityTitle, L10n.string("ui.transfer.up_to_date"))
        XCTAssertNil(transfer.downloadedGameDataText)
    }

    @MainActor
    func testRestoredLibraryOpensGameDetailsAndRejectsLatePreviousGameResponses() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let details = FixtureResponses<DetailsRequest, SteamGameDetails>()
        let news = FixtureResponses<UInt32, [SteamNews]>()
        let fixture = FixtureSteamSession(games: fixtureGames, details: details)
        let model = SteamModel(
            directory: root,
            keychain: FixtureCredentialStore(),
            connect: { _ in fixture },
            news: { try await news.request($0) },
            localeIdentifier: "fr-FR"
        )

        await model.start()
        XCTAssertTrue(model.isConnected)
        XCTAssertEqual(model.filteredGames.map(\.appId), [620, 570, 440])
        XCTAssertFalse(model.isLoadingGames)
        XCTAssertNil(model.qrURL)
        XCTAssertFalse(FileManager.default.fileExists(atPath: root.path))

        model.open(fixtureGames[0])
        XCTAssertEqual(model.selectedGame?.appId, 620)
        try await eventually { await details.wasRequested(DetailsRequest(appId: 620, language: "french")) }
        try await eventually { await news.wasRequested(620) }

        model.open(fixtureGames[1])
        try await eventually { await details.wasRequested(DetailsRequest(appId: 570, language: "french")) }
        try await eventually { await news.wasRequested(570) }
        await details.release(DetailsRequest(appId: 570, language: "french"), value: fixtureDetails(570))
        await news.release(570, value: fixtureNews(570))
        try await eventually { model.details?.appId == 570 && model.news.first?.id == "570-news" }

        // The first game finishes later. Its results must not replace the open game.
        await details.release(DetailsRequest(appId: 620, language: "french"), value: fixtureDetails(620))
        await news.release(620, value: fixtureNews(620))
        await Task.yield()
        await Task.yield()
        XCTAssertEqual(model.selectedGame?.appId, 570)
        XCTAssertEqual(model.details?.appId, 570)
        XCTAssertEqual(model.news.first?.id, "570-news")
        XCTAssertFalse(model.isLoadingDetails)
        XCTAssertFalse(model.isLoadingNews)

        model.backToLibrary()
        XCTAssertNil(model.selectedGame)
        XCTAssertNil(model.details)
        XCTAssertTrue(model.news.isEmpty)
        XCTAssertEqual(model.games.count, 3)
    }

    @MainActor
    func testVerticalScrollerStaysVisibleAcrossLibraryLoadingDetailAndLoadedNews() async throws {
        for size in [NSSize(width: 900, height: 700), NSSize(width: 903, height: 728)] {
            try await verifyScrollerStability(size: size)
        }
    }

    @MainActor
    private func verifyScrollerStability(size: NSSize) async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let details = FixtureResponses<DetailsRequest, SteamGameDetails>()
        let news = FixtureResponses<UInt32, [SteamNews]>()
        let library = fixtureGames + (0..<21).map { index in
            SteamGame(appId: UInt32(10_000 + index), name: "Test game \(index)", iconHash: "", lastPlayed: 0, playtimeMinutes: 0)
        }
        let session = FixtureSteamSession(games: library, details: details)
        let model = SteamModel(directory: root, keychain: FixtureCredentialStore(), connect: { _ in session }, news: { try await news.request($0) }, localeIdentifier: "fr-FR")
        await model.start()

        let host = NSHostingView(rootView: SteamView(model: model).frame(width: size.width, height: size.height))
        let rect = NSRect(x: -20_000, y: -20_000, width: size.width, height: size.height)
        let window = NSWindow(contentRect: rect, styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.sharingType = .none
        window.contentView = host
        window.setFrameOrigin(rect.origin)
        window.orderBack(nil)
        defer { window.close() }

        try await eventually {
            host.layoutSubtreeIfNeeded()
            window.displayIfNeeded()
            guard let scroll = self.firstScrollView(in: host), let document = scroll.documentView else { return false }
            return document.frame.height > scroll.contentView.bounds.height
        }
        let originalScroll = try XCTUnwrap(firstScrollView(in: host))
        let originalScroller = try XCTUnwrap(originalScroll.verticalScroller)
        let viewportWidth = originalScroll.contentView.bounds.width
        let librarySearchFrame = try XCTUnwrap(accessibilityFrame(in: host, label: L10n.string("ui.search.store_library", L10n.string("ui.store.steam"))), "The search field must be accessible for the layout regression")

        func assertVisibleScroller(_ phase: String) throws {
            let scroll = try XCTUnwrap(firstScrollView(in: host))
            XCTAssertTrue(scroll === originalScroll, "The scroll view must survive \(phase)")
            let scroller = try XCTUnwrap(scroll.verticalScroller)
            let knob = scroller.rect(for: .knob).intersection(scroller.bounds)
            let document = try XCTUnwrap(scroll.documentView)
            let diagnostics = "Steam layout during \(phase), fixture \(size): sameScroller=\(scroller === originalScroller), hidden=\(scroller.isHidden), enabled=\(scroller.isEnabled), alpha=\(scroller.alphaValue), knob=\(NSStringFromRect(knob)), proportion=\(scroller.knobProportion), host=\(NSStringFromRect(host.bounds)), scroll=\(NSStringFromRect(scroll.frame)), scroller=\(NSStringFromRect(scroller.frame)), clipFrame=\(NSStringFromRect(scroll.contentView.frame)), clipBounds=\(NSStringFromRect(scroll.contentView.bounds)), documentFrame=\(NSStringFromRect(document.frame)), documentBounds=\(NSStringFromRect(document.bounds)), contentInsets=\(scroll.contentInsets), scrollerInsets=\(scroll.scrollerInsets), documentSafeArea=\(document.safeAreaInsets), autoInsets=\(scroll.automaticallyAdjustsContentInsets), searchAX=\(String(describing: accessibilityFrame(in: host, label: L10n.string("ui.search.store_library", L10n.string("ui.store.steam"))))), backAX=\(String(describing: accessibilityFrame(in: host, label: L10n.string("ui.steam.library")))), actionAX=\(String(describing: accessibilityFrame(in: host, label: L10n.string("ui.game.download"))))"
            XCTContext.runActivity(named: "Scrollbar during \(phase)") { activity in
                activity.add(XCTAttachment(string: diagnostics))
            }
            XCTAssertTrue(scroller === originalScroller, "The same scrollbar must survive \(phase)")
            XCTAssertFalse(scroller.isHidden, "The vertical scroller disappeared during \(phase)")
            XCTAssertGreaterThan(scroller.alphaValue, 0, "The scrollbar is transparent during \(phase)")
            XCTAssertEqual(scroll.scrollerStyle, .legacy, "Overlay scrollers can fade out during \(phase)")
            XCTAssertEqual(scroll.contentView.bounds.width, viewportWidth, accuracy: 0.5, "The content width changed with the scroller during \(phase)")
        }
        try assertVisibleScroller("the library")

        model.searchText = "Portal 2"
        try await eventually {
            host.layoutSubtreeIfNeeded()
            window.displayIfNeeded()
            guard let document = originalScroll.documentView else { return false }
            return document.frame.height <= originalScroll.contentView.bounds.height
        }
        try assertVisibleScroller("the filtered library")
        let filteredSearchFrame = try XCTUnwrap(accessibilityFrame(in: host, label: L10n.string("ui.search.store_library", L10n.string("ui.store.steam"))))
        // The clear button takes space at the field's right edge once text exists.
        XCTAssertEqual(filteredSearchFrame.minX, librarySearchFrame.minX, accuracy: 0.5, "The search field shifted horizontally when filtering the library")
        model.searchText = ""
        try await eventually {
            host.layoutSubtreeIfNeeded()
            window.displayIfNeeded()
            guard let document = originalScroll.documentView else { return false }
            return document.frame.height > originalScroll.contentView.bounds.height
        }

        // Keep both responses pending so the detail is temporarily shorter than
        // the viewport. This is when the existing scrollbar used to disappear.
        model.open(fixtureGames[0])
        try await eventually { await details.wasRequested(DetailsRequest(appId: 620, language: "french")) }
        try await eventually { await news.wasRequested(620) }
        try await eventually {
            host.layoutSubtreeIfNeeded()
            window.displayIfNeeded()
            guard let scroll = self.firstScrollView(in: host), let document = scroll.documentView else { return false }
            return document.frame.height <= scroll.contentView.bounds.height
        }
        XCTAssertTrue(model.isLoadingNews)
        XCTAssertFalse(originalScroller.isEnabled, "A page that fits must remain non-scrollable")
        try assertVisibleScroller("detail loading")
        // This hosting fixture exposes the native search field, but not the
        // detail's virtual SwiftUI buttons, even in an onscreen window. The
        // mandatory search-frame assertion above covers the horizontal shift;
        // detail button positions still require a check in the running app.

        let articles = (0..<8).map { index in
            SteamNews(id: "scroll-news-\(index)", title: "Test news \(index)", url: "https://store.steampowered.com/news/app/620", excerpt: "A test excerpt that makes the game detail taller than the window after the news finishes loading.", date: 1_789_430_000, source: "Local fixture")
        }
        await details.release(DetailsRequest(appId: 620, language: "french"), value: fixtureDetails(620))
        try await eventually { model.details != nil && !model.isLoadingDetails }
        host.layoutSubtreeIfNeeded()
        window.displayIfNeeded()
        try assertVisibleScroller("details loaded, news pending")
        await news.release(620, value: articles)
        try await eventually {
            host.layoutSubtreeIfNeeded()
            window.displayIfNeeded()
            guard !model.isLoadingNews, let scroll = self.firstScrollView(in: host), let document = scroll.documentView else { return false }
            return document.frame.height > scroll.contentView.bounds.height
        }
        try assertVisibleScroller("loaded news")
        XCTAssertTrue(originalScroller.isEnabled)
        XCTAssertTrue(originalScroller.target === originalScroll)
        XCTAssertNotNil(originalScroller.action)
        let document = try XCTUnwrap(originalScroll.documentView)
        let halfway = (document.frame.height - originalScroll.contentView.bounds.height) / 2
        originalScroll.contentView.scroll(to: NSPoint(x: 0, y: halfway))
        originalScroll.reflectScrolledClipView(originalScroll.contentView)
        XCTAssertEqual(originalScroller.doubleValue, 0.5, accuracy: 0.02, "The same scrollbar must follow the document's position")

        model.backToLibrary()
        try await eventually {
            host.layoutSubtreeIfNeeded()
            window.displayIfNeeded()
            guard let scroll = self.firstScrollView(in: host), let document = scroll.documentView else { return false }
            return document.frame.height > scroll.contentView.bounds.height
        }
        try assertVisibleScroller("returning to the library")
    }

    @MainActor
    func testAutomaticDownloadUsesSystemLanguagePrefersMacAndCapturesSelectedGame() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let details = FixtureResponses<DetailsRequest, SteamGameDetails>()
        let news = FixtureResponses<UInt32, [SteamNews]>()
        let downloads = FixtureResponses<DownloadRequest, SteamInstallation>()
        let fixture = FixtureSteamSession(games: fixtureGames, details: details, download: { try await downloads.request($0) })
        let model = SteamModel(directory: root, keychain: FixtureCredentialStore(), connect: { _ in fixture }, news: { try await news.request($0) }, localeIdentifier: "zh-TW")
        await model.start()
        XCTAssertEqual(model.language, "tchinese")
        model.open(fixtureGames[0])
        let macRequest = DetailsRequest(appId: 620, language: "tchinese")
        await details.release(macRequest, value: fixtureDetails(620))
        await news.release(620, value: fixtureNews(620))
        try await eventually { model.details?.appId == 620 }
        XCTAssertEqual(model.platform, "macos")

        model.downloadSelected()
        XCTAssertNil(model.transfer)
        XCTAssertEqual(model.activeTransferAppId, 620)
        model.backToLibrary()
        XCTAssertNil(model.selectedGame)
        XCTAssertEqual(model.activeTransferAppId, 620)
        model.open(fixtureGames[1])
        XCTAssertEqual(model.activeTransferAppId, 620)
        let defaultDirectory = root.appending(path: "Games/Steam")
        let downloadRequest = DownloadRequest(appId: 620, platform: "macos", language: "tchinese", directory: defaultDirectory.path)
        try await eventually { await downloads.wasRequested(downloadRequest) }
        XCTAssertTrue(FileManager.default.fileExists(atPath: defaultDirectory.path))
        XCTAssertTrue(model.isDownloading)

        var windowsOnly = fixtureDetails(570)
        windowsOnly.macos = false
        await details.release(DetailsRequest(appId: 570, language: "tchinese"), value: windowsOnly)
        await news.release(570, value: fixtureNews(570))
        try await eventually { model.details?.appId == 570 }
        XCTAssertEqual(model.platform, "windows")

        let installedDirectory = defaultDirectory.appending(path: "steam-620-macos")
        try FileManager.default.createDirectory(at: installedDirectory, withIntermediateDirectories: true)
        await downloads.release(downloadRequest, value: SteamInstallation(appId: 620, name: "Portal 2", directory: installedDirectory.path, platform: "macos", language: "tchinese", totalBytes: 100))
        try await eventually { !model.isDownloading }
        XCTAssertNil(model.activeTransferAppId)
        XCTAssertEqual(model.installations.first?.appId, 620)
        XCTAssertEqual(model.installations.first?.platform, "macos")
        XCTAssertEqual(model.installations.first?.language, "tchinese")
        XCTAssertFalse(model.isLaunching)
        XCTAssertNil(model.errorMessage)
        model.open(fixtureGames[0])
        try await eventually { model.details?.appId == 620 }
        XCTAssertEqual(model.primaryActionTitle, L10n.string("model.steam.launch"))
    }

    @MainActor
    func testExistingInstallationKeepsItsDownloadDirectory() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let oldParent = root.appending(path: "Previous location")
        let installedDirectory = oldParent.appending(path: "steam-620-macos")
        try FileManager.default.createDirectory(at: installedDirectory, withIntermediateDirectories: true)
        let receipt = SteamInstallReceipt(appId: 620, name: "Portal 2", directory: installedDirectory.path, platform: "macos", language: "french", totalBytes: 100)
        try JSONEncoder().encode([receipt]).write(to: root.appending(path: "steam-installations-42.json"))
        let details = FixtureResponses<DetailsRequest, SteamGameDetails>()
        let downloads = FixtureResponses<DownloadRequest, SteamInstallation>()
        let fixture = FixtureSteamSession(games: fixtureGames, details: details, download: { try await downloads.request($0) })
        let model = SteamModel(directory: root, keychain: FixtureCredentialStore(), connect: { _ in fixture }, news: { _ in [] }, localeIdentifier: "fr-FR")
        await model.start()
        model.open(fixtureGames[0])
        await details.release(DetailsRequest(appId: 620, language: "french"), value: fixtureDetails(620))
        try await eventually { model.details != nil }
        model.downloadSelected()
        let request = DownloadRequest(appId: 620, platform: "macos", language: "french", directory: oldParent.path)
        try await eventually { await downloads.wasRequested(request) }
        XCTAssertFalse(FileManager.default.fileExists(atPath: root.appending(path: "Games/Steam").path))
        await downloads.release(request, value: SteamInstallation(appId: 620, name: "Portal 2", directory: installedDirectory.path, platform: "macos", language: "french", totalBytes: 100))
        try await eventually { !model.isDownloading }
        XCTAssertEqual(model.installations.first?.directory, installedDirectory.path)
    }

    @MainActor
    func testFailedUpdateRetainsDirectoryAndVerificationStateAcrossRestart() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let oldParent = root.appending(path: "Another drive")
        let installedDirectory = oldParent.appending(path: "steam-620-macos")
        try FileManager.default.createDirectory(at: installedDirectory, withIntermediateDirectories: true)
        let receiptFile = root.appending(path: "steam-installations-42.json")
        let receipt = SteamInstallReceipt(appId: 620, name: "Portal 2", directory: installedDirectory.path, platform: "macos", language: "french", totalBytes: 100)
        try JSONEncoder().encode([receipt]).write(to: receiptFile)
        let details = FixtureResponses<DetailsRequest, SteamGameDetails>()
        await details.release(DetailsRequest(appId: 620, language: "french"), value: fixtureDetails(620))
        let failingSession = FixtureSteamSession(games: fixtureGames, details: details, download: { _ in throw SteamError.Failure(reason: "Fixture: update interrupted") })
        let first = SteamModel(directory: root, keychain: FixtureCredentialStore(), connect: { _ in failingSession }, news: { _ in [] }, localeIdentifier: "fr-FR")
        await first.start()
        first.open(fixtureGames[0])
        try await eventually { first.details != nil }
        first.downloadSelected()
        try await eventually { !first.isDownloading && first.errorMessage != nil }
        XCTAssertNil(first.activeTransferAppId)
        let interrupted = try XCTUnwrap(first.installations.first)
        XCTAssertEqual(interrupted.directory, installedDirectory.path)
        let persisted = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: receiptFile)) as? [[String: Any]])
        XCTAssertEqual(persisted.first?["needsVerification"] as? Bool, true)

        let downloads = FixtureResponses<DownloadRequest, SteamInstallation>()
        let resumedSession = FixtureSteamSession(games: fixtureGames, details: details, download: { try await downloads.request($0) })
        let reopened = SteamModel(directory: root, keychain: FixtureCredentialStore(), connect: { _ in resumedSession }, news: { _ in [] }, localeIdentifier: "fr-FR")
        await reopened.start()
        XCTAssertEqual(reopened.installations.first?.directory, installedDirectory.path)
        reopened.open(fixtureGames[0])
        try await eventually { reopened.details != nil }
        reopened.downloadSelected()
        let request = DownloadRequest(appId: 620, platform: "macos", language: "french", directory: oldParent.path)
        try await eventually { await downloads.wasRequested(request) }
        XCTAssertFalse(FileManager.default.fileExists(atPath: root.appending(path: "Games/Steam").path))
        await downloads.release(request, value: SteamInstallation(appId: 620, name: "Portal 2", directory: installedDirectory.path, platform: "macos", language: "french", totalBytes: 100))
        try await eventually { !reopened.isDownloading }
        let verified = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: receiptFile)) as? [[String: Any]])
        XCTAssertNil(verified.first?["needsVerification"])
        XCTAssertEqual(reopened.installations.first?.directory, installedDirectory.path)
    }

    @MainActor
    func testLaunchVerifiesBeforePlanningAndExecutingDespiteSelectionChangeAndDoubleClick() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        XCTAssertTrue(fixture.model.verifyBeforeLaunch)
        XCTAssertEqual(fixture.model.primaryActionTitle, L10n.string("model.steam.launch"))
        fixture.model.primaryAction()
        XCTAssertTrue(fixture.model.isLaunching)
        XCTAssertTrue(fixture.model.isVerifyingBeforeLaunch)
        XCTAssertFalse(fixture.model.isDownloading)
        XCTAssertNil(fixture.model.transfer)
        XCTAssertEqual(fixture.model.activeTransferAppId, 620)
        XCTAssertEqual(fixture.model.primaryActionTitle, L10n.string("model.steam.verifying"))
        fixture.model.primaryAction()
        fixture.model.open(fixtureGames[1])
        XCTAssertEqual(fixture.model.activeTransferAppId, 620)
        try await eventually { await fixture.downloads.wasRequested(fixture.request) }
        XCTAssertTrue(fixture.model.isDownloading)
        XCTAssertEqual(fixture.model.activeTransferAppId, 620)
        let pending = await fixture.events.snapshot()
        XCTAssertEqual(pending, ["verify:620"])
        await fixture.downloads.release(fixture.request, value: fixture.installation)
        try await eventually { !fixture.model.isLaunching }
        XCTAssertNil(fixture.model.activeTransferAppId)
        let completed = await fixture.events.snapshot()
        XCTAssertEqual(completed, ["verify:620", "plan:620", "execute:620"])
        let downloadCount = await fixture.downloads.numberOfRequests(fixture.request)
        XCTAssertEqual(downloadCount, 1)
        XCTAssertEqual(fixture.model.selectedGame?.appId, 570)
        XCTAssertNil(fixture.model.errorMessage)
        XCTAssertNil(fixture.model.installations.first?.needsVerification)
    }

    @MainActor
    func testLaunchCanSkipVerificationAndSettingPersists() async throws {
        let fixture = try await launchFixture(verify: false)
        defer { fixture.cleanUp() }
        fixture.model.primaryAction()
        XCTAssertNil(fixture.model.activeTransferAppId)
        try await eventually { !fixture.model.isLaunching }
        let completed = await fixture.events.snapshot()
        XCTAssertEqual(completed, ["plan:620", "execute:620"])
        let restarted = SteamModel(directory: fixture.root, preferences: fixture.preferences)
        XCTAssertFalse(restarted.verifyBeforeLaunch)
        restarted.verifyBeforeLaunch = true
        let reloaded = SteamModel(directory: fixture.root, preferences: fixture.preferences)
        XCTAssertTrue(reloaded.verifyBeforeLaunch)
    }

    @MainActor
    func testUnverifiedReceiptForcesVerificationWhenPreferenceDisabled() async throws {
        let fixture = try await launchFixture(verify: false, needsVerification: true)
        defer { fixture.cleanUp() }
        fixture.model.primaryAction()
        try await eventually { await fixture.downloads.wasRequested(fixture.request) }
        XCTAssertTrue(fixture.model.isVerifyingBeforeLaunch)
        await fixture.downloads.release(fixture.request, value: fixture.installation)
        try await eventually { !fixture.model.isLaunching }
        let completed = await fixture.events.snapshot()
        XCTAssertEqual(completed, ["verify:620", "plan:620", "execute:620"])
    }

    @MainActor
    func testFailedVerificationDoesNotLaunch() async throws {
        let fixture = try await launchFixture(failVerification: true)
        defer { fixture.cleanUp() }
        fixture.model.primaryAction()
        try await eventually { !fixture.model.isLaunching }
        XCTAssertNil(fixture.model.activeTransferAppId)
        XCTAssertNotNil(fixture.model.errorMessage)
        XCTAssertEqual(fixture.model.installations.first?.needsVerification, true)
        let completed = await fixture.events.snapshot()
        XCTAssertEqual(completed, ["verify:620"])
    }

    @MainActor
    func testCancelledVerificationDoesNotLaunchEvenIfDownloadReturnsSuccess() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        fixture.model.primaryAction()
        try await eventually { await fixture.downloads.wasRequested(fixture.request) }
        XCTAssertEqual(fixture.model.activeTransferAppId, 620)
        fixture.model.cancelDownload()
        await fixture.downloads.release(fixture.request, value: fixture.installation)
        try await eventually { !fixture.model.isLaunching }
        XCTAssertNil(fixture.model.activeTransferAppId)
        XCTAssertNil(fixture.model.errorMessage)
        XCTAssertEqual(fixture.model.installations.first?.needsVerification, true)
        let completed = await fixture.events.snapshot()
        XCTAssertEqual(completed, ["verify:620"])
    }

    @MainActor
    func testSignOutBeforeLaunchTaskStartsDoesNotRestoreBusyStateOrStartVerification() async throws {
        let fixture = try await launchFixture(cancelAuthRestart: true)
        defer { fixture.cleanUp() }
        // There is deliberately no suspension between these two actions.
        fixture.model.primaryAction()
        fixture.model.signOut()
        XCTAssertFalse(fixture.model.isLaunching)
        XCTAssertFalse(fixture.model.isDownloading)
        XCTAssertNil(fixture.model.activeTransferAppId)
        await Task.yield()
        await Task.yield()
        await Task.yield()
        XCTAssertFalse(fixture.model.isConnected)
        XCTAssertFalse(fixture.model.isLaunching)
        XCTAssertFalse(fixture.model.isDownloading)
        XCTAssertTrue(fixture.model.installations.isEmpty)
        let events = await fixture.events.snapshot()
        XCTAssertTrue(events.isEmpty, "A cancelled launch task must not even start verification after sign-out")
    }

    @MainActor
    func testCancelBeforeLaunchTaskStartsClearsBusyStateWithoutVerification() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        fixture.model.primaryAction()
        XCTAssertEqual(fixture.model.activeTransferAppId, 620)
        fixture.model.cancelDownload()
        try await eventually { !fixture.model.isLaunching }
        XCTAssertFalse(fixture.model.isDownloading)
        XCTAssertNil(fixture.model.activeTransferAppId)
        let events = await fixture.events.snapshot()
        XCTAssertTrue(events.isEmpty)
    }

    @MainActor
    func testUninstallRemovesCapturedSteamGameAndPersistsReceiptsWhenSelectionChanges() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        let receipt = try XCTUnwrap(fixture.model.selectedInstallation)
        XCTAssertTrue(fixture.model.canUninstallSelectedGame)
        fixture.model.uninstall(receipt)
        XCTAssertTrue(fixture.model.isUninstalling)
        XCTAssertFalse(fixture.model.primaryActionAvailable)
        XCTAssertFalse(fixture.model.canUninstallSelectedGame)
        fixture.model.uninstall(receipt)
        fixture.model.open(fixtureGames[1])
        try await eventually { !fixture.model.isUninstalling }
        XCTAssertNil(fixture.model.errorMessage)
        XCTAssertFalse(FileManager.default.fileExists(atPath: receipt.directory))
        XCTAssertTrue(fixture.model.installations.isEmpty)
        XCTAssertEqual(fixture.model.selectedGame?.appId, fixtureGames[1].appId)
        let stored = try JSONDecoder().decode([SteamInstallReceipt].self, from: Data(contentsOf: fixture.root.appending(path: "steam-installations-42.json")))
        XCTAssertTrue(stored.isEmpty)
        fixture.model.open(fixtureGames[0])
        try await eventually { fixture.model.details != nil }
        XCTAssertEqual(fixture.model.primaryActionTitle, L10n.string("model.steam.download"))
        XCTAssertTrue(fixture.model.primaryActionAvailable)
    }

    @MainActor
    func testUninstallKeepsSteamReceiptAfterFilesystemLockFailure() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
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
        XCTAssertTrue(fixture.model.canUninstallSelectedGame)
    }

    @MainActor
    func testUninstallRejectsSteamStaleReceiptAndActiveLaunch() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        let receipt = try XCTUnwrap(fixture.model.selectedInstallation)
        let forged = SteamInstallReceipt(appId: receipt.appId, name: receipt.name, directory: fixture.root.path,
                                         platform: receipt.platform, language: receipt.language, totalBytes: receipt.totalBytes)
        fixture.model.uninstall(forged)
        XCTAssertFalse(fixture.model.isUninstalling)
        fixture.model.primaryAction()
        fixture.model.uninstall(receipt)
        XCTAssertFalse(fixture.model.isUninstalling)
        XCTAssertFalse(fixture.model.canUninstallSelectedGame)
        fixture.model.cancelDownload()
        try await eventually { !fixture.model.isLaunching }
        XCTAssertEqual(fixture.model.selectedInstallation, receipt)
        XCTAssertTrue(FileManager.default.fileExists(atPath: receipt.directory))
    }

    @MainActor
    func testUninstallReconcilesSteamActionIfReceiptPersistenceFailsAfterRemoval() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        let receipt = try XCTUnwrap(fixture.model.selectedInstallation)
        let receiptFile = fixture.root.appending(path: "steam-installations-42.json")
        try FileManager.default.removeItem(at: receiptFile)
        try FileManager.default.createDirectory(at: receiptFile, withIntermediateDirectories: true)
        fixture.model.uninstall(receipt)
        try await eventually { !fixture.model.isUninstalling }
        XCTAssertFalse(FileManager.default.fileExists(atPath: receipt.directory))
        XCTAssertNil(fixture.model.selectedInstallation)
        XCTAssertNotNil(fixture.model.errorMessage)
        XCTAssertEqual(fixture.model.primaryActionTitle, L10n.string("model.steam.download"))
        XCTAssertTrue(fixture.model.primaryActionAvailable)
    }

    @MainActor
    func testSignOutBeforeSteamUninstallTaskStartsPreservesGameAndReceipt() async throws {
        let fixture = try await launchFixture(cancelAuthRestart: true)
        defer { fixture.cleanUp() }
        let receipt = try XCTUnwrap(fixture.model.selectedInstallation)
        fixture.model.uninstall(receipt)
        fixture.model.signOut()
        try await eventually { !fixture.model.isUninstalling }
        XCTAssertFalse(fixture.model.isConnected)
        XCTAssertTrue(fixture.model.installations.isEmpty)
        XCTAssertTrue(FileManager.default.fileExists(atPath: receipt.directory))
        let stored = try JSONDecoder().decode([SteamInstallReceipt].self, from: Data(contentsOf: fixture.root.appending(path: "steam-installations-42.json")))
        XCTAssertEqual(stored, [receipt])
    }

    @MainActor
    func testRunningSessionBlocksRelaunchVerificationAndUninstallUntilActualExit() async throws {
        let lifetime = FixtureResponses<UInt32, Bool>()
        let fixture = try await launchFixture(verify: false, lifetime: lifetime)
        defer { fixture.cleanUp() }
        fixture.model.primaryAction()
        fixture.model.primaryAction()
        try await eventually { fixture.model.gameSessions.active?.running == true }
        XCTAssertFalse(fixture.model.isLaunching)
        XCTAssertEqual(fixture.model.primaryActionTitle, L10n.string("model.steam.running"))
        XCTAssertFalse(fixture.model.primaryActionAvailable)
        XCTAssertFalse(fixture.model.canUninstallSelectedGame)
        let receipt = try XCTUnwrap(fixture.model.selectedInstallation)
        fixture.model.primaryAction()
        fixture.model.uninstall(receipt)
        fixture.model.downloadSelected()
        fixture.model.download(fixtureGames[0], platform: "macos", language: "french", directory: fixture.root)
        XCTAssertFalse(fixture.model.isDownloading)
        XCTAssertFalse(fixture.model.isUninstalling)
        XCTAssertNotNil(fixture.model.selectedInstallation)
        let reservation = try XCTUnwrap(fixture.model.gameSessions.active?.id)
        fixture.model.gameSessions.receive(.host(.disconnected), for: reservation)
        XCTAssertEqual(fixture.model.gameSessionMessage, L10n.string("model.steam.gameDisconnected"))
        fixture.model.gameSessions.receive(.host(.online), for: reservation)
        XCTAssertNil(fixture.model.gameSessionMessage)
        fixture.model.gameSessions.receive(.host(.lost), for: reservation)
        XCTAssertEqual(fixture.model.gameSessionMessage, L10n.string("model.steam.gameHostLost"))
        XCTAssertFalse(fixture.model.primaryActionAvailable)
        XCTAssertNil(fixture.model.gameSessions.reserve(appId: 570))
        let events = await fixture.events.snapshot()
        XCTAssertEqual(events, ["plan:620", "execute:620"])
        await lifetime.release(620, value: true)
        try await eventually { !fixture.model.gameSessions.isBusy }
        XCTAssertTrue(fixture.model.primaryActionAvailable)
        XCTAssertTrue(fixture.model.canUninstallSelectedGame)
        XCTAssertNil(fixture.model.gameSessionMessage)
    }

    @MainActor
    func testSignOutWhileGameRunsKeepsGlobalReservationUntilExit() async throws {
        let lifetime = FixtureResponses<UInt32, Bool>()
        let fixture = try await launchFixture(verify: false, cancelAuthRestart: true, lifetime: lifetime)
        defer { fixture.cleanUp() }
        fixture.model.primaryAction()
        try await eventually { fixture.model.gameSessions.active?.running == true }
        fixture.model.signOut()
        XCTAssertTrue(fixture.model.gameSessions.isBusy)
        let replacement = SteamModel(directory: fixture.root, preferences: fixture.preferences, gameSessions: fixture.model.gameSessions)
        XCTAssertTrue(replacement.gameSessions.protects(620))
        XCTAssertNil(replacement.gameSessions.reserve(appId: 570))
        await lifetime.release(620, value: true)
        try await eventually { !replacement.gameSessions.isBusy }
        XCTAssertFalse(fixture.model.isLaunching)
        XCTAssertNil(fixture.model.errorMessage)
    }

    @MainActor
    func testFailedProcessStartReleasesReservationAndReportsError() async throws {
        let fixture = try await launchFixture(verify: false, failLaunch: true)
        defer { fixture.cleanUp() }
        fixture.model.primaryAction()
        try await eventually { !fixture.model.gameSessions.isBusy }
        XCTAssertNotNil(fixture.model.errorMessage)
        XCTAssertTrue(fixture.model.primaryActionAvailable)
    }

    @MainActor
    private func launchFixture(verify: Bool? = nil, needsVerification: Bool? = nil, failVerification: Bool = false, cancelAuthRestart: Bool = false, lifetime: FixtureResponses<UInt32, Bool>? = nil, failLaunch: Bool = false) async throws -> LaunchFixture {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        let parent = root.appending(path: "Installed games")
        let installedDirectory = parent.appending(path: "steam-620-macos")
        try FileManager.default.createDirectory(at: installedDirectory, withIntermediateDirectories: true)
        let receipt = SteamInstallReceipt(appId: 620, name: "Portal 2", directory: installedDirectory.path, platform: "macos", language: "french", totalBytes: 100, needsVerification: needsVerification)
        try JSONEncoder().encode([receipt]).write(to: root.appending(path: "steam-installations-42.json"))
        let suite = "ForgeLaunchFixture.\(UUID().uuidString)"
        let preferences = try XCTUnwrap(UserDefaults(suiteName: suite))
        if let verify { preferences.set(verify, forKey: SteamModel.verifyBeforeLaunchPreferenceKey) }
        let details = FixtureResponses<DetailsRequest, SteamGameDetails>()
        await details.release(DetailsRequest(appId: 620, language: "french"), value: fixtureDetails(620))
        await details.release(DetailsRequest(appId: 570, language: "french"), value: fixtureDetails(570))
        let downloads = FixtureResponses<DownloadRequest, SteamInstallation>()
        let events = FixtureEvents()
        let session = FixtureSteamSession(games: fixtureGames, details: details, download: { request in
            await events.append("verify:\(request.appId)")
            if failVerification { throw SteamError.Failure(reason: "Fixture: verification failed") }
            return try await downloads.request(request)
        }, launchPlan: { appId, platform, directory in
            await events.append("plan:\(appId)")
            return SteamLaunchPlan(appId: appId, platform: platform, target: LaunchTarget(executable: directory + "/Game.app", arguments: [], workingDirectory: directory, isApplicationBundle: true))
        })
        let credentialStore = FixtureCredentialStore(cancelTaskOnRemove: cancelAuthRestart)
        let model = SteamModel(directory: root, keychain: credentialStore, connect: { _ in session }, news: { _ in [] }, localeIdentifier: "fr-FR", preferences: preferences, gameSessions: SteamGameSessions(), launch: { plan, credentials, onEvent in
            XCTAssertEqual(credentials.steamId, 42)
            XCTAssertEqual(credentials.accountName, "Test account")
            XCTAssertEqual(credentials.refreshToken, "fixture-only-never-sent")
            let loads = await credentialStore.loadCount
            XCTAssertEqual(loads, 1, "Launching must reuse the connected session without reading Keychain again")
            await events.append("execute:\(plan.appId)")
            if failLaunch { throw SteamError.Failure(reason: "model.steam.gameExited") }
            if let lifetime {
                onEvent(.running)
                _ = try await lifetime.request(plan.appId)
            }
        })
        await model.start()
        model.open(fixtureGames[0])
        try await eventually { model.details != nil }
        return LaunchFixture(root: root, model: model, preferences: preferences, suite: suite, downloads: downloads, events: events, request: DownloadRequest(appId: 620, platform: "macos", language: "french", directory: parent.path), installation: SteamInstallation(appId: 620, name: "Portal 2", directory: installedDirectory.path, platform: "macos", language: "french", totalBytes: 100))
    }

    @MainActor
    func testRenderFixtureLibraryAndDetailWhenRequested() async throws {
        guard let destination = ProcessInfo.processInfo.environment["FORGE_RENDER_PREVIEWS"], !destination.isEmpty else {
            throw XCTSkip("Set FORGE_RENDER_PREVIEWS to a build/previews directory to render isolated fixtures")
        }
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let details = FixtureResponses<DetailsRequest, SteamGameDetails>()
        let news = FixtureResponses<UInt32, [SteamNews]>()
        let fixture = FixtureSteamSession(games: fixtureGames, details: details)
        let model = SteamModel(directory: root, keychain: FixtureCredentialStore(), connect: { _ in fixture }, news: { try await news.request($0) }, localeIdentifier: "fr-FR")
        await model.start()
        let output = URL(fileURLWithPath: destination, isDirectory: true)
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        try await render(model, to: output.appending(path: "steam-library-fixture.png"))

        model.open(fixtureGames[0])
        await details.release(DetailsRequest(appId: 620, language: "french"), value: fixtureDetails(620))
        await news.release(620, value: fixtureNews(620))
        try await eventually { model.details != nil && !model.isLoadingNews }
        try await render(model, to: output.appending(path: "steam-detail-fixture.png"))

        let installed = try await launchFixture()
        defer { installed.cleanUp() }
        XCTAssertEqual(installed.model.primaryActionTitle, L10n.string("model.steam.launch"))
        try await render(installed.model, to: output.appending(path: "steam-detail-installed-fixture.png"))
    }

    @MainActor
    private func accessibilityFrame(in root: Any, label: String, depth: Int = 0) -> NSRect? {
        guard depth < 30, let element = root as? NSObject else { return nil }
        if label == L10n.string("ui.search.store_library", L10n.string("ui.store.steam")),
           let field = root as? NSTextField, field.isEditable, field.placeholderString == L10n.string("ui.search.placeholder"),
           let window = field.window {
            return window.convertToScreen(field.convert(field.bounds, to: nil))
        }
        // SwiftUI nodes may implement the public accessibility getters without
        // declaring conformance to the complete NSAccessibilityProtocol.
        func attribute(_ key: String) -> Any? {
            guard element.responds(to: NSSelectorFromString(key)) else { return nil }
            return element.value(forKey: key)
        }
        let names = ["accessibilityLabel", "accessibilityTitle", "accessibilityValue"]
            .compactMap { attribute($0) as? String }
        if names.contains(label), let frame = attribute("accessibilityFrame") as? NSValue {
            return frame.rectValue
        }
        let children = (attribute("accessibilityChildren") as? [Any] ?? [])
            + (attribute("accessibilityChildrenInNavigationOrder") as? [Any] ?? [])
        for child in children {
            if let frame = accessibilityFrame(in: child, label: label, depth: depth + 1) { return frame }
        }
        if let view = root as? NSView {
            for child in view.subviews {
                if let frame = accessibilityFrame(in: child, label: label, depth: depth + 1) { return frame }
            }
        }
        return nil
    }

    @MainActor
    private func firstScrollView(in view: NSView) -> NSScrollView? {
        if let scroll = view as? NSScrollView { return scroll }
        for child in view.subviews {
            if let scroll = firstScrollView(in: child) { return scroll }
        }
        return nil
    }

    @MainActor
    private func render(_ model: SteamModel, to file: URL) async throws {
        let view = SteamView(model: model)
            .frame(width: 900, height: 1000)
            .environment(\.colorScheme, .dark)
            .tint(ForgeStyle.accent)
            .background(ForgePalette(colorScheme: .dark).background)
        // ScrollView contains AppKit views, which ImageRenderer may omit entirely.
        // Host the fixture in its own offscreen window so AppKit lays out and paints it.
        let host = NSHostingView(rootView: view)
        let rect = NSRect(x: -20_000, y: -20_000, width: 900, height: 1000)
        let window = NSWindow(contentRect: rect, styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.sharingType = .none
        window.contentView = host
        window.setFrameOrigin(rect.origin)
        window.orderBack(nil)
        defer { window.close() }
        host.layoutSubtreeIfNeeded()
        try await Task.sleep(for: .milliseconds(200))
        host.layoutSubtreeIfNeeded()
        window.displayIfNeeded()
        let bitmap = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: bitmap)

        // A PNG file size alone cannot distinguish real content from an empty surface.
        var luminances: [Double] = []
        for y in stride(from: 0, to: bitmap.pixelsHigh, by: 8) {
            for x in stride(from: 0, to: bitmap.pixelsWide, by: 8) {
                if let color = bitmap.colorAt(x: x, y: y)?.usingColorSpace(.deviceRGB) {
                    luminances.append(Double(0.2126 * color.redComponent + 0.7152 * color.greenComponent + 0.0722 * color.blueComponent))
                }
            }
        }
        let count = Double(luminances.count)
        let mean = luminances.reduce(0, +) / max(count, 1)
        let variance = luminances.reduce(0) { $0 + ($1 - mean) * ($1 - mean) } / max(count, 1)
        XCTAssertGreaterThan(variance, 0.002, "The preview must contain visible interface content, not a uniform background")
        guard variance > 0.002 else { throw FixtureFailure.emptyPreview }
        let png = try XCTUnwrap(bitmap.representation(using: .png, properties: [:]))
        try png.write(to: file)
    }

    @MainActor
    private func eventually(_ predicate: @MainActor () async -> Bool) async throws {
        let deadline = ContinuousClock.now + .seconds(3)
        while !(await predicate()) {
            if ContinuousClock.now >= deadline { throw FixtureFailure.timeout }
            try await Task.sleep(for: .milliseconds(5))
        }
    }
}

private actor FixtureCredentialStore: SteamCredentialStore {
    private let cancelTaskOnRemove: Bool
    private var value: SteamCredentials? = SteamCredentials(accountName: "Test account", steamId: 42, refreshToken: "fixture-only-never-sent")
    init(cancelTaskOnRemove: Bool = false) { self.cancelTaskOnRemove = cancelTaskOnRemove }
    private(set) var loadCount = 0
    func load() -> SteamCredentials? { loadCount += 1; return value }
    func save(_ credentials: SteamCredentials) { value = credentials }
    func remove() {
        value = nil
        // Prevent this offline sign-out test from starting a real QR HTTP session.
        if cancelTaskOnRemove { withUnsafeCurrentTask { $0?.cancel() } }
    }
}

private struct DetailsRequest: Hashable, Sendable {
    let appId: UInt32
    let language: String
}

private struct DownloadRequest: Hashable, Sendable {
    let appId: UInt32
    let platform: String
    let language: String
    let directory: String
}

private actor FixtureResponses<Key: Hashable & Sendable, Value: Sendable> {
    private var values: [Key: Value] = [:]
    private var pending: [Key: [CheckedContinuation<Value, any Error>]] = [:]
    private var requests: [Key: Int] = [:]

    func request(_ key: Key) async throws -> Value {
        requests[key, default: 0] += 1
        if let value = values[key] { return value }
        return try await withCheckedThrowingContinuation { pending[key, default: []].append($0) }
    }

    func release(_ key: Key, value: Value) {
        values[key] = value
        for continuation in pending.removeValue(forKey: key) ?? [] { continuation.resume(returning: value) }
    }

    func wasRequested(_ key: Key) -> Bool { requests[key] != nil }
    func numberOfRequests(_ key: Key) -> Int { requests[key, default: 0] }
}

private final class FixtureSteamSession: SteamSessionProtocol, Sendable {
    private let library: [SteamGame]
    private let responses: FixtureResponses<DetailsRequest, SteamGameDetails>
    private let downloadHandler: @Sendable (DownloadRequest) async throws -> SteamInstallation
    private let launchHandler: @Sendable (UInt32, String, String) async throws -> SteamLaunchPlan

    init(games: [SteamGame], details: FixtureResponses<DetailsRequest, SteamGameDetails>, download: @escaping @Sendable (DownloadRequest) async throws -> SteamInstallation = { _ in throw FixtureFailure.downloadNotExpected }, launchPlan: @escaping @Sendable (UInt32, String, String) async throws -> SteamLaunchPlan = { _, _, _ in throw FixtureFailure.launchNotExpected }) {
        library = games
        responses = details
        downloadHandler = download
        launchHandler = launchPlan
    }

    func games() async throws -> [SteamGame] { library }
    func profile() async throws -> SteamProfile { SteamProfile(displayName: nil, avatarUrl: nil) }
    func details(appId: UInt32, language: String) async throws -> SteamGameDetails {
        try await responses.request(DetailsRequest(appId: appId, language: language))
    }
    func download(appId: UInt32, platform: String, language: String, directory: String) async throws -> SteamInstallation {
        try await downloadHandler(DownloadRequest(appId: appId, platform: platform, language: language, directory: directory))
    }
    func downloadProgress() -> SteamTransfer? { nil }
    func cancelDownload() {}
    func launchPlan(appId: UInt32, platform: String, directory: String) async throws -> SteamLaunchPlan {
        try await launchHandler(appId, platform, directory)
    }
}

private enum FixtureFailure: Error { case timeout, downloadNotExpected, launchNotExpected, emptyPreview }

private actor FixtureEvents {
    private var values: [String] = []
    func append(_ value: String) { values.append(value) }
    func snapshot() -> [String] { values }
}

@MainActor
private struct LaunchFixture {
    let root: URL
    let model: SteamModel
    let preferences: UserDefaults
    let suite: String
    let downloads: FixtureResponses<DownloadRequest, SteamInstallation>
    let events: FixtureEvents
    let request: DownloadRequest
    let installation: SteamInstallation

    func cleanUp() {
        preferences.removePersistentDomain(forName: suite)
        try? FileManager.default.removeItem(at: root)
    }
}

private let fixtureGames = [
    SteamGame(appId: 620, name: "Portal 2", iconHash: "", lastPlayed: 1_789_430_000, playtimeMinutes: 824),
    SteamGame(appId: 570, name: "Dota 2", iconHash: "", lastPlayed: 1_789_300_000, playtimeMinutes: 4200),
    SteamGame(appId: 440, name: "Team Fortress 2", iconHash: "", lastPlayed: 0, playtimeMinutes: 0),
]

private func fixtureDetails(_ appId: UInt32) -> SteamGameDetails {
    SteamGameDetails(appId: appId, name: fixtureGames.first { $0.appId == appId }?.name ?? "Game", windows: true, macos: true, windowsBytes: 12_300_000_000, macosBytes: 11_900_000_000)
}

private func fixtureNews(_ appId: UInt32) -> [SteamNews] {
    [SteamNews(id: "\(appId)-news", title: "Test news for the game detail", url: "https://store.steampowered.com/news/app/\(appId)", excerpt: "This excerpt verifies how news appears in presentation tests. It covers the article title, date, and space available for text.", date: 1_789_430_000, source: "Local fixture")]
}
