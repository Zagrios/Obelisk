import Foundation
import Observation
import XCTest
@testable import Forge

@MainActor
final class EpicModelTests: XCTestCase {
    func testLibrarySearchIgnoresAccentsAndSortsNamesWithStableIdentifiers() async throws {
        let games = [game("z", title: "Portal"), game("b", title: "Étoiles"), game("a", title: "Portal")]
        let root = FileManager.default.temporaryDirectory.appending(path: "ForgeEpicSearch-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let stored = credentials("current")
        let session = EpicTestSession(credentials: stored, games: games)
        let model = EpicModel(directory: root, keychain: EpicMemoryStore(stored), restore: { _ in session })
        await model.start()
        defer { model.signOut() }

        XCTAssertEqual(model.filteredGames.map(\.appName), ["b", "a", "z"])
        model.searchText = "  etoile  "
        XCTAssertEqual(model.filteredGames.map(\.appName), ["b"])
        model.searchText = "absent"
        XCTAssertTrue(model.filteredGames.isEmpty)
    }

    func testRestoreRunsOnceAndSavesRotatedRefreshToken() async {
        let original = credentials("original")
        let rotated = credentials("rotated")
        let store = EpicMemoryStore(original)
        let session = EpicTestSession(credentials: original, games: [game("one")], rotated: rotated)
        let model = EpicModel(keychain: store, restore: { _ in session })

        await model.start()
        await model.start()

        XCTAssertTrue(model.isConnected)
        XCTAssertFalse(model.isRestoring)
        XCTAssertEqual(model.accountName, "Fixture")
        XCTAssertEqual(model.games.map(\.appName), ["one"])
        let saved = await store.value
        let loads = await store.loads
        XCTAssertEqual(saved?.refreshToken, "rotated")
        XCTAssertEqual(loads, 1)
    }

    func testConnectionStatusNotifiesObserversWhenRestoringSession() async {
        let store = EpicMemoryStore(credentials("saved"))
        let session = EpicTestSession(credentials: credentials("restored"), games: [game("one")])
        let model = EpicModel(keychain: store, restore: { _ in session })
        let changed = expectation(description: "The disconnected view observes the restored session")
        withObservationTracking {
            XCTAssertFalse(model.isConnected)
        } onChange: {
            changed.fulfill()
        }

        await model.start()

        XCTAssertTrue(model.isConnected)
        XCTAssertEqual(model.games.count, 1)
        await fulfillment(of: [changed], timeout: 0.2)
    }

    func testConnectionStatusNotifiesObserversAfterSignIn() async {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: credentials("signed-in"), games: [game("one")])
        let model = EpicModel(keychain: store, authorize: { "fixture-authorization" }, signIn: { _ in session })
        await model.start()
        let changed = expectation(description: "The login view observes the new session")
        withObservationTracking {
            XCTAssertFalse(model.isConnected)
        } onChange: {
            changed.fulfill()
        }

        model.signIn()
        await eventually { model.isConnected && !model.isLoadingGames }

        XCTAssertEqual(model.games.count, 1)
        await fulfillment(of: [changed], timeout: 0.2)
    }

    func testTransientRestoreFailureKeepsStoredSessionAndCanRetry() async {
        let original = credentials("original")
        let store = EpicMemoryStore(original)
        let session = EpicTestSession(credentials: original, games: [game("one")])
        let attempt = EpicRestoreAttempt(session: session)
        let model = EpicModel(keychain: store, restore: { _ in try await attempt.restore() })

        await model.start()
        XCTAssertFalse(model.isConnected)
        XCTAssertTrue(model.canRetryRestore)
        let retained = await store.value
        XCTAssertEqual(retained, original)
        model.retryRestore()
        await eventually { model.isConnected && !model.isLoadingGames }
        XCTAssertEqual(model.games.map(\.appName), ["one"])
        XCTAssertNil(model.errorMessage)
    }

    func testRetryRestoreReservesAuthenticationBeforeAnotherSignIn() async {
        let original = credentials("original")
        let store = EpicMemoryStore(original)
        let session = EpicTestSession(credentials: original, games: [game("one")])
        let attempt = EpicRestoreAttempt(session: session)
        let login = EpicSignInCounter(session: session)
        let model = EpicModel(keychain: store, signIn: { _ in await login.signIn() }, restore: { _ in try await attempt.restore() })
        await model.start()

        model.retryRestore()
        model.signIn(input: "0123456789abcdef0123456789abcdef")
        await eventually { model.isConnected && !model.isLoadingGames }
        let signIns = await login.calls
        XCTAssertEqual(signIns, 0)
    }

