import Foundation
import Observation
import XCTest
@testable import Obelisk

/// One account's library, fed its facts directly: no session, no Keychain.
@MainActor
final class AccountLibraryTests: XCTestCase {
    // MARK: Orders

    func testEverySortRanksTheGreatestFirstAndBreaksTiesByNameThenID() throws {
        let root = try temporaryDirectory()
        let games = [
            steamGame(1, "Étoiles", lastPlayed: 100, minutes: 800),
            steamGame(2, "Portal", lastPlayed: 200, minutes: 100),
            steamGame(4, "zéro"),
            steamGame(3, "Alpha"),
            steamGame(5, "Portal", lastPlayed: 200, minutes: 100),
        ]
        let fake = FakeInstallations<SteamGames>(receipts: [
            .steam(appId: 4, name: "zéro", directory: root.appending(path: "4-windows").path, platform: .windows, totalBytes: 100),
            .steam(appId: 4, name: "zéro", directory: root.appending(path: "4-macos").path, platform: .mac, totalBytes: 50),
            .steam(appId: 2, name: "Portal", directory: root.appending(path: "2-macos").path, platform: .mac, totalBytes: 120),
        ])
        let library = steamLibrary(root: root, installations: fake)
        let values: [UInt32: [StorefrontLibrarySort: Double]] = [
            1: [.dateAdded: 10], 3: [.dateAdded: 30], 4: [.dateAdded: 30], 5: [.achievements: 1], 2: [.achievements: 0.25],
        ]
        XCTAssertTrue(library.showCache(of: 42) { _ in
            games.map { CachedLibraryGame(game: $0, values: values[$0.appId] ?? [:]) }
        })

        let expected: [(StorefrontLibrarySort, [UInt32])] = [
            (.lastPlayed, [2, 5, 1, 3, 4]),
            (.name, [3, 1, 2, 5, 4]),
            (.playtime, [1, 2, 5, 3, 4]),
            (.dateAdded, [3, 4, 1, 2, 5]),
            (.achievements, [5, 2, 3, 1, 4]),
            (.size, [4, 2, 3, 1, 5]),
        ]
        for (sort, order) in expected {
            library.sort = sort
            XCTAssertEqual(library.shown.map(\.appId), order, "\(sort)")
            XCTAssertEqual(library.games(by: sort).map(\.appId), order, "\(sort), read again")
        }

        var older = EpicGames.game("older", title: "Older"), newer = EpicGames.game("newer", title: "Newer")
        older.addedAt = 1_590_000_000
        newer.addedAt = 1_700_000_000
        let epic = epicLibrary(root: root)
        epic.show([older, EpicGames.game("undated", title: "Undated"), newer])
        XCTAssertEqual(epic.games(by: .dateAdded).map(\.appName), ["newer", "older", "undated"],
                       "Epic's entry tells the date it joined the library")
        XCTAssertEqual(epic.value(of: older, for: .dateAdded), 1_590_000_000)
    }

