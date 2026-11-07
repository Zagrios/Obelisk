import AppKit
import Foundation
import Observation
import OSLog
import SwiftUI

private let customLog = Logger(subsystem: AppIdentity.storageIdentifier, category: "CustomGames")

/// Sort orders for the custom games library.
enum CustomGameSortOption: String, CaseIterable, Identifiable {
    case lastPlayed, name, playtime

    var id: String { rawValue }

    var title: String {
        switch self {
        case .lastPlayed: L10n.string("model.library.sort.lastPlayed")
        case .name: L10n.string("model.library.sort.name")
        case .playtime: L10n.string("model.library.sort.playtime")
        }
    }
}

/// The state of a currently running custom game process.
struct CustomGameRunningState: Equatable {
    let pid: UInt32
    let startedAt: Date
    var isStopping: Bool
}

/// Manages standalone/non-store Windows executables in Obelisk.
@MainActor @Observable
final class CustomGamesModel {
    private(set) var games: [CustomGame] = []
    var selectedGame: CustomGame?
    var searchText: String = ""
    var sortOption: CustomGameSortOption = .lastPlayed
    var isAddingGame = false
    var editingGame: CustomGame?
    var settingsGame: CustomGame?
    var gamePendingRemoval: CustomGame?

    @ObservationIgnored private(set) var runningProcesses: [String: CustomGameRunningState] = [:]
    @ObservationIgnored let directory: URL
    @ObservationIgnored let locations: ObeliskLocations
    @ObservationIgnored private var monitorTask: Task<Void, Never>?

    private var storageFile: URL {
        directory.appendingPathComponent("custom_games.json")
    }

    private var artworkDirectory: URL {
        directory.appendingPathComponent("CustomArtwork")
    }

    init(directory: URL = AppIdentity.libraryDirectory, locations: ObeliskLocations = .live) {
        self.directory = directory
        self.locations = locations
        loadGames()
        startProcessMonitor()
    }

    deinit {
        monitorTask?.cancel()
    }

    // MARK: - Persistence

    private func loadGames() {
        guard FileManager.default.fileExists(atPath: storageFile.path) else { return }
        do {
            let data = try Data(contentsOf: storageFile)
            games = try JSONDecoder().decode([CustomGame].self, from: data)
        } catch {
            customLog.error("Failed to load custom games: \(error.localizedDescription)")
        }
    }

    func saveGames() {
        do {
            let data = try JSONEncoder().encode(games)
            try data.write(to: storageFile, options: [.atomic])
        } catch {
            customLog.error("Failed to save custom games: \(error.localizedDescription)")
        }
    }

    // MARK: - Game Management

    func addGame(title: String,
                 executablePath: String,
                 launchArguments: String = "",
                 customCoverPath: String? = nil,
                 customBannerPath: String? = nil,
                 winePrefixPath: String? = nil,
                 runnerPath: String? = nil,
                 engine: LaunchEngine = .rosetta,
                 graphics: LaunchGraphics = .d3dMetal,
                 rayTracing: Bool = true,
                 performanceHud: Bool = false,
                 renderPercent: UInt32 = 100,
                 environmentVariables: [String: String] = [:]) -> CustomGame {
        let game = CustomGame(
            title: title,
            executablePath: executablePath,
            launchArguments: launchArguments,
            customCoverPath: customCoverPath,
            customBannerPath: customBannerPath,
            winePrefixPath: winePrefixPath,
            runnerPath: runnerPath,
            engine: engine,
            graphics: graphics,
            rayTracing: rayTracing,
            performanceHud: performanceHud,
            renderPercent: renderPercent,
            environmentVariables: environmentVariables
        )
        games.append(game)
        saveGames()
        return game
    }

    func updateGame(_ updated: CustomGame) {
        if let index = games.firstIndex(where: { $0.id == updated.id }) {
            games[index] = updated
            if selectedGame?.id == updated.id {
                selectedGame = updated
            }
            saveGames()
        }
    }

    func removeGame(_ game: CustomGame) {
        if isRunning(game) {
            forceQuit(game)
        }
        games.removeAll { $0.id == game.id }
        if selectedGame?.id == game.id {
            selectedGame = nil
        }
        saveGames()

        // Clean up custom artwork folder if exists
        let gameArtwork = artworkDirectory.appendingPathComponent(game.id)
        try? FileManager.default.removeItem(at: gameArtwork)
    }

    func copyArtwork(from sourceURL: URL, for gameId: String, kind: String) -> String? {
        let folder = artworkDirectory.appendingPathComponent(gameId)
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let destination = folder.appendingPathComponent("\(kind).\(sourceURL.pathExtension)")
        try? FileManager.default.removeItem(at: destination)
        do {
            try FileManager.default.copyItem(at: sourceURL, to: destination)
            return destination.path
        } catch {
            customLog.error("Failed to copy artwork: \(error.localizedDescription)")
            return nil
        }
    }

