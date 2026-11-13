import Foundation
import XCTest
@testable import Obelisk

@MainActor
final class EpicRememberedSessionTests: XCTestCase {
    func testEveryRotationIsSavedWithoutAnyCallAskingForIt() async throws {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: EpicGames.credentials("initial"), games: [])
        let remembered = await activated(store, session)
        let initial = await store.value
        XCTAssertEqual(initial?.refreshToken, "initial", "the account is saved before it is used")
        await session.rotate(EpicGames.credentials("background-rotation"))
        try await eventually { await store.value?.refreshToken == "background-rotation" }
        XCTAssertFalse(session.credentialRevisions.isStopped)
        await remembered.forget().value
    }

    func testARotationDuringAHeldWriteResolvesTheLatestToken() async throws {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: EpicGames.credentials("initial"), games: [])
        let remembered = await activated(store, session)
        try await eventually { session.credentialRevisions.isWaiting }

        await store.holdNextSave()
        await session.rotate(EpicGames.credentials("held"))
        try await eventually { await store.waitingForSave }
        await session.rotate(EpicGames.credentials("latest"))
        await store.completeSave()
        try await eventually { await store.value?.refreshToken == "latest" }
        try await eventually { session.credentialRevisions.isWaiting }
        let writes = await store.writes.compactMap { $0?.refreshToken }
        let held = try XCTUnwrap(writes.firstIndex(of: "held"))
        XCTAssertEqual(writes.last, "latest")
        XCTAssertTrue(writes.suffix(from: held + 1).allSatisfy { $0 == "latest" })
        await remembered.forget().value
    }

    func testAForgottenSessionsLateRotationCannotOverwriteTheReplacementAccount() async throws {
        // Reuse the same account ID to prove isolation follows the session lifetime.
        let store = EpicMemoryStore(nil)
        let old = EpicTestSession(credentials: EpicGames.credentials("old"), games: [])
        let replacement = EpicTestSession(credentials: EpicGames.credentials("replacement"), games: [])
        let remembered = await activated(store, old)
        try await eventually { old.credentialRevisions.isWaiting }
        await store.holdNextSave()
        await old.rotate(EpicGames.credentials("old-held"))
        try await eventually { await store.waitingForSave }
        let removal = remembered.forget()
        XCTAssertTrue(old.credentialRevisions.isStopped)
        await store.completeSave()
        await removal.value
        let removed = await store.value
        XCTAssertNil(removed)
        await remembered.activate(rotating: RotatingSession(replacement), accountID: EpicGames.fixtureAccount)
        await old.rotate(EpicGames.credentials("old-late"))
        try await eventually { replacement.credentialRevisions.isWaiting }
        let saved = await store.value
        let writes = await store.writes
        let removalIndex = try XCTUnwrap(writes.firstIndex(where: { $0 == nil }))
        XCTAssertEqual(saved?.refreshToken, "replacement")
        XCTAssertTrue(writes.suffix(from: removalIndex + 1).allSatisfy { $0?.refreshToken == "replacement" })
        let disconnections = await old.disconnections
        XCTAssertEqual(disconnections, 0, "Storage invalidation must not revoke OAuth")
        await remembered.forget().value
    }

    func testRememberChangesWhileWriteIsHeldApplyToLatestRevision() async throws {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: EpicGames.credentials("initial"), games: [])
        let remembered = await activated(store, session)
        try await eventually { session.credentialRevisions.isWaiting }
        await store.holdNextSave()
        await session.rotate(EpicGames.credentials("held"))
        try await eventually { await store.waitingForSave }
        remembered.rememberSession = false
        await session.rotate(EpicGames.credentials("unremembered"))
        await store.completeSave()
        try await eventually { await store.value == nil }
        try await eventually { session.credentialRevisions.isWaiting }
        let removed = await store.value
        XCTAssertNil(removed)
        XCTAssertFalse(session.credentialRevisions.isStopped)
        remembered.rememberSession = true
        try await eventually { await store.value?.refreshToken == "unremembered" }
        await remembered.forget().value
    }

    func testAStorageFailureWarnsUntilTheNextRotationIsSaved() async throws {
        let store = EpicMemoryStore(nil)
        await store.rejectSaves()
        let session = EpicTestSession(credentials: EpicGames.credentials("initial"), games: [])
        let remembered = await activated(store, session)
        XCTAssertNotNil(remembered.storageWarning)
        try await eventually { session.credentialRevisions.isWaiting }
        await store.rejectSaves(false)
        await session.rotate(EpicGames.credentials("recovered"))
        try await eventually { session.credentialRevisions.isWaiting && remembered.storageWarning == nil }
        let saved = await store.value
        XCTAssertEqual(saved?.refreshToken, "recovered")
        await remembered.forget().value
    }

    func testMismatchedAccountCredentialsAreNeverWritten() async throws {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: EpicGames.credentials("initial"), games: [])
        let remembered = await activated(store, session)
        try await eventually { session.credentialRevisions.isWaiting }
        await session.rotate(EpicGames.credentials("wrong-account", account: "other"))
        try await eventually { remembered.storageWarning != nil }
        let saved = await store.value
        XCTAssertEqual(saved?.refreshToken, "initial")
        await remembered.forget().value
    }

    func testReleasingModuleStopsObservationWithoutRevokingSessionOrRemovingCredentials() async throws {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: EpicGames.credentials("initial"), games: [])
        var remembered: RememberedSession<WebAccountCredentials>? = await activated(store, session)
        try await eventually { session.credentialRevisions.isWaiting }
        weak var released = remembered
        remembered = nil
        try await eventually { released == nil && session.credentialRevisions.isStopped }
        let saved = await store.value
        let disconnections = await session.disconnections
        XCTAssertEqual(saved?.refreshToken, "initial")
        XCTAssertEqual(disconnections, 0)
    }

    /// The store's remembered session, once it saves `session`'s fixture account.
    private func activated(_ store: EpicMemoryStore, _ session: EpicTestSession) async -> RememberedSession<WebAccountCredentials> {
        let remembered = RememberedSession(keychain: store, accountID: \.accountId,
                                           notSaved: EpicGames.sessionNotSaved)
        await remembered.activate(rotating: RotatingSession(session), accountID: EpicGames.fixtureAccount)
        return remembered
    }
}

/// Scripted watch shared by model, download and presentation sessions.
final class EpicCredentialRevisionFeed: @unchecked Sendable {
    private let lock = NSLock()
    private var revision: UInt64 = 1
    private var stopped = false
    private var waiters: [CheckedContinuation<UInt64?, Never>] = []

    var isWaiting: Bool { lock.withLock { !waiters.isEmpty } }
    var isStopped: Bool { lock.withLock { stopped } }

    func next(after: UInt64) async -> UInt64? {
        await withCheckedContinuation { continuation in
            lock.withLock {
                if stopped { continuation.resume(returning: nil) }
                else if after != revision { continuation.resume(returning: revision) }
                else { waiters.append(continuation) }
            }
        }
    }

    func publish() {
        lock.withLock {
            revision += 1
            waiters.forEach { $0.resume(returning: revision) }
            waiters.removeAll()
        }
    }

    func stop() {
        lock.withLock {
            stopped = true
            waiters.forEach { $0.resume(returning: nil) }
            waiters.removeAll()
        }
    }
}
