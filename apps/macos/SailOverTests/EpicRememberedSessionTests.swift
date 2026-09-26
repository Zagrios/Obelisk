import Foundation
import XCTest
@testable import SailOver

@MainActor
final class EpicRememberedSessionTests: XCTestCase {
    func testPreparationIsSavedBeforeRequestAndFailureSavesItsRotation() async throws {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: credentials("initial"), games: [])
        let remembered = EpicRememberedSession(store: store)
        await remembered.activate(session, accountID: "account")
        await session.rotateOnPrepare(credentials("prepared"))

        do {
            try await remembered.perform(on: session) {
                let saved = await store.value
                XCTAssertEqual(saved?.refreshToken, "prepared")
                await session.rotate(credentials("failed-request"))
                throw EpicError.Network
            }
            XCTFail("Expected the request error")
        } catch EpicError.Network {
            let saved = await store.value
            XCTAssertEqual(saved?.refreshToken, "failed-request")
        }
        await remembered.forget().value
    }

    func testObservedRotationAndOperationCompletionResolveLatestTokenAfterHeldWrite() async throws {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: credentials("initial"), games: [])
        let remembered = EpicRememberedSession(store: store)
        await remembered.activate(session, accountID: "account")
        try await eventually { session.credentialRevisions.isWaiting }
        let gate = EpicOperationGate()
        let request = Task { try await remembered.perform(on: session) { await gate.wait() } }
        try await eventually { gate.isWaiting && session.credentialRevisions.isWaiting }

        await store.holdNextSave()
        await session.rotate(credentials("held"))
        try await eventually { await store.waitingForSave }
        gate.release()
        await session.rotate(credentials("latest"))
        await store.completeSave()
        try await request.value
        try await eventually { session.credentialRevisions.isWaiting }
        let writes = await store.writes.compactMap { $0?.refreshToken }
        let held = try XCTUnwrap(writes.firstIndex(of: "held"))
        XCTAssertEqual(writes.last, "latest")
        XCTAssertTrue(writes.suffix(from: held + 1).allSatisfy { $0 == "latest" })
        await remembered.forget().value
    }

    func testCancelledOperationAndObservedRotationCannotOverwriteReplacementAccount() async throws {
        // Reuse the same account ID to prove isolation follows the session lifetime.
        let store = EpicMemoryStore(nil)
        let old = EpicTestSession(credentials: credentials("old"), games: [])
        let replacement = EpicTestSession(credentials: credentials("replacement"), games: [])
        let remembered = EpicRememberedSession(store: store)
        await remembered.activate(old, accountID: "account")
        try await eventually { old.credentialRevisions.isWaiting }
        let gate = EpicOperationGate()
        let request = Task { try await remembered.perform(on: old) { await gate.wait() } }
        try await eventually { gate.isWaiting && old.credentialRevisions.isWaiting }
        await store.holdNextSave()
        await old.rotate(credentials("old-held"))
        try await eventually { await store.waitingForSave }
        request.cancel()
        let removal = remembered.forget()
        XCTAssertTrue(old.credentialRevisions.isStopped)
        await store.completeSave()
        await removal.value
        let removed = await store.value
        XCTAssertNil(removed)
        await remembered.activate(replacement, accountID: "account")
        await old.rotate(credentials("old-late"))
        gate.release()
        do {
            try await request.value
            XCTFail("Expected cancellation")
        } catch is CancellationError {}
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

    func testCancellationDuringPreparedSaveDoesNotStartRequest() async throws {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: credentials("initial"), games: [])
        let remembered = EpicRememberedSession(store: store)
        await remembered.activate(session, accountID: "account")
        try await eventually { session.credentialRevisions.isWaiting }
        await store.holdNextSave()
        let request = Task {
            try await remembered.perform(on: session) { XCTFail("Cancelled request must not start") }
        }
        try await eventually { await store.waitingForSave }
        request.cancel()
        await session.rotate(credentials("late-prepare"))
        await store.completeSave()
        do {
            try await request.value
            XCTFail("Expected cancellation")
        } catch is CancellationError {}
        let saved = await store.value
        XCTAssertEqual(saved?.refreshToken, "late-prepare")
        XCTAssertFalse(session.credentialRevisions.isStopped)
        await remembered.forget().value
    }

    func testRotationAfterCancelledRequestStillPersistsWithoutAnOperation() async throws {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: credentials("initial"), games: [])
        let remembered = EpicRememberedSession(store: store)
        await remembered.activate(session, accountID: "account")
        let gate = EpicOperationGate()
        let request = Task { try await remembered.perform(on: session) { await gate.wait() } }
        try await eventually { gate.isWaiting }
        request.cancel()
        gate.release()
        do { try await request.value; XCTFail("Expected cancellation") }
        catch is CancellationError {}
        await session.rotate(credentials("background-rotation"))
        try await eventually { await store.value?.refreshToken == "background-rotation" }
        XCTAssertFalse(session.credentialRevisions.isStopped)
        await remembered.forget().value
    }

    func testRememberChangesWhileWriteIsHeldApplyToLatestRevision() async throws {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: credentials("initial"), games: [])
        let remembered = EpicRememberedSession(store: store)
        await remembered.activate(session, accountID: "account")
        try await eventually { session.credentialRevisions.isWaiting }
        await store.holdNextSave()
        await session.rotate(credentials("held"))
        try await eventually { await store.waitingForSave }
        remembered.rememberSession = false
        await session.rotate(credentials("unremembered"))
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

    func testStorageFailureDoesNotFailOperationAndNextRotationClearsWarning() async throws {
        let store = EpicMemoryStore(nil)
        await store.rejectSaves()
        let session = EpicTestSession(credentials: credentials("initial"), games: [])
        let remembered = EpicRememberedSession(store: store)
        await remembered.activate(session, accountID: "account")
        let result = try await remembered.perform(on: session) { 42 }
        XCTAssertEqual(result, 42)
        XCTAssertNotNil(remembered.storageWarning)
        try await eventually { session.credentialRevisions.isWaiting }
        await store.rejectSaves(false)
        await session.rotate(credentials("recovered"))
        try await eventually { session.credentialRevisions.isWaiting && remembered.storageWarning == nil }
        let saved = await store.value
        XCTAssertEqual(saved?.refreshToken, "recovered")
        await remembered.forget().value
    }

    func testMismatchedAccountCredentialsAreNeverWritten() async throws {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: credentials("initial"), games: [])
        let remembered = EpicRememberedSession(store: store)
        await remembered.activate(session, accountID: "account")
        try await eventually { session.credentialRevisions.isWaiting }
        await session.rotate(credentials("wrong-account", account: "other"))
        try await eventually { remembered.storageWarning != nil }
        let saved = await store.value
        XCTAssertEqual(saved?.refreshToken, "initial")
        await remembered.forget().value
    }

    func testReleasingModuleStopsObservationWithoutRevokingSessionOrRemovingCredentials() async throws {
        let store = EpicMemoryStore(nil)
        let session = EpicTestSession(credentials: credentials("initial"), games: [])
        var remembered: EpicRememberedSession? = EpicRememberedSession(store: store)
        await remembered?.activate(session, accountID: "account")
        try await eventually { session.credentialRevisions.isWaiting }
        weak var released = remembered
        remembered = nil
        try await eventually { released == nil && session.credentialRevisions.isStopped }
        let saved = await store.value
        let disconnections = await session.disconnections
        XCTAssertEqual(saved?.refreshToken, "initial")
        XCTAssertEqual(disconnections, 0)
    }

    private func credentials(_ token: String, account: String = "account") -> EpicCredentials {
        EpicCredentials(accountId: account, displayName: "Fixture", refreshToken: token)
    }

    private func eventually(_ predicate: @MainActor () async -> Bool) async throws {
        for _ in 0..<200 {
            if await predicate() { return }
            try await Task.sleep(for: .milliseconds(5))
        }
        throw EpicRememberedTestError.timeout
    }
}

private enum EpicRememberedTestError: Error { case timeout }

@MainActor
private final class EpicOperationGate {
    private(set) var isWaiting = false
    private var continuation: CheckedContinuation<Void, Never>?
    func wait() async {
        isWaiting = true
        await withCheckedContinuation { continuation = $0 }
        isWaiting = false
    }
    func release() { continuation?.resume(); continuation = nil }
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