    func testSearchAndFiltersNarrowEveryOrder() throws {
        let root = try temporaryDirectory()
        let steam = steamLibrary(root: root, installations: FakeInstallations(receipts: [
            .steam(appId: 1, name: "Portal Z", directory: root.appending(path: "1").path, platform: .windows),
            .steam(appId: 2, name: "Portal A", directory: root.appending(path: "2").path, platform: .mac),
        ]))
        steam.connect(42)
        steam.show([
            steamGame(1, "Portal Z", lastPlayed: 300),
            steamGame(2, "Portal A", minutes: 60),
            steamGame(3, "Portal Never"),
            steamGame(4, "Other", lastPlayed: 900, minutes: 120),
        ])
        XCTAssertEqual(steam.shown.map(\.appId), [4, 1, 2, 3])
        steam.searchText = "  portal  "
        steam.filter = .played
        XCTAssertEqual(steam.shown.map(\.appId), [1, 2], "Played: a playtime or a last played date")
        steam.sort = .name
        XCTAssertEqual(steam.shown.map(\.appId), [2, 1])
        steam.sort = .playtime
        XCTAssertEqual(steam.shown.map(\.appId), [2, 1])
        steam.filter = .installed
        steam.searchText = ""
        XCTAssertEqual(steam.shown.map(\.appId), [2, 1])
        steam.searchText = "Portal Z"
        XCTAssertEqual(steam.shown.map(\.appId), [1])
        steam.filter = .all
        steam.searchText = "Portal Never"
        XCTAssertEqual(steam.shown.map(\.appId), [3])

        let installedWindows = epicGame("shared", namespace: "first", title: "Étoiles Z", macOS: true)
        let otherNamespace = epicGame("shared", namespace: "second", title: "Étoiles A")
        let installedMac = epicGame("native", title: "Étoiles B", macOS: true)
        let availableMac = epicGame("available", title: "Étoiles C", macOS: true)
        let unknown = epicGame("unknown", title: "Unknown")
        let epic = epicLibrary(root: root, installations: FakeInstallations(receipts: [
            epicReceipt(installedWindows, platform: .windows, root: root), epicReceipt(installedMac, platform: .mac, root: root),
        ]))
        epic.show([installedWindows, unknown, availableMac, otherNamespace, installedMac])
        XCTAssertEqual(epic.shown.count, 5)
        epic.searchText = "  etoiles  "
        XCTAssertEqual(epic.shown.map(\.libraryID), [otherNamespace, installedMac, availableMac, installedWindows].map(\.libraryID))
        epic.filter = .installed
        XCTAssertEqual(epic.shown.map(\.libraryID), [installedMac, installedWindows].map(\.libraryID),
                       "An installation on either platform, of that namespace only")
        epic.searchText = "Unknown"
        XCTAssertTrue(epic.shown.isEmpty)
        epic.filter = .all
        XCTAssertEqual(epic.shown.map(\.libraryID), [unknown.libraryID])
    }

    func testSizeOrderFollowsTheInstallations() async throws {
        let root = try temporaryDirectory()
        let small = epicGame("small", title: "Small"), large = epicGame("large", title: "Large")
        let absent = epicGame("absent", title: "Absent")
        let fake = FakeInstallations<EpicGames>(receipts: [
            epicReceipt(small, platform: .windows, root: root, bytes: 10),
            epicReceipt(large, platform: .windows, root: root, bytes: 400),
            epicReceipt(large, platform: .mac, root: root, bytes: 400),
        ])
        let library = epicLibrary(root: root, installations: fake)
        library.show([small, absent, large])
        library.sort = .size
        XCTAssertEqual(library.shown.map(\.appName), ["large", "small", "absent"],
                       "Both of a game's installations count, and a game not installed comes last")
        fake.publish { $0.receipts.append(self.epicReceipt(absent, platform: .windows, root: root, bytes: 5_000)) }
        try await eventually { library.shown.map(\.appName) == ["absent", "large", "small"] }
    }

    func testTheInstalledCountIsWhatTheInstalledFilterKeeps() async throws {
        let root = try temporaryDirectory()
        let both = epicGame("both", title: "Both", macOS: true), one = epicGame("one", title: "One")
        let absent = epicGame("absent", title: "Absent"), delisted = epicGame("delisted", title: "Delisted")
        let fake = FakeInstallations<EpicGames>(receipts: [
            epicReceipt(both, platform: .windows, root: root), epicReceipt(both, platform: .mac, root: root),
            epicReceipt(delisted, platform: .windows, root: root),
        ])
        let library = epicLibrary(root: root, installations: fake)
        XCTAssertEqual(library.installedCount, 0, "No game is listed yet")

        library.show([both, one, absent])
        XCTAssertEqual(library.installedCount, 1, "A game installed for two platforms counts once, an unlisted one never")
        XCTAssertEqual(library.installedCount, library.games(by: .lastPlayed, filter: .installed).count)

        fake.publish { $0.receipts.append(self.epicReceipt(one, platform: .windows, root: root)) }
        try await eventually { library.installedCount == 2 }
        XCTAssertEqual(library.installedCount, library.games(by: .lastPlayed, filter: .installed).count)
    }

