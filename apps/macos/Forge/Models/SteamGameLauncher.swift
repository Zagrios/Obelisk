import Foundation

@MainActor
enum SteamGameLauncher {
    static func launch(_ plan: SteamLaunchPlan, credentials: SteamCredentials) async throws {
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
            helpers: Bundle.main.bundleURL.appending(path: "Contents/Helpers/SteamNative").path,
            source: home.appending(path: "Library/Application Support/Steam/Steam.AppBundle/Steam/Contents/MacOS").path,
            cache: home.appending(path: "Library/Caches/com.zagrios.forge/SteamRuntime").path,
            diagnostics: home.appending(path: "Library/Logs/Forge/Steam").path,
            steamApi: api.path,
            home: home.path
        )
        let runtime = try await withTaskCancellationHandler {
            try await attempt.prepare(configuration: configuration)
        } onCancel: { attempt.cancel() }
        do {
            try Task.checkCancellation()
            var environment = runtime.environment()
            environment["SteamAppId"] = String(plan.appId)
            environment["SteamGameId"] = String(plan.appId)
            try await NativeGameLauncher.launch(
                plan.target, platform: plan.platform, nativePlatform: "macos",
                environment: environment,
                ownershipToken: nil, steamRuntime: runtime
            )
        } catch {
            runtime.stop(exitCode: nil)
            throw error
        }
    }

    // Low-level process fixtures exercise bundle/argument handling without an account.
    #if DEBUG
    static func launchForTesting(_ plan: SteamLaunchPlan) async throws {
        try await NativeGameLauncher.launch(
            plan.target, platform: plan.platform, nativePlatform: "macos",
            environment: ["SteamAppId": String(plan.appId), "SteamGameId": String(plan.appId)], ownershipToken: nil
        )
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
        try await NativeGameLauncher.launch(
            plan.target, platform: plan.platform, nativePlatform: "Mac", environment: [:], ownershipToken: plan.ownershipToken
        )
    }
}

/// Storefront arguments remain literal process arguments. No shell interprets them.
@MainActor
private enum NativeGameLauncher {
    static func launch(_ target: LaunchTarget, platform: String, nativePlatform: String,
                       environment: [String: String], ownershipToken: Data?, steamRuntime: SteamNativeRuntime? = nil) async throws {
        try Task.checkCancellation()
        let worker = Task.detached(priority: .userInitiated) {
            try run(platform: platform, nativePlatform: nativePlatform, path: target.executable, arguments: target.arguments,
                    workingDirectory: target.workingDirectory, isApplicationBundle: target.isApplicationBundle,
                    environment: environment, ownershipToken: ownershipToken, steamRuntime: steamRuntime)
        }
        try await withTaskCancellationHandler {
            try await worker.value
        } onCancel: {
            worker.cancel()
        }
    }

    // Process.run() can wait on macOS executable checks. Keep that work off the UI.
    private nonisolated static func run(platform: String, nativePlatform: String, path: String, arguments: [String],
                                        workingDirectory: String, isApplicationBundle: Bool, environment: [String: String], ownershipToken: Data?, steamRuntime: SteamNativeRuntime?) throws {
        try Task.checkCancellation()
        guard platform == nativePlatform else {
            throw GameLaunchFailure.windowsRuntimeUnavailable
        }
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

        let process = Process()
        let lifetime = UUID()
        process.executableURL = executable
        process.arguments = arguments
        var ownershipDirectory: URL?
        var launched = false
        defer {
            if !launched, let ownershipDirectory { try? FileManager.default.removeItem(at: ownershipDirectory) }
            if !launched, let steamRuntime {
                steamRuntime.stop(exitCode: nil)
                NativeSteamProcesses.shared.remove(lifetime)
            }
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
        process.environment = [
            "HOME": NSHomeDirectory(),
            "USER": NSUserName(),
            "LOGNAME": NSUserName(),
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            "TMPDIR": NSTemporaryDirectory(),
        ]
        process.environment?.merge(environment) { _, storefrontValue in storefrontValue }
        for key in ["LANG", "LC_ALL", "LC_CTYPE"] {
            if let value = ProcessInfo.processInfo.environment[key] {
                process.environment?[key] = value
            }
        }
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        if let steamRuntime {
            process.terminationHandler = { child in
                steamRuntime.stop(exitCode: child.terminationStatus)
                NativeSteamProcesses.shared.remove(lifetime)
            }
            NativeSteamProcesses.shared.retain(process, id: lifetime)
        }
        try Task.checkCancellation()
        try process.run()
        if let steamRuntime {
            do { try steamRuntime.gameStarted(pid: UInt32(process.processIdentifier)) }
            catch {
                if process.isRunning { process.terminate() }
                throw error
            }
        }
        launched = true
    }
}

/// Retain the process and its runtime until the game's termination callback.
private final class NativeSteamProcesses: @unchecked Sendable {
    static let shared = NativeSteamProcesses()
    private let lock = NSLock()
    private var processes: [UUID: Process] = [:]
    func retain(_ process: Process, id: UUID) { lock.withLock { processes[id] = process } }
    func remove(_ id: UUID) { lock.withLock { _ = processes.removeValue(forKey: id) } }
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
