import AppKit
import SwiftUI
import XCTest
@testable import Obelisk

final class SteamPresentationTests: XCTestCase {
    @MainActor
    func testInstalledGameLaunchesWithoutRemoteDetails() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let receipts = [
            InstallationReceipt.steam(appId: 620, name: "Portal 2", directory: root.appending(path: "steam-620-macos").path, platform: .mac, totalBytes: 100),
            InstallationReceipt.steam(appId: 570, name: "Dota 2", directory: root.appending(path: "steam-570-windows").path, platform: .windows, totalBytes: 200)
        ]
        let details = FixtureResponses<DetailsRequest, SteamGameDetails>()
        let session = FixtureSteamSession(games: fixtureGames, details: details)
        let model = SteamModel(directory: root, keychain: FixtureCredentialStore(), connect: { _ in session },
                               news: { _ in [] }, localeIdentifier: "fr-FR", gameSessions: SteamGameSessions(),
                               installations: SteamModel.installations(FakeInstallations(receipts: receipts)))
        await model.start()

        model.open(fixtureGames[0])
        XCTAssertNil(model.details)
        XCTAssertFalse(model.isLoadingDetails, "An installed game needs no remote details")
        XCTAssertEqual(model.platform, .mac)
        XCTAssertEqual(model.installations.selectedReceipt, receipts[0])
        XCTAssertEqual(model.installations.selectedAction?.title, L10n.string("model.game.launch"))
        XCTAssertEqual(model.installations.selectedAction?.launches, true)
        XCTAssertEqual(model.installations.selectedAction?.isAvailable, true)

        // Switching away from a Mac game must not carry its platform into a
        // Windows installation.
        model.open(fixtureGames[1])
        XCTAssertFalse(model.isLoadingDetails)
        XCTAssertEqual(model.platform, .windows)
        XCTAssertEqual(model.installations.selectedReceipt, receipts[1])
        XCTAssertEqual(model.installations.selectedAction?.title, L10n.string("model.game.launch"))
        XCTAssertEqual(model.installations.selectedAction?.isAvailable, true)