    // MARK: Facts

    func testLastPlayedIsTheLaterOfTheStorefrontAndThisAccountsLaunch() throws {
        let root = try temporaryDirectory()
        let history = LaunchHistory(cache: LibraryCache(directory: root.path))
        let portal = steamGame(620, "Portal 2", lastPlayed: 1_789_430_000)
        let dota = steamGame(570, "Dota 2"), fortress = steamGame(440, "Team Fortress 2")
        let library = steamLibrary(root: root, history: history)
        library.connect(42)
        library.show([portal, dota, fortress])
        XCTAssertEqual(library.shown.map(\.appId), [620, 570, 440])

        let launched = Date(timeIntervalSince1970: 1_789_500_000)
        history.record(570, account: 42, in: SteamGames.self, at: launched)
        XCTAssertEqual(library.lastPlayed(of: dota), launched, "A launch from Obelisk")
        XCTAssertEqual(library.shown.map(\.appId), [570, 620, 440], "The library orders again")
        history.record(620, account: 42, in: SteamGames.self, at: Date(timeIntervalSince1970: 1_789_000_000))
        XCTAssertEqual(library.lastPlayed(of: portal), Date(timeIntervalSince1970: 1_789_430_000),
                       "Steam's later date wins over an older launch")
        history.record(440, account: 43, in: SteamGames.self, at: launched + 60)
        XCTAssertNil(library.lastPlayed(of: fortress), "Only this account's launches count")
        library.filter = .played
        XCTAssertEqual(library.shown.map(\.appId), [570, 620],
                       "Launched from Obelisk, never played on Steam; never launched by this account, never played")
        library.filter = .all

        let cached = steamLibrary(root: root, history: history)
        cached.showCache(of: 42) { _ in [CachedLibraryGame(game: dota)] }
        XCTAssertEqual(cached.lastPlayed(of: dota), launched, "The cached account's launches already count")
    }

    func testTheHeroIsTheLastPlayedElseTheMostPlayedElseTheFirstByName() throws {
        let root = try temporaryDirectory()
        let history = LaunchHistory(cache: LibraryCache(directory: root.path))
        func library(_ entries: [(String, String, Int64?)]) -> AccountLibrary<EpicGames, EpicGame> {
            let library = epicLibrary(root: root, history: history)
            library.showCache(of: EpicGames.fixtureAccount) { _ in
                entries.map { key, title, minutes in
                    CachedLibraryGame(game: EpicGames.game(key, title: title), playtime: minutes.map { .seconds($0 * 60) })
                }
            }
            return library
        }

        XCTAssertNil(epicLibrary(root: root).featured, "An empty library has none")
        XCTAssertEqual(library([("b", "Beta", 0), ("a", "Alpha", 0)]).featured?.title, "Alpha",
                       "Every game at zero hours: the first by name")
        XCTAssertEqual(library([("1", "Zeta", 30), ("2", "beta", 30), ("3", "Alpha", nil)]).featured?.title, "beta",
                       "Equal playtimes: the first by name, whatever its case")

        let played = library([("a", "Alpha", 0), ("b", "Beta", 10), ("c", "Gamma", 600), ("d", "Delta", nil)])
        XCTAssertEqual(played.featured?.title, "Gamma", "The most played game leads before any launch")
        history.record(epicLibraryID(namespace: "fixture", appName: "a"), account: EpicGames.fixtureAccount, in: EpicGames.self)
        XCTAssertEqual(played.featured?.title, "Alpha", "The last game launched leads")
        history.record(epicLibraryID(namespace: "fixture", appName: "b"), account: "another-account", in: EpicGames.self,
                       at: .now + 60)
        XCTAssertEqual(played.featured?.title, "Alpha", "Only this account's launches count")

        XCTAssertEqual(played.hero?.title, "Alpha")
        played.searchText = "  b "
        XCTAssertNil(played.hero, "The hero belongs to the whole library, never to a search")
        XCTAssertEqual(played.featured?.title, "Alpha")
        played.searchText = " "
        played.filter = .played
        XCTAssertNil(played.hero, "Nor to a filter")
        played.filter = .all
        XCTAssertEqual(played.hero?.title, "Alpha")
    }

