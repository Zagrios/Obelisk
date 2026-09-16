import Foundation
import Observation
import XCTest
@testable import Forge

final class SteamIntegrationTests: XCTestCase {
    @MainActor
    func testOwnedLibraryDefaultsToMostRecentlyPlayedWithStableUnplayedOrder() {
        let games = [
            SteamGame(appId: 1, name: "Étoiles", iconHash: "", lastPlayed: 100, playtimeMinutes: 800),
            SteamGame(appId: 2, name: "Portal", iconHash: "", lastPlayed: 200, playtimeMinutes: 100),
            SteamGame(appId: 4, name: "Zéro", iconHash: "", lastPlayed: 0, playtimeMinutes: 0),
            SteamGame(appId: 3, name: "Alpha", iconHash: "", lastPlayed: 0, playtimeMinutes: 0),
        ]
        XCTAssertEqual(SteamModel.filterAndSort(games, query: "", sort: .lastPlayed).map(\.appId), [2, 1, 3, 4])
        XCTAssertEqual(SteamModel.filterAndSort(games, query: "  etoile  ", sort: .name).map(\.appId), [1])
        XCTAssertEqual(SteamModel.filterAndSort(games, query: "", sort: .playtime).first?.appId, 1)
        XCTAssertEqual(SteamModel.filterAndSort(games, query: "missing", sort: .lastPlayed).count, 0)
    }

    @MainActor
    func testCachedLibraryKeepsSortTiesAndSearchResultsWhenSwitchingOrders() async {
        let session = CachedSteamSession(games: cachedSteamGames)
        let model = cachedSteamModel(session: session)
        await model.start()

        for _ in 0..<2 {
            for (sort, expected) in cachedSteamOrders {
                model.sort = sort
                XCTAssertEqual(model.filteredGames.map(\.appId), expected)
                XCTAssertEqual(model.filteredGames.map(\.appId), expected)
            }
        }
        model.searchText = "  etoile  "
        for sort in SteamLibrarySort.allCases {
            model.sort = sort
            XCTAssertEqual(model.filteredGames.map(\.appId), [1])
        }
        model.searchText = "portal"
        XCTAssertEqual(model.filteredGames.map(\.appId), [2, 4])
        model.searchText = "absent"
        XCTAssertTrue(model.filteredGames.isEmpty)
        model.searchText = ""
        for (sort, expected) in cachedSteamOrders {
            model.sort = sort
            XCTAssertEqual(model.filteredGames.map(\.appId), expected)
        }
    }

    @MainActor
    func testRefreshInvalidatesEveryCachedOrderForChangedGamesWithSameIdentifiers() async throws {
        let session = CachedSteamSession(games: cachedSteamGames)
        let model = cachedSteamModel(session: session)
        await model.start()
        for sort in SteamLibrarySort.allCases {
            model.sort = sort
            _ = model.filteredGames
        }
        let refreshed = [
            SteamGame(appId: 1, name: "Zèbre", iconHash: "", lastPlayed: 300, playtimeMinutes: 1),
            SteamGame(appId: 2, name: "Alpha", iconHash: "", lastPlayed: 0, playtimeMinutes: 50),
            SteamGame(appId: 3, name: "Portal", iconHash: "", lastPlayed: 400, playtimeMinutes: 900),
            SteamGame(appId: 4, name: "Beta", iconHash: "", lastPlayed: 0, playtimeMinutes: 0),
            SteamGame(appId: 5, name: "Étoiles", iconHash: "", lastPlayed: 0, playtimeMinutes: 0),
        ]
        await session.replaceGames(refreshed)
        model.refresh()
        try await waitForCachedLibrary { model.games.first?.name == "Zèbre" && !model.isLoadingGames }

        let expected: [(SteamLibrarySort, [UInt32])] = [
            (.lastPlayed, [3, 1, 2, 4, 5]),
            (.name, [2, 4, 5, 3, 1]),
            (.playtime, [3, 2, 1, 4, 5]),
        ]
        for (sort, identifiers) in expected {
            model.sort = sort
            XCTAssertEqual(model.filteredGames.map(\.appId), identifiers)
        }
        model.searchText = "etoile"
        XCTAssertEqual(model.filteredGames.map(\.appId), [5])
    }

