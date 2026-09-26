import Foundation

/// A storefront's dedicated installation, recorded per account once every
/// file succeeded. Rust keeps the account's receipts.
protocol InstallReceipt: Codable, Equatable, Sendable, Identifiable where ID == String {
    associatedtype GameID: Hashable & Sendable
    var gameID: GameID { get }
    var platform: String { get }
    var directory: String { get }
    var totalBytes: UInt64 { get }
    /// A Transfer started over this installation and has not succeeded since.
    var needsVerification: Bool? { get set }
}

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