    // MARK: Lifecycle

    func testTheCacheShowsUntilItsAccountConnectsAndAnotherAccountStartsEmpty() throws {
        let root = try temporaryDirectory()
        let older = steamGame(1, "Older"), newer = steamGame(2, "Newer")
        let cached = [CachedLibraryGame(game: older, values: [.dateAdded: 10]),
                      CachedLibraryGame(game: newer, values: [.dateAdded: 20])]

        let live = steamLibrary(root: root)
        live.show([older])
        var read = false
        XCTAssertFalse(live.showCache(of: 42) { _ in read = true; return cached })
        XCTAssertFalse(read, "The cache is not read while games show")
        XCTAssertFalse(steamLibrary(root: root).showCache(of: 42) { _ in [] }, "An empty cache shows nothing")
        XCTAssertFalse(steamLibrary(root: root, cache: .some(nil)).showCache(of: 42) { _ in cached })

        let same = steamLibrary(root: root)
        XCTAssertTrue(same.showCache(of: 42) { _ in cached })
        XCTAssertTrue(same.isCached)
        XCTAssertEqual(same.games(by: .dateAdded).map(\.appId), [2, 1], "What the cache knew orders the library")
        XCTAssertTrue(same.connect(42))
        XCTAssertFalse(same.isCached)
        XCTAssertEqual(same.games, [older, newer], "The same account keeps its cached games")
        XCTAssertEqual(same.value(of: newer, for: .dateAdded), 20, "And what the cache knew of them")
        XCTAssertFalse(same.dropCache(), "A connected library is no cache")
        XCTAssertEqual(same.games.count, 2)

        let other = steamLibrary(root: root)
        other.showCache(of: 42) { _ in cached }
        XCTAssertFalse(other.connect(43))
        XCTAssertTrue(other.games.isEmpty, "Another account's cached games never show as this one's")
        XCTAssertNil(other.value(of: newer, for: .dateAdded))

        let dropped = steamLibrary(root: root)
        dropped.showCache(of: 42) { _ in cached }
        XCTAssertTrue(dropped.dropCache())
        XCTAssertFalse(dropped.isCached)
        XCTAssertTrue(dropped.games.isEmpty)
        XCTAssertFalse(dropped.dropCache())
    }

    func testForgetClearsGamesFactsSearchAndFilterButKeepsTheSort() throws {
        let root = try temporaryDirectory()
        let portal = steamGame(620, "Portal 2", minutes: 5)
        let cache = try LibraryCache.holding(steam: [42: [CachedSteamGame(
            game: portal, platforms: ["macos"], addedAt: 1_300_000_000, achievementShare: 0.5
        )]], in: root)
        let history = LaunchHistory(cache: cache)
        let steam = steamLibrary(root: root, history: history, cache: cache)
        steam.showCache(of: 42) { cache in
            cache.steamLibrary(steamId: 42, language: "english").map { entry in
                CachedLibraryGame(game: entry.game, values: [.dateAdded: entry.addedAt.map { Double($0) }].compactMapValues { $0 })
            }
        }
        steam.connect(42)
        history.record(620, account: 42, in: SteamGames.self)
        steam.searchText = "portal"
        steam.filter = .played
        steam.sort = .name
        XCTAssertEqual(steam.shown, [portal])

        steam.forget()

        XCTAssertTrue(steam.games.isEmpty)
        XCTAssertTrue(steam.shown.isEmpty)
        XCTAssertEqual(steam.searchText, "")
        XCTAssertEqual(steam.filter, .all)
        XCTAssertEqual(steam.sort, .name, "The order stays the player's")
        XCTAssertTrue(cache.steamLibrary(steamId: 42, language: "english").isEmpty, "The account's Library cache entry goes")
        XCTAssertNil(history.lastLaunch(of: 620, account: 42, in: SteamGames.self), "And its launches")
        steam.show([portal])
        XCTAssertNil(steam.value(of: portal, for: .dateAdded), "And what the cache knew")

        let epic = epicLibrary(root: root)
        let alpha = EpicGames.game("a", title: "Alpha")
        epic.showCache(of: EpicGames.fixtureAccount) { _ in [CachedLibraryGame(game: alpha, playtime: .seconds(600))] }
        epic.forget()
        epic.show([alpha])
        XCTAssertNil(epic.playtime(of: alpha), "And the playtimes")
    }

