import Foundation

/// An installation action that failed, as the player reads it. Reasons are
/// localization keys.
enum GameInstallationFailure: LocalizedError {
    case downloadDirectory(String)
    case uninstall(String)

    var errorDescription: String? {
        switch self {
        case let .downloadDirectory(message): L10n.string("model.game.downloadDirectoryFailed", message)
        case let .uninstall(reason): L10n.string("model.game.uninstallFailed", L10n.message(reason))
        }
    }
}
