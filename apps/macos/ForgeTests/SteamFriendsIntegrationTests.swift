import AppKit
import Foundation
import Observation
import XCTest
@testable import Forge

final class SteamFriendsIntegrationTests: XCTestCase {
    @MainActor
    func testWireEventsCrossRustBridgeAndUpdateClosedPanelWithoutPolling() async throws {
        let session = FriendsBridgeTestSession()
        let model = friendsTestModel(session: session)
        await model.start()
        defer { model.signOut() }
        await session.settle()
        XCTAssertEqual(model.friends?.connection, .loading)
        XCTAssertFalse(model.isFriendsPresented)

        // Independent hand-authored protobuf fixtures, never project encoder output.
        session.fixture.receive(kind: 766, body: FriendsWireFixture.initialPersona)
        session.fixture.receive(kind: 767, body: FriendsWireFixture.fullList)
        await session.settle()
        XCTAssertEqual(model.friends?.connection, .connected)
        XCTAssertEqual(model.friends?.friends.first?.displayName, "Alice")
        XCTAssertEqual(model.friends?.onlineCount, 1)
        XCTAssertNil(model.friends?.friends.first?.avatarURL)

        let app = AppModel(directory: FileManager.default.temporaryDirectory.appending(path: UUID().uuidString), steam: model)
        app.selection = .epic
        model.openFriends()
        model.closeFriends()
        session.fixture.receive(kind: 766, body: FriendsWireFixture.playing)
        await session.settle()
        XCTAssertEqual(model.friends?.friends.first?.activity?.name, "Portal 2")
        XCTAssertEqual(model.friends?.sections(matching: "alice").playing.count, 1)
        XCTAssertEqual(app.selection, .epic)
        model.openFriends()
        XCTAssertEqual(model.friends?.friends.first?.status.contains("Portal 2"), true)

        session.fixture.receive(kind: 766, body: FriendsWireFixture.closedGame)
        await session.settle()
        XCTAssertNil(model.friends?.friends.first?.activity)
        XCTAssertEqual(model.friends?.onlineCount, 1)
        session.fixture.receive(kind: 766, body: FriendsWireFixture.offline)
        await session.settle()
        XCTAssertEqual(model.friends?.onlineCount, 0)
        XCTAssertEqual(model.friends?.sections(matching: "ALICE").offline.count, 1)
        XCTAssertEqual(session.maximumSimultaneousWaits, 1)
        XCTAssertEqual(session.shutdownCount, 0)
    }

    @MainActor
    func testDisconnectKeepsStaleRowsAndReconnectReplacesWithFullEmptyList() async {
        let first = FriendsBridgeTestSession(hydrated: true)
        let connections = FriendsTestConnections(first: first)
        let model = friendsTestModel(connect: { try await connections.connect($0) })
        await model.start()
        defer { model.signOut() }
        await first.settle()
        model.openFriends()
        first.fixture.disconnect(authenticationRequired: false)
        await connections.waitForRequest(2)
        XCTAssertEqual(model.friends?.connection, .interrupted)
        XCTAssertEqual(model.friends?.friends.count, 1)
        XCTAssertEqual(model.friends?.friends.first?.presence, .online)
        XCTAssertTrue(model.isFriendsPresented)

        let second = FriendsBridgeTestSession()
        second.fixture.receive(kind: 767, body: Data([0x08, 0x00]))
        await connections.release(2, session: second)
        await second.settle()
        XCTAssertEqual(model.friends?.connection, .connected)
        XCTAssertEqual(model.friends?.friends.count, 0)
        XCTAssertEqual(first.shutdownCount, 1)
    }

    @MainActor
    func testSignOutCancelsSuspendedWaitAndPurgesModal() async {
        let session = FriendsBridgeTestSession(hydrated: true)
        let model = friendsTestModel(session: session)
        await model.start()
        await session.settle()
        model.openFriends()
        model.signOut()
        await session.waitForNoWaiters()
        XCTAssertNil(model.friends)
        XCTAssertFalse(model.isFriendsPresented)
        XCTAssertFalse(model.isConnected)
        XCTAssertGreaterThanOrEqual(session.shutdownCount, 1)
        session.fixture.receive(kind: 766, body: FriendsWireFixture.playing)
        XCTAssertNil(model.friends)
    }

