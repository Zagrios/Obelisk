import Foundation
import XCTest
@testable import SailOver

final class SteamSortingPerformanceTests: XCTestCase {
    @MainActor
    func testProfileModelSortingWhenRequested() async throws {
        guard ProcessInfo.processInfo.environment["SAILOVER_PROFILE_SORTING"] == "1" else {
            throw XCTSkip("Set SAILOVER_PROFILE_SORTING=1 to profile the sorting algorithm and model cache")
        }
        var samples: [SortingSample] = []

        for count in [250, 1_000] {
            let games = sortingLibrary(count: count)
            XCTAssertGreaterThan(games.filter { $0.lastPlayed == 0 }.count, count / 2)
            XCTAssertEqual(Set(games.map(\.appId)).count, count)

            // Measure only the synchronous production filter/sort. Validation
            // and JSON logging remain outside the timed section.
            for iteration in 0..<5 {
                for sort in [SteamLibrarySort.lastPlayed, .name, .playtime] {
                    let began = ContinuousClock.now
                    let result = SteamModel.filterAndSort(games, query: "", sort: sort)
                    let elapsed = milliseconds(since: began)
                    assertOrder(result, count: count, sort: sort)
                    samples.append(SortingSample(phase: "filterAndSort", games: count, sort: sort.rawValue, iteration: iteration, milliseconds: elapsed))
                }
            }

            let root = FileManager.default.temporaryDirectory.appending(path: "SailOverSorting-\(UUID().uuidString)")
            defer { try? FileManager.default.removeItem(at: root) }
            let session = SortingSteamSession(games: games)
            let model = SteamModel(directory: root, keychain: SortingCredentials(), connect: { _ in session }, news: { _ in [] }, localeIdentifier: "fr-FR")
            await model.start()
            XCTAssertTrue(model.isConnected)
            XCTAssertEqual(model.games.count, count)

            // Profile the model path separately from the standalone algorithm:
            // repeated sorting can reuse cached results even though the pure
            // filterAndSort implementation remains unchanged.
            for iteration in 0..<5 {
                for sort in [SteamLibrarySort.lastPlayed, .name, .playtime] {
                    let began = ContinuousClock.now
                    model.sort = sort
                    let result = model.filteredGames
                    let elapsed = milliseconds(since: began)
                    assertOrder(result, count: count, sort: sort)
                    samples.append(SortingSample(phase: "model-sort+filteredGames", games: count, sort: sort.rawValue, iteration: iteration, milliseconds: elapsed))

                    let rereadBegan = ContinuousClock.now
                    let reread = model.filteredGames
                    let rereadElapsed = milliseconds(since: rereadBegan)
                    XCTAssertEqual(reread.map(\.appId), result.map(\.appId))
                    samples.append(SortingSample(phase: "model-filteredGames-same-sort", games: count, sort: sort.rawValue, iteration: iteration, milliseconds: rereadElapsed))
                }
            }
        }

        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        for sample in samples {
            print("SAILOVER_SORT_PROFILE \(String(decoding: try encoder.encode(sample), as: UTF8.self))")
        }
        let attachment = XCTAttachment(data: try encoder.encode(samples), uniformTypeIdentifier: "public.json")
        attachment.name = "steam-sorting-profile"
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    private func milliseconds(since began: ContinuousClock.Instant) -> Double {
        let components = began.duration(to: .now).components
        return Double(components.seconds) * 1_000 + Double(components.attoseconds) / 1_000_000_000_000_000
    }

    private func assertOrder(_ games: [SteamGame], count: Int, sort: SteamLibrarySort, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(games.count, count, file: file, line: line)
        for (left, right) in zip(games, games.dropFirst()) {
            switch sort {
            case .lastPlayed where left.lastPlayed != right.lastPlayed:
                XCTAssertGreaterThan(left.lastPlayed, right.lastPlayed, file: file, line: line)
            case .playtime where left.playtimeMinutes != right.playtimeMinutes:
                XCTAssertGreaterThan(left.playtimeMinutes, right.playtimeMinutes, file: file, line: line)
            default:
                let comparison = left.name.localizedStandardCompare(right.name)
                XCTAssertNotEqual(comparison, .orderedDescending, file: file, line: line)
                if comparison == .orderedSame { XCTAssertLessThan(left.appId, right.appId, file: file, line: line) }
            }
        }
    }
}

private struct SortingSample: Encodable {
    let phase: String
    let games: Int
    let sort: String
    let iteration: Int
    let milliseconds: Double
}

private func sortingLibrary(count: Int) -> [SteamGame] {
    (0..<count).map { index in
        let nameIndex = (index * 37) % count
        let prefix = ["Étoiles", "Portal", "Civilization", "Test game"][nameIndex % 4]
        return SteamGame(
            appId: UInt32(4_000_000_000 + index),
            name: "\(prefix) \(nameIndex / 2)",
            iconHash: "",
            lastPlayed: index % 5 == 0 ? UInt64(1_789_430_000 + index * 300) : 0,
            playtimeMinutes: index % 5 == 0 ? UInt32((index * 173) % 20_000) : 0
        )
    }
}

private actor SortingCredentials: SteamCredentialStore {
    func load() -> SteamCredentials? { SteamCredentials(accountName: "Benchmark account", steamId: 42, refreshToken: "fixture-only-never-sent") }
    func save(_ credentials: SteamCredentials) {}
    func remove() {}
}

private final class SortingSteamSession: SteamSessionProtocol, Sendable {
    let library: [SteamGame]
    init(games: [SteamGame]) { library = games }
    func games() async throws -> [SteamGame] { library }
    func profile() async throws -> SteamProfile { SteamProfile(displayName: nil, avatarUrl: nil) }
    func details(appId: UInt32, language: String) async throws -> SteamGameDetails { throw SortingFailure.unexpectedOperation }
    func download(appId: UInt32, platform: String, language: String, directory: String) async throws -> SteamInstallation { throw SortingFailure.unexpectedOperation }
    func downloadProgress() -> GameTransfer? { nil }
    func cancelDownload() {}
    func launchPlan(appId: UInt32, platform: String, directory: String) async throws -> SteamLaunchPlan { throw SortingFailure.unexpectedOperation }
}

private enum SortingFailure: Error { case unexpectedOperation }