    // MARK: Kept orders

    func testShownGamesWithTheSameIDsReorderEveryKeptOrder() throws {
        let library = steamLibrary(root: try temporaryDirectory())
        library.connect(42)
        library.show([
            steamGame(1, "Étoiles", lastPlayed: 100, minutes: 800),
            steamGame(2, "Portal", lastPlayed: 200, minutes: 100),
            steamGame(3, "Alpha"),
            steamGame(4, "Portal", lastPlayed: 200, minutes: 100),
            steamGame(5, "Zéro"),
        ])
        let orders: [(StorefrontLibrarySort, [UInt32])] = [
            (.lastPlayed, [2, 4, 1, 3, 5]), (.name, [3, 1, 2, 4, 5]), (.playtime, [1, 2, 4, 3, 5]),
        ]
        for _ in 0..<2 {
            for (sort, expected) in orders {
                library.sort = sort
                XCTAssertEqual(library.shown.map(\.appId), expected)
                XCTAssertEqual(library.shown.map(\.appId), expected)
            }
        }
        library.searchText = "  etoile  "
        for sort in StorefrontLibrarySort.allCases {
            library.sort = sort
            XCTAssertEqual(library.shown.map(\.appId), [1])
        }
        library.searchText = "portal"
        XCTAssertEqual(library.shown.map(\.appId), [2, 4])
        library.searchText = "absent"
        XCTAssertTrue(library.shown.isEmpty)
        library.searchText = ""

        library.show([
            steamGame(1, "Zèbre", lastPlayed: 300, minutes: 1),
            steamGame(2, "Alpha", minutes: 50),
            steamGame(3, "Portal", lastPlayed: 400, minutes: 900),
            steamGame(4, "Beta"),
            steamGame(5, "Étoiles"),
        ])
        let refreshed: [(StorefrontLibrarySort, [UInt32])] = [
            (.lastPlayed, [3, 1, 2, 4, 5]), (.name, [2, 4, 5, 3, 1]), (.playtime, [3, 2, 1, 4, 5]),
        ]
        for (sort, expected) in refreshed {
            library.sort = sort
            XCTAssertEqual(library.shown.map(\.appId), expected)
        }
        library.searchText = "etoile"
        XCTAssertEqual(library.shown.map(\.appId), [5])
    }

