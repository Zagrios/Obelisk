import AppKit
import Foundation
import SwiftUI
import XCTest
@testable import Forge

final class EpicFriendsIntegrationTests: XCTestCase {
    func testLiveEpicFriendsWhenRequested() async throws {
        guard ProcessInfo.processInfo.environment["FORGE_EPIC_FRIENDS_SMOKE"] == "1" else {
            throw XCTSkip("Set FORGE_EPIC_FRIENDS_SMOKE=1 to validate the saved Epic account explicitly")
        }
        let store = EpicKeychain()
        let saved = try await store.load()
        let credentials = try XCTUnwrap(saved, "A saved Epic session is required")
        let session = try await EpicSession.restore(credentials: credentials)
        try await store.save(session.credentials())
        let persistence = Task {
            var revision: UInt64 = 0
            while let changed = await session.nextCredentialsRevision(afterRevision: revision) {
                revision = changed
                do { try await store.save(session.credentials()) }
                catch { XCTFail("Rotated Epic credentials could not be saved") }
            }
        }
        let requestedSeconds = Int(ProcessInfo.processInfo.environment["FORGE_EPIC_FRIENDS_SMOKE_SECONDS"] ?? "") ?? 40
        let observationSeconds = min(300, max(40, requestedSeconds))
        let deadline = Task {
            do { try await Task.sleep(for: .seconds(observationSeconds)); session.stopFriends() } catch {}
        }
        let started = Date()
        await session.startFriends()
        var revision: UInt64 = 0
        var result: EpicFriendsSnapshot?
        var previous: [EpicFriend] = []
        var presenceChanges = 0
        while true {
            let snapshot = await session.nextFriends(afterRevision: revision)
            revision = snapshot.revision
            if snapshot.connection == .stopped { break }
            if snapshot.connection == .loading || snapshot.connection == .interrupted { continue }
            // Safe, opt-in live diagnostics: enums and counts only.
            print("Epic friends observation: elapsedMs=\(Int(Date().timeIntervalSince(started) * 1000)), connection=\(snapshot.connection), accepted=\(snapshot.friends.count), known=\(snapshot.friends.filter { $0.presence != .unknown }.count), online=\(snapshot.friends.filter { $0.presence == .online }.count), away=\(snapshot.friends.filter { $0.presence == .away || $0.presence == .extendedAway }.count), offline=\(snapshot.friends.filter { $0.presence == .offline }.count)")
            if result == nil { result = snapshot }
            else if snapshot.friends.contains(where: { current in
                previous.contains { $0.accountId == current.accountId &&
                    ($0.presence != current.presence || $0.activity != current.activity) }
            }) { presenceChanges += 1 }
            previous = snapshot.friends
            if snapshot.connection == .failed || snapshot.connection == .authenticationRequired { break }
        }
        deadline.cancel()
        session.shutdownObservers()
        await persistence.value
        let observed = try XCTUnwrap(result)
        // Counts and connection enums only. Do not log account/friend IDs, names or tokens.
        print("Epic friends live check: connection=\(observed.connection), accepted=\(observed.friends.count), named=\(observed.friends.filter { $0.displayName != nil }.count), knownPresence=\(observed.friends.filter { $0.presence != .unknown }.count), playing=\(observed.friends.filter { $0.activity != nil }.count), subsequentPresenceSnapshots=\(presenceChanges)")
        XCTAssertEqual(observed.connection, .ready, "Live Epic did not deliver an initial social snapshot")
        if ProcessInfo.processInfo.environment["FORGE_EPIC_FRIENDS_EXPECT_PRESENT"] == "1" {
            XCTAssertTrue(observed.friends.contains { $0.presence == .online || $0.presence == .away || $0.presence == .extendedAway || $0.presence == .busy }, "The controlled friend was not present in the first ready state")
        }
    }

    @MainActor
    func testEventsStayActiveAcrossModalCloseAndSocialRetryPreservesSession() async throws {
        let session = EpicFriendsTestSession()
        let model = epicFriendsModel(session)
        await model.start()
        defer { model.signOut() }
        await session.settled()
        XCTAssertEqual(model.friends?.friends.first?.displayName, "Alice")
        model.openFriends()
        model.closeFriends()
        session.publish(presence: .away, game: "Game A")
        await session.settled()
        XCTAssertEqual(model.friends?.friends.first?.activity?.name, "Game A")
        XCTAssertFalse(model.isFriendsPresented)
        model.openFriends()
        XCTAssertEqual(session.starts, 1)
        XCTAssertEqual(session.maximumWaiters, 1)
        model.retryFriends()
        await session.waitForReconnect()
        await session.settled()
        XCTAssertEqual(session.reconnections, 1)
        XCTAssertEqual(session.disconnects, 0)
        XCTAssertEqual(session.downloadCancellations, 0)
        XCTAssertTrue(model.isConnected)
    }

