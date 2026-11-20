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
    /// What the launch started, which its Quit and Force Quit reach even
    /// when the game was edited since.
    enum Launch: Equatable {
        /// A program Wine or the Linux engine runs.
        case program
        /// An app macOS opens.
        case app(CustomAppSession)
    }

    let pid: UInt32
    /// When the program started or macOS opened the app. While macOS opens
    /// it, when the player pressed Play, which tells that launch apart.
    let startedAt: Date
    var isStopping: Bool
    /// An app macOS has not opened yet: it has no pid, and a second click
    /// does not open it twice.
    var isLaunching: Bool = false
    var launch: Launch = .program
}

/// A process as the process table lists it.
struct CustomAppProcess: Equatable {
    let pid: pid_t
    let parent: pid_t
    let startedAt: Date
    /// The path of its executable, empty when it cannot be read.
    let executable: String
}

/// A Mac app macOS lists as running.
struct CustomRunningApp: Equatable {
    let pid: pid_t
    let bundleIdentifier: String?
    let bundle: String?
    let executable: String?
    let launchedAt: Date?
}

/// The app macOS opened for a game.
struct CustomOpenedApp: Equatable {
    let pid: pid_t
    let bundleIdentifier: String?
    /// The bundle it runs from. macOS runs a quarantined app the player has
    /// not moved from a copy elsewhere, such as
    /// `/private/var/folders/…/AppTranslocation/…/d/Example Game.app`.
    let bundle: String?
}

/// What an app game launched: the app the game named, and the app macOS
/// opened for it, which may run from a translocated copy of it.
struct CustomAppSession: Equatable {
    /// How long before Play a process may have started and still be the
    /// game's, since clocks are not exact.
    static let startTolerance: TimeInterval = 1

    let bundle: String
    /// When the player pressed Play. A process of the bundle that started
    /// before, such as a helper that stays resident, is not the game's.
    let pressedAt: Date
    var opened: CustomOpenedApp?

    /// The bundles the game's processes run from: the app it named, and the
    /// copy macOS runs, each as written and with its symlinks resolved, in
    /// the form `comparable(_:)` gives.
    var bundles: [String] {
        var paths: [String] = []
        for bundle in [bundle, opened?.bundle].compactMap(\.self) {
            for path in [bundle, URL(fileURLWithPath: bundle).resolvingSymlinksInPath().path] {
                let form = Self.comparable(path)
                if !paths.contains(form) { paths.append(form) }
            }
        }
        return paths
    }

    /// A path in one form whichever way it is written: macOS reports
    /// /private/var and /private/tmp (proc_pidpath), while a path typed or
    /// resolved by URL reads /var and /tmp, so the /private prefix is dropped
    /// for those, and a trailing slash too.
    static func comparable(_ path: String) -> String {
        var path = path.count > 1 && path.hasSuffix("/") ? String(path.dropLast()) : path
        for root in ["/private/var/", "/private/tmp/", "/private/etc/"] where path.hasPrefix(root) {
            path.removeFirst("/private".count)
        }
        return path
    }

    /// Whether the path is one of its bundles or lies inside one.
    func contains(_ path: String?) -> Bool { contains(path, in: bundles) }

    private func contains(_ path: String?, in bundles: [String]) -> Bool {
        guard let path, !path.isEmpty else { return false }
        let form = Self.comparable(path)
        return bundles.contains { form == $0 || form.hasPrefix($0 + "/") }
    }

    /// Whether macOS lists this app as one of the game's: the app macOS
    /// opened, known by its identifier and bundle, or an app inside its
    /// bundles, such as a helper app.
    func includes(_ app: CustomRunningApp) -> Bool { includes(app, in: bundles) }

    private func includes(_ app: CustomRunningApp, in bundles: [String]) -> Bool {
        if let identifier = opened?.bundleIdentifier, app.bundleIdentifier == identifier,
           contains(app.bundle.map { URL(fileURLWithPath: $0).resolvingSymlinksInPath().path }, in: bundles) {
            return true
        }
        return contains(app.bundle, in: bundles) || contains(app.executable, in: bundles)
    }

