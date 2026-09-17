import Foundation
import Observation

@MainActor
enum SteamGameLauncher {
    static func launch(_ plan: SteamLaunchPlan, credentials: SteamCredentials, onEvent: @escaping SteamGameEventHandler) async throws {
        guard plan.platform == "macos" else { throw GameLaunchFailure.windowsRuntimeUnavailable }
        let target = plan.target
        let api = try await Task.detached(priority: .userInitiated) {
            try steamAPI(for: target)
        }.value
        try Task.checkCancellation()
        let attempt = SteamNativeLaunch(credentials: credentials)
        let home = URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true)
        let configuration = SteamNativeConfiguration(
            appId: plan.appId,
            helpers: helpersDirectory.path,
            package: Bundle.main.bundleURL.appending(path: "Contents/Resources/SteamRuntime/runtime.zip").path,
            cache: home.appending(path: "Library/Caches/com.zagrios.forge/SteamRuntime").path,
            diagnostics: home.appending(path: "Library/Logs/Forge/Steam").path,
            steamApi: api.path,
            home: home.path
        )
        let supervisedTarget = try await Task.detached(priority: .userInitiated) {
            try NativeGameLauncher.supervisedTarget(plan.target)
        }.value
        let session: SteamGameSession
        do {
            session = try await withTaskCancellationHandler {
                try await attempt.startGame(configuration: configuration, target: supervisedTarget, sessionDirectory: sessionDirectory)
            } onCancel: { attempt.cancel() }
        } catch {
            // A lost acknowledgement must not release guards if handoff already
            // created a supervisor. Recovery resolves that ambiguity first.
            SteamGameSessions.shared.recover()
            throw error
        }
        try await withTaskCancellationHandler {
            try await observe(session, onEvent: onEvent)
        } onCancel: { Task.detached { session.cancelPreparation() } }
    }

    static var sessionDirectory: String {
        AppIdentity.libraryDirectory.appending(path: "SteamSessions").path
    }

    static var helpersDirectory: URL {
        Bundle.main.bundleURL.appending(path: "Contents/Helpers/SteamNative", directoryHint: .isDirectory)
    }

    static var helper: String {
        helpersDirectory.appending(path: "forge-steam-host").path
    }

    static func observe(_ session: any SteamGameSessionProtocol, initial: SteamGameSnapshot? = nil,
                        onEvent: @escaping SteamGameEventHandler) async throws {
        var pending = initial
        var revision: UInt64?
        while true {
            let snapshot: SteamGameSnapshot
            do {
                if let first = pending { snapshot = first; pending = nil }
                else {
                    let after = revision
                    snapshot = try await Task.detached { try await session.snapshot(after: after) }.value
                }
            } catch {
                // Control loss is not evidence of game exit. Cancellation after
                // spawn does not release the installation guards either.
                onEvent(.connection(.reconnecting))
                continue
            }
            revision = snapshot.revision
            onEvent(.connection(snapshot.connection))
            onEvent(.host(snapshot.host))
            if snapshot.finished {
                if let error = snapshot.error { throw SteamError.Failure(reason: error) }
                if let code = snapshot.exitCode, code != 0 { throw SteamError.Failure(reason: "model.steam.gameExited") }
                onEvent(.phase(snapshot.phase))
                return
            }
            onEvent(.phase(snapshot.phase))
        }
    }

    // Low-level process fixtures exercise bundle/argument handling without an account.
    #if DEBUG
    static func launchForTesting(_ plan: SteamLaunchPlan, onEvent: @escaping SteamGameEventHandler = { _ in }) async throws {
        let launched = try await NativeGameLauncher.spawn(
            plan.target, platform: plan.platform, nativePlatform: "macos",
            environment: ["SteamAppId": String(plan.appId), "SteamGameId": String(plan.appId)], ownershipToken: nil
        )
        try await awaitExit(launched, onEvent: onEvent)
    }

    /// After Process.run succeeds, cancellation only cancels preparation. Own the
    /// process until its actual exit, including on sign-out.
    private static func awaitExit(_ launched: LaunchedGameProcess,
                                  onEvent: @escaping SteamGameEventHandler) async throws {
        defer { withExtendedLifetime(launched.process) {} }
        onEvent(.phase(.running))
        if await launched.exit.wait() != 0 { throw SteamError.Failure(reason: "model.steam.gameExited") }
    }
    #endif

    private nonisolated static func steamAPI(for target: LaunchTarget) throws -> URL {
        let root = URL(fileURLWithPath: target.executable).resolvingSymlinksInPath()
        let directory = target.isApplicationBundle ? root : root.deletingLastPathComponent()
        guard let entries = FileManager.default.enumerator(at: directory,
            includingPropertiesForKeys: [.isRegularFileKey, .isSymbolicLinkKey], options: [.skipsHiddenFiles]) else {
            throw SteamError.Failure(reason: "backend.steam.runtime.apiMissing")
        }
        var inspected = 0
        for case let entry as URL in entries {
            try Task.checkCancellation()
            inspected += 1
            if inspected > 20_000 { break }
            let attributes = try entry.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey])
            if attributes.isSymbolicLink == true { entries.skipDescendants(); continue }
            if entry.lastPathComponent == "libsteam_api.dylib", attributes.isRegularFile == true {
                let canonical = entry.resolvingSymlinksInPath()
                guard canonical.path.hasPrefix(directory.path + "/") else { continue }
                return canonical
            }
        }
        throw SteamError.Failure(reason: "backend.steam.runtime.apiMissing")
    }
}