    // MARK: - Execution Management

    func isRunning(_ game: CustomGame) -> Bool {
        runningProcesses[game.id] != nil
    }

    func isStopping(_ game: CustomGame) -> Bool {
        runningProcesses[game.id]?.isStopping == true
    }

    func launch(_ game: CustomGame) {
        guard !isRunning(game) else { return }

        let defaultPrefix = directory
            .appendingPathComponent("Prefixes")
            .appendingPathComponent("Custom")
            .appendingPathComponent(game.id)
            .path

        let prefix = (game.winePrefixPath?.isEmpty == false) ? game.winePrefixPath! : defaultPrefix

        // Parse launch arguments
        let args = game.launchArguments
            .split(whereSeparator: \.isWhitespace)
            .map(String.init)

        let config = CustomGameLaunchConfig(
            executable: game.executablePath,
            arguments: args,
            prefix: prefix,
            runner: (game.runnerPath?.isEmpty == false) ? game.runnerPath : nil,
            engine: game.engine,
            graphics: game.graphics,
            rayTracing: game.rayTracing,
            performanceHud: game.performanceHud,
            renderPercent: game.renderPercent,
            environment: game.environmentVariables
        )

        do {
            let pid = try launchCustomExecutable(config: config, locations: locations)
            runningProcesses[game.id] = CustomGameRunningState(pid: pid, startedAt: .now, isStopping: false)
            customLog.info("Launched custom game \(game.title) with PID \(pid)")
        } catch {
            customLog.error("Failed to launch custom game \(game.title): \(error.localizedDescription)")
        }
    }

    func stop(_ game: CustomGame) {
        guard let state = runningProcesses[game.id] else { return }
        runningProcesses[game.id]?.isStopping = true

        let defaultPrefix = directory
            .appendingPathComponent("Prefixes")
            .appendingPathComponent("Custom")
            .appendingPathComponent(game.id)
            .path
        let prefix = (game.winePrefixPath?.isEmpty == false) ? game.winePrefixPath! : defaultPrefix

        do {
            try stopCustomExecutable(
                pid: state.pid,
                prefix: prefix,
                runner: (game.runnerPath?.isEmpty == false) ? game.runnerPath : nil,
                locations: locations
            )
        } catch {
            customLog.error("Failed to stop custom game \(game.title): \(error.localizedDescription)")
        }
    }

    func forceQuit(_ game: CustomGame) {
        guard let state = runningProcesses[game.id] else { return }

        let defaultPrefix = directory
            .appendingPathComponent("Prefixes")
            .appendingPathComponent("Custom")
            .appendingPathComponent(game.id)
            .path
        let prefix = (game.winePrefixPath?.isEmpty == false) ? game.winePrefixPath! : defaultPrefix

        try? forceQuitCustomExecutable(
            pid: state.pid,
            prefix: prefix,
            runner: (game.runnerPath?.isEmpty == false) ? game.runnerPath : nil,
            locations: locations
        )

        recordExit(for: game.id, startedAt: state.startedAt)
        runningProcesses.removeValue(forKey: game.id)
    }

    private func recordExit(for gameId: String, startedAt: Date) {
        let duration = Date.now.timeIntervalSince(startedAt)
        if let index = games.firstIndex(where: { $0.id == gameId }) {
            games[index].playtimeSeconds += UInt64(max(0, duration))
            games[index].lastPlayed = .now
            if selectedGame?.id == gameId {
                selectedGame = games[index]
            }
            saveGames()
        }
    }

