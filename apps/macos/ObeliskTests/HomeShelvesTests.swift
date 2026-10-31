import AppKit
import SwiftUI
import XCTest
@testable import Obelisk

/// What Home takes of each account library and how it merges them; each
/// library's own orders and featured game are AccountLibraryTests'.
@MainActor
final class HomeShelvesTests: XCTestCase {
    func testEachLibraryGivesHomeItsLastPlayedOrder() throws {
        let library = try library([
            game("Celeste", installed: true), game("Portal 2", played: 100, minutes: 5),
            game("Hades", played: 300, installed: true), game("Braid", minutes: 60_000),
        ])
        XCTAssertEqual(library.featured?.title, "Hades")
        XCTAssertEqual(library.recent.map(\.title), ["Hades", "Portal 2"], "Played games only")
        XCTAssertEqual(library.installed.map(\.title), ["Hades", "Celeste"])
        XCTAssertEqual(library.covers.map(\.title), ["Hades", "Portal 2", "Braid"])
        XCTAssertNil(library.featured?.playtime)
        XCTAssertEqual(library.recent.last?.playtime, .seconds(300))
        XCTAssertEqual(library.recent.last?.lastPlayed, Date(timeIntervalSince1970: 100))
    }

    func testTheLastGamePlayedAnywhereLeadsAndTheShelvesFollowIt() throws {
        let shelves = HomeShelves([
            try library([game("Portal 2", played: 100, installed: true), game("Never played", minutes: 150, installed: true)]),
            try library([game("Rocket League", played: 300, installed: true)], in: .epic),
            try library([game("Hades", played: 200)], in: .gog),
        ])
        XCTAssertEqual(shelves.hero?.title, "Rocket League")
        XCTAssertEqual(shelves.recent.map(\.title), ["Hades", "Portal 2"])
        XCTAssertEqual(shelves.installed.map(\.title), ["Rocket League", "Portal 2", "Never played"])
    }

    func testTheVerifiedShelfHoldsEveryLibrarysVerifiedGamesTheLastPlayedFirst() throws {
        let shelves = HomeShelves([
            try library([game("Skyrim", played: 100, verified: true), game("Portal 2", played: 400),
                         game("Fallout 4", verified: true)]),
            try library([game("Rocket League", played: 500, verified: true)], in: .epic),
        ])
        XCTAssertEqual(shelves.hero?.title, "Rocket League")
        XCTAssertEqual(shelves.verified.map(\.title), ["Rocket League", "Skyrim", "Fallout 4"],
                       "The hero stays on the shelf, and unplayed games follow")
        XCTAssertTrue(HomeShelves([try library([game("Portal 2", played: 1)])]).verified.isEmpty)
    }

    func testAPlayedGameLeadsBeforeAnotherLibrarysMostPlayed() throws {
        let shelves = HomeShelves([
            try library([game("Celeste", played: 10, minutes: 1)]),
            try library([game("Braid", minutes: 10_000)], in: .epic),
        ])
        XCTAssertEqual(shelves.hero?.title, "Celeste")
        XCTAssertTrue(shelves.recent.isEmpty)
    }

    func testWithNothingPlayedTheMostPlayedLeadsThenTheFirstByName() throws {
        let played = HomeShelves([
            try library([game("Celeste", minutes: 1), game("Antichamber")]),
            try library([game("Braid", minutes: 10)], in: .epic),
        ])
        XCTAssertEqual(played.hero?.title, "Braid")
        XCTAssertTrue(played.recent.isEmpty)
        XCTAssertEqual(HomeShelves([try library([game("Celeste")]), try library([game("Braid")], in: .gog)]).hero?.title,
                       "Braid")
        XCTAssertNil(HomeShelves([try library([]), try library([])]).hero)
    }

    func testTheRecentShelfKeepsTheLastPlayedGamesOnly() throws {
        let shelves = HomeShelves([
            try library((0..<15).map { game("Steam \($0)", played: UInt64(2 * $0 + 2)) }),
            try library((0..<15).map { game("Epic \($0)", played: UInt64(2 * $0 + 3)) }, in: .epic),
        ])
        XCTAssertEqual(shelves.hero?.title, "Epic 14")
        XCTAssertEqual(shelves.recent.count, HomeShelves.shelfLimit)
        XCTAssertEqual(shelves.recent.prefix(3).map(\.title), ["Steam 14", "Epic 13", "Steam 13"])
        XCTAssertFalse(shelves.recent.contains { $0.id == shelves.hero?.id }, "The hero shows once")

        let alone = HomeShelves([try library((0..<25).map { game("Steam \($0)", played: UInt64($0 + 1)) })])
        XCTAssertEqual(alone.hero?.title, "Steam 24")
        XCTAssertEqual(alone.recent.count, HomeShelves.shelfLimit, "The library's hero leaves its shelf full")
    }

