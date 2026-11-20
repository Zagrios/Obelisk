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
        game.graphics = .dxmt
        model.updateGame(game)

        XCTAssertEqual(model.games.first?.title, "Hollow Knight: Silksong")
        XCTAssertEqual(model.games.first?.launchArguments, "--debug")
        XCTAssertEqual(model.games.first?.graphics, .dxmt)
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

    func testAnEditKeepsThePlaytimeRecordedSinceItsSheetOpened() {
        let model = CustomGamesModel(directory: tempDirectory)
        let opened = model.addGame(title: "Example Game", executablePath: "/Games/Game.exe")
        var played = opened
        played.playtimeSeconds = 600
        played.lastPlayed = .now
        model.updateGame(played)

        var edited = opened
        edited.title = "Example Game: Renamed"
        model.saveEdits(edited)

        XCTAssertEqual(model.games.first?.title, "Example Game: Renamed")
        XCTAssertEqual(model.games.first?.playtimeSeconds, 600)
        XCTAssertNotNil(model.games.first?.lastPlayed)
    }

    func testProgramLaunchFailureReadsAsText() {
        XCTAssertEqual(CustomGamesModel.launchFailureMessage(CustomLaunchError.NotExecutable),
                       L10n.string("ui.custom.launch_not_found"))
        XCTAssertEqual(CustomGamesModel.launchFailureMessage(CustomLaunchError.WineUnavailable),
                       L10n.string("backend.runtime.wineUnavailable"))
        XCTAssertEqual(CustomGamesModel.launchFailureMessage(CustomLaunchError.Failed(message: "spawn")),
                       L10n.string("ui.custom.launch_start_failed"))
    }

    // MARK: - Apps

    func testCatalogWithoutDocumentStillLoads() throws {
        let saved = """
        [{"id":"A","title":"Example Game","executablePath":"/Games/Game.exe","launchArguments":"",\
        "engine":"rosetta","graphics":"d3dMetal","rayTracing":true,"performanceHud":false,\
        "renderPercent":100,"environmentVariables":{},"dateAdded":0,"playtimeSeconds":0}]
        """
        try Data(saved.utf8).write(to: tempDirectory.appendingPathComponent("custom_games.json"))

        let model = CustomGamesModel(directory: tempDirectory)

        XCTAssertEqual(model.games.map(\.id), ["A"])
        XCTAssertNil(model.games.first?.documentPath)
        XCTAssertFalse(model.games.first?.isApp == true)
    }

    func testAppIsRecognisedByItsBundle() {
        XCTAssertTrue(CustomGame.isApp(path: "/Games/Example Game.app"))
        XCTAssertTrue(CustomGame.isApp(path: "/Games/Example Game 1.2.app"))
        XCTAssertTrue(CustomGame.isApp(path: "/Applications/Emulator.app/"))
        XCTAssertFalse(CustomGame.isApp(path: "/Games/Game.exe"))
        XCTAssertFalse(CustomGame.isApp(path: "/Games/start.sh"))
        XCTAssertFalse(CustomGame.isApp(path: "/Games/game.x86_64"))
        XCTAssertFalse(CustomGame.isApp(path: "/Games/app"))
    }

    func testPlainFileNamedLikeAnAppIsAProgram() throws {
        let file = tempDirectory.appendingPathComponent("Example Game.app")
        try Data("#!/bin/sh\n".utf8).write(to: file)
        XCTAssertFalse(CustomGame.isApp(path: file.path))

        let bundle = try makeApp(named: "Example Port")
        XCTAssertTrue(CustomGame.isApp(path: bundle.path))
    }

    func testAnAppForgetsWhatOnlyAProgramUses() throws {
        let model = CustomGamesModel(directory: tempDirectory)
        let app = try makeApp(named: "Example Game")
        let added = model.addGame(title: "Example Game", executablePath: app.path,
                                  winePrefixPath: "/Games/Prefix", runnerPath: "/Games/Wine",
                                  engine: .linux, graphics: .dxmt, renderPercent: 50,
                                  environmentVariables: ["EXAMPLE": "1"])
        XCTAssertNil(added.winePrefixPath)
        XCTAssertNil(model.games.first?.runnerPath)
        XCTAssertEqual(model.games.first?.engine, .rosetta)
        XCTAssertEqual(model.games.first?.graphics, .d3dMetal)
        XCTAssertEqual(model.games.first?.renderPercent, 100)
        XCTAssertEqual(model.games.first?.environmentVariables, [:])

        // A program edited into an app forgets its prefix too.
        let program = model.addGame(title: "Example Program", executablePath: "/Games/Game.exe",
                                    winePrefixPath: "/Games/Prefix")
        XCTAssertEqual(program.winePrefixPath, "/Games/Prefix")
        var edited = program
        edited.executablePath = app.path
        model.saveEdits(edited)
        XCTAssertNil(model.games.first { $0.id == program.id }?.winePrefixPath)
    }

    func testPathsMatchWithOrWithoutThePrivatePrefix() {
        // proc_pidpath reports /private/var and /private/tmp; a path typed or
        // resolved by URL reads /var and /tmp. Both name the same bundle.
        let typed = CustomAppSession(bundle: "/tmp/Example Game.app", pressedAt: .now)
        XCTAssertTrue(typed.contains("/private/tmp/Example Game.app/Contents/MacOS/game-main"))
        var opened = CustomAppSession(bundle: "/Applications/Example Game.app", pressedAt: .now)
        opened.opened = CustomOpenedApp(pid: 4000, bundleIdentifier: "com.example.game",
                                        bundle: "/var/folders/xy/T/AppTranslocation/0000/d/Example Game.app")
        XCTAssertTrue(opened.contains("/private/var/folders/xy/T/AppTranslocation/0000/d/Example Game.app/Contents/MacOS/game"))
        XCTAssertFalse(opened.contains("/private/var/folders/xy/T/Other.app/Contents/MacOS/game"))
    }

    func testAProgramOnADiskThatIsNotConnectedKeepsItsSettings() {
        var game = CustomGame(title: "Example Program", executablePath: "/Volumes/Missing/Example Game.app")
        game.winePrefixPath = "/Games/Prefix"
        game.renderPercent = 50
        let saved = game.forgettingProgramSettings()
        XCTAssertEqual(saved.winePrefixPath, "/Games/Prefix")
        XCTAssertEqual(saved.renderPercent, 50)
    }

    func testAppNamesItself() throws {
        let app = try makeApp(named: "Example Game (v1.2)", info: ["CFBundleDisplayName": "Example Game"])

        XCTAssertEqual(CustomGame.appTitle(at: app.path), "Example Game")
        XCTAssertNil(CustomGame.appProblem(at: app.path))
    }

    func testAppsMacOSOpensAreNotRefused() throws {
        // An executable that is a script has no architecture to check.
        let scripted = try makeApp(named: "Scripted Game")
        let script = scripted.appendingPathComponent("Contents/MacOS/game")
        try FileManager.default.removeItem(at: script)
        try Data("#!/bin/sh\nexit 0\n".utf8).write(to: script)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: script.path)
        XCTAssertNil(CustomGame.appProblem(at: scripted.path))

        // An iPhone or iPad app wrapped for the Mac has no Contents of its own.
        let wrapped = tempDirectory.appendingPathComponent("Wrapped Game.app")
        let inner = try makeApp(named: "Inner", info: ["CFBundleDisplayName": "Wrapped Game"])
        try FileManager.default.createDirectory(at: wrapped.appendingPathComponent("Wrapper"),
                                                withIntermediateDirectories: true)
        try FileManager.default.moveItem(at: inner, to: wrapped.appendingPathComponent("Wrapper/Inner.app"))
        XCTAssertNil(CustomGame.appProblem(at: wrapped.path))
        XCTAssertEqual(CustomGame.appTitle(at: wrapped.path), "Wrapped Game")
    }

    func testAppLaunchOpensItsDocumentWithArguments() async throws {
        let app = try makeApp(named: "Emulator")
        let document = tempDirectory.appendingPathComponent("Example Game.rom")
        try Data().write(to: document)
        let fakes = AppFakes()
        let model = makeModel(fakes)
        let game = model.addGame(title: "Example Game", executablePath: app.path,
                                 launchArguments: "--fullscreen -v", documentPath: document.path)

        model.launch(game)
        XCTAssertEqual(model.gameAction(for: game).kind, .waiting(.launch))
        await waitUntil { model.runningProcesses[game.id]?.isLaunching == false }

        XCTAssertEqual(fakes.opened.count, 1)
        XCTAssertEqual(fakes.opened.first?.app.path, app.path)
        XCTAssertEqual(fakes.opened.first?.document?.path, document.path)
        XCTAssertEqual(fakes.opened.first?.arguments, ["--fullscreen", "-v"])
        XCTAssertEqual(model.runningProcesses[game.id]?.pid, UInt32(fakes.pid))
        guard case .app(let session)? = model.runningProcesses[game.id]?.launch else {
            return XCTFail("The game should run as an app")
        }
        XCTAssertEqual(session.bundle, app.path)
        XCTAssertEqual(session.opened?.pid, fakes.pid)
        XCTAssertEqual(model.gameAction(for: game).kind, .running(.quit))
        XCTAssertFalse(model.isDownloadingEngine)
        XCTAssertNil(model.launchFailure)
    }

    func testPlaytimeCountsFromWhenMacOSOpenedTheApp() async throws {
        let app = try makeApp(named: "Example Game")
        let fakes = AppFakes()
        fakes.openDelay = .milliseconds(300)
        let model = makeModel(fakes)
        let game = model.addGame(title: "Example Game", executablePath: app.path)

        let pressed = Date.now
        model.launch(game)
        await waitUntil { model.runningProcesses[game.id]?.isLaunching == false }

        let startedAt = try XCTUnwrap(model.runningProcesses[game.id]?.startedAt)
        XCTAssertGreaterThanOrEqual(startedAt.timeIntervalSince(pressed), 0.25)
    }

    func testAppLaunchFailureIsShown() async throws {
        let app = try makeApp(named: "Example Game")
        let fakes = AppFakes()
        fakes.failure = CocoaError(.fileReadNoPermission)
        let model = makeModel(fakes)
        let game = model.addGame(title: "Example Game", executablePath: app.path)

        model.launch(game)
        await waitUntil { model.launchFailure != nil }

        XCTAssertFalse(model.isRunning(game))
        XCTAssertEqual(model.gameAction(for: game).kind, .launch)
        // Not quarantined, and not a refusal Gatekeeper makes.
        XCTAssertFalse(model.launchFailure?.contains(L10n.string("ui.custom.app_blocked")) == true)
    }

    func testLateFailureOfARemovedGameShowsNothing() async throws {
        let app = try makeApp(named: "Example Game")
        let fakes = AppFakes()
        fakes.openDelay = .milliseconds(100)
        fakes.failure = CocoaError(.fileReadNoPermission)
        let model = makeModel(fakes)
        let game = model.addGame(title: "Example Game", executablePath: app.path)

        model.launch(game)
        model.removeGame(game)
        try await Task.sleep(for: .milliseconds(400))

        XCTAssertNil(model.launchFailure)
        XCTAssertFalse(model.isRunning(game))
    }

    func testAppOpenedForARemovedGameIsAskedToQuit() async throws {
        let app = try makeApp(named: "Example Game")
        let fakes = AppFakes()
        fakes.openDelay = .milliseconds(100)
        let model = makeModel(fakes)
        let game = model.addGame(title: "Example Game", executablePath: app.path)

        model.launch(game)
        model.removeGame(game)
        await waitUntil { !fakes.terminated.isEmpty }

        XCTAssertEqual(fakes.terminated, [fakes.pid])
        XCTAssertFalse(model.isRunning(game))
    }

    func testRemovingAGameLeavesItsOpenAppRunning() async throws {
        let app = try makeApp(named: "Example Game")
        let fakes = AppFakes()
        let model = makeModel(fakes)
        let game = model.addGame(title: "Example Game", executablePath: app.path)
        XCTAssertEqual(model.removalMessage(for: game), L10n.string("ui.custom.remove_confirm_message"))
        model.launch(game)
        await waitUntil { model.runningProcesses[game.id]?.isLaunching == false }

        XCTAssertEqual(model.removalMessage(for: game), L10n.string("ui.custom.remove_confirm_message_app_running"))
        model.removeGame(game)
        try await Task.sleep(for: .milliseconds(100))

        XCTAssertTrue(model.games.isEmpty)
        XCTAssertFalse(model.isRunning(game))
        XCTAssertTrue(fakes.terminated.isEmpty)
        XCTAssertTrue(fakes.signals.isEmpty)
    }

    func testAppThatMacOSNeverOpensGivesPlayBack() async throws {
        let app = try makeApp(named: "Example Game")
        let fakes = AppFakes()
        fakes.openDelay = .milliseconds(300)
        let model = makeModel(fakes)
        model.apps.launchTimeout = .milliseconds(20)
        let game = model.addGame(title: "Example Game", executablePath: app.path)

        model.launch(game)
        await waitUntil { !model.isRunning(game) }
        XCTAssertEqual(model.gameAction(for: game).kind, .launch)

        // It opened after all: it plays.
        await waitUntil { model.isRunning(game) }
        XCTAssertEqual(model.gameAction(for: game).kind, .running(.quit))
    }

    func testContextMenuWaitsWhileMacOSOpensTheApp() async throws {
        let app = try makeApp(named: "Example Game")
        let fakes = AppFakes()
        fakes.openDelay = .milliseconds(200)
        let model = makeModel(fakes)
        let game = model.addGame(title: "Example Game", executablePath: app.path)

        model.launch(game)
        let launching = model.menuAction(for: game)
        XCTAssertEqual(launching.kind, .waiting(.launch))
        XCTAssertEqual(launching.title, L10n.string("model.game.launching"))
        XCTAssertFalse(launching.isAvailable)

        await waitUntil { model.runningProcesses[game.id]?.isLaunching == false }
        XCTAssertEqual(model.menuAction(for: game).kind, .running(.quit))

        // A game already closing is asked again, never forced from the menu.
        model.stop(game)
        let closing = model.menuAction(for: game)
        XCTAssertEqual(model.gameAction(for: game).kind, .running(.forceQuit))
        XCTAssertEqual(closing.kind, .running(.quit))
        closing.perform()
        XCTAssertEqual(fakes.terminated.count, 2)
        XCTAssertTrue(fakes.signals.isEmpty)
    }

    func testGatekeeperAdviceOnlyForItsRefusals() {
        let noPermission = NSError(domain: NSOSStatusErrorDomain, code: -10826)
        let unexpected = NSError(domain: NSCocoaErrorDomain, code: 0, userInfo: [
            NSUnderlyingErrorKey: NSError(domain: NSOSStatusErrorDomain, code: -10810),
        ])
        let missing = NSError(domain: NSOSStatusErrorDomain, code: -10827)

        XCTAssertTrue(CustomGamesModel.isPlausiblyGatekeeper(noPermission, quarantined: false))
        XCTAssertTrue(CustomGamesModel.isPlausiblyGatekeeper(unexpected, quarantined: true))
        XCTAssertFalse(CustomGamesModel.isPlausiblyGatekeeper(unexpected, quarantined: false))
        XCTAssertFalse(CustomGamesModel.isPlausiblyGatekeeper(missing, quarantined: true))
    }

    func testAppThatIsNotABundleIsRefusedBeforeOpening() {
        let notAnApp = tempDirectory.appendingPathComponent("Broken.app")
        try? FileManager.default.createDirectory(at: notAnApp, withIntermediateDirectories: true)
        let fakes = AppFakes()
        let model = makeModel(fakes)
        let game = model.addGame(title: "Broken", executablePath: notAnApp.path)

        XCTAssertEqual(CustomGame.appProblem(at: notAnApp.path), "ui.custom.app_invalid")
        model.launch(game)

        XCTAssertEqual(model.launchFailure, L10n.string("ui.custom.app_invalid"))
        XCTAssertTrue(fakes.opened.isEmpty)
        XCTAssertFalse(model.isRunning(game))
    }

    func testAppWithMissingDocumentCannotLaunch() throws {
        let app = try makeApp(named: "Emulator")
        let document = tempDirectory.appendingPathComponent("Missing.rom")
        let fakes = AppFakes()
        let model = makeModel(fakes)
        let game = model.addGame(title: "Missing", executablePath: app.path, documentPath: document.path)

        XCTAssertFalse(model.gameAction(for: game).isAvailable)
        model.launch(game)
        XCTAssertEqual(model.launchFailure, L10n.string("ui.custom.document_missing"))
        XCTAssertTrue(fakes.opened.isEmpty)

        try Data().write(to: document)
        XCTAssertTrue(model.gameAction(for: game).isAvailable)
    }

    func testAppQuitAsksTheAppToQuitThenItsLauncher() async throws {
        let app = try makeApp(named: "Example Game")
        let fakes = AppFakes()
        fakes.appsAccepted = false
        let model = makeModel(fakes)
        let game = model.addGame(title: "Example Game", executablePath: app.path)
        model.launch(game)
        await waitUntil { model.runningProcesses[game.id]?.isLaunching == false }

        model.stop(game)

        XCTAssertEqual(fakes.terminated, [fakes.pid])
        XCTAssertEqual(fakes.signals.map(\.signal), [SIGTERM])
        XCTAssertEqual(fakes.signals.first?.pid, fakes.pid)
        XCTAssertTrue(model.isStopping(game))
        XCTAssertEqual(model.gameAction(for: game).kind, .running(.forceQuit))
    }

    func testQuitReachesTheAppItLaunchedAfterAnEdit() async throws {
        let app = try makeApp(named: "Example Game")
        let fakes = AppFakes()
        let model = makeModel(fakes)
        var game = model.addGame(title: "Example Game", executablePath: app.path)
        model.launch(game)
        await waitUntil { model.runningProcesses[game.id]?.isLaunching == false }
        // The launcher handed off to the game, an app inside its bundle.
        fakes.alive.remove(fakes.pid)
        fakes.start(5001, at: app.appendingPathComponent("Contents/Helpers/Game.app"), asApp: true)

        game.executablePath = "/Games/Game.exe"
        model.updateGame(game)
        model.stop(game)

        XCTAssertEqual(fakes.terminated, [5001])
        XCTAssertTrue(fakes.signals.isEmpty)
    }

    func testHandOffLauncherKeepsTheGamePlaying() async throws {
        let app = try makeApp(named: "Example Game")
        let fakes = AppFakes()
        let model = makeModel(fakes)
        let game = model.addGame(title: "Example Game", executablePath: app.path)
        model.launch(game)
        await waitUntil { model.runningProcesses[game.id]?.isLaunching == false }

        // The launcher started the game from inside its bundle, then quit.
        fakes.start(5001, at: app.appendingPathComponent("Contents/MacOS/game-main"))
        fakes.alive = []
        try await Task.sleep(for: .milliseconds(800))
        XCTAssertTrue(model.isRunning(game))
        XCTAssertNil(model.games.first?.lastPlayed)

        fakes.processes = []
        await waitUntil { !model.isRunning(game) }
        XCTAssertFalse(model.isRunning(game))
        XCTAssertNotNil(model.games.first?.lastPlayed)
    }

    func testAppForceQuitKillsEveryProcessOfItsBundle() async throws {
        let app = try makeApp(named: "Example Game")
        let fakes = AppFakes()
        let model = makeModel(fakes)
        let game = model.addGame(title: "Example Game", executablePath: app.path)
        model.launch(game)
        await waitUntil { model.runningProcesses[game.id]?.isLaunching == false }
        fakes.start(5001, at: app.appendingPathComponent("Contents/MacOS/game-main"))
        fakes.start(5002, at: app.appendingPathComponent("Contents/Helpers/Game.app"), asApp: true)
        fakes.groupLeaders = [fakes.pid]

        model.forceQuit(game)

        let killed = Set(fakes.signals.filter { $0.signal == SIGKILL }.map(\.pid))
        XCTAssertEqual(killed, [5001, 5002, fakes.pid, -fakes.pid])
        XCTAssertEqual(fakes.signals.count, 4)
        // It plays until they are gone.
        XCTAssertTrue(model.isRunning(game))
        XCTAssertTrue(model.isStopping(game))

        fakes.processes = []
        fakes.runningApps = []
        fakes.alive = []
        await waitUntil { !model.isRunning(game) }
        XCTAssertFalse(model.isRunning(game))
        XCTAssertNotNil(model.games.first?.lastPlayed)
    }

    func testAppForceQuitStopsCountingAfterItsGrace() async throws {
        let app = try makeApp(named: "Example Game")
        let fakes = AppFakes()
        let model = makeModel(fakes)
        model.apps.forceQuitGrace = .milliseconds(50)
        let game = model.addGame(title: "Example Game", executablePath: app.path)
        model.launch(game)
        await waitUntil { model.runningProcesses[game.id]?.isLaunching == false }

        // A process Force Quit cannot end, such as one stuck in the kernel.
        model.forceQuit(game)
        await waitUntil { !model.isRunning(game) }

        XCTAssertFalse(model.isRunning(game))
        XCTAssertNotNil(model.games.first?.lastPlayed)
    }

    func testTranslocatedAppIsTrackedWhereMacOSRunsIt() async throws {
        let app = try makeApp(named: "Example Game")
        let translocated = URL(fileURLWithPath: "/private/var/folders/xy/T/AppTranslocation/0000-1111/d/Example Game.app")
        let fakes = AppFakes()
        fakes.runningBundle = translocated
        let model = makeModel(fakes)
        let game = model.addGame(title: "Example Game", executablePath: app.path)
        model.launch(game)
        await waitUntil { model.runningProcesses[game.id]?.isLaunching == false }
        guard case .app(let session)? = model.runningProcesses[game.id]?.launch else {
            return XCTFail("The game should run as an app")
        }
        XCTAssertEqual(session.opened?.bundle, translocated.path)

        // Its launcher hands off to the game and its helper app in the
        // translocated copy, then quits.
        fakes.start(5001, at: translocated.appendingPathComponent("Contents/MacOS/game-main"))
        fakes.start(5002, at: translocated.appendingPathComponent("Contents/Helpers/Game.app"), asApp: true)
        fakes.alive.remove(fakes.pid)
        try await Task.sleep(for: .milliseconds(800))
        XCTAssertTrue(model.isRunning(game))

        // Quit asks the app that runs to quit, rather than signal it.
        model.stop(game)
        XCTAssertEqual(fakes.terminated, [5002])
        XCTAssertTrue(fakes.signals.isEmpty)

        fakes.processes = []
        fakes.runningApps = []
        await waitUntil { !model.isRunning(game) }
        XCTAssertNotNil(model.games.first?.lastPlayed)
    }

    func testHelpersRunningBeforePlayAreNotTheGames() async throws {
        let app = try makeApp(named: "Example Game")
        let fakes = AppFakes()
        fakes.appsAccepted = false
        // A helper of the bundle that stays resident, started an hour before.
        fakes.start(5003, at: app.appendingPathComponent("Contents/Library/LoginItems/Helper.app"),
                    asApp: true, startedAt: .now.addingTimeInterval(-3600))
        let model = makeModel(fakes)
        let game = model.addGame(title: "Example Game", executablePath: app.path)
        model.launch(game)
        await waitUntil { model.runningProcesses[game.id]?.isLaunching == false }
        // The app's own child, which runs from elsewhere, and a process of
        // the bundle started since Play.
        fakes.start(5004, at: URL(fileURLWithPath: "/bin/sh"), parent: fakes.pid)
        fakes.start(5001, at: app.appendingPathComponent("Contents/MacOS/game-main"))

        model.stop(game)
        XCTAssertEqual(fakes.terminated.sorted(), [fakes.pid])
        XCTAssertEqual(Set(fakes.signals.map(\.pid)), [fakes.pid, 5001, 5004])
        XCTAssertEqual(Set(fakes.signals.map(\.signal)), [SIGTERM])

        fakes.signals = []
        model.forceQuit(game)
        XCTAssertEqual(Set(fakes.signals.map(\.pid)), [fakes.pid, 5001, 5004])

        // Once the game's processes are gone, the resident helper does not
        // keep it playing.
        fakes.alive.remove(fakes.pid)
        fakes.processes.removeAll { $0.pid != 5003 }
        fakes.runningApps.removeAll { $0.pid != 5003 }
        await waitUntil { !model.isRunning(game) }
        XCTAssertFalse(model.isRunning(game))
    }

    func testSecondGameOnTheSameAppTakesTheSession() async throws {
        let app = try makeApp(named: "Emulator")
        let first = tempDirectory.appendingPathComponent("First.rom")
        let second = tempDirectory.appendingPathComponent("Second.rom")
        try Data().write(to: first)
        try Data().write(to: second)
        let fakes = AppFakes()
        let model = makeModel(fakes)
        let one = model.addGame(title: "First", executablePath: app.path, documentPath: first.path)
        let two = model.addGame(title: "Second", executablePath: app.path, documentPath: second.path)

        model.launch(one)
        await waitUntil { model.runningProcesses[one.id]?.isLaunching == false }
        model.launch(two)
        await waitUntil { model.runningProcesses[two.id]?.isLaunching == false }

        XCTAssertFalse(model.isRunning(one))
        XCTAssertTrue(model.isRunning(two))
        XCTAssertNotNil(model.games.first { $0.id == one.id }?.lastPlayed)
    }

    func testDocumentSurvivesAnUpdateAndAReload() throws {
        let model = CustomGamesModel(directory: tempDirectory)
        var game = model.addGame(title: "Example Game", executablePath: "/Applications/Emulator.app",
                                 documentPath: "/Games/Example Game.rom")
        game.launchArguments = "--fullscreen"
        model.updateGame(game)

        let reloaded = CustomGamesModel(directory: tempDirectory)
        XCTAssertEqual(reloaded.games.first?.documentPath, "/Games/Example Game.rom")
        XCTAssertEqual(reloaded.games.first?.launchArguments, "--fullscreen")
        XCTAssertTrue(reloaded.games.first?.isApp == true)
    }

    // MARK: - App helpers

    /// What the model asked of macOS, which the tests never reach: no real
    /// process is opened, asked to quit or signalled.
    @MainActor
    private final class AppFakes {
        /// The pid of the app macOS opens.
        let pid: pid_t = 4242
        /// Where macOS runs the app from, when not where the game says.
        var runningBundle: URL?
        var opened: [(app: URL, document: URL?, arguments: [String])] = []
        var terminated: [pid_t] = []
        var signals: [(pid: pid_t, signal: Int32)] = []
        var alive: Set<pid_t> = []
        var processes: [CustomAppProcess] = []
        var runningApps: [CustomRunningApp] = []
        var groupLeaders: Set<pid_t> = []
        var failure: Error?
        var openDelay: Duration?
        var appsAccepted = true

        /// A process that starts from this executable, or from this app,
        /// which macOS then lists.
        func start(_ pid: pid_t, at path: URL, parent: pid_t = 1, asApp: Bool = false, startedAt: Date = .now) {
            let executable = asApp ? path.appendingPathComponent("Contents/MacOS/game") : path
            processes.append(CustomAppProcess(pid: pid, parent: parent, startedAt: startedAt, executable: executable.path))
            if asApp {
                runningApps.append(CustomRunningApp(pid: pid, bundleIdentifier: "test.obelisk.\(pid)", bundle: path.path,
                                                    executable: executable.path, launchedAt: startedAt))
            }
        }
    }

    private func makeModel(_ fakes: AppFakes) -> CustomGamesModel {
        let model = CustomGamesModel(directory: tempDirectory)
        model.apps = CustomAppOpening(
            open: { app, document, arguments in
                fakes.opened.append((app, document, arguments))
                if let delay = fakes.openDelay { try await Task.sleep(for: delay) }
                if let failure = fakes.failure { throw failure }
                let bundle = fakes.runningBundle ?? app
                fakes.alive.insert(fakes.pid)
                fakes.runningApps.append(CustomRunningApp(pid: fakes.pid, bundleIdentifier: "test.obelisk.game",
                                                          bundle: bundle.path,
                                                          executable: bundle.appendingPathComponent("Contents/MacOS/game").path,
                                                          launchedAt: .now))
                return CustomOpenedApp(pid: fakes.pid, bundleIdentifier: "test.obelisk.game", bundle: bundle.path)
            },
            // macOS lists an app while its process lives.
            runningApps: {
                fakes.runningApps.filter { app in
                    fakes.alive.contains(app.pid) || fakes.processes.contains { $0.pid == app.pid }
                }
            },
            processTable: { fakes.processes },
            terminate: { pid in
                fakes.terminated.append(pid)
                return fakes.appsAccepted
            },
            isAlive: { fakes.alive.contains($0) },
            leadsProcessGroup: { fakes.groupLeaders.contains($0) },
            signal: { pid, signal in fakes.signals.append((pid, signal)) }
        )
        return model
    }

    /// An app bundle whose executable is a copy of an arm64 system tool.
    private func makeApp(named name: String, info: [String: Any] = [:]) throws -> URL {
        let app = tempDirectory.appendingPathComponent("\(name).app")
        let macOS = app.appendingPathComponent("Contents/MacOS")
        try FileManager.default.createDirectory(at: macOS, withIntermediateDirectories: true)
        try FileManager.default.copyItem(at: URL(fileURLWithPath: "/usr/bin/true"),
                                         to: macOS.appendingPathComponent("game"))
        var plist: [String: Any] = [
            "CFBundleExecutable": "game",
            "CFBundleIdentifier": "test.obelisk.\(UUID().uuidString)",
            "CFBundlePackageType": "APPL",
        ]
        plist.merge(info) { _, new in new }
        let data = try PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0)
        try data.write(to: app.appendingPathComponent("Contents/Info.plist"))
        return app
    }

    private func waitUntil(_ condition: @MainActor () -> Bool) async {
        for _ in 0..<300 where !condition() {
            try? await Task.sleep(for: .milliseconds(10))
        }
    }
}