    @MainActor
    func testLateAccountAReconnectionCannotAffectAccountB() async {
        let first = FriendsBridgeTestSession(hydrated: true)
        let connections = FriendsTestConnections(first: first)
        let modelA = friendsTestModel(connect: { try await connections.connect($0) })
        await modelA.start()
        await first.settle()
        first.fixture.disconnect(authenticationRequired: false)
        await connections.waitForRequest(2)
        modelA.signOut()

        let accountB = FriendsWireFixture.accountID + 1
        let second = FriendsBridgeTestSession(accountID: accountB, hydrated: true)
        // Sign-out advanced the same model's authentication generation from 0
        // to 1. Hand off a new authenticated account exactly as login polling does.
        let switchAccount = Task {
            await modelA.connect(
                SteamCredentials(accountName: "Fixture B", steamId: accountB, refreshToken: "fixture-only-never-sent"),
                generation: 1, persist: false
            )
        }
        await connections.waitForRequest(3)
        await connections.release(3, session: second)
        await switchAccount.value
        defer { modelA.signOut() }
        await second.settle()
        let late = FriendsBridgeTestSession(hydrated: true)
        await connections.release(2, session: late)
        await late.waitForShutdown()
        XCTAssertEqual(modelA.friends?.account.accountID, String(accountB))
        XCTAssertEqual(modelA.friends?.friends.count, 1)
        XCTAssertEqual(late.shutdownCount, 1)
    }

    @MainActor
    func testAuthenticationRejectionDoesNotReconnect() async {
        let first = FriendsBridgeTestSession(hydrated: true)
        let connections = FriendsTestConnections(first: first)
        let model = friendsTestModel(connect: { try await connections.connect($0) })
        await model.start()
        defer { model.signOut() }
        await first.settle()
        first.fixture.disconnect(authenticationRequired: true)
        await first.waitForShutdown()
        XCTAssertEqual(model.friends?.connection, .authenticationRequired)
        XCTAssertEqual(model.friends?.friends.count, 1)
        let beforeRetry = await connections.count
        XCTAssertEqual(beforeRetry, 1)
        model.retryFriends()
        let afterRetry = await connections.count
        XCTAssertEqual(afterRetry, 1)
    }

    @MainActor
    func testReconnectStopsAfterThreeFailuresAndRetainsStaleFriends() async {
        let first = FriendsBridgeTestSession(hydrated: true)
        let connections = FriendsTestConnections(first: first)
        let delays = FriendsTestReconnectDelays()
        let model = friendsTestModel(
            connect: { try await connections.connect($0) },
            reconnectDelay: { await delays.record($0) }
        )
        await model.start()
        defer { model.signOut() }
        await first.settle()
        first.fixture.disconnect(authenticationRequired: false)

        for request in 2...3 {
            await connections.waitForRequest(request)
            XCTAssertEqual(model.friends?.connection, .interrupted)
            await connections.reject(request, error: FriendsTestFailure.unexpected)
        }
        await connections.waitForRequest(4)
        let exhausted = expectation(description: "Reconnect exhaustion is published")
        withObservationTracking {
            _ = model.friends?.connection
        } onChange: { exhausted.fulfill() }
        await connections.reject(4, error: FriendsTestFailure.unexpected)
        await fulfillment(of: [exhausted], timeout: 3)

        XCTAssertEqual(model.friends?.connection, .failed(L10n.string("model.friends.reconnectFailed")))
        XCTAssertEqual(model.friends?.friends.first?.displayName, "Alice")
        XCTAssertEqual(model.friends?.friends.first?.presence, .online)
        let attempts = await delays.attempts
        let connectionCount = await connections.count
        XCTAssertEqual(attempts, [0, 1, 2])
        XCTAssertEqual(connectionCount, 4)
        XCTAssertEqual(first.shutdownCount, 1)
    }