    /// The processes the game plays in: the one macOS opened, when it
    /// lives, and its descendants, wherever they run from, and the processes
    /// running from its bundles that started since Play, such as the game a
    /// launcher hands off to before it quits. Never Obelisk itself.
    func processes(pid: pid_t, isAlive: Bool, table: [CustomAppProcess], apps: [CustomRunningApp]) -> Set<pid_t> {
        var found: Set<pid_t> = []
        if pid > 0 {
            if isAlive { found.insert(pid) }
            var children: [pid_t: [pid_t]] = [:]
            for process in table where process.pid != process.parent {
                children[process.parent, default: []].append(process.pid)
            }
            var pending = children[pid] ?? []
            while let child = pending.popLast() {
                guard found.insert(child).inserted else { continue }
                pending += children[child] ?? []
            }
        }

        let since = pressedAt.addingTimeInterval(-Self.startTolerance)
        let roots = bundles   // once per scan: resolving symlinks touches the disk
        var started: [pid_t: Date] = [:]
        for process in table { started[process.pid] = process.startedAt }
        for process in table where process.startedAt >= since && contains(process.executable, in: roots) {
            found.insert(process.pid)
        }
        for app in apps {
            guard let startedAt = started[app.pid] ?? app.launchedAt, startedAt >= since,
                  includes(app, in: roots) else { continue }
            found.insert(app.pid)
        }
        found.remove(getpid())
        return found
    }
}

/// How a game added by hand that is a Mac app reaches macOS: macOS opens it,
/// and its Quit and Force Quit reach every process the game plays in.
struct CustomAppOpening {
    /// Opens an app through LaunchServices, with a file when one is set, and
    /// tells which app received it.
    var open: @MainActor (_ app: URL, _ document: URL?, _ arguments: [String]) async throws -> CustomOpenedApp
        = CustomAppOpening.openWithLaunchServices
    /// The apps macOS lists as running.
    var runningApps: @MainActor () -> [CustomRunningApp] = CustomAppOpening.listRunningApps
    /// Every process the process table lists.
    var processTable: @MainActor () -> [CustomAppProcess] = CustomAppOpening.listProcesses
    /// Asks a running app to quit, as its own Quit menu does, and tells
    /// whether it was asked.
    var terminate: @MainActor (pid_t) -> Bool = { pid in
        NSRunningApplication(processIdentifier: pid)?.terminate() ?? false
    }
    /// Whether one process lives.
    var isAlive: @MainActor (pid_t) -> Bool = { pid in pid > 0 && (kill(pid, 0) == 0 || errno == EPERM) }
    /// Whether a process leads its own process group, which its children
    /// join unless they leave it.
    var leadsProcessGroup: @MainActor (pid_t) -> Bool = { pid in getpgid(pid) == pid }
    /// Sends a signal to one process, or to a process group as a negative pid.
    var signal: @MainActor (pid_t, Int32) -> Void = { pid, signal in _ = kill(pid, signal) }
    /// How long macOS may take to open an app, such as while it asks the
    /// player about it, before Play comes back. An app that opens later still
    /// counts as playing.
    var launchTimeout: Duration = .seconds(120)
    /// How long a Force Quit waits for the app's processes to end before the
    /// game no longer counts as playing anyway.
    var forceQuitGrace: Duration = .seconds(5)

    /// An app already open receives the file, as from Finder, rather than a
    /// second copy of it opening.
    @MainActor static func openWithLaunchServices(app: URL, document: URL?, arguments: [String]) async throws -> CustomOpenedApp {
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.arguments = arguments
        configuration.addsToRecentItems = false
        let running: NSRunningApplication
        if let document {
            running = try await NSWorkspace.shared.open([document], withApplicationAt: app, configuration: configuration)
        } else {
            running = try await NSWorkspace.shared.openApplication(at: app, configuration: configuration)
        }
        return CustomOpenedApp(pid: running.processIdentifier, bundleIdentifier: running.bundleIdentifier,
                               bundle: running.bundleURL?.path)
    }

    @MainActor static func listRunningApps() -> [CustomRunningApp] {
        NSWorkspace.shared.runningApplications.map { app in
            CustomRunningApp(pid: app.processIdentifier, bundleIdentifier: app.bundleIdentifier,
                             bundle: app.bundleURL?.path, executable: app.executableURL?.path,
                             launchedAt: app.launchDate)
        }
    }

