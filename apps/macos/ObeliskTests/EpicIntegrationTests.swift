import Foundation
import AppKit
import XCTest
@testable import Obelisk

final class EpicIntegrationTests: XCTestCase {
    @MainActor
    func testArtworkSharesPendingDownloadAndKeepsItAfterOneConsumerCancels() async throws {
        let bitmap = try artworkBitmap()
        let gate = EpicArtworkTestLoader(results: [bitmap], holdFirst: true)
        let cache = ArtworkCache { _ in await gate.load() }
        let request = ArtworkCache.Request(url: URL(string: "https://artwork.invalid/cover.png")!, maxPixelSize: 768)
        let first = Task { await cache.image(for: request) }
        let second = Task { await cache.image(for: request) }
        for _ in 0..<100 {
            if await gate.isWaiting { break }
            try await Task.sleep(for: .milliseconds(5))
        }
        let isWaiting = await gate.isWaiting
        XCTAssertTrue(isWaiting)
        first.cancel()
        await gate.release()
        let firstImage = await first.value
        let secondImage = await second.value
        let cached = cache.cachedImage(for: request)
        XCTAssertNotNil(firstImage)
        XCTAssertTrue(firstImage === secondImage)
        XCTAssertTrue(cached === secondImage, "The image must be available synchronously when the library returns")
        let again = await cache.image(for: request)
        XCTAssertTrue(again === cached)
        let calls = await gate.calls
        XCTAssertEqual(calls, 1)
    }

    @MainActor
    func testArtworkFailureCanRetryAndDifferentURLOrResolutionLoadsItsOwnImage() async throws {
        let bitmap = try artworkBitmap()
        let loader = EpicArtworkTestLoader(results: [nil, bitmap, bitmap, bitmap])
        let cache = ArtworkCache { _ in await loader.load() }
        let url = URL(string: "https://artwork.invalid/cover.png")!
        let request = ArtworkCache.Request(url: url, maxPixelSize: 768)
        let failed = await cache.image(for: request)
        XCTAssertNil(failed)
        XCTAssertNil(cache.cachedImage(for: request))
        let retried = await cache.image(for: request)
        XCTAssertNotNil(retried)
        let banner = await cache.image(for: ArtworkCache.Request(url: url, maxPixelSize: 2048))
        XCTAssertNotNil(banner)
        let changed = await cache.image(for: ArtworkCache.Request(url: url.appendingPathComponent("updated"), maxPixelSize: 768))
        XCTAssertNotNil(changed)
        let calls = await loader.calls
        XCTAssertEqual(calls, 4)
    }

    @MainActor
    private func artworkBitmap() throws -> CGImage {
        let context = try XCTUnwrap(CGContext(
            data: nil, width: 2, height: 2, bitsPerComponent: 8, bytesPerRow: 8,
            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ))
        return try XCTUnwrap(context.makeImage())
    }

    func testPlaytimeFormattingDistinguishesZeroAndPositiveDurations() {
        XCTAssertEqual(GameFormatting.playtime(.zero), L10n.string("ui.game.never_played"))
        XCTAssertEqual(GameFormatting.playtime(.seconds(59)), "0 min")
        XCTAssertEqual(GameFormatting.playtime(.seconds(125)), "2 min")
        XCTAssertEqual(GameFormatting.playtime(.seconds(3600)), "1 h")
        let fractionalHours = 1.5.formatted(.number.precision(.fractionLength(0...1)).locale(L10n.locale))
        XCTAssertEqual(GameFormatting.playtime(.seconds(5400)), L10n.string("ui.game.playtime_hours", fractionalHours))
        XCTAssertFalse(GameFormatting.playtime(.seconds(Int64.max)).isEmpty)
    }

    func testEpicKeychainRoundTripUsesSeparateItemFromSteam() async throws {
        let service = "com.zagrios.obelisk.tests.\(UUID().uuidString)"
        let epic = EpicKeychain(service: service)
        let steam = SteamKeychain(service: service)
        let initial = try await epic.load()
        XCTAssertNil(initial)
        let fixture = EpicCredentials(accountId: "fixture-account", displayName: "Epic Fixture", refreshToken: "test-only")
        try await epic.save(fixture)
        do {
            let loaded = try await epic.load()
            XCTAssertEqual(loaded, fixture)
            let steamSession = try await steam.load()
            XCTAssertNil(steamSession)
            let rotated = EpicCredentials(accountId: "fixture-account", displayName: "Epic Fixture", refreshToken: "test-only-rotated")
            try await epic.save(rotated)
            let updated = try await epic.load()
            XCTAssertEqual(updated, rotated)
        } catch {
            try? await epic.remove()
            throw error
        }
        try await epic.remove()
        let removed = try await epic.load()
        XCTAssertNil(removed)
    }

    func testAuthorizationCodeParsingAcrossRustBridge() throws {
        let code = "0123456789abcdef0123456789abcdef"
        XCTAssertEqual(try epicAuthorizationCode(input: "  \(code)\n"), code)
        XCTAssertEqual(try epicAuthorizationCode(input: "{\"authorizationCode\":\"\(code)\"}"), code)
        XCTAssertThrowsError(try epicAuthorizationCode(input: "{\"authorizationCode\":null}"))
        XCTAssertThrowsError(try epicAuthorizationCode(input: "invalid code"))
        let url = try XCTUnwrap(URLComponents(string: epicLoginUrl()))
        XCTAssertEqual(url.scheme, "https")
        XCTAssertEqual(url.host, "www.epicgames.com")
    }

    func testMalformedSavedSessionFailsWithoutExposingToken() async {
        do {
            _ = try await EpicSession.restore(credentials: EpicCredentials(accountId: "", displayName: "Fixture", refreshToken: "private-test-token"), library: nil)
            XCTFail("Missing account identity must fail before authentication")
        } catch {
            XCTAssertTrue(error is EpicError)
            XCTAssertFalse(String(describing: error).contains("private-test-token"))
        }
    }

    func testEpicAvailabilityAcrossBridge() {
        let epic = storefronts().first { $0.id == .epic }
        XCTAssertEqual(epic?.available, true)
        XCTAssertEqual(epic?.displayName, "Epic Games")
    }
}

private actor EpicArtworkTestLoader {
    private var results: [CGImage?]
    private let holdFirst: Bool
    private var continuation: CheckedContinuation<Void, Never>?
    private(set) var isWaiting = false
    private(set) var calls = 0

    init(results: [CGImage?], holdFirst: Bool = false) {
        self.results = results
        self.holdFirst = holdFirst
    }

    func load() async -> CGImage? {
        calls += 1
        if holdFirst && calls == 1 {
            await withCheckedContinuation { continuation = $0; isWaiting = true }
        }
        return results.isEmpty ? nil : results.removeFirst()
    }

    func release() { continuation?.resume(); continuation = nil; isWaiting = false }
}