    @MainActor
    func testAuthenticationRejectedDuringReconnectEndsRetryCycle() async {
        let first = FriendsBridgeTestSession(hydrated: true)
        let connections = FriendsTestConnections(first: first)
        let delays = FriendsTestReconnectDelays()
        let model = friendsTestModel(
            connect: { try await connections.connect($0) },
            reconnectDelay: { await delays.record($0) }
        )
        await model.start()
        defer { model.signOut() }
        await first.settle()
        first.fixture.disconnect(authenticationRequired: false)
        await connections.waitForRequest(2)

        let rejected = expectation(description: "Authentication rejection is published")
        withObservationTracking {
            _ = model.friends?.connection
        } onChange: { rejected.fulfill() }
        await connections.reject(2, error: SteamError.AuthenticationRequired)
        await fulfillment(of: [rejected], timeout: 3)

        XCTAssertEqual(model.friends?.connection, .authenticationRequired)
        XCTAssertEqual(model.friends?.friends.count, 1)
        model.retryFriends()
        let attempts = await delays.attempts
        let connectionCount = await connections.count
        XCTAssertEqual(attempts, [0])
        XCTAssertEqual(connectionCount, 2)
    }

    @MainActor
    func testReconnectReloadsLibraryWhenInitialRequestIsStillSuspended() async {
        let initialGames = FriendsTestLibraryRequest()
        let first = FriendsBridgeTestSession(hydrated: true, games: { try await initialGames.request() })
        let connections = FriendsTestConnections(first: first)
        let model = friendsTestModel(connect: { try await connections.connect($0) })
        let start = Task { await model.start() }
        defer { model.signOut() }
        await initialGames.waitUntilRequested()
        await first.settle()
        XCTAssertTrue(model.isLoadingGames)

        first.fixture.disconnect(authenticationRequired: false)
        await connections.waitForRequest(2)
        let recoveredGame = SteamGame(appId: 620, name: "Portal 2", iconHash: "", lastPlayed: 0, playtimeMinutes: 0)
        let replacement = FriendsBridgeTestSession(hydrated: true, games: { [recoveredGame] })
        let libraryRecovered = expectation(description: "Replacement transport reloads the library")
        withObservationTracking {
            _ = model.games
        } onChange: { libraryRecovered.fulfill() }
        await connections.release(2, session: replacement)
        await fulfillment(of: [libraryRecovered], timeout: 3)
        await replacement.settle()

        XCTAssertEqual(model.games.map(\.appId), [620])
        XCTAssertFalse(model.isLoadingGames)
        XCTAssertEqual(model.friends?.connection, .connected)
        await initialGames.reject()
        await start.value
        XCTAssertEqual(model.games.map(\.appId), [620])
        XCTAssertFalse(model.isLoadingGames)
        XCTAssertNil(model.errorMessage, "A failure from the previous transport must not overwrite recovery")
        let connectionCount = await connections.count
        XCTAssertEqual(connectionCount, 2)
    }

    @MainActor
    func testInitialConnectionCompletingAfterSignOutIsClosed() async {
        let connections = FriendsTestConnections()
        let model = friendsTestModel(connect: { try await connections.connect($0) })
        let start = Task { await model.start() }
        await connections.waitForRequest(1)
        model.signOut()
        let late = FriendsBridgeTestSession(hydrated: true)
        await connections.release(1, session: late)
        await start.value
        XCTAssertNil(model.friends)
        XCTAssertFalse(model.isConnected)
        XCTAssertEqual(late.shutdownCount, 1)
    }