    @MainActor
    func testSocialFailureKeepsLibraryAndUnknownCountIsExplicit() async {
        let session = EpicFriendsTestSession()
        let model = epicFriendsModel(session)
        await model.start()
        defer { model.signOut() }
        await session.settled()
        session.publish(presence: .unknown)
        await session.settled()
        XCTAssertEqual(model.friends?.onlineSummary, L10n.string("ui.friends.totalCount", Int64(1)))
        session.fail(.forbidden)
        await session.settled()
        XCTAssertTrue(model.isConnected)
        XCTAssertNil(model.errorMessage)
        XCTAssertEqual(model.friends?.friends.count, 1)
        XCTAssertEqual(model.friends?.menuTitle, L10n.string("ui.friends.menuUnavailable"))
    }

    @MainActor
    func testCredentialRotationIsSavedWithoutAnyFriendEventAndHonorsRememberSetting() async {
        let session = EpicFriendsTestSession()
        let store = EpicFriendsTestStore()
        let model = epicFriendsModel(session, store: store)
        await model.start()
        defer { model.signOut() }
        await session.credentialsSettled()
        session.rotate("test-rotated")
        await session.credentialsSettled()
        let saved = await store.value
        XCTAssertEqual(saved?.refreshToken, "test-rotated")
        model.rememberSession = false
        session.rotate("test-not-remembered")
        await session.credentialsSettled()
        let removed = await store.value
        XCTAssertNil(removed)
        XCTAssertEqual(session.starts, 1)
    }

    @MainActor
    func testAccountChangeWaitsForRotatedCredentialWriteAndRejectsLateSocialData() async {
        let old = EpicFriendsTestSession()
        let replacement = EpicFriendsTestSession(accountID: "replacement-account")
        let store = EpicFriendsTestStore()
        let model = EpicModel(keychain: store, signIn: { _ in replacement }, restore: { _ in old })
        await model.start()
        await old.credentialsSettled()
        await store.holdNextSave()
        old.rotate("test-old-rotation")
        await store.waitForHeldSave()
        model.signOut()
        old.publish(presence: .online, game: "Obsolete game")
        model.signIn(input: "test-code")
        XCTAssertFalse(model.isConnecting, "A new account must wait for the previous Keychain removal")
        XCTAssertNil(model.friends)
        await store.releaseSaveAndWaitForRemoval()
        // Removal wakes the storage task; allow its MainActor completion to clear sign-out.
        await withCheckedContinuation { c in DispatchQueue.main.async { c.resume() } }
        model.signIn(input: "test-code")
        await replacement.credentialsSettled()
        await replacement.settled()
        XCTAssertEqual(model.friends?.account.accountID, "replacement-account")
        let saved = await store.value
        XCTAssertEqual(saved?.accountId, "replacement-account")
        XCTAssertEqual(saved?.refreshToken, "test-initial")
        model.signOut()
    }