        model.open(fixtureGames[2])
        XCTAssertTrue(model.isLoadingDetails)
        XCTAssertNil(model.installations.selectedReceipt)
        XCTAssertEqual(model.installations.selectedAction?.title, L10n.string("model.game.download"))
        XCTAssertEqual(model.installations.selectedAction?.launches, false)
        XCTAssertEqual(model.installations.selectedAction?.isAvailable, false, "A download waits for the details")
        await details.release(DetailsRequest(appId: 440, language: "french"), value: fixtureDetails(440))
        try await eventually { model.details != nil }
        XCTAssertEqual(model.installations.selectedAction?.isAvailable, true)
        let portalRequested = await details.wasRequested(DetailsRequest(appId: 620, language: "french"))
        let dotaRequested = await details.wasRequested(DetailsRequest(appId: 570, language: "french"))
        XCTAssertFalse(portalRequested || dotaRequested, "Installed games never request remote details")
    }

    @MainActor
    func testRunningActionUsesPlaySymbolBeforeDetailsAndWithoutAccountReceipt() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let details = FixtureResponses<DetailsRequest, SteamGameDetails>()
        let session = FixtureSteamSession(games: fixtureGames, details: details)
        let sessions = SteamGameSessions()
        var running = GameSessionState.steam(appID: 620)
        sessions.fake.show(running)
        try await eventually { sessions.active?.running == true }
        let model = SteamModel(directory: root, keychain: FixtureCredentialStore(), connect: { _ in session },
                               news: { _ in [] }, localeIdentifier: "fr-FR", gameSessions: sessions)
        await model.start()
        model.open(fixtureGames[0])
        XCTAssertNil(model.details)
        XCTAssertNil(model.installations.selectedReceipt)
        XCTAssertEqual(model.installations.selectedAction?.title, L10n.string("model.game.quit"))
        XCTAssertEqual(model.installations.selectedAction?.launches, true)
        XCTAssertEqual(model.installations.selectedAction?.isAvailable, true, "the running game can be quit")
        await details.release(DetailsRequest(appId: 620, language: "french"), value: fixtureDetails(620))
        try await eventually { model.details != nil }
        XCTAssertEqual(model.installations.selectedAction?.launches, true)
        running.reconnecting = true
        sessions.fake.show(running)
        try await eventually { sessions.active?.reconnecting == true }
        XCTAssertEqual(sessions.active?.phase, .running)
        XCTAssertEqual(sessions.active?.host, .online)
        XCTAssertEqual(model.gameSessionMessage, L10n.string("model.steam.recoveringSession"))
        XCTAssertEqual(model.installations.selectedAction?.title, L10n.string("model.game.running"))
        XCTAssertEqual(model.installations.selectedAction?.isAvailable, false)
        XCTAssertEqual(sessions.active?.game, 620, "The game stays reserved while it reconnects")
        XCTAssertTrue(sessions.isBusy)
        running.reconnecting = false
        sessions.fake.show(running)
        try await eventually { sessions.active?.reconnecting == false }
        XCTAssertNil(model.gameSessionMessage)
        sessions.fake.show(nil)
        try await eventually { !sessions.isBusy }
        XCTAssertEqual(model.installations.selectedAction?.launches, false)
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
        XCTAssertEqual(model.library.shown.map(\.appId), [620, 570, 440])
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

        model.closeGame()
        XCTAssertNil(model.selectedGame)
        XCTAssertNil(model.details)
        XCTAssertTrue(model.news.isEmpty)
        XCTAssertEqual(model.library.games.count, 3)
    }

    @MainActor
    func testLibraryLoadsAllArtworkInBackgroundBeforeAnyGameIsOpened() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let details = FixtureResponses<DetailsRequest, SteamGameDetails>()
        let platforms = FixtureResponses<[UInt32], [SteamGamePlatforms]>()
        let session = FixtureSteamSession(games: fixtureGames, details: details,
                                          platforms: { try await platforms.request($0.sorted()) })
        let model = SteamModel(directory: root, keychain: FixtureCredentialStore(), connect: { _ in session },
                               news: { _ in [] }, localeIdentifier: "fr-FR", gameSessions: SteamGameSessions())

        await model.start()
        let appIDs = fixtureGames.map(\.appId).sorted()
        try await eventually { await platforms.wasRequested(appIDs) }
        XCTAssertEqual(model.library.shown.map(\.appId), fixtureGames.map(\.appId))
        XCTAssertFalse(model.isLoadingGames, "App info must not block the library")
        XCTAssertTrue(model.libraryArtwork.games.isEmpty)
        XCTAssertNil(model.selectedGame)
        XCTAssertNil(model.details)
        for appID in appIDs {
            let requests = await details.numberOfRequests(DetailsRequest(appId: appID, language: "french"))
            XCTAssertEqual(requests, 0, "Library metadata must not open game details")
        }

        let portal = SteamGameArtwork(cover: "620/library_600x900.jpg", hero: nil, logo: nil, header: nil)
        await platforms.release(appIDs, value: [
            SteamGamePlatforms(appId: 620, platforms: ["macos", "windows"], artwork: portal),
            SteamGamePlatforms(appId: 570, platforms: ["windows"]),
            SteamGamePlatforms(appId: 440, platforms: ["linux"])
        ])
        try await eventually { !model.libraryArtwork.games.isEmpty }
        XCTAssertEqual(model.libraryArtwork.games, [620: portal])
        let requests = await platforms.numberOfRequests(appIDs)
        XCTAssertEqual(requests, 1)
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
            guard let scroll = firstScrollView(in: host), let document = scroll.documentView else { return false }
            return document.frame.height > scroll.contentView.bounds.height
        }
        // The library and a game's page each keep their own scroll view, whose
        // scrollbar floats over the artwork. Neither changes the content width.
        var scrolling = try StableScroller(in: host)
        let librarySearchFrame = try XCTUnwrap(accessibilityFrame(in: host, label: L10n.string("ui.search.store_library", L10n.string("ui.store.steam"))), "The search field must be accessible for the layout regression")
        try scrolling.assertKept(in: host, during: "the library")

        model.library.searchText = "Portal 2"
        try await eventually {
            host.layoutSubtreeIfNeeded()
            window.displayIfNeeded()
            guard let document = scrolling.scroll.documentView else { return false }
            return document.frame.height <= scrolling.scroll.contentView.bounds.height
        }
        try scrolling.assertKept(in: host, during: "the filtered library")
        let filteredSearchFrame = try XCTUnwrap(accessibilityFrame(in: host, label: L10n.string("ui.search.store_library", L10n.string("ui.store.steam"))))
        // The clear button takes space at the field's right edge once text exists.
        XCTAssertEqual(filteredSearchFrame.minX, librarySearchFrame.minX, accuracy: 0.5, "The search field shifted horizontally when filtering the library")
        model.library.searchText = ""
        try await eventually {
            host.layoutSubtreeIfNeeded()
            window.displayIfNeeded()
            guard let document = scrolling.scroll.documentView else { return false }
            return document.frame.height > scrolling.scroll.contentView.bounds.height
        }

        // Keep both responses pending: the page must keep its scroll view
        // and width while details, then news, arrive.
        model.open(fixtureGames[0])
        try await eventually { await details.wasRequested(DetailsRequest(appId: 620, language: "french")) }
        try await eventually { await news.wasRequested(620) }
        try await eventually {
            host.layoutSubtreeIfNeeded()
            window.displayIfNeeded()
            guard let scroll = firstScrollView(in: host) else { return false }
            return scroll !== scrolling.scroll
        }
        scrolling = try StableScroller(in: host, width: size.width)
        XCTAssertTrue(model.isLoadingNews)
        try scrolling.assertKept(in: host, during: "detail loading")
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
        try scrolling.assertKept(in: host, during: "details loaded, news pending")
        await news.release(620, value: articles)
        try await eventually {
            host.layoutSubtreeIfNeeded()
            window.displayIfNeeded()
            guard !model.isLoadingNews, let scroll = firstScrollView(in: host), let document = scroll.documentView else { return false }
            return document.frame.height > scroll.contentView.bounds.height
        }
        try scrolling.assertKept(in: host, during: "loaded news")
        let (scroll, scroller) = (scrolling.scroll, scrolling.scroller)
        XCTAssertTrue(scroller.isEnabled)
        XCTAssertTrue(scroller.target === scroll)
        XCTAssertNotNil(scroller.action)
        let document = try XCTUnwrap(scroll.documentView)
        let halfway = (document.frame.height - scroll.contentView.bounds.height) / 2
        scroll.contentView.scroll(to: NSPoint(x: 0, y: halfway))
        scroll.reflectScrolledClipView(scroll.contentView)
        XCTAssertEqual(scroller.doubleValue, 0.5, accuracy: 0.02, "The same scrollbar must follow the document's position")

        model.closeGame()
        try await eventually {
            host.layoutSubtreeIfNeeded()
            window.displayIfNeeded()
            guard let scroll = firstScrollView(in: host), let document = scroll.documentView else { return false }
            return scroll !== scrolling.scroll && document.frame.height > scroll.contentView.bounds.height
        }
        try StableScroller(in: host).assertKept(in: host, during: "returning to the library")
    }

    @MainActor
    func testAutomaticDownloadUsesSystemLanguageShowsTheNativeBuildAndCapturesSelectedGame() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let details = FixtureResponses<DetailsRequest, SteamGameDetails>()
        let news = FixtureResponses<UInt32, [SteamNews]>()
        let backend = FakeInstallations<SteamGames>()
        backend.onCall = { call in
            guard case let .install(offer) = call, let appId = offer.steamApp else { return }
            backend.publish {
                $0.downloading = true
                $0.gate = .action(appId)
                $0.transferGame = appId
                $0.transfer = GameTransfer(game: "\(appId)", state: .preparing, completedBytes: 0, totalBytes: 0,
                                           networkBytes: 0, contentBytes: 0, currentFile: "", message: "")
            }
        }
        let fixture = FixtureSteamSession(games: fixtureGames, details: details)
        let model = SteamModel(directory: root, keychain: FixtureCredentialStore(), connect: { _ in fixture }, news: { try await news.request($0) }, localeIdentifier: "zh-TW", installations: SteamModel.installations(backend))
        await model.start()
        XCTAssertEqual(model.language, "tchinese")
        model.open(fixtureGames[0])
        await details.release(DetailsRequest(appId: 620, language: "tchinese"), value: fixtureDetails(620))
        await news.release(620, value: fixtureNews(620))
        try await eventually { model.details?.appId == 620 }
        XCTAssertEqual(model.platform, .mac)

        model.installations.selectedAction?.perform()

        XCTAssertEqual(backend.actions, [.install(GameOffer.steam(appId: 620, name: "Portal 2", language: "tchinese"))])
        XCTAssertEqual(model.installations.transfer?.state, .preparing, "Progress shows before its first snapshot")
        XCTAssertEqual(model.installations.activeTransferGame, 620)
        model.closeGame()
        XCTAssertNil(model.selectedGame)
        XCTAssertEqual(model.installations.activeTransferGame, 620)
        model.open(fixtureGames[1])
        XCTAssertEqual(model.installations.activeTransferGame, 620)
        var windowsOnly = fixtureDetails(570)
        windowsOnly.platform = .windows
        await details.release(DetailsRequest(appId: 570, language: "tchinese"), value: windowsOnly)
        await news.release(570, value: fixtureNews(570))
        try await eventually { model.details?.appId == 570 }
        XCTAssertEqual(model.platform, .windows)

        backend.publish {
            $0.downloading = false
            $0.gate = .open
            $0.transfer?.state = .complete
            $0.receipts = [.steam(appId: 620, name: "Portal 2", directory: root.appending(path: "Games/Steam/steam-620-macos").path,
                                  platform: .mac, totalBytes: 100)]
        }
        try await eventually { !model.installations.isDownloading }
        XCTAssertNil(model.installations.activeTransferGame)
        XCTAssertEqual(model.installations.receipts.first?.gameID, 620)
        XCTAssertFalse(model.installations.isLaunching)
        XCTAssertNil(model.errorMessage)
        model.open(fixtureGames[0])
        XCTAssertFalse(model.isLoadingDetails)
        XCTAssertEqual(model.installations.selectedAction?.title, L10n.string("model.game.launch"))
    }

    @MainActor
    func testLibraryLaunchesAnInstalledGameWithoutOpeningIt() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        fixture.model.closeGame()

        libraryAction(fixtureGames[0], of: fixture.model).perform()

        XCTAssertEqual(fixture.backend.actions, [.launch(fixture.receipt)])
        XCTAssertNil(fixture.model.selectedGame)
        XCTAssertEqual(libraryAction(fixtureGames[1], of: fixture.model).kind, .download(.install),
                       "A game without an installation never launches")
    }

    @MainActor
    func testLibraryDownloadLetsSteamPickTheBuildWithoutOpeningTheGame() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let details = FixtureResponses<DetailsRequest, SteamGameDetails>()
        let backend = FakeInstallations<SteamGames>()
        let fixture = FixtureSteamSession(games: fixtureGames, details: details)
        let model = SteamModel(directory: root, keychain: FixtureCredentialStore(), connect: { _ in fixture }, news: { _ in [] }, localeIdentifier: "fr-FR", installations: SteamModel.installations(backend))
        await model.start()

        libraryAction(fixtureGames[0], of: model).perform()
        XCTAssertEqual(backend.actions, [.install(GameOffer.steam(appId: 620, name: "Portal 2", language: "french"))],
                       "Steam picks the build as the Transfer starts, without the game's details")
        XCTAssertNil(model.selectedGame)
        XCTAssertNil(model.errorMessage)
    }

    @MainActor
    func testALaunchFromObeliskOrdersTheLibraryAgain() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let history = LaunchHistory(cache: LibraryCache(directory: root.path))
        let details = FixtureResponses<DetailsRequest, SteamGameDetails>()
        let fixture = FixtureSteamSession(games: fixtureGames, details: details)
        let model = SteamModel(directory: root, keychain: FixtureCredentialStore(), connect: { _ in fixture }, news: { _ in [] },
                               localeIdentifier: "fr-FR", gameSessions: SteamGameSessions(FakeSessions(), history: history))
        await model.start()

        XCTAssertEqual(model.library.shown.map(\.appId), [620, 570, 440])
        history.record(570, account: 42, in: SteamGames.self, at: Date(timeIntervalSince1970: 1_789_500_000))
        XCTAssertEqual(model.library.shown.map(\.appId), [570, 620, 440], "The connected account's launch is its last play")
    }

    @MainActor
    func testTheCachedLibraryShowsWhileSteamConnectsAndSigningOutForgetsIt() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let cached = SteamGame(appId: 620, name: "Portal 2 (cached)", iconHash: "", lastPlayed: 1_789_000_000, playtimeMinutes: 5)
        let freshArtwork = SteamGameArtwork(cover: "fresh/library_600x900.jpg", hero: nil, logo: nil, header: nil)
        let cache = try LibraryCache.holding(steam: [42: [CachedSteamGame(
            game: cached, platforms: ["macos"], addedAt: 1_300_000_000, achievementShare: 0.5
        )]], in: root)
        let connection = GameSessionGate<FixtureSteamSession>()
        let fixture = FixtureSteamSession(games: fixtureGames, details: FixtureResponses<DetailsRequest, SteamGameDetails>()) { ids in
            ids.map { SteamGamePlatforms(appId: $0, platforms: ["windows"], artwork: freshArtwork) }
        }
        let model = SteamModel(directory: root, keychain: FixtureCredentialStore(), connect: { _ in try await connection.wait() },
                               news: { _ in [] }, localeIdentifier: "fr-FR", libraryCache: cache)
        let start = Task { await model.start() }

        try await eventually { model.showsLibrary }
        XCTAssertFalse(model.isConnected, "The cached library shows before Steam answers")
        XCTAssertEqual(model.library.games, [cached])
        XCTAssertEqual(model.library.value(of: cached, for: .dateAdded), 1_300_000_000)
        XCTAssertEqual(model.library.value(of: cached, for: .achievements), 0.5)
        model.library.sort = .achievements
        XCTAssertNil(model.library.pendingMetadata, "What the cache knew orders the library before Steam answers")
        await connection.resolve(fixture)
        await start.value
        try await eventually { model.library.games == fixtureGames && model.libraryArtwork.games[620] == freshArtwork }
        XCTAssertTrue(model.showsLibrary)

        model.signOut()
        XCTAssertTrue(cache.steamLibrary(steamId: 42, language: "english").isEmpty, "Signing out forgets the account's library")
    }

    @MainActor
    func testAFailedRestorationHidesTheCachedLibrary() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let cache = try LibraryCache.holding(steam: [42: [CachedSteamGame(game: fixtureGames[0], platforms: nil)]], in: root)
        let model = SteamModel(directory: root, keychain: FixtureCredentialStore(),
                               connect: { _ in throw SteamError.Failure(reason: "backend.steam.cm.directoryUnavailable") },
                               news: { _ in [] },
                               beginLogin: { _ in throw SteamError.Failure(reason: "Fixture: sign-in is offline in tests") },
                               localeIdentifier: "fr-FR", libraryCache: cache)
        await model.start()

        XCTAssertFalse(model.showsLibrary)
        XCTAssertTrue(model.library.games.isEmpty, "Another account may sign in next")
        XCTAssertFalse(cache.steamLibrary(steamId: 42, language: "english").isEmpty, "The account may restore later")
    }

    @MainActor
    func testLaunchShowsItsVerificationAndKeepsItsGameAcrossSelectionChangeAndDoubleClick() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        XCTAssertEqual(fixture.model.installations.selectedAction?.title, L10n.string("model.game.launch"))

        fixture.model.installations.selectedAction?.perform()

        XCTAssertEqual(fixture.backend.actions, [.launch(fixture.receipt)])
        fixture.preparing(verifying: true)
        try await eventually { fixture.model.installations.isVerifyingBeforeLaunch }
        XCTAssertEqual(fixture.model.installations.activeTransferGame, 620)
        XCTAssertEqual(fixture.model.installations.selectedAction?.title, L10n.string("model.game.verifying"))
        fixture.model.installations.selectedAction?.perform()
        fixture.model.open(fixtureGames[1])
        XCTAssertEqual(fixture.model.installations.activeTransferGame, 620)
        XCTAssertEqual(fixture.backend.actions.count, 1, "A double click starts one launch")
    }

    @MainActor
    func testSignOutWhileALaunchPreparesClearsItsState() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        fixture.model.installations.selectedAction?.perform()
        fixture.preparing(verifying: true)
        try await eventually { fixture.model.installations.isLaunching }

        fixture.model.signOut()

        XCTAssertEqual(fixture.backend.actions.last, .signedOut, "Rust stops the launch still preparing")
        XCTAssertFalse(fixture.model.isConnected)
        XCTAssertFalse(fixture.model.installations.isLaunching)
        XCTAssertNil(fixture.model.installations.activeTransferGame)
        XCTAssertTrue(fixture.model.installations.receipts.isEmpty)
    }

    @MainActor
    func testUninstallRemovesCapturedSteamGameAndRestoresDownloadWhenSelectionChanges() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        let receipt = fixture.receipt
        XCTAssertTrue(fixture.model.installations.canUninstallSelectedGame)

        fixture.model.installations.uninstall(receipt)

        XCTAssertEqual(fixture.backend.actions, [.uninstall(receipt)])
        fixture.backend.publish {
            $0.uninstallGame = 620
            $0.gate = .action(620)
        }
        try await eventually { fixture.model.installations.isUninstalling }
        XCTAssertEqual(fixture.model.installations.selectedAction?.isAvailable, false)
        XCTAssertFalse(fixture.model.installations.canUninstallSelectedGame)
        fixture.model.installations.uninstall(receipt)
        XCTAssertEqual(fixture.backend.actions.count, 1)
        fixture.model.open(fixtureGames[1])
        fixture.backend.publish {
            $0.uninstallGame = nil
            $0.receipts = []
            $0.gate = .open
        }
        try await eventually { !fixture.model.installations.isUninstalling }
        XCTAssertNil(fixture.model.errorMessage)
        XCTAssertTrue(fixture.model.installations.receipts.isEmpty)
        XCTAssertEqual(fixture.model.selectedGame?.appId, fixtureGames[1].appId)
        fixture.model.open(fixtureGames[0])
        try await eventually { fixture.model.details != nil }
        XCTAssertEqual(fixture.model.installations.selectedAction?.title, L10n.string("model.game.download"))
        XCTAssertEqual(fixture.model.installations.selectedAction?.isAvailable, true)
    }

    @MainActor
    func testUninstallingTheGameOnScreenLoadsWhatADownloadNeeds() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        XCTAssertNil(fixture.model.details)
        fixture.model.installations.uninstall(fixture.receipt)
        fixture.backend.publish { $0.receipts = [] }
        try await eventually { fixture.model.details != nil }
        XCTAssertNil(fixture.model.installations.selectedReceipt)
        XCTAssertEqual(fixture.model.installations.selectedAction?.title, L10n.string("model.game.download"))
        XCTAssertEqual(fixture.model.installations.selectedAction?.isAvailable, true)
    }

    @MainActor
    func testUninstallRejectsSteamStaleReceiptAndActiveLaunch() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        let receipt = fixture.receipt
        let forged = InstallationReceipt(id: receipt.id, gameID: receipt.gameID, name: receipt.name,
                                         directory: fixture.root.path, platform: receipt.platform,
                                         totalBytes: receipt.totalBytes)
        fixture.model.installations.uninstall(forged)
        XCTAssertEqual(fixture.backend.actions, [])
        fixture.model.installations.selectedAction?.perform()
        fixture.preparing(verifying: true)
        try await eventually { fixture.model.installations.isLaunching }
        fixture.model.installations.uninstall(receipt)
        XCTAssertEqual(fixture.backend.actions.count, 1, "Only the launch reached Rust")
        XCTAssertFalse(fixture.model.installations.canUninstallSelectedGame)
    }

    @MainActor
    func testCloudProgressShowsOnTheLaunchActionAndOnlyQuestionsPresentSheets() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        let model = fixture.model
        model.installations.selectedAction?.perform()
        fixture.preparing()
        fixture.sessions.fake.show(.steam(appID: 620, phase: .preparing, cloud: .showing(.checking)))
        fixture.sync(.launch(620, decision: 17, working: true, cancellable: true))
        try await eventually { model.cloud.canCancelSynchronization(for: 620) }

        XCTAssertTrue(model.installations.isSynchronizingSaves)
        XCTAssertEqual(model.installations.selectedAction?.title, L10n.string("ui.cloud.sync_short"))
        XCTAssertEqual(model.installations.selectedAction?.isAvailable, false)
        XCTAssertFalse(model.cloud.promptPresented)
        XCTAssertFalse(model.cloud.failurePresented)
        XCTAssertFalse(model.cloud.isSynchronizing(for: 570))

        if let destination = ProcessInfo.processInfo.environment["OBELISK_RENDER_PREVIEWS"], !destination.isEmpty {
            let output = URL(fileURLWithPath: destination, isDirectory: true)
            try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
            try await render(model, to: output.appending(path: "steam-cloud-sync-button.png"))
        }

        fixture.sync(.launch(620, question: .failed(17, error: "backend.steam.cloud.network", launchAnyway: true)))
        try await eventually { model.cloud.failurePresented }
        XCTAssertFalse(model.cloud.promptPresented)
        XCTAssertFalse(model.installations.isSynchronizingSaves)
        XCTAssertTrue(model.cloud.canLaunchAnyway)
        // Error text never grants permission; only the supervisor's choices do.
        fixture.sync(.launch(620, question: .failed(17, error: "backend.steam.cloud.network")))
        try await eventually { !model.cloud.canLaunchAnyway }
        model.cloud.respond(.retry)
        try await eventually { fixture.backend.actions.last == .answer(620, decision: 17, .retry) && !model.cloud.promptBusy }

        fixture.sync(.launch(620, question: .conflict(18, error: "backend.steam.cloud.conflict")))
        try await eventually { model.cloud.promptPresented }
        XCTAssertFalse(model.cloud.failurePresented)
        XCTAssertFalse(model.cloud.canLaunchAnyway)
        model.cloud.respond(.cloud)
        try await eventually { fixture.backend.actions.last == .answer(620, decision: 18, .cloud) && !model.cloud.promptBusy }
        fixture.sync(.launch(620, working: true, cancellable: true))
        try await eventually { model.installations.isSynchronizingSaves && !model.cloud.promptPresented }

        fixture.sync(nil)
        fixture.run(.steam(appID: 620))
        try await eventually { model.gameSessions.active?.running == true }
        XCTAssertFalse(model.installations.isSynchronizingSaves)
        XCTAssertEqual(model.installations.selectedAction?.title, L10n.string("model.game.quit"))
        fixture.sessions.fake.show(.steam(appID: 620, phase: .finalizing, cloud: .showing(.syncing)))
        fixture.sync(.exit(620, working: true))
        try await eventually { model.installations.isSynchronizingSaves }
        XCTAssertEqual(model.installations.selectedAction?.title, L10n.string("ui.cloud.sync_short"))
        XCTAssertFalse(model.cloud.promptPresented)
        XCTAssertFalse(model.cloud.canCancelSynchronization(for: 620))
        fixture.sync(nil)
        fixture.run(nil)
        try await eventually { !model.gameSessions.isBusy }
        XCTAssertFalse(model.installations.isSynchronizingSaves)
        XCTAssertEqual(model.installations.selectedAction?.title, L10n.string("model.game.launch"))
        XCTAssertEqual(model.installations.selectedAction?.isAvailable, true)
    }

    @MainActor
    func testRosettaPromptSurvivesNavigationAndCancellationRestoresLaunchWithoutAnError() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        let model = fixture.model
        model.installations.selectedAction?.perform()
        fixture.preparing()
        fixture.sessions.fake.show(.steam(appID: 620, phase: .preparing, cloud: .showing(.ready)))
        fixture.sessions.fake.requireRosetta(decisionID: 17)
        try await eventually { model.rosetta.prompt != nil }
        let prompt = model.rosetta.prompt
        XCTAssertEqual(model.installations.selectedAction?.isAvailable, false)
        XCTAssertFalse(model.installations.canUninstallSelectedGame)
        model.closeGame()
        XCTAssertEqual(model.rosetta.prompt, prompt)
        model.open(fixtureGames[1])
        XCTAssertEqual(model.rosetta.prompt, prompt)
        model.installations.selectedAction?.perform()
        XCTAssertEqual(model.gameSessions.active?.game, 620)
        model.rosetta.cancel()
        XCTAssertEqual(fixture.sessions.fake.declines.map(\.decisionID), [17])
        XCTAssertNil(model.rosetta.prompt)
        XCTAssertTrue(model.gameSessions.isBusy)
        fixture.run(nil)
        try await eventually { !model.gameSessions.isBusy && !model.installations.isLaunching }
        XCTAssertNil(model.errorMessage)
    }

    @MainActor
    func testSigningOutDismissesRosettaAndRetainsPreparationUntilSupervisorFinishes() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        let model = fixture.model
        model.installations.selectedAction?.perform()
        fixture.preparing()
        fixture.sessions.fake.show(.steam(appID: 620, phase: .preparing, cloud: .showing(.ready)))
        fixture.sessions.fake.requireRosetta(decisionID: 17)
        try await eventually { model.rosetta.prompt != nil }
        model.signOut()
        XCTAssertNil(model.rosetta.prompt)
        XCTAssertTrue(model.gameSessions.isBusy)
        XCTAssertEqual(fixture.sessions.fake.declines.count, 1)
        fixture.run(nil)
        try await eventually { !model.gameSessions.isBusy }
        XCTAssertNil(model.errorMessage)
        XCTAssertFalse(model.installations.isLaunching)
    }

    @MainActor
    func testPostGameConflictPresentsImmediatelyWithoutInlineMessage() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        let model = fixture.model
        model.installations.selectedAction?.perform()
        fixture.run(.steam(appID: 620))
        try await eventually { model.gameSessions.active?.running == true }
        fixture.sessions.fake.show(.steam(appID: 620, phase: .finalizing,
                                          cloud: .asking(.conflict, decision: 4, error: "backend.steam.cloud.conflict")))
        fixture.sync(.exit(620, question: .conflict(4, error: "backend.steam.cloud.conflict")))
        try await eventually { model.cloud.promptPresented }
        XCTAssertFalse(model.cloud.failurePresented)
        XCTAssertNil(model.gameSessionMessage)

        model.cloud.respond(.cancel)
        try await eventually { !model.cloud.promptBusy }
        XCTAssertEqual(fixture.backend.actions.last, .answer(620, decision: 4, .cancel))
        // Rust closes the answered question.
        fixture.sync(.exit(620))
        try await eventually { !model.cloud.promptPresented }
        fixture.sync(nil)
        fixture.run(nil)
        try await eventually { !model.gameSessions.isBusy }
        XCTAssertFalse(model.cloud.promptPresented, "A finished session never reopens the sheet by itself")
        XCTAssertNil(model.errorMessage)
    }

    @MainActor
    func testCancellingAPreGameCloudConflictRestoresLaunchWithoutAnError() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        let model = fixture.model
        model.installations.selectedAction?.perform()
        fixture.preparing()
        fixture.sync(.launch(620, question: .conflict(4, error: "backend.steam.cloud.conflict")))
        try await eventually { model.cloud.promptPresented }

        model.cloud.respond(.cancel)
        try await eventually { fixture.backend.actions.last == .answer(620, decision: 4, .cancel) }
        // The launch ends as a cancellation, which Rust reports as no failure.
        fixture.sync(nil)
        fixture.run(nil)
        try await eventually { !model.gameSessions.isBusy && !model.installations.isLaunching }
        XCTAssertNil(model.errorMessage)
        XCTAssertNil(model.gameSessions.recoveryError)
        XCTAssertEqual(model.installations.selectedAction?.isAvailable, true)
    }

    @MainActor
    func testCloudMaintenanceHoldsTheReservationAndTheSyncMenuFollowsItsRules() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        let (model, backend) = (fixture.model, fixture.backend)
        backend.onCall = { call in
            if case .maintain = call {
                backend.publish {
                    $0.cloudSync = .maintenance(620)
                    $0.gate = .closed
                }
            }
        }
        XCTAssertTrue(model.installations.canSyncCloud)
        XCTAssertEqual(model.installations.selectedAction?.isAvailable, true)

        model.syncCloud()

        XCTAssertTrue(model.cloud.isMaintaining)
        XCTAssertEqual(backend.actions, [.maintain(620, installed: fixture.receipt)])
        XCTAssertEqual(model.installations.selectedAction?.isAvailable, false, "Maintenance holds the game's Reservation")
        XCTAssertFalse(model.installations.canUninstallSelectedGame)
        XCTAssertFalse(model.installations.canSyncCloud)
        XCTAssertEqual(model.installations.cloudIndicator?.value, L10n.string("ui.cloud.indicator.syncing"))
        backend.publish {
            $0.cloudSync = nil
            $0.gate = .open
        }
        try await eventually { !model.cloud.isMaintaining }
        XCTAssertEqual(model.installations.selectedAction?.isAvailable, true)
        XCTAssertTrue(model.installations.canSyncCloud)

        model.installations.selectedAction?.perform()
        fixture.run(.steam(appID: 620))
        try await eventually { model.gameSessions.active?.running == true }
        XCTAssertFalse(model.installations.canSyncCloud, "A running Steam game keeps Steam Cloud maintenance closed")
        fixture.run(nil)
        try await eventually { !model.gameSessions.isBusy }
        XCTAssertTrue(model.installations.canSyncCloud)
        model.signOut()
        XCTAssertFalse(model.installations.canSyncCloud)
        XCTAssertNil(model.installations.cloudIndicator)
    }

    @MainActor
    func testAFailedSyncShowsNoBannerAndSyncNowRetries() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        let (model, backend) = (fixture.model, fixture.backend)
        model.syncCloud()
        backend.publish { $0.cloudSync = .maintenance(620) }
        try await eventually { model.cloud.isMaintaining }

        backend.publish { $0.cloudSync = nil }

        try await eventually { !model.cloud.isMaintaining }
        XCTAssertFalse(model.cloud.failurePresented)
        XCTAssertTrue(model.installations.canSyncCloud, "Sync now is the retry")
        model.syncCloud()
        XCTAssertEqual(backend.actions.count, 2)
    }

    @MainActor
    func testACloudQuestionAppearsOverTheWholeAppWhateverTabIsOpen() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        let app = AppModel(directory: fixture.root, defaults: fixture.preferences, steam: fixture.model,
                           epic: EpicModel(directory: fixture.root, gameSessions: EpicGameSessions()))
        app.selection = .epic
        XCTAssertNil(app.presentedPrompt)

        fixture.model.syncCloud()
        fixture.backend.publish { $0.cloudSync = .maintenance(620, question: .conflict(1)) }

        try await eventually { app.presentedPrompt == AccountPrompt(section: .steam, kind: .cloud) }
        app.selection = .home
        XCTAssertEqual(app.presentedPrompt, AccountPrompt(section: .steam, kind: .cloud),
                       "The question does not wait for the Steam tab")
        fixture.model.cloud.respond(.cancel)
        try await eventually { fixture.backend.actions.last == .answer(620, decision: 1, .cancel) }
        fixture.backend.publish { $0.cloudSync = nil }
        try await eventually { app.presentedPrompt == nil }
    }

    @MainActor
    func testAFailedLaunchCheckAsksOverTheWholeApp() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        let app = AppModel(directory: fixture.root, defaults: fixture.preferences, steam: fixture.model,
                           epic: EpicModel(directory: fixture.root, gameSessions: EpicGameSessions()))
        XCTAssertNil(app.presentedPrompt)

        fixture.model.installations.selectedAction?.perform()
        fixture.preparing()
        fixture.sync(.launch(620, question: .failed(17, error: "backend.steam.cloud.unsupported", launchAnyway: true)))

        try await eventually { app.presentedPrompt == AccountPrompt(section: .steam, kind: .cloud) }
        XCTAssertTrue(fixture.model.cloud.failurePresented)
        fixture.model.cloud.respond(.launchAnyway)
        try await eventually { fixture.backend.actions.last == .answer(620, decision: 17, .launchAnyway) }
        fixture.sync(.launch(620, working: true, cancellable: true))
        try await eventually { app.presentedPrompt == nil }
    }

    @MainActor
    func testRunningSessionBlocksRelaunchVerificationAndUninstallUntilActualExit() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        fixture.model.installations.selectedAction?.perform()
        fixture.model.installations.selectedAction?.perform()
        var session = GameSessionState.steam(appID: 620)
        fixture.run(session)
        try await eventually { fixture.model.gameSessions.active?.running == true }
        XCTAssertFalse(fixture.model.installations.isLaunching)
        XCTAssertEqual(fixture.model.installations.selectedAction?.title, L10n.string("model.game.quit"))
        XCTAssertEqual(fixture.model.installations.selectedAction?.isAvailable, true)
        XCTAssertFalse(fixture.model.installations.canUninstallSelectedGame)
        fixture.model.installations.selectedAction?.perform()
        fixture.model.installations.uninstall(fixture.receipt)
        fixture.model.installations.verify(fixture.receipt)
        try await eventually { fixture.sessions.fake.quits == [session.id] }
        XCTAssertEqual(fixture.backend.actions.count, 1, "Only the first launch reached Rust")
        XCTAssertNotNil(fixture.model.installations.selectedReceipt)
        session.host = .disconnected
        fixture.sessions.fake.show(session)
        try await eventually { fixture.model.gameSessions.active?.host == .disconnected }
        XCTAssertEqual(fixture.model.gameSessionMessage, L10n.string("model.steam.gameDisconnected"))
        session.host = .online
        fixture.sessions.fake.show(session)
        try await eventually { fixture.model.gameSessions.active?.host == .online }
        XCTAssertNil(fixture.model.gameSessionMessage)
        session.host = .lost
        fixture.sessions.fake.show(session)
        try await eventually { fixture.model.gameSessions.active?.host == .lost }
        XCTAssertEqual(fixture.model.gameSessionMessage, L10n.string("model.steam.gameHostLost"))
        XCTAssertEqual(fixture.model.installations.selectedAction?.title, L10n.string("model.game.closing"))
        XCTAssertEqual(fixture.model.installations.selectedAction?.isAvailable, false)
        XCTAssertTrue(fixture.model.gameSessions.isBusy)
        fixture.run(nil)
        try await eventually { !fixture.model.gameSessions.isBusy }
        XCTAssertEqual(fixture.model.installations.selectedAction?.isAvailable, true)
        XCTAssertTrue(fixture.model.installations.canUninstallSelectedGame)
        XCTAssertNil(fixture.model.gameSessionMessage)
    }

    @MainActor
    func testReopenedModelRestoresRunningActionAndFileGuardsUntilRecoveredGameExits() async throws {
        let sessions = SteamGameSessions()
        sessions.fake.show(.steam(appID: 620))
        let fixture = try await launchFixture(sessions: sessions)
        defer { fixture.cleanUp() }
        fixture.backend.publish { $0.gate = .session(620) }
        try await eventually { sessions.active?.running == true && fixture.model.installations.gate == .session(620) }
        XCTAssertEqual(fixture.model.installations.selectedAction?.title, L10n.string("model.game.quit"))
        XCTAssertEqual(fixture.model.installations.selectedAction?.isAvailable, true)
        XCTAssertFalse(fixture.model.installations.canUninstallSelectedGame)
        fixture.model.installations.selectedAction?.perform()
        fixture.model.installations.uninstall(fixture.receipt)
        fixture.model.installations.verify(fixture.receipt)
        try await eventually { sessions.fake.quits.count == 1 }
        XCTAssertEqual(fixture.backend.actions, [])
        fixture.run(nil)
        try await eventually { !sessions.isBusy }
        XCTAssertEqual(fixture.model.installations.selectedAction?.title, L10n.string("model.game.launch"))
        XCTAssertEqual(fixture.model.installations.selectedAction?.isAvailable, true)
        XCTAssertTrue(fixture.model.installations.canUninstallSelectedGame)
    }

    @MainActor
    func testSignOutWhileGameRunsKeepsGlobalReservationUntilExit() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        fixture.model.installations.selectedAction?.perform()
        fixture.run(.steam(appID: 620))
        try await eventually { fixture.model.gameSessions.active?.running == true }
        fixture.model.signOut()
        XCTAssertTrue(fixture.model.gameSessions.isBusy)
        XCTAssertEqual(fixture.sessions.fake.revocations, [.steam(steamId: 42)])
        let replacement = SteamModel(directory: fixture.root, gameSessions: fixture.model.gameSessions)
        XCTAssertEqual(replacement.gameSessions.active?.game, 620)
        XCTAssertTrue(replacement.gameSessions.isBusy)
        fixture.sessions.fake.show(nil)
        try await eventually { !replacement.gameSessions.isBusy }
        XCTAssertFalse(fixture.model.installations.isLaunching)
        XCTAssertNil(fixture.model.errorMessage)
    }

    @MainActor
    func testAFailedLaunchIsReportedAndLeavesTheGameLaunchable() async throws {
        let fixture = try await launchFixture()
        defer { fixture.cleanUp() }
        fixture.model.installations.selectedAction?.perform()
        fixture.preparing()
        fixture.backend.publish {
            $0.launching = false
            $0.launchGame = nil
            $0.gate = .open
            $0.report = .storefront(key: "backend.steam.runtime.startFailed")
        }
        try await eventually { fixture.model.errorMessage != nil }
        XCTAssertFalse(fixture.model.installations.isLaunching)
        XCTAssertEqual(fixture.model.installations.selectedAction?.isAvailable, true)
    }

    @MainActor
    func testQRSignInRunsThroughTheLoginPortWithoutReachingSteam() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        let details = FixtureResponses<DetailsRequest, SteamGameDetails>()
        let session = FixtureSteamSession(games: fixtureGames, details: details)
        let starts = FixtureLoginStarts()
        let login = FixtureLogin(qrUrl: "https://s.team/q/fixture",
                                 credentials: SteamCredentials(accountName: "Test account", steamId: 42, refreshToken: "fixture-only-never-sent"))
        let model = SteamModel(directory: root, keychain: FixtureCredentialStore(value: nil),
                               connect: { _ in session }, news: { _ in [] },
                               beginLogin: { start in await starts.record(start); return login },
                               localeIdentifier: "fr-FR")

        // No stored session, so start() opens a QR challenge.
        await model.start()
        try await eventually { model.qrURL != nil }
        XCTAssertEqual(model.qrURL, "https://s.team/q/fixture")
        XCTAssertFalse(model.isCredentialChallenge)

        // The library loads once the account connects.
        try await eventually(timeout: .seconds(10)) { model.isConnected && model.library.games.count == fixtureGames.count }
        let recorded = await starts.all
        XCTAssertEqual(recorded, [.qr], "Sign-in must start exactly once, through the port")
    }

    @MainActor
    func testHomeRestoresWithoutAQRCodeAndTheSteamTabAsksForOneOnce() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        let starts = FixtureLoginStarts()
        let login = FixtureLogin(qrUrl: "https://s.team/q/fixture", failsPolling: true)
        let model = SteamModel(directory: root, keychain: FixtureCredentialStore(value: nil),
                               connect: { _ in throw SteamError.Failure(reason: "Fixture: nothing to restore") },
                               news: { _ in [] },
                               beginLogin: { start in await starts.record(start); return login },
                               localeIdentifier: "fr-FR")

        // Home restores every storefront at launch: no saved session, and no QR code.
        await model.start(signIn: false)
        XCTAssertFalse(model.isRestoring)
        XCTAssertNil(model.qrURL)
        var recorded = await starts.all
        XCTAssertEqual(recorded, [], "Restoring alone never asks Steam for a QR code")

        // The Steam tab then offers the QR sign-in, once however often it opens.
        await model.start()
        await model.start()
        try await eventually { model.qrURL != nil }
        recorded = await starts.all
        XCTAssertEqual(recorded, [.qr])
    }

    @MainActor
    func testCredentialSignInSubmitsSteamGuardAndConnects() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        let details = FixtureResponses<DetailsRequest, SteamGameDetails>()
        let session = FixtureSteamSession(games: fixtureGames, details: details)
        let starts = FixtureLoginStarts()
        let login = FixtureLogin(confirmations: [SteamGuardMethod(kind: .deviceCode, message: "Fixture")],
                                 credentials: SteamCredentials(accountName: "Test account", steamId: 42, refreshToken: "fixture-only-never-sent"),
                                 guardRequired: true)
        let model = SteamModel(directory: root, keychain: FixtureCredentialStore(value: nil),
                               connect: { _ in session }, news: { _ in [] },
                               beginLogin: { start in await starts.record(start); return login },
                               localeIdentifier: "fr-FR")

        model.signIn(account: "account", password: "password")
        try await eventually { model.challenge != nil }
        XCTAssertTrue(model.isCredentialChallenge)
        // The first offered confirmation decides which Guard prompt is shown.
        XCTAssertEqual(model.guardKind, .deviceCode)
        XCTAssertFalse(model.isConnected, "Steam Guard must gate the session")

        model.submitGuard("12345")
        try await eventually(timeout: .seconds(10)) { model.isConnected }
        XCTAssertEqual(login.submittedKinds, [.deviceCode])
        let recorded = await starts.all
        XCTAssertEqual(recorded, [.credentials(account: "account", password: "password")])
    }

    @MainActor
    func testRejectedCredentialsReportTheErrorAndFallBackToQR() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        let details = FixtureResponses<DetailsRequest, SteamGameDetails>()
        let session = FixtureSteamSession(games: fixtureGames, details: details)
        let starts = FixtureLoginStarts()
        let model = SteamModel(directory: root, keychain: FixtureCredentialStore(value: nil),
                               connect: { _ in session }, news: { _ in [] },
                               beginLogin: { start in
                                   await starts.record(start)
                                   if case .credentials = start { throw SteamError.Failure(reason: "Fixture: rejected") }
                                   return FixtureLogin(qrUrl: "https://s.team/q/fallback", failsPolling: true)
                               },
                               localeIdentifier: "fr-FR")

        model.signIn(account: "account", password: "wrong")
        try await eventually { model.errorMessage != nil }
        XCTAssertFalse(model.isSubmittingCredentials)
        XCTAssertFalse(model.isCredentialChallenge, "A rejected password returns the view to the QR challenge")

        try await eventually { model.qrURL == "https://s.team/q/fallback" }
        let recorded = await starts.all
        XCTAssertEqual(recorded, [.credentials(account: "account", password: "wrong"), .qr])
        XCTAssertFalse(model.isConnected)
    }

    /// The library's button for `game`, as its card and hero show it.
    @MainActor
    private func libraryAction(_ game: SteamGame, of model: SteamModel) -> GameAction {
        GameActions(model.installations, in: .library)(model.librarySelection(game))
    }

    @MainActor
    private func launchFixture(sessions: SteamGameSessions? = nil) async throws -> LaunchFixture {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        let installedDirectory = root.appending(path: "Installed games/steam-620-macos")
        let receipt = InstallationReceipt.steam(appId: 620, name: "Portal 2", directory: installedDirectory.path, platform: .mac, totalBytes: 100)
        let suite = "ObeliskLaunchFixture.\(UUID().uuidString)"
        let preferences = try XCTUnwrap(UserDefaults(suiteName: suite))
        let details = FixtureResponses<DetailsRequest, SteamGameDetails>()
        await details.release(DetailsRequest(appId: 620, language: "french"), value: fixtureDetails(620))
        await details.release(DetailsRequest(appId: 570, language: "french"), value: fixtureDetails(570))
        let session = FixtureSteamSession(games: fixtureGames, details: details)
        let backend = FakeInstallations<SteamGames>(receipts: [receipt])
        backend.indicators = [(620, .neverSynced)]
        // Rust holds a launch from the call on.
        backend.onCall = { call in
            guard case .launch = call else { return }
            backend.publish {
                $0.launching = true
                $0.launchGame = 620
                $0.gate = .action(620)
            }
        }
        let gameSessions = sessions ?? SteamGameSessions()
        let model = SteamModel(directory: root, keychain: FixtureCredentialStore(), connect: { _ in session }, news: { _ in [] }, beginLogin: { _ in throw SteamError.Failure(reason: "Fixture: sign-in is offline in tests") }, localeIdentifier: "fr-FR", gameSessions: gameSessions, installations: SteamModel.installations(backend))
        await model.start()
        model.open(fixtureGames[0])
        return LaunchFixture(root: root, model: model, preferences: preferences, suite: suite, backend: backend,
                             sessions: gameSessions, receipt: receipt)
    }

    @MainActor
    func testRenderFixtureLibraryAndDetailWhenRequested() async throws {
        guard let destination = ProcessInfo.processInfo.environment["OBELISK_RENDER_PREVIEWS"], !destination.isEmpty else {
            throw XCTSkip("Set OBELISK_RENDER_PREVIEWS to a build/previews directory to render isolated fixtures")
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
        XCTAssertEqual(installed.model.installations.selectedAction?.title, L10n.string("model.game.launch"))
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
    private func render(_ model: SteamModel, to file: URL) async throws {
        let view = SteamView(model: model)
            .frame(width: 900, height: 1000)
            .environment(\.colorScheme, .dark)
            .tint(ObeliskStyle.accent)
            .background(ObeliskPalette(colorScheme: .dark).background)
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
    private func eventually(timeout: Duration = .seconds(3), _ predicate: @MainActor () async -> Bool) async throws {
        let deadline = ContinuousClock.now + timeout
        while !(await predicate()) {
            if ContinuousClock.now >= deadline { throw FixtureFailure.timeout }
            try await Task.sleep(for: .milliseconds(5))
        }
    }
}

private actor FixtureCredentialStore: CredentialStore {
    private var value: SteamCredentials?
    init(value: SteamCredentials? = SteamCredentials(accountName: "Test account", steamId: 42, refreshToken: "fixture-only-never-sent")) {
        self.value = value
    }
    private(set) var loadCount = 0
    func load() -> SteamCredentials? { loadCount += 1; return value }
    func save(_ credentials: SteamCredentials) { value = credentials }
    func remove() { value = nil }
}

/// A Steam login that never reaches the network. Sign-out restarts QR sign-in,
/// so every model in these tests is built with one.
private final class FixtureLogin: SteamLoginProtocol, @unchecked Sendable {
    private let snapshot: SteamChallenge
    private let credentials: SteamCredentials?
    private let guardRequired: Bool
    private let failsPolling: Bool
    private let state = NSLock()
    private var guardAccepted = false
    private var submissions: [SteamGuardKind] = []

    init(qrUrl: String? = nil, confirmations: [SteamGuardMethod] = [], credentials: SteamCredentials? = nil,
         guardRequired: Bool = false, failsPolling: Bool = false, pollIntervalMs: UInt32 = 1) {
        snapshot = SteamChallenge(qrUrl: qrUrl, confirmations: confirmations, pollIntervalMs: pollIntervalMs, remoteInteraction: false)
        self.credentials = credentials
        self.guardRequired = guardRequired
        self.failsPolling = failsPolling
    }

    func challenge() async -> SteamChallenge { snapshot }

    func poll() async throws -> SteamCredentials? {
        if failsPolling { throw SteamError.Failure(reason: "Fixture: offline") }
        return state.withLock { guardRequired && !guardAccepted ? nil : credentials }
    }

    func submitGuard(code: String, kind: SteamGuardKind) async throws {
        guard code == "12345" else { throw SteamError.Failure(reason: "Fixture: wrong Steam Guard code") }
        state.withLock {
            submissions.append(kind)
            guardAccepted = true
        }
    }

    var submittedKinds: [SteamGuardKind] { state.withLock { submissions } }
}

private actor FixtureLoginStarts {
    private(set) var all: [SteamLoginStart] = []
    func record(_ start: SteamLoginStart) { all.append(start) }
}

private struct DetailsRequest: Hashable, Sendable {
    let appId: UInt32
    let language: String
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
    private let platformsHandler: @Sendable ([UInt32]) async throws -> [SteamGamePlatforms]

    init(games: [SteamGame], details: FixtureResponses<DetailsRequest, SteamGameDetails>, platforms: @escaping @Sendable ([UInt32]) async throws -> [SteamGamePlatforms] = { _ in [] }) {
        library = games
        responses = details
        platformsHandler = platforms
    }

    func games() async throws -> [SteamGame] { library }
    func platforms(appIds: [UInt32], language: String) async throws -> [SteamGamePlatforms] {
        try await platformsHandler(appIds)
    }
    func profile() async throws -> SteamProfile { SteamProfile(displayName: nil, avatarUrl: nil) }
    func details(appId: UInt32, language: String) async throws -> SteamGameDetails {
        try await responses.request(DetailsRequest(appId: appId, language: language))
    }
}

private enum FixtureFailure: Error { case timeout, emptyPreview }

/// A signed-in account with Portal 2 installed for macOS and on screen.
@MainActor
private struct LaunchFixture {
    let root: URL
    let model: SteamModel
    let preferences: UserDefaults
    let suite: String
    let backend: FakeInstallations<SteamGames>
    let sessions: SteamGameSessions
    let receipt: InstallationReceipt<UInt32>

    /// Rust admitted the launch, which prepares and holds the storefront.
    func preparing(verifying: Bool = false) {
        backend.publish {
            $0.launching = true
            $0.verifyingBeforeLaunch = verifying
            $0.launchGame = 620
            $0.gate = .action(620)
        }
    }

    /// Rust shows `sync` as the account's Cloud sync.
    func sync(_ sync: GameCloudSync<UInt32>?) {
        backend.publish { $0.cloudSync = sync }
    }

    /// The launch's supervisor shows `session`, which holds its game; nil once
    /// the game exited.
    func run(_ session: GameSessionState?) {
        sessions.fake.show(session)
        backend.publish {
            $0.launching = false
            $0.verifyingBeforeLaunch = false
            $0.launchGame = nil
            $0.gate = session == nil ? .open : .session(620)
        }
    }

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
    SteamGameDetails(appId: appId, name: fixtureGames.first { $0.appId == appId }?.name ?? "Game", platform: .mac, windowsBytes: 12_300_000_000, macosBytes: 11_900_000_000)
}

private func fixtureNews(_ appId: UInt32) -> [SteamNews] {
    [SteamNews(id: "\(appId)-news", title: "Test news for the game detail", url: "https://store.steampowered.com/news/app/\(appId)", excerpt: "This excerpt verifies how news appears in presentation tests. It covers the article title, date, and space available for text.", date: 1_789_430_000, source: "Local fixture")]
}