    func testExplicitBridgeConnectionCancellationPrecedesAnyNetworkRequest() async {
        let attempt = SteamConnectionAttempt()
        attempt.cancel()
        do {
            _ = try await attempt.connect(credentials: SteamCredentials(accountName: "fixture", steamId: 1, refreshToken: "unused"))
            XCTFail("Cancelled connection unexpectedly succeeded")
        } catch {
            guard case SteamError.Cancelled = error else { return XCTFail("Unexpected error: \(error)") }
        }
    }
}

@MainActor
func friendsTestModel(
    session: FriendsBridgeTestSession? = nil,
    accountID: UInt64 = FriendsWireFixture.accountID,
    connect: (@Sendable (SteamCredentials) async throws -> any SteamSessionProtocol)? = nil,
    reconnectDelay: @escaping @Sendable (Int) async throws -> Void = { _ in try Task.checkCancellation() }
) -> SteamModel {
    SteamModel(
        directory: FileManager.default.temporaryDirectory.appending(path: UUID().uuidString),
        keychain: FriendsTestCredentials(accountID: accountID),
        connect: connect ?? { _ in session! },
        news: { _ in [] }, reconnectDelay: reconnectDelay
    )
}

private struct FriendsTestCredentials: SteamCredentialStore {
    let accountID: UInt64
    func load() async throws -> SteamCredentials? {
        SteamCredentials(accountName: "Fixture", steamId: accountID, refreshToken: "fixture-only-never-sent")
    }
    func save(_ credentials: SteamCredentials) async throws {}
    func remove() async throws { withUnsafeCurrentTask { $0?.cancel() } }
}

final class FriendsBridgeTestSession: SteamSessionProtocol, @unchecked Sendable {
    let fixture: SteamFriendsFixture
    private let requests = FriendsTestSignals<UInt64>()
    private let events = FriendsTestSignals<String>()
    private let lock = NSLock()
    private var waiting = 0
    private var maximumWaiting = 0
    private var shutdowns = 0
    private let loadGames: @Sendable () async throws -> [SteamGame]
    var maximumSimultaneousWaits: Int { lock.withLock { maximumWaiting } }
    var shutdownCount: Int { lock.withLock { shutdowns } }

    init(
        accountID: UInt64 = FriendsWireFixture.accountID,
        hydrated: Bool = false,
        games: @escaping @Sendable () async throws -> [SteamGame] = { [] }
    ) {
        loadGames = games
        fixture = SteamFriendsFixture(accountId: accountID)
        if hydrated {
            fixture.receive(kind: 767, body: FriendsWireFixture.fullList)
            fixture.receive(kind: 766, body: FriendsWireFixture.initialPersona)
        }
    }
    func nextFriends(afterRevision: UInt64) async -> SteamFriendsSnapshot {
        lock.withLock { waiting += 1; maximumWaiting = max(maximumWaiting, waiting) }
        await requests.signal(afterRevision)
        let snapshot = await fixture.nextFriends(afterRevision: afterRevision)
        let remaining = lock.withLock { waiting -= 1; return waiting }
        if remaining == 0 && shutdownCount > 0 { await events.signal("finished-after-shutdown") }
        return snapshot
    }
    func shutdown() {
        lock.withLock { shutdowns += 1 }
        fixture.disconnect(authenticationRequired: false)
        Task { await events.signal("shutdown") }
    }
    func settle() async {
        let snapshot = await fixture.nextFriends(afterRevision: 0)
        await requests.wait(snapshot.revision)
    }
    func waitForShutdown() async { await events.wait("shutdown") }
    func waitForNoWaiters() async {
        if lock.withLock({ waiting == 0 }) { return }
        await events.wait("finished-after-shutdown")
    }
    func games() async throws -> [SteamGame] { try await loadGames() }
    func profile() async throws -> SteamProfile { SteamProfile(displayName: nil, avatarUrl: nil) }
    func details(appId: UInt32, language: String) async throws -> SteamGameDetails { throw FriendsTestFailure.unexpected }
    func download(appId: UInt32, platform: String, language: String, directory: String) async throws -> SteamInstallation { throw FriendsTestFailure.unexpected }
    func downloadProgress() -> SteamTransfer? { nil }
    func cancelDownload() {}
    func launchPlan(appId: UInt32, platform: String, directory: String) async throws -> SteamLaunchPlan { throw FriendsTestFailure.unexpected }
}