    func testExpiredRestoreRemovesSavedSession() async {
        let store = EpicMemoryStore(credentials("expired"))
        let model = EpicModel(keychain: store, restore: { _ in throw EpicError.Unauthorized })
        await model.start()
        let saved = await store.value
        XCTAssertNil(saved)
        XCTAssertFalse(model.isConnected)
        XCTAssertFalse(model.canRetryRestore)
        XCTAssertNotNil(model.errorMessage)
    }

    func testFailedLibraryRequestKeepsGamesAndSavesRotatedTokenThenRetries() async {
        let original = credentials("original")
        let store = EpicMemoryStore(original)
        let session = EpicTestSession(credentials: original, games: [game("one")])
        let model = EpicModel(keychain: store, restore: { _ in session })
        await model.start()
        await session.failNextGames(rotated: credentials("rotated"))

        model.refresh()
        await eventually { !model.isLoadingGames }
        XCTAssertEqual(model.games.map(\.appName), ["one"])
        XCTAssertNotNil(model.errorMessage)
        let saved = await store.value
        XCTAssertEqual(saved?.refreshToken, "rotated")

        model.refresh()
        await eventually { !model.isLoadingGames }
        XCTAssertNil(model.errorMessage)
        XCTAssertTrue(model.isConnected)
    }

    func testRefreshIsSerializedAndSignOutDiscardsLateLibraryResponse() async {
        let original = credentials("original")
        let store = EpicMemoryStore(original)
        let session = EpicTestSession(credentials: original, games: [game("one")])
        let model = EpicModel(keychain: store, restore: { _ in session })
        await model.start()
        await session.holdNextGames()
        model.refresh()
        model.refresh()
        await eventually { await session.waitingForGames }
        let calls = await session.gameCalls
        XCTAssertEqual(calls, 2)

        model.signOut()
        await session.completeGames([game("late")])
        await eventually {
            let disconnections = await session.disconnections
            return !model.isSigningOut && disconnections == 1
        }
        XCTAssertFalse(model.isConnected)
        XCTAssertTrue(model.games.isEmpty)
        XCTAssertFalse(model.isLoadingGames)
        let saved = await store.value
        XCTAssertNil(saved)
    }

    func testExpiredLibrarySessionClearsAccountSelectionAndSavedToken() async {
        let original = credentials("original")
        let store = EpicMemoryStore(original)
        let owned = game("one")
        let session = EpicTestSession(credentials: original, games: [owned])
        let model = EpicModel(keychain: store, restore: { _ in session })
        await model.start()
        model.open(owned)
        model.searchText = "one"
        await session.expireNextGames()

        model.refresh()
        await eventually { !model.isConnected && !model.isSigningOut }
        XCTAssertTrue(model.games.isEmpty)
        XCTAssertNil(model.selectedGame)
        XCTAssertEqual(model.searchText, "")
        XCTAssertNotNil(model.errorMessage)
        let saved = await store.value
        XCTAssertNil(saved)
    }

