import Foundation

@MainActor
enum SteamGameLauncher {
    /// Rust resolves the game inside its installation, then hands it to a
    /// supervisor that owns its Steam session.
    static func start(_ plan: SteamLaunchPlan, credentials: SteamCredentials) async throws -> any GameSessionProtocol {
        // Windows games run in SailOver's Wine runtime, which is installed apart from the app.
        let wine = SailOverLocations.live.wineRuntime + "/bin/wine"
        guard plan.platform == "macos" || FileManager.default.isExecutableFile(atPath: wine) else {
            throw GameLaunchFailure.windowsRuntimeUnavailable
        }
        try Task.checkCancellation()
        let attempt = SteamNativeLaunch(credentials: credentials)
        try await withTaskCancellationHandler {
            try await attempt.prepare(plan: plan, locations: .live)
        } onCancel: { attempt.cancel() }
        try Task.checkCancellation()
        do {
            return try await withTaskCancellationHandler {
                try await attempt.startGame()
            } onCancel: { attempt.cancel() }
        } catch {
            // The supervisor may have accepted the game before its reply was lost.
            // The session owner must resolve this before releasing its reservation.
            throw GameHandoffFailure(cause: error)
        }
    }
}

enum GameLaunchFailure: LocalizedError {
    case windowsRuntimeUnavailable

    var errorDescription: String? {
        switch self {
        case .windowsRuntimeUnavailable: L10n.string("model.launch.windowsRuntimeUnavailable")
        }
    }
}