@MainActor
enum EpicGameLauncher {
    static func launch(_ plan: EpicLaunchPlan) async throws {
        _ = try await NativeGameLauncher.spawn(
            plan.target, platform: plan.platform, nativePlatform: "Mac", environment: [:], ownershipToken: plan.ownershipToken
        )
    }
}

/// Storefront arguments remain literal process arguments. No shell interprets them.
@MainActor
private enum NativeGameLauncher {
    static func spawn(_ target: LaunchTarget, platform: String, nativePlatform: String,
                      environment: [String: String], ownershipToken: Data?) async throws -> LaunchedGameProcess {
        try Task.checkCancellation()
        let worker = Task.detached(priority: .userInitiated) {
            try run(platform: platform, nativePlatform: nativePlatform, path: target.executable, arguments: target.arguments,
                    workingDirectory: target.workingDirectory, isApplicationBundle: target.isApplicationBundle,
                    environment: environment, ownershipToken: ownershipToken)
        }
        return try await withTaskCancellationHandler {
            try await worker.value
        } onCancel: {
            worker.cancel()
        }
    }

    nonisolated static func supervisedTarget(_ target: LaunchTarget) throws -> SteamSupervisedTarget {
        let executable = try resolveExecutable(path: target.executable, isApplicationBundle: target.isApplicationBundle)
        return SteamSupervisedTarget(executable: executable.path, arguments: target.arguments,
            workingDirectory: target.workingDirectory, environment: gameEnvironment())
    }

    private nonisolated static func resolveExecutable(path: String, isApplicationBundle: Bool) throws -> URL {
        let target = URL(fileURLWithPath: path).resolvingSymlinksInPath()
        let executable: URL
        var declaredMachO = false
        if isApplicationBundle {
            guard let bundle = Bundle(url: target), let binary = bundle.executableURL else {
                throw GameLaunchFailure.invalidApplication
            }
            executable = binary.resolvingSymlinksInPath()
            guard executable.path.hasPrefix(target.path + "/") else {
                throw GameLaunchFailure.invalidApplication
            }
            declaredMachO = bundle.executableArchitectures?.isEmpty == false
        } else {
            executable = target
        }
        guard try executable.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile == true else {
            throw GameLaunchFailure.notExecutable
        }
        // Some Steam manifests omit Executable on valid Mac bundle binaries.
        // Repair only the declared Mach-O after verification, which can reset its mode.
        if declaredMachO && !FileManager.default.isExecutableFile(atPath: executable.path) {
            try Task.checkCancellation()
            let attributes = try FileManager.default.attributesOfItem(atPath: executable.path)
            guard let mode = attributes[.posixPermissions] as? NSNumber else {
                throw GameLaunchFailure.notExecutable
            }
            try FileManager.default.setAttributes([.posixPermissions: mode.intValue | 0o100], ofItemAtPath: executable.path)
        }
        guard FileManager.default.isExecutableFile(atPath: executable.path) else {
            throw GameLaunchFailure.notExecutable
        }

        return executable
    }

