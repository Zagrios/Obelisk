import Foundation

@MainActor
enum SteamGameLauncher {
    static func start(_ plan: SteamLaunchPlan, credentials: SteamCredentials) async throws -> any SteamGameSessionProtocol {
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
        try Task.checkCancellation()
        do {
            return try await withTaskCancellationHandler {
                try await attempt.startGame(configuration: configuration, target: supervisedTarget, sessionDirectory: sessionDirectory)
            } onCancel: { attempt.cancel() }
        } catch {
            // The supervisor may have accepted the game before its reply was lost.
            // The session owner must resolve this before releasing its reservation.
            throw SteamGameHandoffFailure(cause: error)
        }
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

/// A failed native handoff is ambiguous until durable recovery completes.
struct SteamGameHandoffFailure: Error {
    let cause: any Error
}

// Used only by disposable-process launch fixtures.
enum SteamGameEvent: Sendable {
    case phase(SteamGamePhase)
}

typealias SteamGameEventHandler = @MainActor @Sendable (SteamGameEvent) -> Void

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