    /// The processes the player may signal, with their parent, start time
    /// and executable: also those macOS does not list as apps, such as the
    /// game a launcher starts and hands off to.
    @MainActor static func listProcesses() -> [CustomAppProcess] {
        let capacity = proc_listallpids(nil, 0)
        guard capacity > 0 else { return [] }
        // Room for the processes that start meanwhile.
        var pids = [pid_t](repeating: 0, count: Int(capacity) + 64)
        let count = pids.withUnsafeMutableBytes { buffer in
            proc_listallpids(buffer.baseAddress, Int32(buffer.count))
        }
        var path = [CChar](repeating: 0, count: Int(MAXPATHLEN) * 4)
        var table: [CustomAppProcess] = []
        for pid in pids.prefix(Int(max(0, count))) where pid > 0 {
            var info = proc_bsdinfo()
            let size = Int32(MemoryLayout<proc_bsdinfo>.size)
            guard proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, size) == size else { continue }
            let length = proc_pidpath(pid, &path, UInt32(path.count))
            let executable = length > 0
                ? String(decoding: path.prefix(Int(length)).map { UInt8(bitPattern: $0) }, as: UTF8.self)
                : ""
            let startedAt = Date(timeIntervalSince1970: TimeInterval(info.pbi_start_tvsec)
                + TimeInterval(info.pbi_start_tvusec) / 1_000_000)
            table.append(CustomAppProcess(pid: pid, parent: pid_t(info.pbi_ppid), startedAt: startedAt,
                                          executable: executable))
        }
        return table
    }
}