actor FriendsTestSignals<Key: Hashable & Sendable> {
    private var signalled: Set<Key> = []
    private var waiters: [Key: [CheckedContinuation<Void, Never>]] = [:]
    func signal(_ key: Key) {
        signalled.insert(key)
        for waiter in waiters.removeValue(forKey: key) ?? [] { waiter.resume() }
    }
    func wait(_ key: Key) async {
        if signalled.contains(key) { return }
        await withCheckedContinuation { waiters[key, default: []].append($0) }
    }
}

private actor FriendsTestConnections {
    private let first: FriendsBridgeTestSession?
    private var pending: [Int: CheckedContinuation<any SteamSessionProtocol, any Error>] = [:]
    private let requests = FriendsTestSignals<Int>()
    private(set) var count = 0
    init(first: FriendsBridgeTestSession? = nil) { self.first = first }
    func connect(_ credentials: SteamCredentials) async throws -> any SteamSessionProtocol {
        count += 1
        let number = count
        if number == 1, let first { return first }
        return try await withCheckedThrowingContinuation { continuation in
            pending[number] = continuation
            Task { await requests.signal(number) }
        }
    }
    func waitForRequest(_ number: Int) async { await requests.wait(number) }
    func release(_ number: Int, session: any SteamSessionProtocol) {
        pending.removeValue(forKey: number)?.resume(returning: session)
    }
    func reject(_ number: Int, error: any Error) {
        pending.removeValue(forKey: number)?.resume(throwing: error)
    }
}

private actor FriendsTestReconnectDelays {
    private(set) var attempts: [Int] = []
    func record(_ attempt: Int) { attempts.append(attempt) }
}

private actor FriendsTestLibraryRequest {
    private var continuation: CheckedContinuation<[SteamGame], any Error>?
    private let requested = FriendsTestSignals<String>()

    func request() async throws -> [SteamGame] {
        try await withCheckedThrowingContinuation { continuation in
            self.continuation = continuation
            Task { await requested.signal("requested") }
        }
    }

    func waitUntilRequested() async { await requested.wait("requested") }

    func reject() {
        continuation?.resume(throwing: FriendsTestFailure.unexpected)
        continuation = nil
    }
}

enum FriendsTestFailure: Error { case unexpected }

/// Wire tags checked against SteamTracking 08f0f630, not encoded by steam-kit.
/// Friend ID 0x0110000100000001 is synthetic; all names are fixture data.
enum FriendsWireFixture {
    static let accountID: UInt64 = 0x0110000100000064
    static let fullList = Data([0x08, 0x00, 0x12, 0x0b, 0x09, 1, 0, 0, 0, 1, 0, 0x10, 1, 0x10, 3])
    static let identity: [UInt8] = [0x09, 1, 0, 0, 0, 1, 0, 0x10, 1]
    // mask Status | PlayerName | Presence, name tag15, avatar tag31 explicit empty.
    static let initialPersona = Data([0x08, 0x13, 0x12, 21] + identity + [0x10, 1, 0x7a, 5] + Array("Alice".utf8) + [0xfa, 1, 0])
    // GameExtraInfo mask256, game_name tag55, game_id tag56 fixed64 = 620.
    static let playing = Data([0x08, 0x80, 2, 0x12, 30] + identity + [0xba, 3, 8] + Array("Portal 2".utf8) + [0xc1, 3, 0x6c, 2, 0, 0, 0, 0, 0, 0])
    static let closedGame = Data([0x08, 0x80, 2, 0x12, 19] + identity + [0xc1, 3, 0, 0, 0, 0, 0, 0, 0, 0])
    static let offline = Data([0x08, 1, 0x12, 11] + identity + [0x10, 0])
}
