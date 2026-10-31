import AppKit
import SwiftUI
import XCTest
@testable import Obelisk

/// Which storefront's running game Obelisk follows: the menu bar item that
/// tells the lifecycle whether to ask before quitting, and the in-game
/// overlay, with what each shows of the game's storefront.
@MainActor
final class RunningGameAcrossStorefrontsTests: XCTestCase {
    // MARK: The menu bar item and the lifecycle

    /// Each storefront's game session, still preparing: its game is about to run.
    private func sessions(of fixture: StorefrontsFixture) -> [(name: String, start: () -> Void, end: () -> Void)] {
        [
            ("Steam", { fixture.steamSessions.fake.show(.steam(appID: 620, accountID: StorefrontsFixture.steamAccount,
                                                               phase: .preparing, observed: false)) },
             { fixture.steamSessions.fake.show(nil) }),
            ("Epic", { fixture.epicSessions.fake.show(.epic(account: StorefrontsFixture.epicAccount, phase: .preparing,
                                                            observed: false)) },
             { fixture.epicSessions.fake.show(nil) }),
            ("GOG", { fixture.gogSessions.fake.show(.gog(phase: .preparing, observed: false)) },
             { fixture.gogSessions.fake.show(nil) }),
        ]
    }

    func testTheAppKnowsAGameRunsWhenAnyStorefrontHasAGameSessionAndStopsEveryStorefrontsGames() async throws {
        let fixture = StorefrontsFixture()
        defer { fixture.finish() }
        XCTAssertFalse(fixture.app.runsGame)
        for account in fixture.app.accounts { XCTAssertFalse(account.hasGameSession, "\(account.section)") }

        for (name, start, end) in sessions(of: fixture) {
            start()
            try await eventually { fixture.app.runsGame }
            XCTAssertEqual(fixture.app.accounts.filter(\.hasGameSession).count, 1, "\(name) alone has a session")
            end()
            try await eventually { !fixture.app.runsGame }
        }

        await fixture.app.stopGames()
        XCTAssertEqual(fixture.steamSessions.fake.stops, 1)
        XCTAssertEqual(fixture.epicSessions.fake.stops, 1)
        XCTAssertEqual(fixture.gogSessions.fake.stops, 1)
    }