    @MainActor
    func testSignOutClearsCachedGamesForEveryOrder() async {
        let session = CachedSteamSession(games: cachedSteamGames)
        let model = cachedSteamModel(session: session)
        await model.start()
        for sort in SteamLibrarySort.allCases {
            model.sort = sort
            XCTAssertFalse(model.filteredGames.isEmpty)
        }
        model.searchText = "portal"
        XCTAssertFalse(model.filteredGames.isEmpty)

        model.signOut()

        XCTAssertFalse(model.isConnected)
        XCTAssertEqual(model.searchText, "")
        for sort in SteamLibrarySort.allCases {
            model.sort = sort
            XCTAssertTrue(model.filteredGames.isEmpty)
        }
    }

    @MainActor
    func testCachedLibraryStillObservesSearchSortAndRefreshedGames() async throws {
        let session = CachedSteamSession(games: cachedSteamGames)
        let model = cachedSteamModel(session: session)
        await model.start()
        _ = model.filteredGames
        let sortChanged = expectation(description: "A cached library observes the selected sort")
        withObservationTracking { _ = model.filteredGames } onChange: { sortChanged.fulfill() }
        model.sort = .name
        XCTAssertEqual(model.filteredGames.map(\.appId), [3, 1, 2, 4, 5])
        await fulfillment(of: [sortChanged], timeout: 0.2)

        let searchChanged = expectation(description: "A cached library observes the search text")
        withObservationTracking { _ = model.filteredGames } onChange: { searchChanged.fulfill() }
        model.searchText = "portal"
        XCTAssertEqual(model.filteredGames.map(\.appId), [2, 4])
        await fulfillment(of: [searchChanged], timeout: 0.2)

        let gamesChanged = expectation(description: "A cached library observes the refreshed games")
        withObservationTracking { _ = model.filteredGames } onChange: { gamesChanged.fulfill() }
        await session.replaceGames([])
        model.refresh()
        try await waitForCachedLibrary { model.games.isEmpty && !model.isLoadingGames }
        XCTAssertTrue(model.filteredGames.isEmpty)
        await fulfillment(of: [gamesChanged], timeout: 0.2)
    }

    @MainActor
    func testDelayedPublicProfileDoesNotBlockRestoredLibraryOrConnection() async throws {
        let profiles = CachedSteamProfiles()
        let session = CachedSteamSession(games: cachedSteamGames, profile: { try await profiles.request() })
        let model = cachedSteamModel(session: session)
        let started = expectation(description: "Restoring finishes while the public profile is pending")
        Task {
            await model.start()
            started.fulfill()
        }
        try await waitForProfile { await profiles.requestCount == 1 }
        await fulfillment(of: [started], timeout: 2)
        XCTAssertTrue(model.isConnected)
        XCTAssertFalse(model.isRestoring)
        XCTAssertFalse(model.isLoadingGames)
        XCTAssertEqual(model.games.map(\.appId), cachedSteamGames.map(\.appId))
        XCTAssertNil(model.avatarURL)
        XCTAssertNil(model.displayName, "The private login must not serve as the public display name")

        await profiles.release(1, result: .success(cachedSteamProfile))
        try await waitForProfile { model.avatarURL?.absoluteString == cachedSteamAvatarURL && model.displayName == cachedSteamProfile.displayName }
        XCTAssertEqual(model.displayName, "Public name")
        XCTAssertNotEqual(model.displayName, "Cache fixture")
        XCTAssertNil(model.errorMessage)
    }

    @MainActor
    func testRefreshClearsRemovedAvatarAndKeepsPublicNameForInitials() async throws {
        let profiles = CachedSteamProfiles()
        let session = CachedSteamSession(games: cachedSteamGames, profile: { try await profiles.request() })
        let model = cachedSteamModel(session: session)
        await profiles.release(1, result: .success(cachedSteamProfile))
        await model.start()
        try await waitForProfile { model.avatarURL?.absoluteString == cachedSteamAvatarURL && model.displayName == cachedSteamProfile.displayName }

        model.refresh()
        try await waitForProfile { await profiles.requestCount == 2 }
        XCTAssertEqual(model.avatarURL?.absoluteString, cachedSteamAvatarURL)
        XCTAssertFalse(model.isLoadingGames)
        await profiles.release(2, result: .success(SteamProfile(displayName: "Public name", avatarUrl: nil)))
        try await waitForProfile { model.avatarURL == nil }
        XCTAssertEqual(model.displayName, "Public name")
        XCTAssertTrue(model.isConnected)
        XCTAssertEqual(model.games.count, cachedSteamGames.count)
        XCTAssertNil(model.errorMessage)
    }

