import Foundation
import Observation
import XCTest
@testable import Obelisk

/// The account lifecycle, library and game page every web storefront shares.
/// Each test runs once per store, through its subclass.
@MainActor
class WebAccountModelTests: XCTestCase {
    nonisolated class var store: (any WebStoreFixture.Type)? { nil }

    /// The base class runs nothing: each store's subclass runs every test.
    nonisolated override class var defaultTestSuite: XCTestSuite {
        store == nil ? XCTestSuite(name: "\(self)") : super.defaultTestSuite
    }

    // MARK: Library

    func testPlaytimesLoadFromOneRequestAndOrderTheLibrary() async throws {
        try await playtimeOrder(Self.store!)
    }
    private func playtimeOrder<S: WebStoreFixture>(_: S.Type) async throws {
        let games = [S.game("a", title: "Alpha"), S.game("b", title: "Beta"), S.game("c", title: "Gamma"),
                     S.game("d", title: "Delta")]
        let stored = S.credentials("current")
        let session = WebTestSession<S>(credentials: stored, games: games, playtimes: [
            games[2].id: 36_000, games[1].id: 600, games[0].id: 0,
        ])
        let model = webModel(S.self, MemoryStore(stored), restore: { _ in session })
        await model.start()
        defer { model.signOut() }
        try await eventually { model.library.playtime(of: games[2]) == .seconds(36_000) }

        XCTAssertEqual(model.library.shown.map(\.title), ["Alpha", "Beta", "Delta", "Gamma"], "By name at first")
        model.library.sort = .playtime
        XCTAssertEqual(model.library.shown.map(\.title), ["Gamma", "Beta", "Alpha", "Delta"])
        XCTAssertNil(model.library.pendingMetadata)
        XCTAssertNil(model.library.missingMetadata)
        let asked = await session.playtimeRequests
        XCTAssertEqual(asked, [games.map(\.id)], "One request names every game, as the library names it")

        model.signOut()
        XCTAssertNil(model.library.playtime(of: games[2]))
    }

    func testAnAbsurdPlaytimeSaturates() async throws {
        try await absurdPlaytime(Self.store!)
    }
    private func absurdPlaytime<S: WebStoreFixture>(_: S.Type) async throws {
        let owned = S.game("a", title: "Alpha")
        let session = WebTestSession<S>(credentials: S.credentials("current"), games: [owned],
                                        playtimes: [owned.id: .max])
        let model = await connectedModel(session)
        defer { model.signOut() }
        try await eventually { model.library.playtime(of: owned) == .seconds(Int64.max) }
    }

    func testAFailedPlaytimeRequestKeepsTheLibraryAndOffersARetry() async throws {
        try await failedPlaytimes(Self.store!)
    }
    private func failedPlaytimes<S: WebStoreFixture>(_: S.Type) async throws {
        let games = [S.game("a", title: "Alpha"), S.game("b", title: "Beta")]
        let stored = S.credentials("current")
        let session = WebTestSession<S>(credentials: stored, games: games, playtimes: [games[1].id: 60])
        await session.failNextPlaytimes()
        let model = await connectedModel(session)
        defer { model.signOut() }
        model.library.sort = .playtime
        try await eventually { model.library.missingMetadata != nil }

        model.library.sort = .name
        XCTAssertEqual(model.library.shown.count, 2)
        XCTAssertNil(model.library.missingMetadata, "The name order needs no playtime")
        XCTAssertNil(model.errorMessage)
        model.library.sort = .playtime
        XCTAssertNotNil(model.library.missingMetadata)

        await session.holdNextGames()
        model.refresh()
        try await eventually { await session.waitingForGames }
        XCTAssertNil(model.library.missingMetadata, "The retry clears the failure before the games answer")
        XCTAssertNotNil(model.library.pendingMetadata)
        await session.completeGames(games)
        try await eventually {
            !model.isLoadingGames && model.library.pendingMetadata == nil
                && model.library.playtime(of: games[1]) == .seconds(60)
        }
        XCTAssertNil(model.library.missingMetadata)
        XCTAssertEqual(model.library.shown.map(\.title), ["Beta", "Alpha"])
    }

