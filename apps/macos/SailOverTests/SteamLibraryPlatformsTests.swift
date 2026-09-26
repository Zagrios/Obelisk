import Foundation
import XCTest
@testable import SailOver

final class SteamLibraryPlatformsTests: XCTestCase {
    @MainActor
    func testLoadsLibraryInSequentialBatchesAndPublishesIncrementally() async throws {
        let cache = SteamLibraryPlatforms()
        let session = PlatformMetadataSession()
        cache.load(appIds: Array(1...121), session: session)
        XCTAssertTrue(cache.isLoading)

        try await waitForRequests(1, on: session)
        let firstRequests = await session.requests
        XCTAssertEqual(firstRequests, [Array(1...50)])
        XCTAssertTrue(cache.platforms.isEmpty)
        await session.complete(0, platforms: ["windows"])

        try await waitForRequests(2, on: session)
        XCTAssertEqual(cache.platforms.count, 50)
        XCTAssertEqual(cache.platforms[1], ["windows"])
        XCTAssertNil(cache.platforms[51])
        XCTAssertTrue(cache.isLoading)
        await session.complete(1, platforms: ["macos", "windows"])

        try await waitForRequests(3, on: session)
        XCTAssertEqual(cache.platforms.count, 100)
        XCTAssertEqual(cache.platforms[51], ["macos", "windows"])
        XCTAssertNil(cache.platforms[101])
        await session.complete(2, platforms: ["linux"])
        try await eventually { !cache.isLoading }

        let requests = await session.requests
        XCTAssertEqual(requests, [Array(1...50), Array(51...100), Array(101...121)])
        XCTAssertEqual(cache.platforms.count, 121)
        XCTAssertEqual(cache.platforms[121], ["linux"])
        XCTAssertFalse(cache.hasFailures)
    }

    @MainActor
    func testDeduplicatesIDsAndKeepsSuccessfulUnknownResults() async throws {
        let cache = SteamLibraryPlatforms()
        let session = PlatformMetadataSession()
        cache.load(appIds: [42, 42, 7, 42, 7], session: session)
        try await waitForRequests(1, on: session)
        await session.complete(0, with: .success([
            SteamGamePlatforms(appId: 42, platforms: []),
            SteamGamePlatforms(appId: 7, platforms: ["macos"])
        ]))
        try await eventually { !cache.isLoading }

        let requests = await session.requests
        XCTAssertEqual(requests, [[42, 7]])
        XCTAssertEqual(cache.platforms, [42: [], 7: ["macos"]])
        XCTAssertFalse(cache.hasFailures)

        cache.load(appIds: [], session: session)
        XCTAssertTrue(cache.platforms.isEmpty)
        XCTAssertFalse(cache.isLoading)
        XCTAssertFalse(cache.hasFailures)
        let requestsAfterEmptyLoad = await session.requests
        XCTAssertEqual(requestsAfterEmptyLoad, [[42, 7]])
    }

    @MainActor
    func testFailedBatchPreservesSuccessfulBatchesAndContinuesLoading() async throws {
        let cache = SteamLibraryPlatforms()
        let session = PlatformMetadataSession()
        cache.load(appIds: Array(1...121), session: session)
        try await waitForRequests(1, on: session)
        await session.complete(0, platforms: [])
        try await waitForRequests(2, on: session)
        await session.complete(1, with: .failure(.unavailable))
        try await waitForRequests(3, on: session)

        XCTAssertTrue(cache.hasFailures)
        XCTAssertTrue(cache.isLoading)
        XCTAssertEqual(cache.platforms.count, 50)
        XCTAssertEqual(cache.platforms[1], [])
        XCTAssertNil(cache.platforms[51])
        await session.complete(2, platforms: ["windows"])
        try await eventually { !cache.isLoading }
        XCTAssertTrue(cache.hasFailures)
        XCTAssertEqual(cache.platforms.count, 71)
        XCTAssertEqual(cache.platforms[121], ["windows"])

        cache.reset()
        XCTAssertTrue(cache.platforms.isEmpty)
        XCTAssertFalse(cache.hasFailures)
    }

    @MainActor
    func testNewLoadRejectsOldAccountReplyAndPreservesReplacementLoad() async throws {
        let cache = SteamLibraryPlatforms()
        let oldSession = PlatformMetadataSession()
        cache.load(appIds: Array(1...101), session: oldSession)
        try await waitForRequests(1, on: oldSession)

        let newSession = PlatformMetadataSession()
        cache.load(appIds: Array(1...51), session: newSession)
        try await waitForRequests(1, on: newSession)
        await oldSession.complete(0, platforms: ["windows"])
        try await eventually { await oldSession.returnedRequests == [0] }
        await Task.yield()
        XCTAssertTrue(cache.platforms.isEmpty)
        XCTAssertTrue(cache.isLoading)
        XCTAssertFalse(cache.hasFailures)

        await newSession.complete(0, platforms: ["macos"])
        try await waitForRequests(2, on: newSession)
        XCTAssertEqual(cache.platforms.count, 50)
        await newSession.complete(1, platforms: ["macos"])
        try await eventually { !cache.isLoading }
        XCTAssertEqual(cache.platforms.count, 51)
        XCTAssertEqual(cache.platforms[1], ["macos"])
        let oldRequests = await oldSession.requests
        let oldCancelledRequests = await oldSession.cancelledRequests
        XCTAssertEqual(oldRequests, [Array(1...50)])
        XCTAssertEqual(oldCancelledRequests, [0])
    }

