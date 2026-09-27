import Foundation
import Observation
import XCTest
@testable import SailOver

@MainActor
final class StorefrontLibraryFilterTests: XCTestCase {
    func testSteamFiltersCombineWithSearchAndEverySort() async throws {
        let root = try temporaryDirectory()
        let games = [
            steamGame(1, name: "Portal Z", lastPlayed: 300, playtime: 0),
            steamGame(2, name: "Portal A", lastPlayed: 0, playtime: 60),
            steamGame(3, name: "Portal Never"),
            steamGame(4, name: "Other", lastPlayed: 900, playtime: 120)
        ]
        let model = steamModel(root: root, games: games, receipts: steamReceipts([(1, "windows"), (2, "macos")], root: root))
        await model.start()
        defer { model.signOut() }

        XCTAssertEqual(model.availableFilters, [.all, .installed, .played, .macOS])
        XCTAssertEqual(model.filteredGames.map(\.appId), [4, 1, 2, 3])
        model.searchText = "  portal  "
        model.filter = .played
        XCTAssertEqual(model.filteredGames.map(\.appId), [1, 2])
        model.sort = .name
        XCTAssertEqual(model.filteredGames.map(\.appId), [2, 1])
        model.sort = .playtime
        XCTAssertEqual(model.filteredGames.map(\.appId), [2, 1])

        model.filter = .installed
        model.searchText = ""
        XCTAssertEqual(model.filteredGames.map(\.appId), [2, 1])
        model.searchText = "Portal Z"
        XCTAssertEqual(model.filteredGames.map(\.appId), [1])
        model.filter = .all
        model.searchText = "Portal Never"
        XCTAssertEqual(model.filteredGames.map(\.appId), [3])
    }

    func testSteamMacOSFilterObservesLatePlatformBatchesAfterCachedReads() async throws {
        let root = try temporaryDirectory()
        let responses = FilterPlatformResponses()
        let games = (1...51).map { steamGame(UInt32($0), name: "Game \($0)") }
        let model = steamModel(root: root, games: games, platforms: { try await responses.request($0) })
        await model.start()
        defer { model.signOut() }
        try await eventually { await responses.requestCount == 1 }
        model.filter = .macOS
        model.sort = .name

        XCTAssertTrue(model.filteredGames.isEmpty)
        let firstChange = expectation(description: "Cached macOS results observe the first platform batch")
        withObservationTracking {
            XCTAssertTrue(model.filteredGames.isEmpty)
        } onChange: {
            firstChange.fulfill()
        }
        await responses.complete(0, macOSAppIDs: [1])
        try await eventually { await responses.requestCount == 2 }
        await fulfillment(of: [firstChange], timeout: 1)
        XCTAssertTrue(model.libraryPlatforms.isLoading)
        XCTAssertEqual(model.filteredGames.map(\.appId), [1])

        let secondChange = expectation(description: "Cached macOS results observe the next platform batch")
        withObservationTracking {
            XCTAssertEqual(model.filteredGames.map(\.appId), [1])
        } onChange: {
            secondChange.fulfill()
        }
        await responses.complete(1, macOSAppIDs: [51])
        try await eventually { !model.libraryPlatforms.isLoading }
        await fulfillment(of: [secondChange], timeout: 1)
        XCTAssertEqual(model.filteredGames.map(\.appId), [1, 51])
        model.searchText = "Game 51"
        XCTAssertEqual(model.filteredGames.map(\.appId), [51])
    }

    func testSteamSignOutResetsFilterAndCachedLibrary() async throws {
        let model = steamModel(root: try temporaryDirectory(), games: [steamGame(1, name: "Played", playtime: 1)])
        await model.start()
        model.filter = .played
        XCTAssertEqual(model.filteredGames.map(\.appId), [1])

        model.signOut()

        XCTAssertEqual(model.filter, .all)
        XCTAssertTrue(model.filteredGames.isEmpty)
        XCTAssertFalse(model.isConnected)
        try await eventually { !model.isRestoring && !model.isConnecting }
    }

    func testEpicFiltersUseNamespaceIdentityAndInstallationsOnEitherPlatform() async throws {
        let root = try temporaryDirectory()
        let installedWindows = epicGame("shared", namespace: "first", title: "Étoiles Z", macOS: true)
        let otherNamespace = epicGame("shared", namespace: "second", title: "Étoiles A")
        let installedMac = epicGame("native", title: "Étoiles B", macOS: true)
        let availableMac = epicGame("available", title: "Étoiles C", macOS: true)
        let unknown = EpicGame(appName: "unknown", namespace: "first", catalogItemId: "unknown", title: "Unknown",
                               description: "", imageUrl: nil, headerImageUrl: nil, developer: "", windows: false, macos: false)
        let model = epicModel(root: root, games: [installedWindows, unknown, availableMac, otherNamespace, installedMac],
                              receipts: epicReceipts([(installedWindows, "windows"), (installedMac, "macos")], root: root))
        await model.start()
        defer { model.signOut() }

        XCTAssertEqual(model.availableFilters, [.all, .installed, .macOS])
        XCTAssertEqual(model.filteredGames.count, 5)
        model.searchText = "  etoiles  "
        XCTAssertEqual(model.filteredGames.map(\.libraryID), [otherNamespace, installedMac, availableMac, installedWindows].map(\.libraryID))

        model.filter = .installed
        XCTAssertEqual(model.filteredGames.map(\.libraryID), [installedMac, installedWindows].map(\.libraryID))
        model.filter = .macOS
        XCTAssertEqual(model.filteredGames.map(\.libraryID), [installedMac, availableMac, installedWindows].map(\.libraryID))
        model.searchText = "Unknown"
        XCTAssertTrue(model.filteredGames.isEmpty)
        model.filter = .all
        XCTAssertEqual(model.filteredGames.map(\.libraryID), [unknown.libraryID])
    }