    func testTheLifecycleKnowsAGameRunsWhenAnyStorefrontHasAGameSessionAndStopsEveryStorefrontsGames() async throws {
        let fixture = StorefrontsFixture()
        defer { fixture.finish() }
        let lifecycle = AppLifecycle()
        let host = NSHostingView(rootView: AppMenuBarLabel(model: fixture.app, lifecycle: lifecycle))
        let rect = NSRect(x: -20_000, y: -20_000, width: 40, height: 40)
        let window = NSWindow(contentRect: rect, styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.sharingType = .none
        window.contentView = host
        window.setFrameOrigin(rect.origin)
        window.orderBack(nil)
        defer { window.close() }
        // The label tells the lifecycle once it appears.
        try await eventually {
            host.layoutSubtreeIfNeeded()
            window.displayIfNeeded()
            return lifecycle.libraryCache != nil
        }
        XCTAssertFalse(lifecycle.games.running())

        for (name, start, end) in sessions(of: fixture) {
            start()
            try await eventually { lifecycle.games.running() }
            end()
            try await eventually { !lifecycle.games.running() }
            XCTAssertFalse(lifecycle.games.running(), name)
        }

        await lifecycle.games.stop()
        XCTAssertEqual(fixture.steamSessions.fake.stops, 1)
        XCTAssertEqual(fixture.epicSessions.fake.stops, 1)
        XCTAssertEqual(fixture.gogSessions.fake.stops, 1)
    }

    // MARK: The overlay

    func testTheOverlayFollowsSteamsRunningGameBeforeEpicsBeforeGogs() async throws {
        let fixture = StorefrontsFixture()
        defer { fixture.finish() }
        let overlay = fixture.app.overlay
        fixture.gogSessions.fake.show(.gog(id: "gog-session"))
        fixture.epicSessions.fake.show(.epic(account: StorefrontsFixture.epicAccount, namespace: "fixture",
                                             appName: "one", id: "epic-session"))
        fixture.steamSessions.fake.show(.steam(appID: 620, accountID: StorefrontsFixture.steamAccount,
                                               id: "steam-session"))

        try await eventually { overlay.session?.id == "steam-session" }
        XCTAssertEqual(overlay.session?.game, .steam(appId: 620))
        XCTAssertEqual(overlay.session?.section, .steam)

        fixture.steamSessions.fake.show(nil)
        try await eventually { overlay.session?.id == "epic-session" }
        XCTAssertEqual(overlay.session?.game, .epic("fixture:one"))
        XCTAssertEqual(overlay.session?.section, .epic)

        fixture.epicSessions.fake.show(nil)
        try await eventually { overlay.session?.id == "gog-session" }
        XCTAssertEqual(overlay.session?.game, .gog("1207658924"))
        XCTAssertEqual(overlay.session?.section, .gog)

        fixture.gogSessions.fake.show(nil)
        try await eventually { overlay.session == nil }
    }

    func testTheOverlayShowsTheRunningGamesStorefrontsFriendsChatTitleAndArtwork() async throws {
        var epicGame = StorefrontsFixture.epicGame("one", title: "Epic One")
        epicGame.imageUrl = "https://images.example/epic-cover.png"
        var gogGame = StorefrontsFixture.gogGame("1207658924", title: "GOG One")
        gogGame.imageUrl = "https://images.example/gog-cover.png"
        gogGame.headerImageUrl = "https://images.example/gog-header.png"
        let fixture = StorefrontsFixture(.init(steam: [StorefrontsFixture.portal], epic: [epicGame], gog: [gogGame]))
        defer { fixture.finish() }
        await fixture.signIn()
        let overlay = fixture.app.overlay
        let unknown = L10n.string("ui.overlay.unknownGame")
        XCTAssertNil(overlay.friends)
        XCTAssertNil(overlay.chat)
        XCTAssertEqual(overlay.gameTitle, unknown)
        XCTAssertNil(overlay.gameArtwork)

        fixture.steamSessions.fake.show(.steam(appID: 620, accountID: StorefrontsFixture.steamAccount))
        try await eventually { overlay.session?.game == .steam(appId: 620) }
        XCTAssertEqual(overlay.friends?.account.storeID, "steam")
        XCTAssertTrue(overlay.chat === fixture.steam.chat)
        XCTAssertEqual(overlay.gameTitle, "Portal 2")
        XCTAssertNil(overlay.gameArtwork, "Steam publishes its artwork by app")
        XCTAssertTrue(overlay.present(.achievements))
        XCTAssertEqual(overlay.achievements?.storeID, "steam")
        overlay.hide()
        fixture.steamSessions.fake.show(nil)
        try await eventually { overlay.session == nil }

        fixture.epicSessions.fake.show(.epic(account: StorefrontsFixture.epicAccount, namespace: "fixture", appName: "one"))
        try await eventually { overlay.session?.game == .epic("fixture:one") }
        XCTAssertEqual(overlay.friends?.account.storeID, "epic")
        XCTAssertNil(overlay.chat)
        XCTAssertEqual(overlay.gameTitle, "Epic One")
        XCTAssertEqual(overlay.gameArtwork, "https://images.example/epic-cover.png", "Epic's cover without a header")
        XCTAssertTrue(overlay.present(.achievements))
        XCTAssertEqual(overlay.achievements?.storeID, "epic")
        overlay.hide()
        fixture.epicSessions.fake.show(nil)
        try await eventually { overlay.session == nil }

        fixture.gogSessions.fake.show(.gog())
        try await eventually { overlay.session?.game == .gog("1207658924") }
        XCTAssertNil(overlay.friends, "GOG shows no friends")
        XCTAssertNil(overlay.chat)
        XCTAssertEqual(overlay.gameTitle, "GOG One")
        XCTAssertEqual(overlay.gameArtwork, "https://images.example/gog-header.png", "GOG's header before its cover")
        XCTAssertTrue(overlay.present(.achievements))
        XCTAssertEqual(overlay.achievements?.storeID, "gog")
        overlay.hide()
        fixture.gogSessions.fake.show(nil)
        try await eventually { overlay.session == nil }

        fixture.steamSessions.fake.show(.steam(appID: 999, accountID: StorefrontsFixture.steamAccount))
        try await eventually { overlay.session?.game == .steam(appId: 999) }
        XCTAssertEqual(overlay.gameTitle, unknown, "A game the library does not list")
    }

    func testTheOverlaysFriendsRetryReachesTheSessionOfTheRunningGamesStorefront() async throws {
        let epicSession = EpicFriendsTestSession(accountID: StorefrontsFixture.epicAccount)
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: [], epicSession: epicSession))
        defer { fixture.finish() }
        await fixture.signIn()
        try await eventually { epicSession.starts == 1 }
        let overlay = fixture.app.overlay

        fixture.epicSessions.fake.show(.epic(account: StorefrontsFixture.epicAccount, namespace: "fixture", appName: "one"))
        try await eventually { overlay.session?.game == .epic("fixture:one") }
        overlay.retryFriends()
        try await eventually { epicSession.reconnections == 1 }
        XCTAssertEqual(epicSession.reconnections, 1)
        XCTAssertEqual(fixture.steamSession.reconnects, 0)
        fixture.epicSessions.fake.show(nil)
        try await eventually { overlay.session == nil }

        fixture.steamSessions.fake.show(.steam(appID: 620, accountID: StorefrontsFixture.steamAccount))
        try await eventually { overlay.session?.game == .steam(appId: 620) }
        overlay.retryFriends()
        XCTAssertEqual(fixture.steamSession.reconnects, 1)
        await drainMainQueue(times: 3)
        XCTAssertEqual(epicSession.reconnections, 1)
        fixture.steamSessions.fake.show(nil)
        try await eventually { overlay.session == nil }

        overlay.retryFriends()
        fixture.gogSessions.fake.show(.gog())
        try await eventually { overlay.session?.game == .gog("1207658924") }
        overlay.retryFriends()
        await drainMainQueue(times: 3)
        XCTAssertEqual(fixture.steamSession.reconnects, 1, "No game, and a GOG game, retry no storefront's friends")
        XCTAssertEqual(epicSession.reconnections, 1)
    }
}