    @MainActor
    func testBothStoresUseOneAccountScopedModalIndependentOfSelection() async throws {
        let epicSession = EpicFriendsTestSession()
        let epic = epicFriendsModel(epicSession)
        let steamSession = FriendsBridgeTestSession(hydrated: true)
        let steam = friendsTestModel(session: steamSession)
        await epic.start()
        await steam.start()
        defer { epic.signOut(); steam.signOut() }
        await epicSession.settled()
        await steamSession.settle()
        let directory = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let model = AppModel(directory: directory, steam: steam, epic: epic)
        let host = NSHostingView(rootView: RootView(model: model))
        let window = NSWindow(contentRect: NSRect(x: -20_000, y: -20_000, width: 860, height: 600), styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        window.orderFront(nil)
        defer { window.close() }
        func settle() async {
            for _ in 0..<8 {
                await withCheckedContinuation { c in DispatchQueue.main.async { c.resume() } }
                host.layoutSubtreeIfNeeded()
            }
        }
        func modals(_ view: NSView) -> [FriendsModalContainer] {
            (view as? FriendsModalContainer).map { [$0] } ?? view.subviews.flatMap(modals)
        }
        epic.openFriends()
        await settle()
        XCTAssertEqual(model.activeFriendsAccount?.storeID, "epic")
        XCTAssertEqual(modals(host).count, 1)
        XCTAssertEqual(modals(host).first?.host.frame.midX ?? 0, 430, accuracy: 1)
        model.selection = .settings
        epicSession.publish(presence: .online, game: "New game")
        await epicSession.settled()
        await settle()
        XCTAssertEqual(model.presentedFriends?.friends.first?.activity?.name, "New game")
        steam.openFriends()
        await settle()
        XCTAssertEqual(model.activeFriendsAccount?.storeID, "steam")
        XCTAssertFalse(epic.isFriendsPresented)
        XCTAssertEqual(modals(host).count, 1)
        epic.signOut()
        await settle()
        XCTAssertEqual(model.presentedFriends?.account.storeID, "steam")
        XCTAssertTrue(steam.isConnected)
        XCTAssertEqual(epicSession.maximumWaiters, 1)
    }

    @MainActor
    func testSignOutClosesEpicModalAndRejectsOldAccountEvents() async throws {
        let session = EpicFriendsTestSession()
        let model = epicFriendsModel(session)
        await model.start()
        await session.settled()
        model.openFriends()
        model.signOut()
        session.publish(presence: .online, game: "Late game")
        await Task.yield()
        XCTAssertNil(model.friends)
        XCTAssertFalse(model.isFriendsPresented)
        XCTAssertFalse(model.isConnected)
    }
}

@MainActor
func epicFriendsModel(_ session: EpicFriendsTestSession, store: EpicFriendsTestStore = EpicFriendsTestStore()) -> EpicModel {
    EpicModel(keychain: store, restore: { _ in session })
}

actor EpicFriendsTestStore: EpicCredentialStore {
    private(set) var value: EpicCredentials? = EpicCredentials(accountId: "epic-account", displayName: "Epic fixture", refreshToken: "test-initial")
    func load() async throws -> EpicCredentials? { value }
    private var holdSave = false
    private var heldSave: CheckedContinuation<Void, Never>?
    private var holdWaiter: CheckedContinuation<Void, Never>?
    private var removalWaiter: CheckedContinuation<Void, Never>?
    func save(_ value: EpicCredentials) async throws {
        if holdSave {
            holdSave = false
            await withCheckedContinuation { c in
                heldSave = c
                holdWaiter?.resume(); holdWaiter = nil
            }
        }
        self.value = value
    }
    func remove() async throws {
        value = nil
        removalWaiter?.resume(); removalWaiter = nil
    }
    func holdNextSave() { holdSave = true }
    func waitForHeldSave() async {
        if heldSave != nil { return }
        await withCheckedContinuation { holdWaiter = $0 }
    }
    func releaseSaveAndWaitForRemoval() async {
        await withCheckedContinuation { c in
            removalWaiter = c
            heldSave?.resume(); heldSave = nil
        }
    }
}

/// Atomic revision registration is deliberately controllable, without timers or a live account.
final class EpicFriendsTestSession: EpicSessionProtocol, @unchecked Sendable {
    private let lock = NSLock()
    private var snapshot = EpicFriendsSnapshot(accountId: "epic-account", revision: 1, connection: .ready,
        friends: [EpicFriend(accountId: "alice", displayName: "Alice", avatarUrl: nil, presence: .online, activity: nil)], message: nil)
    private var saved = EpicCredentials(accountId: "epic-account", displayName: "Epic fixture", refreshToken: "test-initial")
    private var credentialRevision: UInt64 = 1
    private var credentialStopped = false
    private var friendWaiters: [CheckedContinuation<EpicFriendsSnapshot, Never>] = []
    private var credentialWaiters: [CheckedContinuation<UInt64?, Never>] = []
    private var friendBarriers: [CheckedContinuation<Void, Never>] = []
    private var credentialBarriers: [CheckedContinuation<Void, Never>] = []
    private var reconnectBarriers: [CheckedContinuation<Void, Never>] = []
    private var friendAfter: UInt64 = 0
    private var credentialAfter: UInt64 = 0
    private var startCount = 0, reconnectCount = 0, disconnectCount = 0, cancellationCount = 0, maximum = 0
    init(accountID: String = "epic-account") {
        snapshot.accountId = accountID
        saved.accountId = accountID
    }
    var starts: Int { lock.withLock { startCount } }
    var reconnections: Int { lock.withLock { reconnectCount } }
    var disconnects: Int { lock.withLock { disconnectCount } }
    var downloadCancellations: Int { lock.withLock { cancellationCount } }
    var maximumWaiters: Int { lock.withLock { maximum } }
    func credentials() async throws -> EpicCredentials { lock.withLock { saved } }
    func prepare() async throws -> EpicCredentials { try await credentials() }
    func games() async throws -> [EpicGame] { [] }
    func startFriends() async { lock.withLock { startCount += 1 } }
    func reconnectFriends() async {
        lock.withLock {
            reconnectCount += 1
            snapshot.revision += 1
            snapshot.connection = .ready
            reconnectBarriers.forEach { $0.resume() }; reconnectBarriers.removeAll()
        }
    }
    func waitForReconnect() async {
        await withCheckedContinuation { c in lock.withLock {
            if reconnectCount > 0 { c.resume() } else { reconnectBarriers.append(c) }
        } }
    }
    func friends() -> EpicFriendsSnapshot { lock.withLock { snapshot } }
    func nextFriends(afterRevision: UInt64) async -> EpicFriendsSnapshot {
        await withCheckedContinuation { c in lock.withLock {
            friendAfter = afterRevision
            if snapshot.revision != afterRevision || snapshot.connection == .stopped { c.resume(returning: snapshot) }
            else {
                friendWaiters.append(c); maximum = max(maximum, friendWaiters.count)
                friendBarriers.forEach { $0.resume() }; friendBarriers.removeAll()
            }
        } }
    }
    func settled() async {
        await withCheckedContinuation { c in lock.withLock {
            if friendAfter == snapshot.revision && !friendWaiters.isEmpty { c.resume() }
            else { friendBarriers.append(c) }
        } }
    }
    func credentialsSettled() async {
        await withCheckedContinuation { c in lock.withLock {
            if credentialAfter == credentialRevision && !credentialWaiters.isEmpty { c.resume() }
            else { credentialBarriers.append(c) }
        } }
    }
    func nextCredentialsRevision(afterRevision: UInt64) async -> UInt64? {
        await withCheckedContinuation { c in lock.withLock {
            credentialAfter = afterRevision
            if credentialStopped { c.resume(returning: nil) }
            else if afterRevision != credentialRevision { c.resume(returning: credentialRevision) }
            else {
                credentialWaiters.append(c)
                credentialBarriers.forEach { $0.resume() }; credentialBarriers.removeAll()
            }
        } }
    }
    func rotate(_ token: String) { lock.withLock {
        saved = EpicCredentials(accountId: saved.accountId, displayName: saved.displayName, refreshToken: token)
        credentialRevision += 1
        credentialWaiters.forEach { $0.resume(returning: credentialRevision) }; credentialWaiters.removeAll()
    } }
    func publish(presence: EpicFriendPresence, game: String? = nil) { lock.withLock {
        snapshot.friends = [EpicFriend(accountId: "alice", displayName: "Alice", avatarUrl: nil, presence: presence,
            activity: game.map { EpicFriendActivity(id: $0, name: $0) })]
        snapshot.revision += 1
        friendWaiters.forEach { $0.resume(returning: snapshot) }; friendWaiters.removeAll()
    } }
    func fail(_ connection: EpicFriendsConnection) { lock.withLock {
        snapshot.connection = connection
        snapshot.message = "backend.epic.friends.forbidden"
        snapshot.revision += 1
        friendWaiters.forEach { $0.resume(returning: snapshot) }; friendWaiters.removeAll()
    } }
    func stopFriends() { lock.withLock {
        snapshot.connection = .stopped; snapshot.revision += 1
        friendWaiters.forEach { $0.resume(returning: snapshot) }; friendWaiters.removeAll()
    } }
    func shutdownObservers() {
        stopFriends()
        lock.withLock {
            credentialStopped = true
            credentialWaiters.forEach { $0.resume(returning: nil) }; credentialWaiters.removeAll()
        }
    }
    func disconnect() async throws { shutdownObservers(); lock.withLock { disconnectCount += 1 } }
    func cancelDownload() { lock.withLock { cancellationCount += 1 } }
    func downloadProgress() -> GameTransfer? { nil }
    func download(namespace: String, catalogItemId: String, appName: String, platform: String, directory: String) async throws -> EpicInstallation { throw EpicError.InvalidResponse }
    func gameDetails(namespace: String, catalogItemId: String, appName: String, platform: String) async throws -> EpicGameDetails { throw EpicError.InvalidResponse }
    func launchPlan(namespace: String, catalogItemId: String, appName: String, platform: String, directory: String, language: String) async throws -> EpicLaunchPlan { throw EpicError.InvalidResponse }
}
