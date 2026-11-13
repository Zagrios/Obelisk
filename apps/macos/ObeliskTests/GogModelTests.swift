import Foundation
import WebKit
import XCTest
@testable import Obelisk

/// What only GOG's adapter does. WebAccountModelTests covers the account
/// lifecycle, library and game page GOG shares with Epic.
@MainActor
final class GogModelTests: XCTestCase {
    func testTheLibraryLeavesTheBuildToGog() {
        var both = GogGames.game("1", title: "Both")
        both.macos = true
        XCTAssertEqual(both.offer, .gog(productId: "1", title: "Both"))
        let windows = GogGames.game("2", title: "Windows")
        XCTAssertEqual(windows.offer, .gog(productId: "2", title: "Windows"))
        var neither = windows
        neither.windows = false
        XCTAssertNil(neither.offer)
    }

    func testTheLibraryOrdersByWhenGalaxyTellsEachGameJoinedIt() async throws {
        var classic = GogGames.game("1", title: "Classic"), recent = GogGames.game("2", title: "Recent")
        let unknown = GogGames.game("3", title: "Unknown")
        classic.addedAt = 1_700_000_000
        recent.addedAt = 1_500_000_000
        let model = await connectedModel(GogTestSession(credentials: GogGames.credentials("current"),
                                                        games: [unknown, classic, recent]))
        defer { model.signOut() }

        XCTAssertEqual(model.library.value(of: classic, for: .dateAdded), 1_700_000_000, "Seconds, not their bits")
        model.library.sort = .dateAdded
        XCTAssertEqual(model.library.shown.map(\.title), ["Classic", "Recent", "Unknown"])
    }

    func testTheAchievementsLoadAfterThePlaytimesWithoutTheirOrder() async throws {
        let games = ["1", "2", "3", "4", "5"].map { GogGames.game($0, title: "Game \($0)") }
        let session = GogTestSession(credentials: GogGames.credentials("current"), games: games)
        await session.setAchievements(["1": [true, false, false, false], "2": [true, true], "3": [],
                                       "4": [false, false]], failing: ["5"])
        let model = await connectedModel(session)
        defer { model.signOut() }

        try await eventually { model.library.value(of: games[0], for: .achievements) != nil }
        let asked = await session.achievementReads
        XCTAssertEqual(asked, [AchievementRead(ids: ["1", "2", "3", "4", "5"], locale: "en-US", fresh: false)],
                       "One read names every game, with no order picked, in the player's language")
        model.library.sort = .achievements
        XCTAssertEqual(model.library.shown.map(\.productId), ["2", "1", "4", "3", "5"],
                       "Games without achievements, or whose request failed, follow by name")
        XCTAssertNil(model.library.pendingMetadata)
        XCTAssertNil(model.library.missingMetadata, "A game whose request failed counts as one without achievements")

        model.library.sort = .name
        model.library.sort = .achievements
        let again = await session.achievementReads
        XCTAssertEqual(again.count, 1, "Picking the order asks nothing")
    }

    func testDetailsPassThePlayersLanguage() async throws {
        var owned = GogGames.game("1", title: "Alpha")
        owned.macos = true
        let session = GogTestSession(credentials: GogGames.credentials("current"), games: [owned])
        let model = await connectedModel(session, language: "fr-FR")
        defer { model.signOut() }

        model.open(owned)
        try await eventually { !model.isLoadingGameDetails }
        let requests = await session.detailsCalls
        XCTAssertEqual(requests, [WebDetailsRequest(offer: .gog(productId: "1", title: "Alpha"), language: "fr-FR")])
    }

    func testAchievementsAskGogForTheProductAndFullTag() async throws {
        let owned = GogGames.game("1", title: "Alpha")
        let session = GogTestSession(credentials: GogGames.credentials("current"), games: [owned])
        let model = await connectedModel(session, language: "fr-FR")
        defer { model.signOut() }

        model.open(owned)
        try await eventually { model.achievements.items != nil }
        let reads = await session.achievementReads
        XCTAssertTrue(reads.contains(AchievementRead(ids: ["1"], locale: "fr-FR", fresh: false)))
        XCTAssertTrue(reads.allSatisfy { $0.locale == "fr-FR" })
    }