    @MainActor
    func testRefreshUpdatesPublicNameAndClearsItWhenMissingWithoutUsingPrivateLogin() async throws {
        let profiles = CachedSteamProfiles()
        let session = CachedSteamSession(games: cachedSteamGames, profile: { try await profiles.request() })
        let model = cachedSteamModel(session: session)
        await profiles.release(1, result: .success(cachedSteamProfile))
        await model.start()
        try await waitForProfile { model.displayName == "Public name" }

        model.refresh()
        try await waitForProfile { await profiles.requestCount == 2 }
        await profiles.release(2, result: .success(SteamProfile(displayName: "New display name", avatarUrl: nil)))
        try await waitForProfile { model.displayName == "New display name" && model.avatarURL == nil }
        XCTAssertTrue(model.isConnected)
        XCTAssertNil(model.errorMessage)

        model.refresh()
        try await waitForProfile { await profiles.requestCount == 3 }
        await profiles.release(3, result: .success(SteamProfile(displayName: nil, avatarUrl: nil)))
        try await waitForProfile { model.displayName == nil }
        XCTAssertNil(model.displayName, "A missing public name must not fall back to the private login")
        XCTAssertNil(model.avatarURL)
        XCTAssertTrue(model.isConnected)
        XCTAssertNil(model.errorMessage)
    }

    @MainActor
    func testProfileRefreshFailurePreservesPublicNamePhotoOrInitialsWithoutAnError() async throws {
        let initialProfiles = [
            SteamProfile(displayName: nil, avatarUrl: nil),
            SteamProfile(displayName: "Public name without photo", avatarUrl: nil),
            cachedSteamProfile,
        ]
        for initialProfile in initialProfiles {
            let profiles = CachedSteamProfiles()
            let session = CachedSteamSession(games: cachedSteamGames, profile: { try await profiles.request() })
            let model = cachedSteamModel(session: session)
            await profiles.release(1, result: .success(initialProfile))
            await model.start()
            try await waitForProfile { await profiles.hasReturned(1) }
            if let initialName = initialProfile.displayName {
                try await waitForProfile { model.displayName == initialName && model.avatarURL?.absoluteString == initialProfile.avatarUrl }
            }

            model.refresh()
            try await waitForProfile { await profiles.requestCount == 2 }
            try await releaseProfileWithoutPresentationChange(profiles, request: 2, result: .failure(.profileUnavailable), model: model)
            XCTAssertEqual(model.avatarURL?.absoluteString, initialProfile.avatarUrl)
            XCTAssertEqual(model.displayName, initialProfile.displayName)
            XCTAssertNil(model.errorMessage)
            XCTAssertTrue(model.isConnected)
            XCTAssertFalse(model.isLoadingGames)
            XCTAssertEqual(model.games.count, cachedSteamGames.count)
        }
    }

    @MainActor
    func testSignOutClearsPublicProfileAndRejectsPendingPreviousAccountResponse() async throws {
        let profiles = CachedSteamProfiles()
        let session = CachedSteamSession(games: cachedSteamGames, profile: { try await profiles.request() })
        let model = cachedSteamModel(session: session)
        await profiles.release(1, result: .success(cachedSteamProfile))
        await model.start()
        try await waitForProfile { model.avatarURL?.absoluteString == cachedSteamAvatarURL && model.displayName == cachedSteamProfile.displayName }
        model.refresh()
        try await waitForProfile { await profiles.requestCount == 2 }

        model.signOut()
        XCTAssertNil(model.avatarURL)
        XCTAssertNil(model.displayName)
        XCTAssertFalse(model.isConnected)
        // The fixture deliberately completes successfully despite cancellation,
        // as a bridge response can already be in flight when the account closes.
        try await releaseProfileWithoutPresentationChange(profiles, request: 2, result: .success(cachedSteamProfile), model: model)
        XCTAssertNil(model.avatarURL)
        XCTAssertNil(model.displayName)
        XCTAssertFalse(model.isConnected)
        XCTAssertTrue(model.games.isEmpty)
    }

