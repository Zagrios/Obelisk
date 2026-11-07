import Foundation
import XCTest
@testable import Obelisk

@MainActor
final class CustomGamesModelTests: XCTestCase {
    private var tempDirectory: URL!

    override func setUp() async throws {
        try await super.setUp()
        tempDirectory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: tempDirectory, withIntermediateDirectories: true)
    }

    override func tearDown() async throws {
        try? FileManager.default.removeItem(at: tempDirectory)
        try await super.tearDown()
    }

    func testAddGamePersistsAndReloads() {
        let model = CustomGamesModel(directory: tempDirectory)
        XCTAssertTrue(model.games.isEmpty)

        let game = model.addGame(
            title: "Hades II",
            executablePath: "/Games/Hades2/Hades2.exe",
            launchArguments: "-fullscreen",
            customCoverPath: nil,
            customBannerPath: nil,
            winePrefixPath: "/Prefixes/Custom/Hades2",
            runnerPath: nil,
            engine: .rosetta,
            graphics: .d3dMetal,
            rayTracing: true,
            performanceHud: true,
            renderPercent: 100
        )

        XCTAssertEqual(model.games.count, 1)
        XCTAssertEqual(model.games.first?.title, "Hades II")
        XCTAssertEqual(model.games.first?.executablePath, "/Games/Hades2/Hades2.exe")
        XCTAssertEqual(model.games.first?.launchArguments, "-fullscreen")
        XCTAssertEqual(model.games.first?.engine, .rosetta)
        XCTAssertEqual(model.games.first?.graphics, .d3dMetal)
        XCTAssertTrue(model.games.first?.rayTracing == true)
        XCTAssertTrue(model.games.first?.performanceHud == true)

        // Reload a new model instance pointing to same directory
        let reloadedModel = CustomGamesModel(directory: tempDirectory)
        XCTAssertEqual(reloadedModel.games.count, 1)
        XCTAssertEqual(reloadedModel.games.first?.id, game.id)
        XCTAssertEqual(reloadedModel.games.first?.title, "Hades II")
        XCTAssertEqual(reloadedModel.games.first?.winePrefixPath, "/Prefixes/Custom/Hades2")
    }

    func testUpdateGame() {
        let model = CustomGamesModel(directory: tempDirectory)
        var game = model.addGame(
            title: "Silksong",
            executablePath: "/Games/Silksong/Silksong.exe"
        )

        game.title = "Hollow Knight: Silksong"
        game.launchArguments = "--debug"
        game.graphics = .dxvk
        model.updateGame(game)

        XCTAssertEqual(model.games.first?.title, "Hollow Knight: Silksong")
        XCTAssertEqual(model.games.first?.launchArguments, "--debug")
        XCTAssertEqual(model.games.first?.graphics, .dxvk)
    }

    func testRemoveGame() {
        let model = CustomGamesModel(directory: tempDirectory)
        let game = model.addGame(
            title: "Standalone Game",
            executablePath: "/Games/Game.exe"
        )

        XCTAssertEqual(model.games.count, 1)
        model.selectedGame = game

        model.removeGame(game)
        XCTAssertTrue(model.games.isEmpty)
        XCTAssertNil(model.selectedGame)

        // Ensure persistence file updated
        let reloadedModel = CustomGamesModel(directory: tempDirectory)
        XCTAssertTrue(reloadedModel.games.isEmpty)
    }

    func testSearchFiltering() {
        let model = CustomGamesModel(directory: tempDirectory)
        _ = model.addGame(title: "Celeste", executablePath: "/Games/Celeste.exe")
        _ = model.addGame(title: "Dead Cells", executablePath: "/Games/DeadCells.exe")
        _ = model.addGame(title: "Dark Souls", executablePath: "/Games/DarkSouls.exe")

        model.searchText = "dead"
        XCTAssertEqual(model.sortedGames.map(\.title), ["Dead Cells"])

        model.searchText = "cell"
        XCTAssertEqual(model.sortedGames.map(\.title), ["Dead Cells"])

        model.searchText = "d"
        XCTAssertEqual(model.sortedGames.count, 2)

        model.searchText = ""
        XCTAssertEqual(model.sortedGames.count, 3)
    }

    func testSortingOptions() {
        let model = CustomGamesModel(directory: tempDirectory)
        var g1 = model.addGame(title: "Zelda", executablePath: "/Games/Zelda.exe")
        var g2 = model.addGame(title: "Apex", executablePath: "/Games/Apex.exe")
        var g3 = model.addGame(title: "Metroid", executablePath: "/Games/Metroid.exe")

        g1.playtimeSeconds = 500
        g2.playtimeSeconds = 2000
        g3.playtimeSeconds = 100
        model.updateGame(g1)
        model.updateGame(g2)
        model.updateGame(g3)

        // Sort by name
        model.sortOption = .name
        XCTAssertEqual(model.sortedGames.map(\.title), ["Apex", "Metroid", "Zelda"])

        // Sort by playtime
        model.sortOption = .playtime
        XCTAssertEqual(model.sortedGames.map(\.title), ["Apex", "Zelda", "Metroid"])
    }

    func testHomeLibraryConversion() {
        let model = CustomGamesModel(directory: tempDirectory)
        let g1 = model.addGame(title: "Game 1", executablePath: "/Games/G1.exe")
        let homeLib = model.homeLibrary()

        XCTAssertEqual(homeLib.installed.count, 1)
        XCTAssertEqual(homeLib.installed.first?.title, "Game 1")
        XCTAssertEqual(homeLib.installed.first?.id.section, .custom)
        XCTAssertEqual(homeLib.installed.first?.id.game, g1.id)

        let searchResults = model.homeResults(matching: "Game")
        XCTAssertEqual(searchResults.count, 1)
        XCTAssertEqual(searchResults.first?.title, "Game 1")
    }

    func testFormattedPlaytime() {
        var game = CustomGame(title: "Test", executablePath: "/test.exe", playtimeSeconds: 0)
        XCTAssertEqual(game.formattedPlaytime, L10n.string("ui.game.never_played"))

        game.playtimeSeconds = 45 * 60
        XCTAssertEqual(game.formattedPlaytime, "45m")

        game.playtimeSeconds = 3600 * 2 + 15 * 60
        XCTAssertEqual(game.formattedPlaytime, "2h 15m")
    }

    func testCopyArtwork() throws {
        let model = CustomGamesModel(directory: tempDirectory)
        let sampleImageURL = tempDirectory.appendingPathComponent("sample.png")
        try "fake image data".data(using: .utf8)?.write(to: sampleImageURL)

        let gameId = UUID().uuidString
        let copiedPath = model.copyArtwork(from: sampleImageURL, for: gameId, kind: "cover")

        XCTAssertNotNil(copiedPath)
        XCTAssertTrue(FileManager.default.fileExists(atPath: copiedPath!))
        XCTAssertTrue(copiedPath!.hasSuffix("cover.png"))
    }
}