    private func startProcessMonitor() {
        monitorTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(500))
                self?.checkRunningProcesses()
            }
        }
    }

    private func checkRunningProcesses() {
        var toRemove: [(id: String, startedAt: Date)] = []
        for (gameId, state) in runningProcesses {
            if !isCustomProcessRunning(pid: state.pid) {
                toRemove.append((gameId, state.startedAt))
            }
        }

        for (id, startedAt) in toRemove {
            recordExit(for: id, startedAt: startedAt)
            runningProcesses.removeValue(forKey: id)
            customLog.info("Custom game \(id) exited")
        }
    }

    // MARK: - Game Actions

    func gameAction(for game: CustomGame, place: GameActionPlace = .library) -> GameAction {
        if isRunning(game) {
            let isStopping = isStopping(game)
            return GameAction(
                kind: .running(isStopping ? .forceQuit : .quit),
                title: isStopping ? L10n.string("model.game.closing") : L10n.string("model.game.quit"),
                isAvailable: true,
                launches: false,
                isCancellable: false,
                place: place,
                perform: { [weak self] in
                    if isStopping {
                        self?.forceQuit(game)
                    } else {
                        self?.stop(game)
                    }
                }
            )
        } else {
            return GameAction(
                kind: .launch,
                title: L10n.string("ui.game.play"),
                isAvailable: FileManager.default.fileExists(atPath: game.executablePath),
                launches: true,
                isCancellable: false,
                place: place,
                perform: { [weak self] in
                    self?.launch(game)
                }
            )
        }
    }

    // MARK: - Library & Search

    var sortedGames: [CustomGame] {
        let filtered = searchText.isEmpty
            ? games
            : games.filter { $0.title.localizedCaseInsensitiveContains(searchText) }

        switch sortOption {
        case .lastPlayed:
            return filtered.sorted { ($0.lastPlayed ?? .distantPast) > ($1.lastPlayed ?? .distantPast) }
        case .name:
            return filtered.sorted { $0.title.localizedCaseInsensitiveCompare($1.title) == .orderedAscending }
        case .playtime:
            return filtered.sorted { $0.playtimeSeconds > $1.playtimeSeconds }
        }
    }

    func homeGame(_ game: CustomGame) -> HomeGame {
        let action = gameAction(for: game, place: .library)
        return HomeGame(
            id: HomeGame.ID(section: .custom, game: game.id),
            title: game.title,
            lastPlayed: game.lastPlayed,
            playtime: Duration.seconds(Int64(game.playtimeSeconds)),
            action: action,
            cover: { AnyView(CustomGameCoverView(game: game)) },
            hero: HomeGame.Hero(
                artwork: { AnyView(CustomGameBannerView(game: game)) },
                logo: { AnyView(CustomGameLogoView(game: game)) }
            ),
            open: { [weak self] in
                self?.selectedGame = game
            }
        )
    }

    func homeLibrary() -> HomeLibrary {
        let byLastPlayed = games.sorted { ($0.lastPlayed ?? .distantPast) > ($1.lastPlayed ?? .distantPast) }
        let recentGames = byLastPlayed.filter { $0.lastPlayed != nil }.prefix(HomeShelves.shelfLimit + 1).map { homeGame($0) }
        let installedGames = byLastPlayed.map { homeGame($0) }
        let featured = byLastPlayed.first.map { homeGame($0) }
        let covers = byLastPlayed.prefix(3).map { homeGame($0) }

        return HomeLibrary(
            featured: featured,
            recent: Array(recentGames),
            installed: installedGames,
            verified: [],
            covers: Array(covers)
        )
    }

    func homeResults(matching query: String) -> [HomeGame] {
        games
            .filter { $0.title.localizedCaseInsensitiveContains(query) }
            .sorted { $0.title.localizedCaseInsensitiveCompare($1.title) == .orderedAscending }
            .map { homeGame($0) }
    }
}

// MARK: - Artwork Views

struct CustomGameCoverView: View {
    let game: CustomGame

    var body: some View {
        if let path = game.customCoverPath,
           let image = NSImage(contentsOfFile: path) {
            Image(nsImage: image)
                .resizable()
                .scaledToFill()
        } else {
            ZStack {
                LinearGradient(
                    colors: [TileColors.violet[0], TileColors.violet[1]],
                    startPoint: .topLeading,
                    endPoint: .bottomTrailing
                )
                VStack(spacing: 8) {
                    Image(systemName: "gamecontroller.fill")
                        .font(.system(size: 28))
                        .foregroundStyle(.white.opacity(0.85))
                    Text(verbatim: game.title)
                        .font(.system(size: 13, weight: .bold))
                        .foregroundStyle(.white)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, 10)
                        .lineLimit(3)
                }
            }
        }
    }
}

struct CustomGameBannerView: View {
    let game: CustomGame

    var body: some View {
        if let path = game.customBannerPath,
           let image = NSImage(contentsOfFile: path) {
            Image(nsImage: image)
                .resizable()
                .scaledToFill()
        } else if let path = game.customCoverPath,
                  let image = NSImage(contentsOfFile: path) {
            Image(nsImage: image)
                .resizable()
                .scaledToFill()
                .blur(radius: 20)
                .overlay(Color.black.opacity(0.3))
        } else {
            LinearGradient(
                colors: [TileColors.violet[0].opacity(0.8), TileColors.violet[1].opacity(0.9)],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        }
    }
}

struct CustomGameLogoView: View {
    let game: CustomGame

    var body: some View {
        Text(verbatim: game.title)
            .font(.system(size: 32, weight: .black))
            .foregroundStyle(.white)
            .shadow(color: .black.opacity(0.6), radius: 8, x: 0, y: 4)
    }
}