    @MainActor
    func testNewProfileRefreshRejectsOlderNameAndAvatarResponseThatFinishesLast() async throws {
        let profiles = CachedSteamProfiles()
        let session = CachedSteamSession(games: cachedSteamGames, profile: { try await profiles.request() })
        let model = cachedSteamModel(session: session)
        let started = expectation(description: "The initial library finishes before its public profile")
        Task {
            await model.start()
            started.fulfill()
        }
        try await waitForProfile { await profiles.requestCount == 1 }
        await fulfillment(of: [started], timeout: 2)
        model.refresh()
        try await waitForProfile { await profiles.requestCount == 2 }

        let refreshedURL = "https://avatars.steamstatic.com/2222222222222222222222222222222222222222_full.jpg"
        await profiles.release(2, result: .success(SteamProfile(displayName: "Updated display name", avatarUrl: refreshedURL)))
        try await waitForProfile { model.avatarURL?.absoluteString == refreshedURL && model.displayName == "Updated display name" }
        try await releaseProfileWithoutPresentationChange(profiles, request: 1, result: .success(cachedSteamProfile), model: model)
        XCTAssertEqual(model.avatarURL?.absoluteString, refreshedURL)
        XCTAssertEqual(model.displayName, "Updated display name")
        XCTAssertNil(model.errorMessage)
    }

    @MainActor
    private func waitForProfile(_ predicate: @MainActor () async -> Bool) async throws {
        let deadline = ContinuousClock.now + .seconds(2)
        while !(await predicate()) {
            if ContinuousClock.now >= deadline { throw CachedSteamFailure.timeout }
            await Task.yield()
        }
    }

    @MainActor
    private func releaseProfileWithoutPresentationChange(
        _ profiles: CachedSteamProfiles,
        request: Int,
        result: Result<SteamProfile, CachedSteamFailure>,
        model: SteamModel
    ) async throws {
        let changed = expectation(description: "A failed or obsolete public profile must not change account presentation")
        changed.isInverted = true
        withObservationTracking {
            _ = model.avatarURL
            _ = model.displayName
            _ = model.errorMessage
        } onChange: { changed.fulfill() }
        await profiles.release(request, result: result)
        try await waitForProfile { await profiles.hasReturned(request) }
        await fulfillment(of: [changed], timeout: 0.1)
    }

    @MainActor
    private func cachedSteamModel(session: CachedSteamSession) -> SteamModel {
        SteamModel(
            directory: FileManager.default.temporaryDirectory.appending(path: "ForgeCachedLibrary-\(UUID().uuidString)"),
            keychain: CachedSteamCredentials(),
            connect: { _ in session },
            news: { _ in [] }
        )
    }

    @MainActor
    private func waitForCachedLibrary(_ predicate: @MainActor () -> Bool) async throws {
        let deadline = ContinuousClock.now + .seconds(2)
        while !predicate() {
            if ContinuousClock.now >= deadline { throw CachedSteamFailure.timeout }
            try await Task.sleep(for: .milliseconds(5))
        }
    }

    func testMalformedSavedSessionIsRejectedAcrossAsyncRustBridge() async {
        do {
            _ = try await SteamSession.connect(credentials: SteamCredentials(accountName: "fixture", steamId: 1, refreshToken: "invalid-token"))
            XCTFail("A malformed saved token must fail before a CM connection")
        } catch {
            guard case let SteamError.Failure(reason) = error else {
                XCTFail("Expected the typed Steam error, got \(type(of: error))")
                return
            }
            XCTAssertEqual(reason, "backend.steam.auth.protocol")
            XCTAssertFalse(reason.contains("invalid-token"))
        }
    }