    func testShownObservesSortSearchGamesLaunchesPlaytimesAndReceipts() async throws {
        let root = try temporaryDirectory()
        let history = LaunchHistory(cache: LibraryCache(directory: root.path))
        let fake = FakeInstallations<EpicGames>()
        let library = epicLibrary(root: root, history: history, installations: fake)
        library.connect(EpicGames.fixtureAccount)
        let alpha = EpicGames.game("a", title: "Alpha"), beta = EpicGames.game("b", title: "Beta")
        library.show([alpha, beta])
        _ = library.shown

        var changed = changes(of: { _ = library.shown }, "The sort")
        library.sort = .playtime
        await fulfillment(of: [changed], timeout: 1)

        changed = changes(of: { _ = library.shown }, "The search")
        library.searchText = "a"
        await fulfillment(of: [changed], timeout: 1)
        library.searchText = ""

        changed = changes(of: { _ = library.shown }, "The games")
        library.show([beta, alpha])
        await fulfillment(of: [changed], timeout: 1)

        changed = changes(of: { _ = library.featured }, "A launch")
        history.record(beta.id, account: EpicGames.fixtureAccount, in: EpicGames.self)
        await fulfillment(of: [changed], timeout: 1)
        XCTAssertEqual(library.featured, beta)

        changed = changes(of: { _ = library.shown }, "The playtimes")
        library.loadPlaytimes({ [alpha.id: .seconds(600)] }, then: {})
        await fulfillment(of: [changed], timeout: 1)
        try await eventually { library.shown == [alpha, beta] }

        library.filter = .installed
        XCTAssertTrue(library.shown.isEmpty)
        changed = changes(of: { _ = library.shown }, "The receipts")
        fake.publish { $0.receipts = [self.epicReceipt(alpha, platform: .windows, root: root)] }
        await fulfillment(of: [changed], timeout: 1)
        XCTAssertEqual(library.shown, [alpha])
    }

    // MARK: Loads

    func testPlaytimeLoadsAnnounceThemselvesOnlyWhileAnOrderOrTheFilterReadsThem() async throws {
        let library = epicLibrary(root: try temporaryDirectory())
        let alpha = EpicGames.game("a", title: "Alpha"), beta = EpicGames.game("b", title: "Beta")
        library.connect(EpicGames.fixtureAccount)
        library.show([alpha, beta])
        let ended = Ended()

        let failing = GameSessionGate<[String: Duration]>()
        library.loadPlaytimes({ try await failing.wait() }, then: { ended.loads.append("failing") })
        XCTAssertNil(library.pendingMetadata, "The name order needs no playtime")
        library.sort = .playtime
        XCTAssertNotNil(library.pendingMetadata, "The playtimes show as loading at once")
        library.sort = .name
        library.filter = .played
        XCTAssertNotNil(library.pendingMetadata)
        await failing.fail(EpicError.Network)
        try await eventually { library.missingMetadata != nil }
        XCTAssertNil(library.pendingMetadata)
        XCTAssertEqual(ended.loads, ["failing"], "A failed load still ends")

        let replaced = GameSessionGate<[String: Duration]>(), current = GameSessionGate<[String: Duration]>()
        library.loadPlaytimes({ try await replaced.wait() }, then: { ended.loads.append("replaced") })
        XCTAssertNil(library.missingMetadata, "A new load clears the failure at once")
        XCTAssertNotNil(library.pendingMetadata)
        library.loadPlaytimes({ try await current.wait() }, then: { ended.loads.append("current") })
        await replaced.resolve([alpha.id: .seconds(60)])
        await current.resolve([beta.id: .seconds(120)])
        try await eventually { library.pendingMetadata == nil }
        await drainMainQueue(times: 3)
        XCTAssertEqual(ended.loads, ["failing", "current"], "A replaced load never ends")
        XCTAssertNil(library.missingMetadata)
        XCTAssertNil(library.playtime(of: alpha))
        XCTAssertEqual(library.playtime(of: beta), .seconds(120))
        XCTAssertEqual(library.shown, [beta])
    }

    func testRecordLoadsAreNotRepeatedWhileLoadingAndAFailureKeepsWhatShowed() async throws {
        let library = steamLibrary(root: try temporaryDirectory())
        let portal = steamGame(620, "Portal 2")
        library.connect(42)
        library.show([portal])
        let reads = Reads()

        let dates = GameSessionGate<[UInt32: Double]>()
        library.load(.dateAdded) { await reads.add(); return try await dates.wait() }
        library.load(.dateAdded) { await reads.add(); return [620: 1] }
        await dates.resolve([620: 1_300_000_000])
        try await eventually { library.value(of: portal, for: .dateAdded) == 1_300_000_000 }
        let count = await reads.count
        XCTAssertEqual(count, 1, "A load already running is not asked again")

        library.load(.dateAdded) { await reads.add(); throw EpicError.Network }
        try await eventually { await reads.count == 2 }
        await drainMainQueue(times: 3)
        XCTAssertEqual(library.value(of: portal, for: .dateAdded), 1_300_000_000, "A failure keeps what showed")

        let late = GameSessionGate<[UInt32: Double]>()
        library.load(.achievements) { try await late.wait() }
        try await eventually { await late.requests == 1 }
        library.forget()
        await late.resolve([620: 1])
        await drainMainQueue(times: 3)
        library.show([portal])
        XCTAssertNil(library.value(of: portal, for: .achievements), "A forgotten account's late answer never shows")
    }