    func testEqualGamesGoSteamThenEpicThenGog() throws {
        let shelves = HomeShelves([
            try library([game("Hades", played: 100, installed: true)]),
            try library([game("Hades", played: 100, installed: true)], in: .epic),
            try library([game("Hades", played: 100, installed: true), game("Celeste", installed: true)], in: .gog),
        ])
        XCTAssertEqual(shelves.hero?.id.section, .steam)
        XCTAssertEqual(shelves.recent.map(\.id.section), [.epic, .gog])
        XCTAssertEqual(shelves.installed.map(\.id.section), [.steam, .epic, .gog, .gog])
        XCTAssertEqual(shelves.installed.last?.title, "Celeste")
    }

    func testHomeShowsStorageWarnings() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: "ObeliskHomeNotices-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let suite = "ObeliskTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let epic = await connectedModel(WebTestSession<EpicGames>(
            credentials: EpicGames.credentials("current"),
            games: [EpicGames.game("rocket-league", title: "Rocket League")]
        ))
        let model = AppModel(directory: root, defaults: defaults, epic: epic)
        let host = NSHostingView(rootView: HomeView(model: model).frame(width: 900, height: 700))
        let rect = NSRect(x: -20_000, y: -20_000, width: 900, height: 700)
        let window = NSWindow(contentRect: rect, styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.sharingType = .none
        window.contentView = host
        window.setFrameOrigin(rect.origin)
        window.orderBack(nil)
        defer { window.close() }

        func documentHeight() async throws -> CGFloat {
            for _ in 0..<5 {
                await Task.yield()
                host.layoutSubtreeIfNeeded()
                window.displayIfNeeded()
            }
            let scroll = try XCTUnwrap(firstScrollView(in: host))
            return try XCTUnwrap(scroll.documentView).frame.height
        }

        let baseHeight = try await documentHeight()
        XCTAssertGreaterThan(baseHeight, 0)

        model.steam.storageWarning = "Fixture storage warning"
        let warningHeight = try await documentHeight()
        XCTAssertGreaterThan(warningHeight, baseHeight)

        model.steam.storageWarning = nil
        let clearedWarningHeight = try await documentHeight()
        XCTAssertEqual(clearedWarningHeight, baseHeight, accuracy: 0.5)
    }

    // MARK: Fixtures

    private struct Entry {
        let title: String
        let played: UInt64
        let minutes: UInt32
        let installed: Bool
        let verified: Bool
    }

    private func game(_ title: String, played: UInt64 = 0, minutes: UInt32 = 0, installed: Bool = false,
                      verified: Bool = false) -> Entry {
        Entry(title: title, played: played, minutes: minutes, installed: installed, verified: verified)
    }

    /// The account library of `entries`, as Home shows it under `section`.
    private func library(_ entries: [Entry], in section: LibrarySection = .steam) throws -> HomeLibrary {
        let root = FileManager.default.temporaryDirectory.appending(path: "ObeliskHomeShelves-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: root) }
        let games = entries.enumerated().map { index, entry in
            SteamGame(appId: UInt32(index + 1), name: entry.title, iconHash: "", lastPlayed: entry.played,
                      playtimeMinutes: entry.minutes)
        }
        let receipts = zip(games, entries).filter { $1.installed }.map { game, _ in
            InstallationReceipt.steam(appId: game.appId, name: game.name, directory: root.appending(path: "\(game.appId)").path,
                                      platform: .mac)
        }
        let history = LaunchHistory(cache: LibraryCache(directory: root.path))
        let sessions = GameSessions<SteamGames>(FakeSessions(), history: history)
        let installations = GameInstallations(sessions: sessions, cloud: CloudModel(sessions: sessions))
        installations.connect(account: 42, backend: FakeInstallations(receipts: receipts),
                              port: InstallationPort(report: { _ in }, warn: { _ in }))
        let library = AccountLibrary<SteamGames, SteamGame>(history: history, cache: nil,
                                                            installations: installations)
        library.connect(42)
        library.show(games)
        let verified = Set(zip(games, entries).filter { $1.verified }.map { game, _ in game.appId })
        return HomeLibrary(library, verified: verified) { game in
            HomeGame(game, of: library, in: section,
                     action: GameAction(kind: .download(.install), title: L10n.string("model.game.download"),
                                        isAvailable: true, launches: false),
                     cover: { AnyView(EmptyView()) },
                     hero: HomeGame.Hero(artwork: { AnyView(EmptyView()) }, logo: { AnyView(EmptyView()) }),
                     open: {})
        }
    }
}