    func testSteamKeychainRoundTripUsesIsolatedItem() async throws {
        let store = SteamKeychain(service: "com.zagrios.forge.tests.\(UUID().uuidString)")
        let initial = try await store.load()
        XCTAssertNil(initial)
        // Deliberately not a real authentication token or a real account.
        let fixture = SteamCredentials(accountName: "fixture", steamId: 42, refreshToken: "test-only")
        try await store.save(fixture)
        do {
            let loaded = try await store.load()
            XCTAssertEqual(loaded, fixture)
            try await store.save(SteamCredentials(accountName: "replacement", steamId: 43, refreshToken: "test-only-replacement"))
            let replaced = try await store.load()
            XCTAssertEqual(replaced?.steamId, 43)
        } catch {
            try? await store.remove()
            throw error
        }
        try await store.remove()
        let removed = try await store.load()
        XCTAssertNil(removed)
    }
}

private let cachedSteamGames = [
    SteamGame(appId: 1, name: "Étoiles", iconHash: "", lastPlayed: 100, playtimeMinutes: 800),
    SteamGame(appId: 2, name: "Portal", iconHash: "", lastPlayed: 200, playtimeMinutes: 100),
    SteamGame(appId: 3, name: "Alpha", iconHash: "", lastPlayed: 0, playtimeMinutes: 0),
    SteamGame(appId: 4, name: "Portal", iconHash: "", lastPlayed: 200, playtimeMinutes: 100),
    SteamGame(appId: 5, name: "Zéro", iconHash: "", lastPlayed: 0, playtimeMinutes: 0),
]
private let cachedSteamOrders: [(SteamLibrarySort, [UInt32])] = [
    (.lastPlayed, [2, 4, 1, 3, 5]),
    (.name, [3, 1, 2, 4, 5]),
    (.playtime, [1, 2, 4, 3, 5]),
]

private actor CachedSteamCredentials: SteamCredentialStore {
    func load() -> SteamCredentials? {
        SteamCredentials(accountName: "Cache fixture", steamId: 42, refreshToken: "fixture-only-never-sent")
    }
    func save(_ credentials: SteamCredentials) {}
    func remove() {
        // The offline sign-out test must not start a real Steam QR login.
        withUnsafeCurrentTask { $0?.cancel() }
    }
}

private actor CachedSteamSession: SteamSessionProtocol {
    private var library: [SteamGame]
    private let loadProfile: @Sendable () async throws -> SteamProfile
    init(games: [SteamGame], profile: @escaping @Sendable () async throws -> SteamProfile = { SteamProfile(displayName: nil, avatarUrl: nil) }) {
        library = games
        loadProfile = profile
    }
    func replaceGames(_ games: [SteamGame]) { library = games }
    func games() -> [SteamGame] { library }
    func profile() async throws -> SteamProfile { try await loadProfile() }
    func details(appId: UInt32, language: String) async throws -> SteamGameDetails { throw CachedSteamFailure.unexpectedOperation }
    func download(appId: UInt32, platform: String, language: String, directory: String) async throws -> SteamInstallation { throw CachedSteamFailure.unexpectedOperation }
    nonisolated func downloadProgress() -> SteamTransfer? { nil }
    nonisolated func cancelDownload() {}
    func launchPlan(appId: UInt32, platform: String, directory: String) async throws -> SteamLaunchPlan { throw CachedSteamFailure.unexpectedOperation }
}

private let cachedSteamAvatarURL = "https://avatars.steamstatic.com/1111111111111111111111111111111111111111_full.jpg"

private let cachedSteamProfile = SteamProfile(displayName: "Public name", avatarUrl: cachedSteamAvatarURL)

private actor CachedSteamProfiles {
    private(set) var requestCount = 0
    private var results: [Int: Result<SteamProfile, CachedSteamFailure>] = [:]
    private var pending: [Int: CheckedContinuation<SteamProfile, any Error>] = [:]
    private var returned: Set<Int> = []

    func request() async throws -> SteamProfile {
        requestCount += 1
        let identifier = requestCount
        defer { returned.insert(identifier) }
        if let result = results.removeValue(forKey: identifier) { return try result.get() }
        return try await withCheckedThrowingContinuation { pending[identifier] = $0 }
    }

    func release(_ identifier: Int, result: Result<SteamProfile, CachedSteamFailure>) {
        if let continuation = pending.removeValue(forKey: identifier) {
            continuation.resume(with: result.mapError { $0 as any Error })
        } else {
            results[identifier] = result
        }
    }

    func hasReturned(_ identifier: Int) -> Bool { returned.contains(identifier) }
}

private enum CachedSteamFailure: Error { case timeout, unexpectedOperation, profileUnavailable }