    // MARK: Fixtures

    private func temporaryDirectory() throws -> URL {
        let root = FileManager.default.temporaryDirectory.appending(path: "ObeliskAccountLibrary-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: root) }
        return root
    }

    private func steamLibrary(root: URL, history: LaunchHistory? = nil, cache: LibraryCache?? = .none,
                              installations fake: FakeInstallations<SteamGames> = FakeInstallations())
        -> AccountLibrary<SteamGames, SteamGame> {
        let history = history ?? LaunchHistory(cache: LibraryCache(directory: root.path))
        return AccountLibrary(history: history, cache: cache ?? LibraryCache(directory: root.path),
                              installations: installations(of: 42, fake, history: history))
    }

    private func epicLibrary(root: URL, history: LaunchHistory? = nil,
                             installations fake: FakeInstallations<EpicGames> = FakeInstallations())
        -> AccountLibrary<EpicGames, EpicGame> {
        let history = history ?? LaunchHistory(cache: LibraryCache(directory: root.path))
        return AccountLibrary(history: history, cache: LibraryCache(directory: root.path),
                              installations: installations(of: EpicGames.fixtureAccount, fake, history: history))
    }

    /// The account's installations as `fake` publishes them; connecting
    /// shows its first snapshot at once.
    private func installations<Store: InstallationStorefront>(
        of account: Store.AccountID, _ fake: FakeInstallations<Store>, history: LaunchHistory
    ) -> GameInstallations<Store> {
        let sessions = GameSessions<Store>(FakeSessions(), history: history)
        let installations = GameInstallations(sessions: sessions, cloud: CloudModel(sessions: sessions))
        installations.connect(account: account, backend: fake, port: InstallationPort(report: { _ in }, warn: { _ in }))
        return installations
    }

    private func changes(of read: () -> Void, _ description: String) -> XCTestExpectation {
        let changed = expectation(description: "The library observes \(description.lowercased())")
        withObservationTracking { read() } onChange: { changed.fulfill() }
        return changed
    }

    private func steamGame(_ appID: UInt32, _ name: String, lastPlayed: UInt64 = 0, minutes: UInt32 = 0) -> SteamGame {
        SteamGame(appId: appID, name: name, iconHash: "", lastPlayed: lastPlayed, playtimeMinutes: minutes)
    }

    private func epicGame(_ appName: String, namespace: String = "first", title: String, macOS: Bool = false) -> EpicGame {
        EpicGame(appName: appName, namespace: namespace, catalogItemId: "catalog-\(appName)", title: title,
                 description: "", imageUrl: nil, headerImageUrl: nil, developer: "", windows: true, macos: macOS)
    }

    private func epicReceipt(_ game: EpicGame, platform: GamePlatform, root: URL, bytes: UInt64 = 1)
        -> InstallationReceipt<String> {
        .epic(namespace: game.namespace, appName: game.appName, name: game.title,
              directory: root.appending(path: "epic-\(game.namespace)-\(game.appName)-\(platform == .mac ? "Mac" : "Windows")").path,
              platform: platform, totalBytes: bytes)
    }
}

/// The playtime loads that ended, in order.
@MainActor
private final class Ended {
    var loads: [String] = []
}

private actor Reads {
    private(set) var count = 0
    func add() { count += 1 }
}