    func testCancelledLoginDisposesLateSessionWithoutRestoringAccount() async {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: credentials("late"), games: [game("late")])
        let login = EpicLoginGate()
        let model = EpicModel(keychain: store, signIn: { _ in await login.wait() })
        model.signIn(input: "0123456789abcdef0123456789abcdef")
        await eventually { await login.isWaiting }
        model.cancelSignIn()
        await login.complete(session)
        await eventually { await session.disconnections == 1 }
        XCTAssertFalse(model.isConnected)
        XCTAssertFalse(model.isConnecting)
        XCTAssertTrue(model.games.isEmpty)
        let saved = await store.value
        XCTAssertNil(saved)
    }

    func testAutomaticLoginExchangesBrowserCodeAndLoadsLibrary() async {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: credentials("current"), games: [game("one")])
        let login = EpicSignInCounter(session: session)
        let model = EpicModel(
            keychain: store,
            authorize: { "browser-authorization-code" },
            signIn: { await login.signIn(code: $0) }
        )

        model.signIn()
        XCTAssertTrue(model.isConnecting)
        await eventually { model.isConnected && !model.isLoadingGames }

        let codes = await login.codes
        let saved = await store.value
        XCTAssertEqual(codes, ["browser-authorization-code"])
        XCTAssertEqual(saved?.refreshToken, "current")
        XCTAssertEqual(model.accountName, "Fixture")
        XCTAssertEqual(model.games.map(\.appName), ["one"])
        XCTAssertNil(model.errorMessage)
    }

    func testAutomaticLoginReservesAuthenticationBeforeDuplicateClicks() async {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: credentials("current"), games: [])
        let browser = EpicAuthorizationGate()
        let login = EpicSignInCounter(session: session)
        let model = EpicModel(
            keychain: store,
            authorize: { try await browser.wait() },
            signIn: { await login.signIn(code: $0) }
        )

        model.signIn()
        model.signIn()
        await eventually { browser.calls == 1 }
        browser.complete(1, code: "one-code")
        await eventually { model.isConnected && !model.isLoadingGames }

        let signIns = await login.calls
        XCTAssertEqual(browser.calls, 1)
        XCTAssertEqual(signIns, 1)
    }

    func testClosingAuthorizationWindowReturnsToLoginWithoutAnError() async {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: credentials("unused"), games: [])
        let login = EpicSignInCounter(session: session)
        let model = EpicModel(
            keychain: store,
            authorize: { throw CancellationError() },
            signIn: { await login.signIn(code: $0) }
        )

        model.signIn()
        await eventually { !model.isConnecting }

        let signIns = await login.calls
        XCTAssertEqual(signIns, 0)
        XCTAssertFalse(model.isConnected)
        XCTAssertNil(model.errorMessage)
    }

    func testAuthorizationFailureShowsSafeBrowserErrorWithoutExchangingCode() async {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: credentials("unused"), games: [])
        let login = EpicSignInCounter(session: session)
        let model = EpicModel(
            keychain: store,
            authorize: { throw EpicWebAuthenticationError.network },
            signIn: { await login.signIn(code: $0) }
        )

        model.signIn()
        await eventually { !model.isConnecting }

        let signIns = await login.calls
        XCTAssertEqual(signIns, 0)
        XCTAssertFalse(model.isConnected)
        XCTAssertEqual(model.errorMessage, EpicWebAuthenticationError.network.localizedDescription)
    }

    func testCancelledAuthorizationCannotExchangeLateCodeOrReplaceNewLogin() async {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: credentials("current"), games: [game("one")])
        let browser = EpicAuthorizationGate()
        let login = EpicSignInCounter(session: session)
        let model = EpicModel(
            keychain: store,
            authorize: { try await browser.wait() },
            signIn: { await login.signIn(code: $0) }
        )

        model.signIn()
        await eventually { browser.calls == 1 }
        model.cancelSignIn()
        model.signIn()
        await eventually { browser.calls == 2 }
        browser.complete(1, code: "late-code")
        await eventually { browser.returnedCalls == 1 }
        let codesWhileWaiting = await login.codes
        XCTAssertTrue(codesWhileWaiting.isEmpty)
        XCTAssertTrue(model.isConnecting)
        XCTAssertFalse(model.isConnected)

        browser.complete(2, code: "current-code")
        await eventually { model.isConnected && !model.isLoadingGames }
        await eventually { browser.returnedCalls == 2 }
        let codes = await login.codes
        let saved = await store.value
        XCTAssertEqual(codes, ["current-code"])
        XCTAssertEqual(saved?.refreshToken, "current")
        XCTAssertTrue(model.isConnected)
        XCTAssertFalse(model.isConnecting)
        XCTAssertNil(model.errorMessage)
    }

    func testRememberToggleChangesPersistenceWithoutDisconnecting() async {
        let original = credentials("original")
        let store = EpicMemoryStore(original)
        let session = EpicTestSession(credentials: original, games: [game("one")])
        let model = EpicModel(keychain: store, restore: { _ in session })
        await model.start()

        model.rememberSession = false
        await eventually { await store.value == nil }
        XCTAssertTrue(model.isConnected)
        model.rememberSession = true
        await eventually { await store.value == original }
        let disconnections = await session.disconnections
        XCTAssertEqual(disconnections, 0)
        XCTAssertEqual(model.games.map(\.appName), ["one"])
    }

    func testUnrememberedLoginRemovesPreviousSavedToken() async {
        let store = EpicMemoryStore(credentials("previous"))
        let session = EpicTestSession(credentials: credentials("current"), games: [game("one")])
        let model = EpicModel(keychain: store, signIn: { _ in session })
        model.rememberSession = false
        model.signIn(input: "0123456789abcdef0123456789abcdef")
        await eventually { model.isConnected && !model.isLoadingGames }
        let saved = await store.value
        XCTAssertNil(saved)
        XCTAssertTrue(model.isConnected)
    }

    func testSignOutDuringLoginBlocksAuthenticationUntilRemovalCompletes() async {
        let store = EpicMemoryStore(nil)
        await store.holdNextSave()
        let session = EpicTestSession(credentials: credentials("current"), games: [game("one")])
        let model = EpicModel(keychain: store, signIn: { _ in session })
        model.signIn(input: "0123456789abcdef0123456789abcdef")
        await eventually { await store.waitingForSave }

        model.signOut()
        model.cancelSignIn()
        model.signIn(input: "0123456789abcdef0123456789abcdef")
        XCTAssertTrue(model.isSigningOut)
        XCTAssertFalse(model.isConnecting)
        XCTAssertFalse(model.isConnected)
        await store.completeSave()
        await eventually { !model.isSigningOut }
        XCTAssertTrue(model.games.isEmpty)
    }

    func testStorageFailureLeavesConnectedSessionUsableWithWarning() async {
        let store = EpicMemoryStore(nil)
        await store.rejectSaves()
        let session = EpicTestSession(credentials: credentials("current"), games: [game("one")])
        let model = EpicModel(keychain: store, signIn: { _ in session })
        model.signIn(input: "0123456789abcdef0123456789abcdef")
        await eventually { model.isConnected && !model.isLoadingGames }
        XCTAssertNotNil(model.storageWarning)
        XCTAssertEqual(model.games.map(\.appName), ["one"])
    }

    func testOpeningGameRequestsItsDetailsAndSavesPreparedTokenBeforeResponse() async {
        let original = credentials("original")
        let store = EpicMemoryStore(original)
        var owned = game("one")
        owned.macos = true
        let session = EpicTestSession(credentials: original, games: [owned])
        let model = EpicModel(keychain: store, restore: { _ in session })
        await model.start()
        await session.rotateOnPrepare(credentials("prepared"))
        await session.holdNextDetails()

        model.open(owned)
        await eventually { await session.detailsCalls.count == 1 }
        let request = await session.detailsCalls.first
        let saved = await store.value
        XCTAssertEqual(request, EpicDetailsRequest(namespace: "fixture", catalogItemId: "one", appName: "one", platform: "Mac"))
        XCTAssertEqual(saved?.refreshToken, "prepared")
        XCTAssertTrue(model.isLoadingGameDetails)
        XCTAssertNil(model.selectedGameDetails)

        let details = EpicGameDetails(playtimeSeconds: 3660, installationBytes: 2_000_000)
        await session.completeDetails(1, with: details)
        await eventually { !model.isLoadingGameDetails }
        XCTAssertEqual(model.selectedGameDetails, details)
    }

    func testDetailsUseWindowsForWindowsAndUnspecifiedPlatformGames() async {
        let windowsGame = game("windows")
        var unspecifiedGame = game("unspecified")
        unspecifiedGame.windows = false
        let session = EpicTestSession(credentials: credentials("current"), games: [windowsGame, unspecifiedGame])
        let model = EpicModel(keychain: EpicMemoryStore(credentials("current")), restore: { _ in session })
        await model.start()

        for owned in [windowsGame, unspecifiedGame] {
            model.open(owned)
            await eventually { !model.isLoadingGameDetails }
        }

        let requests = await session.detailsCalls
        XCTAssertEqual(requests.map(\.platform), ["Windows", "Windows"])
        XCTAssertEqual(requests.map(\.appName), ["windows", "unspecified"])
    }

    func testLateDetailsCannotReplaceAnotherGamesDetails() async {
        let first = game("one")
        let second = game("two")
        let session = EpicTestSession(credentials: credentials("current"), games: [first, second])
        let model = EpicModel(keychain: EpicMemoryStore(credentials("current")), restore: { _ in session })
        await model.start()
        await session.holdNextDetails()
        model.open(first)
        await eventually { await session.detailsCalls.count == 1 }

        model.open(second)
        await eventually { !model.isLoadingGameDetails }
        let secondDetails = model.selectedGameDetails
        await session.completeDetails(1, with: EpicGameDetails(playtimeSeconds: 999, installationBytes: 999))
        await eventually { await session.returnedDetails == 2 }
        await Task.yield()

        XCTAssertEqual(model.selectedGame?.libraryID, second.libraryID)
        XCTAssertEqual(model.selectedGameDetails, secondDetails)
        XCTAssertFalse(model.isLoadingGameDetails)
    }

    func testReopeningSameGameDiscardsItsPreviousDetailsRequest() async {
        let owned = game("one")
        let session = EpicTestSession(credentials: credentials("current"), games: [owned])
        let model = EpicModel(keychain: EpicMemoryStore(credentials("current")), restore: { _ in session })
        await model.start()
        await session.holdNextDetails()
        model.open(owned)
        await eventually { await session.detailsCalls.count == 1 }
        model.closeGame()
        await session.holdNextDetails()
        model.open(owned)
        await eventually { await session.detailsCalls.count == 2 }

        await session.completeDetails(1, with: EpicGameDetails(playtimeSeconds: 999, installationBytes: 999))
        await eventually { await session.returnedDetails == 1 }
        await Task.yield()
        XCTAssertNil(model.selectedGameDetails)
        XCTAssertTrue(model.isLoadingGameDetails)

        let current = EpicGameDetails(playtimeSeconds: 60, installationBytes: nil)
        await session.completeDetails(2, with: current)
        await eventually { !model.isLoadingGameDetails }
        XCTAssertEqual(model.selectedGameDetails, current)
    }

    func testSigningOutDiscardsLateDetailsAndClearsSelection() async {
        let owned = game("one")
        let store = EpicMemoryStore(credentials("current"))
        let session = EpicTestSession(credentials: credentials("current"), games: [owned])
        let model = EpicModel(keychain: store, restore: { _ in session })
        await model.start()
        await session.holdNextDetails()
        model.open(owned)
        await eventually { await session.detailsCalls.count == 1 }

        model.signOut()
        await session.completeDetails(1, with: EpicGameDetails(playtimeSeconds: 999, installationBytes: 999))
        await eventually {
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

    func testClosingGameWhileSavingPreparedTokenDoesNotRequestDetails() async {
        let owned = game("one")
        let store = EpicMemoryStore(credentials("current"))
        let session = EpicTestSession(credentials: credentials("current"), games: [owned])
        let model = EpicModel(keychain: store, restore: { _ in session })
        await model.start()
        await store.holdNextSave()
        model.open(owned)
        await eventually { await store.waitingForSave }

        model.closeGame()
        await store.completeSave()
        await eventually { !(await store.waitingForSave) }
        await Task.yield()
        let calls = await session.detailsCalls
        XCTAssertTrue(calls.isEmpty)
        XCTAssertNil(model.selectedGameDetails)
        XCTAssertFalse(model.isLoadingGameDetails)
    }

    func testClosingGameStillSavesTokenRotatedByItsLateDetailsResponse() async {
        let owned = game("one")
        let store = EpicMemoryStore(credentials("current"))
        let session = EpicTestSession(credentials: credentials("current"), games: [owned])
        let model = EpicModel(keychain: store, restore: { _ in session })
        await model.start()
        await session.holdNextDetails()
        model.open(owned)
        await eventually { await session.detailsCalls.count == 1 }

        model.closeGame()
        await session.completeDetails(
            1,
            with: EpicGameDetails(playtimeSeconds: 999, installationBytes: 999),
            rotated: credentials("late-rotation")
        )
        await eventually { await store.value?.refreshToken == "late-rotation" }
        XCTAssertNil(model.selectedGame)
        XCTAssertNil(model.selectedGameDetails)
        XCTAssertFalse(model.isLoadingGameDetails)
        XCTAssertTrue(model.isConnected)
    }

    func testDetailsFailureIsNonblockingAndCanRetryByReopeningOrRefreshing() async {
        let owned = game("one")
        let store = EpicMemoryStore(credentials("current"))
        let session = EpicTestSession(credentials: credentials("current"), games: [owned])
        let model = EpicModel(keychain: store, restore: { _ in session })
        await model.start()
        await session.failNextDetails(rotated: credentials("rotated-details"))
        model.open(owned)
        await eventually { !model.isLoadingGameDetails }
        XCTAssertNil(model.selectedGameDetails)
        XCTAssertNil(model.errorMessage)
        XCTAssertTrue(model.isConnected)
        XCTAssertEqual(model.games, [owned])
        let saved = await store.value
        XCTAssertEqual(saved?.refreshToken, "rotated-details")

        model.open(owned)
        await eventually { !model.isLoadingGameDetails }
        XCTAssertNotNil(model.selectedGameDetails)
        model.refresh()
        await eventually { !model.isLoadingGames && !model.isLoadingGameDetails }
        let calls = await session.detailsCalls
        XCTAssertEqual(calls.count, 3)
    }

    private func credentials(_ token: String) -> EpicCredentials {
        EpicCredentials(accountId: "fixture-id", displayName: "Fixture", refreshToken: token)
    }

    private func game(_ appName: String, title: String? = nil) -> EpicGame {
        EpicGame(appName: appName, namespace: "fixture", catalogItemId: appName, title: title ?? appName, description: "", imageUrl: nil, headerImageUrl: nil, developer: "", windows: true, macos: false)
    }

    private func eventually(
        file: StaticString = #filePath,
        line: UInt = #line,
        _ predicate: @MainActor () async -> Bool
    ) async {
        for _ in 0..<200 {
            if await predicate() { return }
            try? await Task.sleep(for: .milliseconds(5))
        }
        XCTFail("Timed out waiting for the expected Epic state", file: file, line: line)
    }
}

actor EpicMemoryStore: EpicCredentialStore {
    private(set) var value: EpicCredentials?
    private(set) var loads = 0
    private(set) var writes: [EpicCredentials?] = []
    private(set) var waitingForSave = false
    private var shouldHoldSave = false
    private var shouldRejectSaves = false
    private var saveContinuation: CheckedContinuation<Void, Never>?

    init(_ value: EpicCredentials?) { self.value = value }
    func load() async throws -> EpicCredentials? { loads += 1; return value }
    func save(_ credentials: EpicCredentials) async throws {
        if shouldRejectSaves { throw EpicError.Network }
        if shouldHoldSave {
            shouldHoldSave = false
            waitingForSave = true
            await withCheckedContinuation { saveContinuation = $0 }
            waitingForSave = false
        }
        value = credentials
        writes.append(credentials)
    }
    func remove() async throws { value = nil; writes.append(nil) }
    func holdNextSave() { shouldHoldSave = true }
    func completeSave() { saveContinuation?.resume(); saveContinuation = nil }
    func rejectSaves(_ reject: Bool = true) { shouldRejectSaves = reject }
}

actor EpicTestSession: EpicSessionProtocol {
    func startFriends() async {}
    func reconnectFriends() async {}
    nonisolated func stopFriends() {}
    nonisolated func shutdownObservers() { credentialRevisions.stop() }
    nonisolated func friends() -> EpicFriendsSnapshot {
        EpicFriendsSnapshot(accountId: "unused", revision: 1, connection: .stopped, friends: [], message: nil)
    }
    func nextFriends(afterRevision: UInt64) async -> EpicFriendsSnapshot { friends() }
    nonisolated let credentialRevisions = EpicCredentialRevisionFeed()
    func nextCredentialsRevision(afterRevision: UInt64) async -> UInt64? {
        await credentialRevisions.next(after: afterRevision)
    }

    func launchPlan(namespace: String, catalogItemId: String, appName: String, platform: String, directory: String, language: String) async throws -> EpicLaunchPlan {
        throw EpicError.InvalidResponse
    }
    nonisolated func cancelDownload() {}
    nonisolated func downloadProgress() -> GameTransfer? { nil }
    func download(namespace: String, catalogItemId: String, appName: String, platform: String, directory: String) async throws -> EpicInstallation {
        throw EpicError.InvalidResponse
    }
    private var currentCredentials: EpicCredentials {
        didSet { credentialRevisions.publish() }
    }
    private let ownedGames: [EpicGame]
    private var rotated: EpicCredentials?
    private var prepared: EpicCredentials?
    private var failNext = false
    private var expireNext = false
    private var holdNext = false
    private var gameContinuation: CheckedContinuation<[EpicGame], Never>?
    private(set) var waitingForGames = false
    private(set) var disconnections = 0
    private(set) var gameCalls = 0
    private var shouldHoldDetails = false
    private var detailsFailureCredentials: EpicCredentials?
    private var detailsContinuations: [Int: CheckedContinuation<EpicGameDetails, Never>] = [:]
    private(set) var detailsCalls: [EpicDetailsRequest] = []
    private(set) var returnedDetails = 0

    init(credentials: EpicCredentials, games: [EpicGame], rotated: EpicCredentials? = nil) {
        currentCredentials = credentials
        ownedGames = games
        self.rotated = rotated
    }

    func rotate(_ credentials: EpicCredentials) { currentCredentials = credentials }
    func credentials() async throws -> EpicCredentials {
        try Task.checkCancellation()
        return currentCredentials
    }
    func prepare() async throws -> EpicCredentials {
        if let prepared { currentCredentials = prepared; self.prepared = nil }
        return currentCredentials
    }
    func games() async throws -> [EpicGame] {
        gameCalls += 1
        if let rotated { currentCredentials = rotated; self.rotated = nil }
        if expireNext { expireNext = false; throw EpicError.Unauthorized }
        if failNext { failNext = false; throw EpicError.Network }
        if holdNext {
            holdNext = false
            waitingForGames = true
            let result = await withCheckedContinuation { gameContinuation = $0 }
            waitingForGames = false
            return result
        }
        return ownedGames
    }
    func gameDetails(namespace: String, catalogItemId: String, appName: String, platform: String) async throws -> EpicGameDetails {
        detailsCalls.append(EpicDetailsRequest(namespace: namespace, catalogItemId: catalogItemId, appName: appName, platform: platform))
        defer { returnedDetails += 1 }
        if let rotated = detailsFailureCredentials {
            currentCredentials = rotated
            detailsFailureCredentials = nil
            throw EpicError.Network
        }
        if shouldHoldDetails {
            shouldHoldDetails = false
            let invocation = detailsCalls.count
            return await withCheckedContinuation { detailsContinuations[invocation] = $0 }
        }
        return EpicGameDetails(playtimeSeconds: nil, installationBytes: nil)
    }
    func holdNextDetails() { shouldHoldDetails = true }
    func completeDetails(_ invocation: Int, with details: EpicGameDetails, rotated: EpicCredentials? = nil) {
        if let rotated { currentCredentials = rotated }
        detailsContinuations.removeValue(forKey: invocation)?.resume(returning: details)
    }
    func failNextDetails(rotated: EpicCredentials) { detailsFailureCredentials = rotated }
    func disconnect() async throws { disconnections += 1 }
    func failNextGames(rotated: EpicCredentials) { failNext = true; self.rotated = rotated }
    func rotateOnPrepare(_ credentials: EpicCredentials) { prepared = credentials }
    func expireNextGames() { expireNext = true }
    func holdNextGames() { holdNext = true }
    func completeGames(_ games: [EpicGame]) { gameContinuation?.resume(returning: games); gameContinuation = nil }
}

struct EpicDetailsRequest: Equatable, Sendable {
    let namespace: String
    let catalogItemId: String
    let appName: String
    let platform: String
}

private actor EpicRestoreAttempt {
    let session: EpicTestSession
    private var attempts = 0
    init(session: EpicTestSession) { self.session = session }
    func restore() async throws -> any EpicSessionProtocol {
        attempts += 1
        if attempts == 1 { throw EpicError.Network }
        return session
    }
}

private actor EpicLoginGate {
    private var continuation: CheckedContinuation<any EpicSessionProtocol, Never>?
    private(set) var isWaiting = false
    func wait() async -> any EpicSessionProtocol {
        isWaiting = true
        return await withCheckedContinuation { continuation = $0 }
    }
    func complete(_ session: any EpicSessionProtocol) {
        continuation?.resume(returning: session)
        continuation = nil
    }
}

private actor EpicSignInCounter {
    let session: EpicTestSession
    private(set) var calls = 0
    private(set) var codes: [String] = []
    init(session: EpicTestSession) { self.session = session }
    func signIn(code: String? = nil) -> any EpicSessionProtocol {
        calls += 1
        if let code { codes.append(code) }
        return session
    }
}

@MainActor
private final class EpicAuthorizationGate {
    private var continuations: [Int: CheckedContinuation<String, Error>] = [:]
    private(set) var calls = 0
    private(set) var returnedCalls = 0

    func wait() async throws -> String {
        calls += 1
        let invocation = calls
        let code = try await withCheckedThrowingContinuation { continuations[invocation] = $0 }
        returnedCalls += 1
        return code
    }

    func complete(_ invocation: Int, code: String) {
        continuations.removeValue(forKey: invocation)?.resume(returning: code)
    }
}