/// Manages the games added by hand: Windows and Linux programs Obelisk runs in
/// Wine or the Linux engine, and Mac apps macOS opens.
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
    var isDownloadingEngine = false
    var engineDownloadError: String?
    /// Why the last launch failed, shown to the player.
    var launchFailure: String?

    var installedEngineVersion: String? {
        installedVoileRelease(locations: locations)
    }

    var isEngineInstalled: Bool {
        isDefaultWineInstalled(locations: locations)
    }

    func downloadEngine() async {
        guard !isDownloadingEngine else { return }
        isDownloadingEngine = true
        engineDownloadError = nil
        do {
            _ = try await downloadDefaultWineEngine(locations: locations)
            isDownloadingEngine = false
        } catch {
            isDownloadingEngine = false
            engineDownloadError = error.localizedDescription
            customLog.error("Engine download failed: \(error.localizedDescription)")
        }
    }

    /// Observed, so Play turns into Quit as soon as a launch or a quit
    /// changes it.
    private(set) var runningProcesses: [String: CustomGameRunningState] = [:]
    @ObservationIgnored var apps = CustomAppOpening()
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
                 documentPath: String? = nil,
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
            documentPath: documentPath,
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
        ).forgettingProgramSettings()
        games.append(game)
        saveGames()
        return game
    }

    func updateGame(_ updated: CustomGame) {
        let updated = updated.forgettingProgramSettings()
        if let index = games.firstIndex(where: { $0.id == updated.id }) {
            games[index] = updated
            if selectedGame?.id == updated.id {
                selectedGame = updated
            }
            saveGames()
        }
    }

    /// Saves a sheet's copy of the game. The game may have played while the
    /// sheet was open: the playtime and last played it recorded meanwhile stay.
    func saveEdits(_ edited: CustomGame) {
        guard let current = games.first(where: { $0.id == edited.id }) else { return }
        var game = edited
        game.playtimeSeconds = current.playtimeSeconds
        game.lastPlayed = current.lastPlayed
        updateGame(game)
    }

    /// Removes the game. An app open for it stays open, so nothing the
    /// player has not saved is lost: Obelisk only stops tracking it. One
    /// macOS is still opening is asked to quit once it opens.
    func removeGame(_ game: CustomGame) {
        if case .app? = runningProcesses[game.id]?.launch {
            runningProcesses.removeValue(forKey: game.id)
        } else if isRunning(game) {
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

    /// What the player reads before removing the game: an app open for it
    /// stays open.
    func removalMessage(for game: CustomGame) -> String {
        if let state = runningProcesses[game.id], case .app = state.launch, !state.isLaunching {
            return L10n.string("ui.custom.remove_confirm_message_app_running")
        }
        return L10n.string("ui.custom.remove_confirm_message")
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

        // An app needs no Wine engine: macOS opens it.
        if game.isApp {
            launchApp(game)
            return
        }

        // Automatically download the default engine if missing and not using a custom runner
        if game.runnerPath == nil && !isEngineInstalled {
            Task {
                await downloadEngine()
                if isEngineInstalled {
                    launch(game)
                }
            }
            return
        }

        let defaultPrefix = directory
            .appendingPathComponent("Prefixes")
            .appendingPathComponent("Custom")
            .appendingPathComponent(game.id)
            .path

        let prefix = (game.winePrefixPath?.isEmpty == false) ? game.winePrefixPath! : defaultPrefix

        let config = CustomGameLaunchConfig(
            executable: game.executablePath,
            arguments: argumentList(game),
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
            launchFailure = Self.launchFailureMessage(error)
            customLog.error("Failed to launch custom game \(game.title): \(String(describing: error))")
        }
    }

    /// What the player reads when Wine or the Linux engine could not start
    /// the program.
    static func launchFailureMessage(_ error: Error) -> String {
        switch error as? CustomLaunchError {
        case .WineUnavailable: L10n.string("backend.runtime.wineUnavailable")
        case .NotExecutable: L10n.string("ui.custom.launch_not_found")
        default: L10n.string("ui.custom.launch_start_failed")
        }
    }

    /// The launch arguments, split on whitespace.
    private func argumentList(_ game: CustomGame) -> [String] {
        game.launchArguments
            .split(whereSeparator: \.isWhitespace)
            .map(String.init)
    }

    /// Opens an app the way a double-click or Finder's Open With does. Only
    /// a file opened so reaches a sandboxed app, such as an emulator: a path
    /// among its arguments gives it no right to read the file.
    private func launchApp(_ game: CustomGame) {
        if let problem = CustomGame.appProblem(at: game.executablePath) {
            launchFailure = L10n.string(problem)
            return
        }
        if let document = game.document, !FileManager.default.fileExists(atPath: document.path) {
            launchFailure = L10n.string("ui.custom.document_missing")
            return
        }

        let app = URL(fileURLWithPath: game.executablePath)
        let pressedAt = Date.now
        let session = CustomAppSession(bundle: app.path, pressedAt: pressedAt)
        runningProcesses[game.id] = CustomGameRunningState(pid: 0, startedAt: pressedAt, isStopping: false,
                                                           isLaunching: true, launch: .app(session))
        Task {
            do {
                let opened = try await apps.open(app, game.document, argumentList(game))
                var session = session
                session.opened = opened
                didOpenApp(game, session: session, pid: opened.pid, pressedAt: pressedAt)
            } catch {
                guard isThisLaunch(game, pressedAt: pressedAt) else { return }
                runningProcesses.removeValue(forKey: game.id)
                customLog.error("Failed to open app for custom game \(game.title): \(error.localizedDescription)")
                // A game removed meanwhile has no one left to tell.
                guard games.contains(where: { $0.id == game.id }) else { return }
                launchFailure = Self.appLaunchFailureMessage(error, app: app)
            }
        }
        // macOS may never answer: Play comes back rather than wait forever.
        let timeout = apps.launchTimeout
        Task { [weak self] in
            try? await Task.sleep(for: timeout)
            guard let self, runningProcesses[game.id]?.isLaunching == true,
                  runningProcesses[game.id]?.startedAt == pressedAt else { return }
            runningProcesses.removeValue(forKey: game.id)
            customLog.error("macOS has not opened the app for custom game \(game.title) yet")
        }
    }

    /// Whether the game still waits on this launch, or on none: a launch that
    /// timed out may still open its app, but not over a later one.
    private func isThisLaunch(_ game: CustomGame, pressedAt: Date) -> Bool {
        guard let state = runningProcesses[game.id] else { return true }
        return state.isLaunching && state.startedAt == pressedAt
    }

    private func didOpenApp(_ game: CustomGame, session: CustomAppSession, pid: pid_t, pressedAt: Date) {
        // Launched again while macOS opened it.
        guard isThisLaunch(game, pressedAt: pressedAt) else { return }
        guard games.contains(where: { $0.id == game.id }) else {
            // Removed while macOS opened it: it does not play on unseen,
            // unless another game plays in the same app.
            let playsAnotherGame = pid > 0 && runningProcesses.values.contains { $0.pid == UInt32(pid) }
            if !playsAnotherGame {
                quitApp(session, pid: pid)
            }
            customLog.info("Asked the app opened for removed custom game \(game.title) to quit")
            return
        }
        guard pid > 0 else {
            runningProcesses.removeValue(forKey: game.id)
            launchFailure = L10n.string("ui.custom.app_invalid")
            return
        }
        // An app already open for another game, such as an emulator given a
        // second game, received this file: that game's playtime ends here.
        for (otherId, state) in runningProcesses where otherId != game.id && state.pid == UInt32(pid) {
            recordExit(for: otherId, startedAt: state.startedAt)
            runningProcesses.removeValue(forKey: otherId)
        }
        // Its playtime counts from now, not from the click: macOS may have
        // asked the player about the app first.
        runningProcesses[game.id] = CustomGameRunningState(pid: UInt32(pid), startedAt: .now, isStopping: false,
                                                           launch: .app(session))
        customLog.info("Opened app for custom game \(game.title) with PID \(pid)")
    }

    /// macOS explains its own refusals; one that Gatekeeper plausibly made
    /// gets the way past it too. Obelisk never removes the quarantine itself.
    private static func appLaunchFailureMessage(_ error: Error, app: URL) -> String {
        let quarantined = (try? app.resourceValues(forKeys: [.quarantinePropertiesKey]))?.quarantineProperties != nil
        guard isPlausiblyGatekeeper(error, quarantined: quarantined) else { return error.localizedDescription }
        return L10n.string("ui.custom.app_blocked") + "\n\n" + error.localizedDescription
    }

    /// LaunchServices refuses an app it may not launch, and a quarantined one
    /// Gatekeeper stopped with a permission or unexpected error.
    static func isPlausiblyGatekeeper(_ error: Error, quarantined: Bool) -> Bool {
        let noLaunchPermission = -10826, unexpected = -10810, permission = -54
        var next: NSError? = error as NSError
        while let current = next {
            switch (current.domain, current.code) {
            case (NSOSStatusErrorDomain, noLaunchPermission):
                return true
            case (NSOSStatusErrorDomain, unexpected), (NSOSStatusErrorDomain, permission),
                 (NSPOSIXErrorDomain, Int(EPERM)), (NSPOSIXErrorDomain, Int(EACCES)):
                if quarantined { return true }
            default:
                break
            }
            next = current.userInfo[NSUnderlyingErrorKey] as? NSError
        }
        return false
    }

    /// The processes an app game plays in, and which of them macOS lists as
    /// apps. A process of its bundle that ran before Play, such as a helper
    /// that stays resident, is never one.
    private func appProcesses(_ session: CustomAppSession, pid: UInt32) -> (all: Set<pid_t>, apps: [pid_t]) {
        let pid = pid_t(pid)
        let running = apps.runningApps()
        let all = session.processes(pid: pid, isAlive: pid > 0 && apps.isAlive(pid),
                                    table: apps.processTable(), apps: running)
        return (all, running.map(\.pid).filter(all.contains))
    }

    /// Asks the game's apps to quit as from their own Quit menu, so they can
    /// save first. A launcher, or a game it handed off to, has no menu: when
    /// no app answered, the game's processes get SIGTERM.
    private func quitApp(_ session: CustomAppSession, pid: pid_t) {
        let processes = appProcesses(session, pid: UInt32(max(0, pid)))
        let asked = processes.apps.map { apps.terminate($0) }.contains(true)
        guard !asked else { return }
        for process in processes.all {
            apps.signal(process, SIGTERM)
        }
    }

    func stop(_ game: CustomGame) {
        guard let state = runningProcesses[game.id], !state.isLaunching else { return }
        runningProcesses[game.id]?.isStopping = true

        if case .app(let session) = state.launch {
            quitApp(session, pid: pid_t(state.pid))
            return
        }

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

        if case .app(let session) = state.launch {
            forceQuitApp(game, session: session, state: state)
            return
        }

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

    /// Kills every process of the game, as the Force Quit window does, and the
    /// process group of the one macOS opened when it leads its own, which
    /// reaches a game its launcher started from elsewhere. The game plays
    /// until they are gone, or for a short grace at most.
    private func forceQuitApp(_ game: CustomGame, session: CustomAppSession, state: CustomGameRunningState) {
        // An app macOS is still opening has not started playing.
        guard !state.isLaunching else {
            runningProcesses.removeValue(forKey: game.id)
            return
        }
        runningProcesses[game.id]?.isStopping = true
        let pid = pid_t(state.pid)
        let processes = appProcesses(session, pid: state.pid).all
        for process in processes {
            apps.signal(process, SIGKILL)
        }
        if processes.contains(pid), apps.leadsProcessGroup(pid) {
            apps.signal(-pid, SIGKILL)
        }

        let grace = apps.forceQuitGrace
        Task { [weak self] in
            try? await Task.sleep(for: grace)
            guard let self, runningProcesses[game.id]?.startedAt == state.startedAt else { return }
            recordExit(for: game.id, startedAt: state.startedAt)
            runningProcesses.removeValue(forKey: game.id)
            customLog.error("Processes of the app for custom game \(game.title) outlived its Force Quit")
        }
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
        for (gameId, state) in runningProcesses where !state.isLaunching {
            if !isPlaying(state) {
                toRemove.append((gameId, state.startedAt))
            }
        }

        for (id, startedAt) in toRemove {
            recordExit(for: id, startedAt: startedAt)
            runningProcesses.removeValue(forKey: id)
            customLog.info("Custom game \(id) exited")
        }
    }

    /// A program plays while its process lives; an app while any process
    /// of the game does, such as the game its launcher handed off to and quit.
    private func isPlaying(_ state: CustomGameRunningState) -> Bool {
        switch state.launch {
        case .program:
            isCustomProcessRunning(pid: state.pid)
        case .app(let session):
            !appProcesses(session, pid: state.pid).all.isEmpty
        }
    }

    // MARK: - Game Actions

    func gameAction(for game: CustomGame, place: GameActionPlace = .library) -> GameAction {
        if runningProcesses[game.id]?.isLaunching == true {
            return GameAction(
                kind: .waiting(.launch),
                title: L10n.string("model.game.launching"),
                isAvailable: false,
                launches: true,
                place: place
            )
        }
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
                isAvailable: canLaunch(game),
                launches: true,
                isCancellable: false,
                place: place,
                perform: { [weak self] in
                    self?.launch(game)
                }
            )
        }
    }

    /// What a game card's context menu offers first: what its button does,
    /// except that it asks a game already closing to quit again rather than
    /// force it, which the button has its player confirm.
    func menuAction(for game: CustomGame) -> GameAction {
        let action = gameAction(for: game, place: .library)
        guard action.kind == .running(.forceQuit) else { return action }
        return GameAction(
            kind: .running(.quit),
            title: L10n.string("model.game.quit"),
            isAvailable: true,
            launches: false,
            place: .library,
            perform: { [weak self] in
                self?.stop(game)
            }
        )
    }

    /// Whether the game's files are where it says: its program or app, and
    /// the file an app opens.
    func canLaunch(_ game: CustomGame) -> Bool {
        guard FileManager.default.fileExists(atPath: game.executablePath) else { return false }
        guard game.isApp, let document = game.document else { return true }
        return FileManager.default.fileExists(atPath: document.path)
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
            let imgAspect = image.size.width / max(image.size.height, 1)
            if imgAspect > 0.82 {
                ZStack {
                    Image(nsImage: image)
                        .resizable()
                        .scaledToFill()
                        .blur(radius: 20)
                        .opacity(0.45)
                        .overlay(Color.black.opacity(0.25))

                    Image(nsImage: image)
                        .resizable()
                        .scaledToFit()
                        .clipShape(RoundedRectangle(cornerRadius: imgAspect > 1.2 ? 8 : 14, style: .continuous))
                        .overlay {
                            RoundedRectangle(cornerRadius: imgAspect > 1.2 ? 8 : 14, style: .continuous)
                                .strokeBorder(Color.white.opacity(0.25), lineWidth: 1)
                        }
                        .shadow(color: .black.opacity(0.4), radius: 8, y: 4)
                        .padding(imgAspect > 1.2 ? 8 : 14)
                }
            } else {
                Image(nsImage: image)
                    .resizable()
                    .scaledToFill()
            }
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
