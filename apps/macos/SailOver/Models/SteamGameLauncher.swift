import Foundation

@MainActor
enum SteamGameLauncher {
    /// Rust resolves the game inside its installation, then hands it to a
    /// supervisor that owns its Steam session.
    static func start(_ plan: SteamLaunchPlan, credentials: SteamCredentials) async throws -> any GameSessionProtocol {
        // Windows games run in SailOver's Wine runtime, installed apart from the app on first use.
        if plan.platform == "windows" { try await VoileRuntime.live.prepare() }
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
    /// Wine, which runs Windows games, could not be installed.
    case wineUnavailable(reason: String)

    var errorDescription: String? {
        switch self {
        case let .wineUnavailable(reason): L10n.message(reason)
        }
    }
}
