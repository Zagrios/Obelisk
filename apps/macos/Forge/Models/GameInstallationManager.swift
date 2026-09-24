import Foundation

/// A storefront's dedicated installation, recorded per account once every file succeeded.
protocol InstallReceipt: Codable, Equatable, Sendable, Identifiable {
    associatedtype GameID: Hashable & Sendable
    var gameID: GameID { get }
    var platform: String { get }
    var directory: String { get }
    var totalBytes: UInt64 { get }
    /// A Transfer started over this installation and has not succeeded since.
    var needsVerification: Bool? { get set }
}

/// One JSON receipt file per store and account. Receipts whose directory has
/// vanished are dropped on load: the game was removed outside Forge.
enum InstallationReceipts {
    static func load<Receipt: InstallReceipt>(_: Receipt.Type, from file: URL) throws -> [Receipt] {
        guard FileManager.default.fileExists(atPath: file.path) else { return [] }
        let receipts = try JSONDecoder().decode([Receipt].self, from: Data(contentsOf: file))
        return retainingExistingDirectories(receipts)
    }

    static func save<Receipt: InstallReceipt>(_ receipts: [Receipt], to file: URL) throws {
        try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try JSONEncoder().encode(receipts).write(to: file, options: .atomic)
    }

    static func retainingExistingDirectories<Receipt: InstallReceipt>(_ receipts: [Receipt]) -> [Receipt] {
        receipts.filter { FileManager.default.directoryExists(atPath: $0.directory) }
    }
}

extension FileManager {
    func directoryExists(atPath path: String) -> Bool {
        var isDirectory: ObjCBool = false
        return fileExists(atPath: path, isDirectory: &isDirectory) && isDirectory.boolValue
    }
}

enum GameInstallationFailure: LocalizedError {
    case unsafeDirectory
    case busy
    case downloadDirectory(Error)
    case uninstall(Error)

    var errorDescription: String? {
        switch self {
        case .unsafeDirectory: L10n.string("model.game.unsafeInstallation")
        case .busy: L10n.string("model.game.installationBusy")
        case let .downloadDirectory(error): L10n.string("model.game.downloadDirectoryFailed", error.localizedDescription)
        case let .uninstall(error): L10n.string("model.game.uninstallFailed", error.localizedDescription)
        }
    }
}

/// Removes only a storefront's dedicated installation, then records the receipts left.
enum GameInstallationManager {
    /// Removes the game directory, then records `remaining` as the account's receipts.
    static func uninstall<Receipt: Encodable & Sendable>(directory: String, expectedDirectoryName: String,
                                                         protecting libraryDirectory: URL,
                                                         receiptFile: URL, remaining: [Receipt]) async throws {
        let operation = Task.detached(priority: .utility) {
            try Task.checkCancellation()
            try removeInstallation(directory: directory, expectedDirectoryName: expectedDirectoryName,
                                   protecting: libraryDirectory)
            // Finish the captured account's receipt update even if it signs out
            // while the filesystem is removing the game.
            try JSONEncoder().encode(remaining).write(to: receiptFile, options: .atomic)
        }
        try await withTaskCancellationHandler {
            try await operation.value
        } onCancel: {
            operation.cancel()
        }
    }

    /// Rust removes the folder under the lock every Transfer and launch of it takes.
    static func removeInstallation(directory: String, expectedDirectoryName: String, protecting libraryDirectory: URL) throws {
        do {
            try removeGameInstallation(directory: directory, expectedDirectoryName: expectedDirectoryName,
                                       protected: [libraryDirectory.path, NSHomeDirectory()])
        } catch RemovalError.Unsafe {
            throw GameInstallationFailure.unsafeDirectory
        } catch RemovalError.Busy {
            throw GameInstallationFailure.busy
        } catch let RemovalError.Storage(message) {
            throw CocoaError(.fileWriteUnknown, userInfo: [NSLocalizedDescriptionKey: message])
        }
    }
}