    func testEpicSignOutResetsFilterAndCachedLibrary() async throws {
        let model = epicModel(root: try temporaryDirectory(), games: [epicGame("native", title: "Native", macOS: true)])
        await model.start()
        model.filter = .macOS
        XCTAssertEqual(model.filteredGames.map(\.appName), ["native"])

        model.signOut()

        XCTAssertEqual(model.filter, .all)
        XCTAssertTrue(model.filteredGames.isEmpty)
        XCTAssertFalse(model.isConnected)
        try await eventually { !model.isSigningOut }
    }

    private func temporaryDirectory() throws -> URL {
        let root = FileManager.default.temporaryDirectory.appending(path: "SailOverLibraryFilters-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: root) }
        return root
    }

    private func steamGame(_ appID: UInt32, name: String, lastPlayed: UInt64 = 0, playtime: UInt32 = 0) -> SteamGame {
        SteamGame(appId: appID, name: name, iconHash: "", lastPlayed: lastPlayed, playtimeMinutes: playtime)
    }

    private func steamModel(root: URL, games: [SteamGame], receipts: [SteamInstallReceipt] = [],
                            platforms: @escaping @Sendable ([UInt32]) async throws -> [SteamGamePlatforms] = { _ in [] }) -> SteamModel {
        let session = FilterSteamSession(games: games, platforms: platforms)
        return SteamModel(directory: root, keychain: FilterSteamCredentials(), connect: { _ in session }, news: { _ in [] },
                          beginLogin: { _ in throw FilterFixtureFailure.unexpectedOperation },
                          gameSessions: SteamGameSessions(),
                          installations: SteamModel.installations(FakeInstallations(receipts: receipts)))
    }

    private func epicGame(_ appName: String, namespace: String = "first", title: String, macOS: Bool = false) -> EpicGame {
        EpicGame(appName: appName, namespace: namespace, catalogItemId: "catalog-\(appName)", title: title,
                 description: "", imageUrl: nil, headerImageUrl: nil, developer: "", windows: true, macos: macOS)
    }

    private func epicModel(root: URL, games: [EpicGame], receipts: [EpicInstallReceipt] = []) -> EpicModel {
        let credentials = EpicCredentials(accountId: "filters", displayName: "Filter fixture", refreshToken: "fixture-only-never-sent")
        let session = EpicTestSession(credentials: credentials, games: games)
        return EpicModel(directory: root, keychain: EpicMemoryStore(credentials), restore: { _ in session },
                         installations: EpicModel.installations(FakeInstallations(receipts: receipts)))
    }

    private func steamReceipts(_ entries: [(UInt32, String)], root: URL) -> [SteamInstallReceipt] {
        entries.map { appID, platform in
            SteamInstallReceipt(appId: appID, name: "Game \(appID)", directory: root.appending(path: "steam-\(appID)-\(platform)").path,
                                platform: platform, language: "english", totalBytes: 1)
        }
    }

    private func epicReceipts(_ entries: [(EpicGame, String)], root: URL) -> [EpicInstallReceipt] {
        entries.map { game, platform in
            EpicInstallReceipt(namespace: game.namespace, appName: game.appName, name: game.title,
                               directory: root.appending(path: "epic-\(game.namespace)-\(game.appName)-\(platform)").path,
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

private actor FilterSteamCredentials: SteamCredentialStore {
    func load() -> SteamCredentials? {
        SteamCredentials(accountName: "Filter fixture", steamId: 42, refreshToken: "fixture-only-never-sent")
    }
    func save(_ credentials: SteamCredentials) {}
    func remove() {}
}

private final class FilterSteamSession: SteamSessionProtocol, Sendable {
    private let library: [SteamGame]
    private let loadPlatforms: @Sendable ([UInt32]) async throws -> [SteamGamePlatforms]

    init(games: [SteamGame], platforms: @escaping @Sendable ([UInt32]) async throws -> [SteamGamePlatforms]) {
        library = games
        loadPlatforms = platforms
    }

    func games() async throws -> [SteamGame] { library }
    func platforms(appIds: [UInt32]) async throws -> [SteamGamePlatforms] { try await loadPlatforms(appIds) }
    func profile() async throws -> SteamProfile { SteamProfile(displayName: nil, avatarUrl: nil) }
    func details(appId: UInt32, language: String) async throws -> SteamGameDetails { throw FilterFixtureFailure.unexpectedOperation }
}

private actor FilterPlatformResponses {
    private var requests: [[UInt32]] = []
    private var pending: [Int: CheckedContinuation<[SteamGamePlatforms], any Error>] = [:]
    var requestCount: Int { requests.count }

    func request(_ appIDs: [UInt32]) async throws -> [SteamGamePlatforms] {
        let index = requests.count
        requests.append(appIDs)
        return try await withCheckedThrowingContinuation { pending[index] = $0 }
    }

    func complete(_ index: Int, macOSAppIDs: Set<UInt32>) {
        guard let continuation = pending.removeValue(forKey: index) else {
            XCTFail("No pending platform batch \(index)")
            return
        }
        continuation.resume(returning: requests[index].map {
            SteamGamePlatforms(appId: $0, platforms: macOSAppIDs.contains($0) ? ["macos", "windows"] : ["windows"])
        })
    }
}