    func testThePageReadsTheLibraryCacheAndItsRetryReadsGogAgain() async throws {
        let owned = GogGames.game("1", title: "Alpha")
        let session = GogTestSession(credentials: GogGames.credentials("current"), games: [owned])
        let model = await connectedModel(session)
        defer { model.signOut() }

        model.open(owned)
        try await eventually { model.achievements.items?.count == 2 }
        model.reloadAchievements()
        try await eventually { await session.achievementReads.last?.fresh == true }
        let reads = await session.achievementReads
        XCTAssertTrue(reads.dropLast().allSatisfy { !$0.fresh }, "Opening the page asks the Library cache first")
    }

    func testOnlyDecimalProductIDsNameAGogGame() {
        XCTAssertEqual(GogGames.game("1207658924"), "1207658924")
        XCTAssertNil(GogGames.game("../1207658924"))
        XCTAssertNil(GogGames.game(""))
    }

    func testGogSyncsNoCloud() async throws {
        let owned = GogGames.game("1", title: "Alpha")
        let directory = FileManager.default.temporaryDirectory.appending(path: "ObeliskGogCloudTests-\(UUID().uuidString)")
        // Installed, so an Epic game in its place would show an indicator.
        let backend = FakeInstallations<GogGames>(receipts: [InstallationReceipt(
            id: "1-windows", gameID: "1", name: "Alpha", directory: directory.appending(path: "Alpha").path,
            platform: .windows, totalBytes: 100
        )])
        backend.indicators = [("1", .neverSynced)]
        defer { try? FileManager.default.removeItem(at: directory) }
        let session = GogTestSession(credentials: GogGames.credentials("current"), games: [owned])
        let model = GogModel(directory: directory, keychain: MemoryStore(GogGames.credentials("current")),
                             restore: { _ in session }, gameSessions: GameSessions(),
                             installations: GogModel.installations(backend))
        await model.start()
        defer { model.signOut() }

        model.open(owned)
        try await eventually { !model.isLoadingGameDetails }
        XCTAssertNotNil(model.installations.selectedReceipt)
        await drainMainQueue(times: 2)
        XCTAssertFalse(model.installations.canSyncCloud)
        XCTAssertNil(model.installations.cloudIndicator)
        XCTAssertNil(model.cloud.indicator(for: "1"))
        XCTAssertEqual(backend.indicatorRequests, 0, "GOG never asks Rust for a Cloud indicator")
    }

    func testTheGogWindowCapturesTheCodeBeforeTheRedirectLoads() async throws {
        let completed = expectation(description: "GOG callback intercepted")
        var loginWindow: NSWindow?
        let task = Task {
            defer { completed.fulfill() }
            return try await WebAuthentication(.gog).authorize { webView in
                loginWindow = webView.window
                webView.loadHTMLString("""
                    <html><body><script>
                    window.location.href = 'https://embed.gog.com/on_login_success?origin=client&code=fixture-gog';
                    </script></body></html>
                    """, baseURL: nil)
            }
        }
        await fulfillment(of: [completed], timeout: 10)
        task.cancel()
        let code = try await task.value
        XCTAssertEqual(code, "fixture-gog")
        XCTAssertEqual(loginWindow?.title, L10n.string("model.gogWeb.title"))
        XCTAssertEqual(loginWindow?.isVisible, false)
    }

    /// GOG's sign-in button stays disabled until Cloudflare's captcha, which
    /// runs in `about:srcdoc` frames, hands its token over.
    func testTheGogWindowLetsAFrameBuildItsOwnDocument() async throws {
        let completed = expectation(description: "GOG callback intercepted from a srcdoc frame")
        let task = Task {
            defer { completed.fulfill() }
            return try await WebAuthentication(.gog).authorize { webView in
                webView.loadHTMLString("""
                    <html><body>
                    <iframe srcdoc="<script>parent.location.href = 'https://embed.gog.com/on_login_success?origin=client&code=fixture-srcdoc'</script>"></iframe>
                    </body></html>
                    """, baseURL: nil)
            }
        }
        await fulfillment(of: [completed], timeout: 10)
        task.cancel()
        let code = try await task.value
        XCTAssertEqual(code, "fixture-srcdoc")
    }
}