    func testTheAccountsLaunchesDateItsGamesUntilItSignsOut() async throws {
        try await lastLaunch(Self.store!)
    }
    private func lastLaunch<S: WebStoreFixture>(_: S.Type) async throws {
        let games = [S.game("a", title: "Alpha"), S.game("b", title: "Beta"), S.game("c", title: "Gamma")]
        let directory = FileManager.default.temporaryDirectory.appending(path: "ObeliskWebFeatured-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let history = LaunchHistory(cache: LibraryCache(directory: directory.path))
        let stored = S.credentials("current")
        let session = WebTestSession<S>(credentials: stored, games: games)
        let model = webModel(S.self, MemoryStore(stored), restore: { _ in session },
                             gameSessions: GameSessions(FakeSessions(), history: history))
        await model.start()

        history.record(games[0].id, account: S.fixtureAccount, in: S.self)
        XCTAssertNotNil(model.library.lastPlayed(of: games[0]))
        XCTAssertEqual(model.library.featured, games[0], "The last game launched from Obelisk leads the library")

        model.signOut()
        XCTAssertNil(history.lastLaunch(of: games[0].id, account: S.fixtureAccount, in: S.self),
                     "Signing out forgets the account's launches with its library")
    }

    func testTheCachedLibraryShowsWhileRestoringAndSigningOutForgetsIt() async throws {
        try await cachedLibrary(Self.store!)
    }
    private func cachedLibrary<S: WebStoreFixture>(_: S.Type) async throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "ObeliskWebCache-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let cached = S.game("7", title: "Cached game")
        let cache = try S.cache([(cached, 7200)], account: S.fixtureAccount, in: directory)
        let stored = S.credentials("current")
        let listed = S.game("1", title: "Alpha")
        let session = WebTestSession<S>(credentials: stored, games: [listed], playtimes: [listed.id: 60])
        let history = LaunchHistory(cache: cache)
        let launched = Date(timeIntervalSince1970: 1_790_000_000)
        history.record(cached.id, account: S.fixtureAccount, in: S.self, at: launched)
        let restoration = GameSessionGate<WebSession>()
        let model = webModel(S.self, MemoryStore(stored), restore: { _ in try await restoration.wait() },
                             gameSessions: GameSessions(FakeSessions(), history: history), libraryCache: cache)
        let start = Task { await model.start() }

        try await eventually { model.showsLibrary }
        XCTAssertFalse(model.isConnected, "The cached library shows before the storefront answers")
        XCTAssertEqual(model.library.games, [cached])
        XCTAssertEqual(model.library.playtime(of: cached), .seconds(7200),
                       "The cached playtime shows too, whatever unit the storefront's file keeps")
        XCTAssertEqual(model.library.lastPlayed(of: cached), launched, "The cached account's launches already count")
        XCTAssertEqual(model.library.featured, cached, "The last game launched from Obelisk leads the library")
        await restoration.resolve(session)
        await start.value
        try await eventually {
            model.library.games.map(\.title) == ["Alpha"] && model.library.playtime(of: model.library.games[0]) == .seconds(60)
        }

        model.signOut()
        XCTAssertTrue(S.cachedLibrary(in: cache, account: S.fixtureAccount).isEmpty,
                      "Signing out forgets the account's library")
    }

    func testAFailedRestoreHidesTheCachedLibraryButKeepsItOnDisk() async throws {
        try await failedRestoreCache(Self.store!)
    }
    private func failedRestoreCache<S: WebStoreFixture>(_: S.Type) async throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "ObeliskWebCache-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let cached = S.game("7", title: "Cached game")
        let cache = try S.cache([(cached, 7200)], account: S.fixtureAccount, in: directory)
        let restoration = GameSessionGate<WebSession>()
        let model = webModel(S.self, MemoryStore(S.credentials("current")),
                             restore: { _ in try await restoration.wait() }, libraryCache: cache)
        let start = Task { await model.start() }