    private nonisolated static func gameEnvironment() -> [String: String] {
        var environment = ["HOME": NSHomeDirectory(), "USER": NSUserName(), "LOGNAME": NSUserName(),
                           "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "TMPDIR": NSTemporaryDirectory()]
        for key in ["LANG", "LC_ALL", "LC_CTYPE"] {
            if let value = ProcessInfo.processInfo.environment[key] { environment[key] = value }
        }
        return environment
    }

    // Process.run() can wait on macOS executable checks. Keep that work off the UI.
    private nonisolated static func run(platform: String, nativePlatform: String, path: String, arguments: [String],
                                        workingDirectory: String, isApplicationBundle: Bool, environment: [String: String], ownershipToken: Data?) throws -> LaunchedGameProcess {
        try Task.checkCancellation()
        guard platform == nativePlatform else {
            throw GameLaunchFailure.windowsRuntimeUnavailable
        }
        let executable = try resolveExecutable(path: path, isApplicationBundle: isApplicationBundle)

        let process = Process()
        process.executableURL = executable
        process.arguments = arguments
        var ownershipDirectory: URL?
        var launched = false
        defer {
            if !launched, let ownershipDirectory { try? FileManager.default.removeItem(at: ownershipDirectory) }
        }
        if let ownershipToken {
            let directory = FileManager.default.temporaryDirectory.appending(path: "Forge-Epic-\(UUID().uuidString)", directoryHint: .isDirectory)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
            ownershipDirectory = directory
            let token = directory.appending(path: "ownership-token")
            try ownershipToken.write(to: token, options: .atomic)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: token.path)
            process.arguments?.append("-epicovt=\(token.path)")
            process.terminationHandler = { _ in try? FileManager.default.removeItem(at: directory) }
        }
        process.currentDirectoryURL = URL(fileURLWithPath: workingDirectory, isDirectory: true)
        // Use the game's working directory even for .app bundles. LaunchServices
        // does not expose that option through NSWorkspace.OpenConfiguration.
        process.environment = gameEnvironment().merging(environment) { _, storefrontValue in storefrontValue }
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        let completion = GameProcessExit()
        let cleanup = process.terminationHandler
        process.terminationHandler = { child in
            cleanup?(child)
            completion.finish(child.terminationStatus)
        }
        try Task.checkCancellation()
        try process.run()
        launched = true
        return LaunchedGameProcess(process: process, exit: completion)
    }
}

/// The termination callback can run before the async caller receives the process.
/// Keep its result so both orderings complete without blocking a Swift executor.
private final class GameProcessExit: @unchecked Sendable {
    private let lock = NSLock()
    private var status: Int32?
    private var continuation: CheckedContinuation<Int32, Never>?

    func finish(_ value: Int32) {
        let waiting = lock.withLock {
            status = value
            let waiting = continuation
            continuation = nil
            return waiting
        }
        waiting?.resume(returning: value)
    }

    func wait() async -> Int32 {
        await withCheckedContinuation { waiting in
            let completed = lock.withLock { () -> Int32? in
                if let status { return status }
                continuation = waiting
                return nil
            }
            if let completed { waiting.resume(returning: completed) }
        }
    }
}

private struct LaunchedGameProcess: Sendable {
    let process: Process
    let exit: GameProcessExit
}

