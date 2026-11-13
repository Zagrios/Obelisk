import Foundation
import XCTest
@testable import Obelisk

/// The download queue as Swift presents it, over a queue the test publishes.
@MainActor
final class DownloadsModelTests: XCTestCase {
    func testTheQueueShowsWhatRunsAndWaitsAndForwardsEachAction() async throws {
        let queue = FakeDownloads()
        let model = DownloadsModel(backend: queue, period: .seconds(3600))
        XCTAssertFalse(model.isVisible)

        let running = DownloadItem.fixture(1)
        let waiting = DownloadItem.fixture(2, game: "730", kind: .update, automatic: true)
        queue.publish {
            $0.running = running
            $0.waiting = [waiting]
        }
        try await eventually { model.isVisible }
        XCTAssertEqual(model.count, 2)
        XCTAssertEqual(model.waiting, [waiting])

        model.pause(running)
        model.moveToTop(waiting)
        model.remove(waiting)
        XCTAssertEqual(queue.calls, [.pause(1), .moveToTop(2), .remove(2)])
    }

    func testOnlyAutomaticUpdatesWaitingForAGameSaySo() async throws {
        let queue = FakeDownloads()
        let model = DownloadsModel(backend: queue, period: .seconds(3600))
        queue.publish {
            $0.waiting = [.fixture(2, automatic: true)]
            $0.held = true
        }
        try await eventually { model.held }
        XCTAssertTrue(model.waitsForGame)

        queue.publish { $0.waiting.append(.fixture(3, game: "730")) }
        try await eventually { model.waiting.count == 2 }
        XCTAssertFalse(model.waitsForGame, "the player's own download goes on")
    }

    func testTheSpeedComesFromTheRunningTransfersBytesAndStartsAgainWithTheNext() async throws {
        let queue = FakeDownloads()
        let model = DownloadsModel(backend: queue, period: .seconds(3600))
        var bytes: UInt64 = 0
        model.progress = { item in
            GameTransfer(game: item.game, state: .downloading, completedBytes: 0, totalBytes: 100,
                         networkBytes: bytes, contentBytes: bytes, currentFile: "", message: "")
        }
        queue.publish { $0.running = .fixture(1) }
        try await eventually { model.running != nil }

        let start = Date()
        model.sample(at: start)
        XCTAssertTrue(model.samples.isEmpty, "a speed needs two moments")
        bytes = 4_000_000
        model.sample(at: start.addingTimeInterval(2))
        bytes = 10_000_000
        model.sample(at: start.addingTimeInterval(3))
        XCTAssertEqual(model.samples.map(\.bytesPerSecond), [2_000_000, 6_000_000])
        XCTAssertEqual(model.currentSpeed, 6_000_000)
        XCTAssertEqual(model.peakSpeed, 6_000_000)

        queue.publish { $0.running = .fixture(2, game: "730") }
        try await eventually { model.running?.id == 2 }
        XCTAssertTrue(model.samples.isEmpty)
    }

    func testTheSpeedLimitReachesRustAndTheUnitAndTheLimitAreRemembered() throws {
        let suite = "ObeliskTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let queue = FakeDownloads()
        let model = DownloadsModel(backend: queue, period: .seconds(3600), defaults: defaults)
        XCTAssertEqual(model.speedUnit, .bytes)
        XCTAssertEqual(model.speedLimit, 0, "downloads are unlimited until the player limits them")

        model.speedUnit = .bits
        model.speedLimitChoice = 2
        XCTAssertEqual(model.speedLimit, 3_125_000, "25 megabits per second")
        XCTAssertEqual(queue.speedLimit, 3_125_000)

        let next = FakeDownloads()
        let reopened = DownloadsModel(backend: next, period: .seconds(3600), defaults: defaults)
        XCTAssertEqual(reopened.speedUnit, .bits)
        XCTAssertEqual(reopened.speedLimit, 3_125_000)
        XCTAssertEqual(next.speedLimit, 3_125_000, "a new start limits downloads again")
    }

    func testTheLimitsFollowTheUnitAndChangingItLeavesTheLimitAsItIs() throws {
        let suite = "ObeliskTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let queue = FakeDownloads()
        let model = DownloadsModel(backend: queue, period: .seconds(3600), defaults: defaults)
        XCTAssertEqual(model.speedLimitChoice, DownloadSpeedUnit.bytes.speedLimits.count - 1, "none is the last choice")

        model.speedUnit = .bits
        model.speedLimitChoice = 4
        XCTAssertEqual(model.speedLimit, 12_500_000, "100 megabits per second")

        model.speedUnit = .bytes
        XCTAssertEqual(model.speedLimit, 12_500_000, "a unit is only how the limit reads")
        XCTAssertEqual(queue.speedLimit, 12_500_000)
        XCTAssertEqual(DownloadSpeedUnit.bytes.speedLimits[model.speedLimitChoice], 10_000_000, "the nearest")

        model.speedLimitChoice = 4
        XCTAssertEqual(model.speedLimit, 25_000_000, "25 megabytes per second")
        model.speedLimitChoice = DownloadSpeedUnit.bytes.speedLimits.count - 1
        XCTAssertEqual(model.speedLimit, 0)
    }

    func testASpeedReadsInMegabitsWhenThePlayerChoosesThem() {
        XCTAssertEqual(DownloadFormatting.speed(1_500_000, in: .bits), "12 Mbps")
        XCTAssertEqual(DownloadFormatting.speed(-5, in: .bits), "0 Mbps")
        XCTAssertEqual(DownloadFormatting.speed(625_000, in: .bits), "5 Mbps", "the lowest limit")
    }
}