    @MainActor
    func testResetCancelsCurrentBatchAndStopsTheQueue() async throws {
        let cache = SteamLibraryPlatforms()
        let session = PlatformMetadataSession()
        cache.load(appIds: Array(1...101), session: session)
        try await waitForRequests(1, on: session)
        cache.reset()
        XCTAssertFalse(cache.isLoading)

        await session.complete(0, platforms: ["macos"])
        try await eventually { await session.returnedRequests == [0] }
        await Task.yield()
        let requests = await session.requests
        let cancelledRequests = await session.cancelledRequests
        XCTAssertEqual(requests, [Array(1...50)])
        XCTAssertEqual(cancelledRequests, [0])
        XCTAssertTrue(cache.platforms.isEmpty)
        XCTAssertFalse(cache.isLoading)
        XCTAssertFalse(cache.hasFailures)
    }

    @MainActor
    func testDisposalCancelsCurrentBatchWithoutRetainingTheCache() async throws {
        var cache: SteamLibraryPlatforms? = SteamLibraryPlatforms()
        weak var releasedCache = cache
        let session = PlatformMetadataSession()
        cache?.load(appIds: Array(1...101), session: session)
        try await waitForRequests(1, on: session)
        cache = nil
        XCTAssertNil(releasedCache)

        await session.complete(0, platforms: ["macos"])
        try await eventually { await session.returnedRequests == [0] }
        await Task.yield()
        let requests = await session.requests
        let cancelledRequests = await session.cancelledRequests
        XCTAssertEqual(requests, [Array(1...50)])
        XCTAssertEqual(cancelledRequests, [0])
    }

    @MainActor
    private func waitForRequests(_ count: Int, on session: PlatformMetadataSession) async throws {
        try await eventually { await session.requests.count >= count }
    }

    @MainActor
    private func eventually(_ predicate: @MainActor () async -> Bool) async throws {
        for _ in 0..<100 {
            if await predicate() { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTFail("Platform metadata operation did not complete")
        throw PlatformMetadataFailure.timeout
    }
}

private enum PlatformMetadataFailure: Error {
    case unavailable, unexpectedOperation, timeout
}

/// Deliberately ignores cancellation while suspended, like a UniFFI response.
private actor PlatformMetadataSession: SteamSessionProtocol {
    private(set) var requests: [[UInt32]] = []
    private(set) var returnedRequests: [Int] = []
    private(set) var cancelledRequests: [Int] = []
    private var pending: [Int: CheckedContinuation<[SteamGamePlatforms], any Error>] = [:]

    func platforms(appIds: [UInt32]) async throws -> [SteamGamePlatforms] {
        let request = requests.count
        requests.append(appIds)
        defer {
            returnedRequests.append(request)
            if Task.isCancelled { cancelledRequests.append(request) }
        }
        return try await withCheckedThrowingContinuation { continuation in
            pending[request] = continuation
        }
    }

    func complete(_ request: Int, platforms: [String]) {
        complete(request, with: .success(requests[request].map {
            SteamGamePlatforms(appId: $0, platforms: platforms)
        }))
    }

    func complete(_ request: Int, with result: Result<[SteamGamePlatforms], PlatformMetadataFailure>) {
        guard let continuation = pending.removeValue(forKey: request) else {
            XCTFail("No platform request \(request) to complete")
            return
        }
        continuation.resume(with: result.mapError { $0 as any Error })
    }

    func games() async throws -> [SteamGame] { throw PlatformMetadataFailure.unexpectedOperation }
    func profile() async throws -> SteamProfile { throw PlatformMetadataFailure.unexpectedOperation }
    func details(appId: UInt32, language: String) async throws -> SteamGameDetails { throw PlatformMetadataFailure.unexpectedOperation }
    func download(appId: UInt32, platform: String, language: String, directory: String) async throws -> SteamInstallation { throw PlatformMetadataFailure.unexpectedOperation }
    nonisolated func downloadProgress() -> GameTransfer? { nil }
    nonisolated func cancelDownload() {}
    func launchPlan(appId: UInt32, platform: String, directory: String) async throws -> SteamLaunchPlan { throw PlatformMetadataFailure.unexpectedOperation }
}