extension SteamGameSnapshot {
    var finished: Bool { phase == .finished || phase == .failed }
}

enum SteamGameEvent: Sendable {
    case phase(SteamGamePhase)
    case connection(SteamSupervisorConnection)
    case host(SteamHostState)
}

typealias SteamGameEventHandler = @MainActor @Sendable (SteamGameEvent) -> Void

/// One Steam session for this Forge process, independent of account/UI lifetime.
@MainActor @Observable
final class SteamGameSessions {
    static let shared = SteamGameSessions(recoverExisting: true)
    struct Session {
        let id: UUID
        let appId: UInt32
        var phase = SteamGamePhase.preparing
        var connection = SteamSupervisorConnection.connected
        var running: Bool { phase == .running }
        var host = SteamHostState.online
    }
    private(set) var active: Session?
    private(set) var isRecovering = false
    private(set) var recoveryError: String?
    @ObservationIgnored private var recoveryTask: Task<Void, Never>?
    @ObservationIgnored private let recoverSession: @MainActor () async throws -> (any SteamGameSessionProtocol)?
    var isBusy: Bool { isRecovering || active != nil }

    init(recoverExisting: Bool = false,
         recoverSession: @escaping @MainActor () async throws -> (any SteamGameSessionProtocol)? = {
             try await recoverSteamGameSession(sessionDirectory: SteamGameLauncher.sessionDirectory, helper: SteamGameLauncher.helper)
         }) {
        self.recoverSession = recoverSession
        if recoverExisting { recover() }
    }

    func recover() {
        guard recoveryTask == nil else { return }
        isRecovering = true
        recoveryTask = Task {
            defer { recoveryTask = nil }
            while true {
                do {
                    guard let client = try await recoverSession() else {
                        isRecovering = false
                        recoveryError = nil
                        return
                    }
                    let snapshot = try await client.snapshot(after: nil)
                    if snapshot.finished { continue }
                    guard let id = UUID(uuidString: snapshot.id) else { throw SteamError.Failure(reason: "backend.steam.runtime.invalidSession") }
                    active = Session(id: id, appId: snapshot.appId, phase: snapshot.phase, connection: snapshot.connection, host: snapshot.host)
                    isRecovering = false
                    recoveryError = nil
                    do {
                        try await SteamGameLauncher.observe(client, initial: snapshot) { event in
                            self.receive(event, for: id)
                        }
                    } catch { recoveryError = SteamModel.message(error) }
                    release(id)
                    return
                } catch {
                    recoveryError = L10n.string("model.steam.recoveringSession")
                    // Native recovery paces failed attempts.
                    await Task.yield()
                }
            }
        }
    }

    func reserve(appId: UInt32) -> UUID? {
        guard !isBusy else { return nil }
        let id = UUID()
        recoveryError = nil
        active = Session(id: id, appId: appId)
        return id
    }

    func receive(_ event: SteamGameEvent, for id: UUID) {
        guard active?.id == id else { return }
        // Snapshots repeat unchanged fields; only real changes should invalidate views.
        switch event {
        case .phase(let phase) where active?.phase != phase: active?.phase = phase
        case .connection(let state) where active?.connection != state: active?.connection = state
        case .host(let state) where active?.host != state: active?.host = state
        default: break
        }
    }

    func release(_ id: UUID) {
        if active?.id == id { active = nil }
    }

    func protects(_ appId: UInt32?) -> Bool {
        if isRecovering { return true }
        guard let appId else { return false }
        return active?.appId == appId
    }
}

enum GameLaunchFailure: LocalizedError {
    case windowsRuntimeUnavailable
    case invalidApplication
    case notExecutable

    var errorDescription: String? {
        switch self {
        case .windowsRuntimeUnavailable:
            L10n.string("model.launch.windowsRuntimeUnavailable")
        case .invalidApplication:
            L10n.string("model.launch.invalidApplication")
        case .notExecutable:
            L10n.string("model.launch.notExecutable")
        }
    }
}
