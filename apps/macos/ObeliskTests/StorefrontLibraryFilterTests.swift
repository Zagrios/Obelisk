import Foundation
import Observation
import XCTest
@testable import Obelisk

@MainActor
final class StorefrontLibraryFilterTests: XCTestCase {
    func testSteamLoadsDatesAndAchievementsWithTheLibrary() async throws {
        let root = try temporaryDirectory()
        let games = [steamGame(1, name: "Alpha"), steamGame(2, name: "Beta"), steamGame(3, name: "Gamma")]
        let records = FilterRecordRequests()
        let model = steamModel(root: root, games: games, records: records)
        await model.start()
        defer { model.signOut() }
        try await eventually {
            model.library.value(of: games[2], for: .dateAdded) != nil
                && model.library.value(of: games[2], for: .achievements) != nil
        }
        var asked = await records.counts
        XCTAssertEqual(asked.acquisitions, 1, "The library dates load with the library, before any order asks")
        XCTAssertEqual(asked.achievements, 1)
        XCTAssertEqual(asked.achievementApps, [1, 2, 3], "One request names every game")

        model.library.sort = .dateAdded
        XCTAssertEqual(model.library.shown.map(\.appId), [3, 1, 2])
        model.library.sort = .achievements
        XCTAssertEqual(model.library.shown.map(\.appId), [3, 2, 1])
        XCTAssertNil(model.library.pendingMetadata)
        asked = await records.counts
        XCTAssertEqual(asked.acquisitions + asked.achievements, 2, "Picking an order asks nothing")
    }

    func testSteamSignOutResetsFilterAndCachedLibrary() async throws {
        let model = steamModel(root: try temporaryDirectory(), games: [steamGame(1, name: "Played", playtime: 1)])
        await model.start()
        model.library.filter = .played
        XCTAssertEqual(model.library.shown.map(\.appId), [1])

        model.signOut()

        XCTAssertEqual(model.library.filter, .all)
        XCTAssertTrue(model.library.shown.isEmpty)
        XCTAssertFalse(model.isConnected)
        try await eventually { !model.isRestoring && !model.isConnecting }
    }

    func testEpicSignOutResetsFilterAndCachedLibrary() async throws {
        let root = try temporaryDirectory()
        let native = epicGame("native", title: "Native", macOS: true)
        let model = epicModel(root: root, games: [native], receipts: epicReceipts([(native, .mac)], root: root))
        await model.start()
        model.library.filter = .installed
        XCTAssertEqual(model.library.shown.map(\.appName), ["native"])

        model.signOut()

        XCTAssertEqual(model.library.filter, .all)
        XCTAssertTrue(model.library.shown.isEmpty)
        XCTAssertFalse(model.isConnected)
        try await eventually { !model.isSigningOut }
    }

    private func temporaryDirectory() throws -> URL {
        let root = FileManager.default.temporaryDirectory.appending(path: "ObeliskLibraryFilters-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: root) }
        return root
    }

    private func steamGame(_ appID: UInt32, name: String, playtime: UInt32 = 0) -> SteamGame {
        SteamGame(appId: appID, name: name, iconHash: "", lastPlayed: 0, playtimeMinutes: playtime)
    }

    private func steamModel(root: URL, games: [SteamGame],
                            records: FilterRecordRequests = FilterRecordRequests()) -> SteamModel {
        let session = FilterSteamSession(games: games, platforms: { _ in [] }, records: records)
        return SteamModel(directory: root, keychain: FilterSteamCredentials(), connect: { _ in session }, news: { _ in [] },
                          beginLogin: { _ in throw FilterFixtureFailure.unexpectedOperation },
                          gameSessions: SteamGameSessions(),
                          installations: SteamModel.installations(FakeInstallations()))
    }

    private func epicGame(_ appName: String, namespace: String = "first", title: String, macOS: Bool = false) -> EpicGame {
        EpicGame(appName: appName, namespace: namespace, catalogItemId: "catalog-\(appName)", title: title,
                 description: "", imageUrl: nil, headerImageUrl: nil, developer: "", windows: true, macos: macOS)
    }

    private func epicModel(root: URL, games: [EpicGame], receipts: [InstallationReceipt<String>]) -> EpicModel {
        let credentials = WebAccountCredentials(accountId: "filters", name: "Filter fixture", refreshToken: "fixture-only-never-sent")
        let session = EpicTestSession(credentials: credentials, games: games)
        return EpicModel(directory: root, keychain: EpicMemoryStore(credentials), restore: { _ in session },
                         installations: EpicModel.installations(FakeInstallations(receipts: receipts)))
    }

    private func epicReceipts(_ entries: [(EpicGame, GamePlatform)], root: URL) -> [InstallationReceipt<String>] {
        entries.map { game, platform in
            InstallationReceipt.epic(namespace: game.namespace, appName: game.appName, name: game.title,
                               directory: root.appending(
                                   path: "epic-\(game.namespace)-\(game.appName)-\(platform == .mac ? "Mac" : "Windows")").path,
                               platform: platform, totalBytes: 1)
        }
    }

    private func eventually(file: StaticString = #filePath, line: UInt = #line,
                            _ condition: @MainActor () async -> Bool) async throws {
        for _ in 0..<200 {
            if await condition() { return }
            try await Task.sleep(for: .milliseconds(5))
        }
        XCTFail("Library filter fixture did not finish", file: file, line: line)
        throw FilterFixtureFailure.timeout
    }
}

private enum FilterFixtureFailure: Error { case unexpectedOperation, timeout }

private actor FilterSteamCredentials: CredentialStore {
    func load() -> SteamCredentials? {
        SteamCredentials(accountName: "Filter fixture", steamId: 42, refreshToken: "fixture-only-never-sent")
    }
    func save(_ credentials: SteamCredentials) {}
    func remove() {}
}

private final class FilterSteamSession: SteamSessionProtocol, Sendable {
    private let library: [SteamGame]
    private let loadPlatforms: @Sendable ([UInt32]) async throws -> [SteamGamePlatforms]
    private let records: FilterRecordRequests

    init(games: [SteamGame], platforms: @escaping @Sendable ([UInt32]) async throws -> [SteamGamePlatforms],
         records: FilterRecordRequests) {
        library = games
        loadPlatforms = platforms
        self.records = records
    }

    func games() async throws -> [SteamGame] { library }
    func platforms(appIds: [UInt32], language: String) async throws -> [SteamGamePlatforms] { try await loadPlatforms(appIds) }
    func acquisitions() async throws -> [UInt32: UInt64] { await records.acquisitions() }
    func achievementShares(appIds: [UInt32], language: String) async throws -> [UInt32: Double] {
        await records.achievementShares(appIds)
    }
    func profile() async throws -> SteamProfile { SteamProfile(displayName: nil, avatarUrl: nil) }
    func details(appId: UInt32, language: String) async throws -> SteamGameDetails { throw FilterFixtureFailure.unexpectedOperation }
}

/// Steam's answers about the account's games: Gamma was got last, Alpha
/// before it and Beta through no license; Gamma's achievements are all
/// unlocked, half of Beta's, and Alpha has none.
private actor FilterRecordRequests {
    private(set) var counts = (acquisitions: 0, achievements: 0, achievementApps: [UInt32]())

    func acquisitions() -> [UInt32: UInt64] {
        counts.acquisitions += 1
        return [1: 1_500_000_000, 3: 1_600_000_000]
    }

    func achievementShares(_ appIDs: [UInt32]) -> [UInt32: Double] {
        counts.achievements += 1
        counts.achievementApps = appIDs
        return [2: 0.5, 3: 1]
    }
}