        try await eventually { model.showsLibrary }
        XCTAssertEqual(model.library.games, [cached])
        await restoration.fail(S.network)
        await start.value
        XCTAssertTrue(model.library.games.isEmpty, "A failed restore hides the cached library")
        XCTAssertFalse(model.showsLibrary)
        XCTAssertTrue(model.canRetryRestore)
        XCTAssertEqual(S.cachedLibrary(in: cache, account: S.fixtureAccount).map(\.game), [cached],
                       "The retry can show it again")
    }

    func testARestoredAccountOtherThanTheCachedOneStartsEmpty() async throws {
        try await otherAccountRestored(Self.store!)
    }
    private func otherAccountRestored<S: WebStoreFixture>(_: S.Type) async throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "ObeliskWebCache-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let cached = S.game("7", title: "A's game")
        let cache = try S.cache([(cached, 1800)], account: "A", in: directory)
        let restored = S.game("1", title: "B's game")
        let session = WebTestSession<S>(credentials: S.credentials("t", account: "B"), games: [restored])
        await session.holdNextGames()
        let restoration = GameSessionGate<WebSession>()
        let model = webModel(S.self, MemoryStore(S.credentials("t", account: "A")),
                             restore: { _ in try await restoration.wait() }, libraryCache: cache)
        let start = Task { await model.start() }

        try await eventually { model.showsLibrary }
        XCTAssertEqual(model.library.games, [cached])
        XCTAssertEqual(model.library.playtime(of: cached), .seconds(1800))
        await restoration.resolve(session)
        try await eventually { model.isConnected }
        XCTAssertTrue(model.library.games.isEmpty, "Another account's cached games never show as this one's")
        XCTAssertEqual(model.accountName, "Name of B")
        XCTAssertNil(model.library.playtime(of: cached), "Nor do its playtimes")

        await session.completeGames([restored])
        await start.value
        XCTAssertEqual(model.library.games, [restored])
        model.signOut()
    }

    func testRefreshReopensTheUpdatedGameAndClosesOneThatLeft() async throws {
        try await refreshSelection(Self.store!)
    }
    private func refreshSelection<S: WebStoreFixture>(_: S.Type) async throws {
        let first = S.game("1", title: "Alpha")
        let second = S.game("2", title: "Beta")
        let stored = S.credentials("current")
        let session = WebTestSession<S>(credentials: stored, games: [first, second])
        let model = await connectedModel(session)
        defer { model.signOut() }
        model.open(first)

        await session.holdNextGames()
        model.refresh()
        try await eventually { await session.waitingForGames }
        await session.completeGames([S.game("1", title: "Alpha Remastered"), second])
        try await eventually { !model.isLoadingGames }
        XCTAssertEqual(model.selectedGame?.title, "Alpha Remastered", "The game on screen follows its update")

        await session.holdNextGames()
        model.refresh()
        try await eventually { await session.waitingForGames }
        await session.completeGames([second])
        try await eventually { !model.isLoadingGames }
        XCTAssertNil(model.selectedGame, "A game the library lost closes")
    }

    // MARK: Account

    func testRestoreRunsOnceAndSavesRotatedRefreshToken() async throws {
        try await restoreOnce(Self.store!)
    }
    private func restoreOnce<S: WebStoreFixture>(_: S.Type) async throws {
        let original = S.credentials("original")
        let rotated = S.credentials("rotated")
        let store = MemoryStore(original)
        let session = WebTestSession<S>(credentials: original, games: [S.game("1", title: "One")], rotated: rotated)
        let model = webModel(S.self, store, restore: { _ in session })

        await model.start()
        await model.start()

        XCTAssertTrue(model.isConnected)
        XCTAssertFalse(model.isRestoring)
        XCTAssertEqual(model.accountName, S.fixtureName)
        XCTAssertEqual(model.library.games.map(\.title), ["One"])
        try await eventually { await store.value == rotated }
        let loads = await store.loads
        XCTAssertEqual(loads, 1)
    }

    func testConnectionStatusNotifiesObserversWhenRestoringSession() async throws {
        try await restoreNotifies(Self.store!)
    }
    private func restoreNotifies<S: WebStoreFixture>(_: S.Type) async throws {
        let store = MemoryStore(S.credentials("saved"))
        let session = WebTestSession<S>(credentials: S.credentials("restored"), games: [S.game("1", title: "One")])
        let model = webModel(S.self, store, restore: { _ in session })
        let changed = expectation(description: "The disconnected view observes the restored session")
        withObservationTracking {
            XCTAssertFalse(model.isConnected)
        } onChange: {
            changed.fulfill()
        }

        await model.start()

        XCTAssertTrue(model.isConnected)
        XCTAssertEqual(model.library.games.count, 1)
        await fulfillment(of: [changed], timeout: 0.2)
    }

    func testConnectionStatusNotifiesObserversAfterSignIn() async throws {
        try await signInNotifies(Self.store!)
    }
    private func signInNotifies<S: WebStoreFixture>(_: S.Type) async throws {
        let session = WebTestSession<S>(credentials: S.credentials("signed-in"), games: [S.game("1", title: "One")])
        let model = webModel(S.self, MemoryStore(nil), signIn: { _ in session })
        await model.start()
        let changed = expectation(description: "The login view observes the new session")
        withObservationTracking {
            XCTAssertFalse(model.isConnected)
        } onChange: {
            changed.fulfill()
        }

        model.signIn()
        try await eventually { model.isConnected && !model.isLoadingGames }

        XCTAssertEqual(model.library.games.count, 1)
        await fulfillment(of: [changed], timeout: 0.2)
    }

    func testTransientRestoreFailureKeepsStoredSessionAndCanRetry() async throws {
        try await transientRestoreFailure(Self.store!)
    }
    private func transientRestoreFailure<S: WebStoreFixture>(_: S.Type) async throws {
        let original = S.credentials("original")
        let store = MemoryStore(original)
        let session = WebTestSession<S>(credentials: original, games: [S.game("1", title: "One")])
        let attempt = RestoreAttempt(session: session, failure: S.network)
        let model = webModel(S.self, store, restore: { _ in try await attempt.restore() })

        await model.start()
        XCTAssertFalse(model.isConnected)
        XCTAssertTrue(model.canRetryRestore)
        let retained = await store.value
        XCTAssertEqual(retained, original)
        model.retryRestore()
        try await eventually { model.isConnected && !model.isLoadingGames }
        XCTAssertEqual(model.library.games.map(\.title), ["One"])
        XCTAssertNil(model.errorMessage)
    }

    func testRetryRestoreReservesAuthenticationBeforeAnotherSignIn() async throws {
        try await retryReserves(Self.store!)
    }
    private func retryReserves<S: WebStoreFixture>(_: S.Type) async throws {
        let original = S.credentials("original")
        let session = WebTestSession<S>(credentials: original, games: [S.game("1", title: "One")])
        let attempt = RestoreAttempt(session: session, failure: S.network)
        let login = SignInCounter(session: session)
        let model = webModel(S.self, MemoryStore(original), signIn: { _ in await login.signIn() },
                             restore: { _ in try await attempt.restore() })
        await model.start()

        model.retryRestore()
        model.signIn()
        try await eventually { model.isConnected && !model.isLoadingGames }
        let signIns = await login.calls
        XCTAssertEqual(signIns, 0)
    }

    func testExpiredRestoreRemovesSavedSession() async throws {
        try await expiredRestore(Self.store!)
    }
    private func expiredRestore<S: WebStoreFixture>(_: S.Type) async throws {
        let store = MemoryStore(S.credentials("expired"))
        let model = webModel(S.self, store, restore: { _ in throw S.unauthorized })
        await model.start()
        let saved = await store.value
        XCTAssertNil(saved)
        XCTAssertFalse(model.isConnected)
        XCTAssertFalse(model.canRetryRestore)
        XCTAssertEqual(model.errorMessage, S.unauthorizedMessage)
    }

    func testFailedLibraryRequestKeepsGamesAndSavesRotatedTokenThenRetries() async throws {
        try await failedLibrary(Self.store!)
    }
    private func failedLibrary<S: WebStoreFixture>(_: S.Type) async throws {
        let original = S.credentials("original")
        let rotated = S.credentials("rotated")
        let store = MemoryStore(original)
        let session = WebTestSession<S>(credentials: original, games: [S.game("1", title: "One")])
        let model = webModel(S.self, store, restore: { _ in session })
        await model.start()
        await session.failNextGames(rotated: rotated)

        model.refresh()
        try await eventually { !model.isLoadingGames }
        XCTAssertEqual(model.library.games.map(\.title), ["One"])
        XCTAssertNotNil(model.errorMessage)
        try await eventually { await store.value == rotated }

        model.refresh()
        try await eventually { !model.isLoadingGames }
        XCTAssertNil(model.errorMessage)
        XCTAssertTrue(model.isConnected)
    }

    func testRefreshIsSerializedAndSignOutDiscardsLateLibraryResponse() async throws {
        try await serializedRefresh(Self.store!)
    }
    private func serializedRefresh<S: WebStoreFixture>(_: S.Type) async throws {
        let original = S.credentials("original")
        let store = MemoryStore(original)
        let session = WebTestSession<S>(credentials: original, games: [S.game("1", title: "One")])
        let model = webModel(S.self, store, restore: { _ in session })
        await model.start()
        await session.holdNextGames()
        model.refresh()
        model.refresh()
        try await eventually { await session.waitingForGames }
        let calls = await session.gameCalls
        XCTAssertEqual(calls, 2)

        model.signOut()
        await session.completeGames([S.game("2", title: "Late")])
        try await eventually {
            let disconnections = await session.disconnections
            return !model.isSigningOut && disconnections == 1
        }
        XCTAssertFalse(model.isConnected)
        XCTAssertTrue(model.library.games.isEmpty)
        XCTAssertFalse(model.isLoadingGames)
        let saved = await store.value
        XCTAssertNil(saved)
    }

    func testExpiredLibrarySessionClearsAccountSelectionAndSavedToken() async throws {
        try await expiredLibrary(Self.store!)
    }
    private func expiredLibrary<S: WebStoreFixture>(_: S.Type) async throws {
        let original = S.credentials("original")
        let store = MemoryStore(original)
        let owned = S.game("1", title: "One")
        let session = WebTestSession<S>(credentials: original, games: [owned])
        let model = webModel(S.self, store, restore: { _ in session })
        await model.start()
        model.open(owned)
        model.library.searchText = "one"
        await session.expireNextGames()

        model.refresh()
        try await eventually { !model.isConnected && !model.isSigningOut }
        XCTAssertTrue(model.library.games.isEmpty)
        XCTAssertNil(model.selectedGame)
        XCTAssertEqual(model.library.searchText, "")
        XCTAssertEqual(model.errorMessage, S.unauthorizedMessage)
        let saved = await store.value
        XCTAssertNil(saved)
    }

    func testCancelledLoginDisposesLateSessionWithoutRestoringAccount() async throws {
        try await cancelledLogin(Self.store!)
    }
    private func cancelledLogin<S: WebStoreFixture>(_: S.Type) async throws {
        let store = MemoryStore<WebAccountCredentials>(nil)
        let session = WebTestSession<S>(credentials: S.credentials("late"), games: [S.game("1", title: "Late")])
        let login = GameSessionGate<WebSession>()
        let model = webModel(S.self, store, signIn: { _ in try await login.wait() })
        model.signIn()
        try await eventually { await login.requests == 1 }
        model.cancelSignIn()
        await login.resolve(session)
        try await eventually { await session.disconnections == 1 }
        XCTAssertFalse(model.isConnected)
        XCTAssertFalse(model.isConnecting)
        XCTAssertTrue(model.library.games.isEmpty)
        let saved = await store.value
        XCTAssertNil(saved)
    }

    func testCancellingSignInWhileCredentialsPendDisconnectsTheLateSession() async throws {
        try await cancelledCredentials(Self.store!)
    }
    private func cancelledCredentials<S: WebStoreFixture>(_: S.Type) async throws {
        let store = MemoryStore<WebAccountCredentials>(nil)
        let session = WebTestSession<S>(credentials: S.credentials("late"), games: [S.game("1", title: "Late")])
        await session.holdNextCredentials()
        let model = webModel(S.self, store, signIn: { _ in session })
        model.signIn()
        try await eventually { await session.waitingForCredentials }

        model.cancelSignIn()
        await session.completeCredentials(failing: false)
        try await eventually { await session.disconnections == 1 }
        XCTAssertNil(model.errorMessage)
        XCTAssertFalse(model.isConnected)
        XCTAssertNil(model.accountName)
        let writes = await store.writes
        XCTAssertTrue(writes.isEmpty, "A cancelled sign-in saves nothing")
    }

    func testCancellingSignInWhileCredentialsFailShowsNoError() async throws {
        try await cancelledFailingCredentials(Self.store!)
    }
    private func cancelledFailingCredentials<S: WebStoreFixture>(_: S.Type) async throws {
        let store = MemoryStore<WebAccountCredentials>(nil)
        let session = WebTestSession<S>(credentials: S.credentials("late"), games: [S.game("1", title: "Late")])
        await session.holdNextCredentials()
        let model = webModel(S.self, store, signIn: { _ in session })
        model.signIn()
        try await eventually { await session.waitingForCredentials }

        model.cancelSignIn()
        await session.completeCredentials(failing: true)
        try await eventually { await session.disconnections == 1 }
        // The failed sign-in's continuation runs on the main actor next.
        await drainMainQueue(times: 2)
        XCTAssertNil(model.errorMessage, "The failure belongs to the cancelled sign-in")
        XCTAssertFalse(model.isConnected)
        let writes = await store.writes
        XCTAssertTrue(writes.isEmpty)
    }

    func testAutomaticLoginExchangesBrowserCodeAndLoadsLibrary() async throws {
        try await automaticLogin(Self.store!)
    }
    private func automaticLogin<S: WebStoreFixture>(_: S.Type) async throws {
        let store = MemoryStore<WebAccountCredentials>(nil)
        let current = S.credentials("current")
        let session = WebTestSession<S>(credentials: current, games: [S.game("1", title: "One")])
        let login = SignInCounter(session: session)
        let model = webModel(S.self, store, authorize: { "browser-authorization-code" },
                             signIn: { await login.signIn(code: $0) })

        model.signIn()
        XCTAssertTrue(model.isConnecting)
        try await eventually { model.isConnected && !model.isLoadingGames }

        let codes = await login.codes
        let saved = await store.value
        XCTAssertEqual(codes, ["browser-authorization-code"])
        XCTAssertEqual(saved, current, "The account is remembered as it signs in")
        XCTAssertEqual(model.accountName, S.fixtureName)
        XCTAssertEqual(model.avatarURL, S.expectedAvatar)
        XCTAssertEqual(model.library.games.map(\.title), ["One"])
        XCTAssertNil(model.errorMessage)
    }

    func testAutomaticLoginReservesAuthenticationBeforeDuplicateClicks() async throws {
        try await duplicateClicks(Self.store!)
    }
    private func duplicateClicks<S: WebStoreFixture>(_: S.Type) async throws {
        let session = WebTestSession<S>(credentials: S.credentials("current"), games: [])
        let browser = AuthorizationGate()
        let login = SignInCounter(session: session)
        let model = webModel(S.self, MemoryStore(nil), authorize: { try await browser.wait() },
                             signIn: { await login.signIn(code: $0) })

        model.signIn()
        model.signIn()
        try await eventually { browser.calls == 1 }
        browser.complete(1, code: "one-code")
        try await eventually { model.isConnected && !model.isLoadingGames }

        let signIns = await login.calls
        XCTAssertEqual(browser.calls, 1)
        XCTAssertEqual(signIns, 1)
    }

    func testClosingAuthorizationWindowReturnsToLoginWithoutAnError() async throws {
        try await closedWindow(Self.store!)
    }
    private func closedWindow<S: WebStoreFixture>(_: S.Type) async throws {
        let session = WebTestSession<S>(credentials: S.credentials("unused"), games: [])
        let login = SignInCounter(session: session)
        let model = webModel(S.self, MemoryStore(nil), authorize: { throw CancellationError() },
                             signIn: { await login.signIn(code: $0) })

        model.signIn()
        try await eventually { !model.isConnecting }

        let signIns = await login.calls
        XCTAssertEqual(signIns, 0)
        XCTAssertFalse(model.isConnected)
        XCTAssertNil(model.errorMessage)
    }

    func testAuthorizationFailureShowsSafeBrowserErrorWithoutExchangingCode() async throws {
        try await authorizationFailure(Self.store!)
    }
    private func authorizationFailure<S: WebStoreFixture>(_: S.Type) async throws {
        let session = WebTestSession<S>(credentials: S.credentials("unused"), games: [])
        let login = SignInCounter(session: session)
        let model = webModel(S.self, MemoryStore(nil), authorize: { throw WebAuthenticationError.network(S.webSignIn.store) },
                             signIn: { await login.signIn(code: $0) })

        model.signIn()
        try await eventually { !model.isConnecting }

        let signIns = await login.calls
        XCTAssertEqual(signIns, 0)
        XCTAssertFalse(model.isConnected)
        XCTAssertEqual(model.errorMessage, WebAuthenticationError.network(S.webSignIn.store).localizedDescription)
    }

    func testCancelledAuthorizationCannotExchangeLateCodeOrReplaceNewLogin() async throws {
        try await cancelledAuthorization(Self.store!)
    }
    private func cancelledAuthorization<S: WebStoreFixture>(_: S.Type) async throws {
        let store = MemoryStore<WebAccountCredentials>(nil)
        let current = S.credentials("current")
        let session = WebTestSession<S>(credentials: current, games: [S.game("1", title: "One")])
        let browser = AuthorizationGate()
        let login = SignInCounter(session: session)
        let model = webModel(S.self, store, authorize: { try await browser.wait() }, signIn: { await login.signIn(code: $0) })

        model.signIn()
        try await eventually { browser.calls == 1 }
        model.cancelSignIn()
        model.signIn()
        try await eventually { browser.calls == 2 }
        browser.complete(1, code: "late-code")
        try await eventually { browser.returnedCalls == 1 }
        let codesWhileWaiting = await login.codes
        XCTAssertTrue(codesWhileWaiting.isEmpty)
        XCTAssertTrue(model.isConnecting)
        XCTAssertFalse(model.isConnected)

        browser.complete(2, code: "current-code")
        try await eventually { model.isConnected && !model.isLoadingGames }
        try await eventually { browser.returnedCalls == 2 }
        let codes = await login.codes
        let saved = await store.value
        XCTAssertEqual(codes, ["current-code"])
        XCTAssertEqual(saved, current)
        XCTAssertTrue(model.isConnected)
        XCTAssertFalse(model.isConnecting)
        XCTAssertNil(model.errorMessage)
    }

    func testRememberToggleChangesPersistenceWithoutDisconnecting() async throws {
        try await rememberToggle(Self.store!)
    }
    private func rememberToggle<S: WebStoreFixture>(_: S.Type) async throws {
        let original = S.credentials("original")
        let store = MemoryStore(original)
        let session = WebTestSession<S>(credentials: original, games: [S.game("1", title: "One")])
        let model = webModel(S.self, store, restore: { _ in session })
        await model.start()

        model.rememberSession = false
        try await eventually { await store.value == nil }
        XCTAssertTrue(model.isConnected)
        model.rememberSession = true
        try await eventually { await store.value == original }
        let disconnections = await session.disconnections
        XCTAssertEqual(disconnections, 0)
        XCTAssertEqual(model.library.games.map(\.title), ["One"])
    }

    func testUnrememberedLoginRemovesPreviousSavedToken() async throws {
        try await unrememberedLogin(Self.store!)
    }
    private func unrememberedLogin<S: WebStoreFixture>(_: S.Type) async throws {
        let store = MemoryStore(S.credentials("previous"))
        let session = WebTestSession<S>(credentials: S.credentials("current"), games: [S.game("1", title: "One")])
        let model = webModel(S.self, store, signIn: { _ in session })
        model.rememberSession = false
        model.signIn()
        try await eventually { model.isConnected && !model.isLoadingGames }
        let saved = await store.value
        XCTAssertNil(saved)
        XCTAssertTrue(model.isConnected)
    }

    func testSignOutDuringLoginBlocksAuthenticationUntilRemovalCompletes() async throws {
        try await signOutDuringLogin(Self.store!)
    }
    private func signOutDuringLogin<S: WebStoreFixture>(_: S.Type) async throws {
        let store = MemoryStore<WebAccountCredentials>(nil)
        await store.holdNextSave()
        let session = WebTestSession<S>(credentials: S.credentials("current"), games: [S.game("1", title: "One")])
        let model = webModel(S.self, store, signIn: { _ in session })
        model.signIn()
        try await eventually { await store.waitingForSave }

        model.signOut()
        model.cancelSignIn()
        model.signIn()
        XCTAssertTrue(model.isSigningOut)
        XCTAssertFalse(model.isConnecting)
        XCTAssertFalse(model.isConnected)
        await store.completeSave()
        try await eventually { !model.isSigningOut }
        XCTAssertTrue(model.library.games.isEmpty)
    }

    func testStorageFailureLeavesConnectedSessionUsableWithWarning() async throws {
        try await storageFailure(Self.store!)
    }
    private func storageFailure<S: WebStoreFixture>(_: S.Type) async throws {
        let store = MemoryStore<WebAccountCredentials>(nil)
        await store.rejectSaves()
        let session = WebTestSession<S>(credentials: S.credentials("current"), games: [S.game("1", title: "One")])
        let model = webModel(S.self, store, signIn: { _ in session })
        model.signIn()
        try await eventually { model.isConnected && !model.isLoadingGames }
        XCTAssertNotNil(model.storageWarning)
        XCTAssertEqual(model.library.games.map(\.title), ["One"])
    }

    func testSignOutWarnsWhenRevocationIsUnconfirmed() async throws {
        try await unconfirmedRevocation(Self.store!)
    }
    private func unconfirmedRevocation<S: WebStoreFixture>(_: S.Type) async throws {
        let session = WebTestSession<S>(credentials: S.credentials("current"), games: [S.game("1", title: "One")])
        let model = webModel(S.self, MemoryStore(nil), signIn: { _ in session })
        model.signIn()
        try await eventually { model.isConnected && !model.isLoadingGames }
        await session.failNextDisconnect()

        model.signOut()
        try await eventually { model.storageWarning == S.revocationMessage }
        let disconnections = await session.disconnections
        XCTAssertEqual(disconnections, 1)
        XCTAssertFalse(model.isSigningOut)
    }

    func testALateRevocationFailureDoesNotWarnTheNextSignIn() async throws {
        try await lateRevocationFailure(Self.store!)
    }
    private func lateRevocationFailure<S: WebStoreFixture>(_: S.Type) async throws {
        let stored = S.credentials("current")
        let session = WebTestSession<S>(credentials: stored, games: [S.game("1", title: "One")])
        let browser = AuthorizationGate()
        let model = webModel(S.self, MemoryStore(stored), authorize: { try await browser.wait() },
                             restore: { _ in session })
        await model.start()
        XCTAssertTrue(model.isConnected)
        await session.holdNextDisconnect()

        model.signOut()
        try await eventually { await session.waitingForDisconnect }
        XCTAssertFalse(model.isSigningOut)
        model.signIn()
        try await eventually { browser.calls == 1 }
        await session.completeDisconnect(failing: true)
        // The failed revocation's continuation runs on the main actor next.
        await drainMainQueue(times: 2)
        XCTAssertNil(model.storageWarning, "A sign-in that started since owns the warning")
        XCTAssertTrue(model.isConnecting)

        model.cancelSignIn()
        browser.complete(1, code: "late-code")
        try await eventually { browser.returnedCalls == 1 }
        XCTAssertFalse(model.isConnecting)
    }

    // MARK: Library loading

    func testPlaytimesAreAskedOnceTheGamesLoaded() async throws {
        try await playtimesAfterGames(Self.store!)
    }
    private func playtimesAfterGames<S: WebStoreFixture>(_: S.Type) async throws {
        let games = [S.game("1", title: "One")]
        let session = WebTestSession<S>(credentials: S.credentials("current"), games: games,
                                        playtimes: [games[0].id: 300])
        await session.holdNextGames()
        let model = webModel(S.self, MemoryStore(nil), signIn: { _ in session })
        model.signIn()
        defer { model.signOut() }
        try await eventually { await session.waitingForGames }
        let callsWhileLoading = await session.playtimeCalls
        XCTAssertEqual(callsWhileLoading, 0, "The playtimes wait for the games")
        model.library.sort = .playtime
        XCTAssertNotNil(model.library.pendingMetadata, "They show as loading before the games answer")

        await session.completeGames(games)
        try await eventually { await session.playtimeCalls == 1 && model.library.pendingMetadata == nil }
        XCTAssertEqual(model.library.playtime(of: games[0]), .seconds(300))
    }

    // MARK: Game page

    func testOpeningGameRequestsItsDetailsAndSavesPreparedTokenWhileResponsePends() async throws {
        try await openingDetails(Self.store!)
    }
    private func openingDetails<S: WebStoreFixture>(_: S.Type) async throws {
        let original = S.credentials("original")
        let prepared = S.credentials("prepared")
        let store = MemoryStore(original)
        let owned = S.game("1", title: "One")
        let session = WebTestSession<S>(credentials: original, games: [owned])
        let model = webModel(S.self, store, restore: { _ in session })
        await model.start()
        await session.rotateOnPrepare(prepared)
        await session.holdNextDetails()

        model.open(owned)
        try await eventually { await session.detailsCalls.count == 1 }
        try await eventually { await store.value == prepared }
        XCTAssertTrue(model.isLoadingGameDetails)
        XCTAssertNil(model.selectedGameDetails)

        let details = S.details(bytes: 2_000_000)
        await session.completeDetails(1, with: details)
        try await eventually { !model.isLoadingGameDetails }
        XCTAssertEqual(model.selectedGameDetails, details)
    }

    func testLateDetailsCannotReplaceAnotherGamesDetails() async throws {
        try await lateDetails(Self.store!)
    }
    private func lateDetails<S: WebStoreFixture>(_: S.Type) async throws {
        let first = S.game("1", title: "One")
        let second = S.game("2", title: "Two")
        let session = WebTestSession<S>(credentials: S.credentials("current"), games: [first, second])
        let model = webModel(S.self, MemoryStore(S.credentials("current")), restore: { _ in session })
        await model.start()
        await session.holdNextDetails()
        model.open(first)
        try await eventually { await session.detailsCalls.count == 1 }

        model.open(second)
        try await eventually { !model.isLoadingGameDetails }
        let secondDetails = model.selectedGameDetails
        await session.completeDetails(1, with: S.details(bytes: 999))
        try await eventually { await session.returnedDetails == 2 }
        await Task.yield()

        XCTAssertEqual(model.selectedGame?.id, second.id)
        XCTAssertEqual(model.selectedGameDetails, secondDetails)
        XCTAssertFalse(model.isLoadingGameDetails)
    }

    func testReopeningSameGameDiscardsItsPreviousDetailsRequest() async throws {
        try await reopenedDetails(Self.store!)
    }
    private func reopenedDetails<S: WebStoreFixture>(_: S.Type) async throws {
        let owned = S.game("1", title: "One")
        let session = WebTestSession<S>(credentials: S.credentials("current"), games: [owned])
        let model = webModel(S.self, MemoryStore(S.credentials("current")), restore: { _ in session })
        await model.start()
        await session.holdNextDetails()
        model.open(owned)
        try await eventually { await session.detailsCalls.count == 1 }
        model.closeGame()
        await session.holdNextDetails()
        model.open(owned)
        try await eventually { await session.detailsCalls.count == 2 }

        await session.completeDetails(1, with: S.details(bytes: 999))
        try await eventually { await session.returnedDetails == 1 }
        await Task.yield()
        XCTAssertNil(model.selectedGameDetails)
        XCTAssertTrue(model.isLoadingGameDetails)

        let current = S.details(bytes: nil)
        await session.completeDetails(2, with: current)
        try await eventually { !model.isLoadingGameDetails }
        XCTAssertEqual(model.selectedGameDetails, current)
    }

    func testSigningOutDiscardsLateDetailsAndClearsSelection() async throws {
        try await signOutDetails(Self.store!)
    }
    private func signOutDetails<S: WebStoreFixture>(_: S.Type) async throws {
        let owned = S.game("1", title: "One")
        let store = MemoryStore(S.credentials("current"))
        let session = WebTestSession<S>(credentials: S.credentials("current"), games: [owned])
        let model = webModel(S.self, store, restore: { _ in session })
        await model.start()
        await session.holdNextDetails()
        model.open(owned)
        try await eventually { await session.detailsCalls.count == 1 }

        model.signOut()
        await session.completeDetails(1, with: S.details(bytes: 999))
        try await eventually {
            let returned = await session.returnedDetails
            return returned == 1 && !model.isSigningOut
        }
        await Task.yield()
        XCTAssertNil(model.selectedGame)
        XCTAssertNil(model.selectedGameDetails)
        XCTAssertFalse(model.isLoadingGameDetails)
        XCTAssertFalse(model.isConnected)
        let saved = await store.value
        XCTAssertNil(saved)
    }

    func testClosingGameStillSavesTokenRotatedByItsLateDetailsResponse() async throws {
        try await closedGameRotation(Self.store!)
    }
    private func closedGameRotation<S: WebStoreFixture>(_: S.Type) async throws {
        let owned = S.game("1", title: "One")
        let store = MemoryStore(S.credentials("current"))
        let session = WebTestSession<S>(credentials: S.credentials("current"), games: [owned])
        let model = webModel(S.self, store, restore: { _ in session })
        await model.start()
        await session.holdNextDetails()
        model.open(owned)
        try await eventually { await session.detailsCalls.count == 1 }

        model.closeGame()
        let rotation = S.credentials("late-rotation")
        await session.completeDetails(1, with: S.details(bytes: 999), rotated: rotation)
        try await eventually { await store.value == rotation }
        XCTAssertNil(model.selectedGame)
        XCTAssertNil(model.selectedGameDetails)
        XCTAssertFalse(model.isLoadingGameDetails)
        XCTAssertTrue(model.isConnected)
    }

    func testDetailsFailureIsNonblockingAndCanRetryByReopeningOrRefreshing() async throws {
        try await detailsFailure(Self.store!)
    }
    private func detailsFailure<S: WebStoreFixture>(_: S.Type) async throws {
        let owned = S.game("1", title: "One")
        let rotation = S.credentials("rotated-details")
        let store = MemoryStore(S.credentials("current"))
        let session = WebTestSession<S>(credentials: S.credentials("current"), games: [owned])
        let model = webModel(S.self, store, restore: { _ in session })
        await model.start()
        await session.failNextDetails(rotated: rotation)
        model.open(owned)
        try await eventually { !model.isLoadingGameDetails }
        XCTAssertNil(model.selectedGameDetails)
        XCTAssertNil(model.errorMessage)
        XCTAssertTrue(model.isConnected)
        XCTAssertEqual(model.library.games, [owned])
        try await eventually { await store.value == rotation }

        model.open(owned)
        try await eventually { !model.isLoadingGameDetails }
        XCTAssertNotNil(model.selectedGameDetails)
        model.refresh()
        try await eventually { !model.isLoadingGames && !model.isLoadingGameDetails }
        let calls = await session.detailsCalls
        XCTAssertEqual(calls.count, 3)
    }

    /// Each store's tests check how its request names the player's language.
    func testOpeningAGameLoadsItsAchievements() async throws {
        try await achievements(Self.store!)
    }
    private func achievements<S: WebStoreFixture>(_: S.Type) async throws {
        let session = WebTestSession<S>(credentials: S.credentials("current"), games: [S.game("1", title: "One")])
        let model = webModel(S.self, MemoryStore(nil), signIn: { _ in session })
        model.signIn()
        defer { model.signOut() }
        try await eventually { model.isConnected && !model.isLoadingGames }
        model.open(model.library.games[0])
        try await eventually { model.achievements.items != nil }
        XCTAssertEqual(model.achievements.countText, "1/2")
        model.closeGame()
        XCTAssertNil(model.achievements.items)
    }
}

final class EpicWebAccountTests: WebAccountModelTests {
    nonisolated override class var store: (any WebStoreFixture.Type)? { EpicGames.self }
}

final class GogWebAccountTests: WebAccountModelTests {
    nonisolated override class var store: (any WebStoreFixture.Type)? { GogGames.self }
}
