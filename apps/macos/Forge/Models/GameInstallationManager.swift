import Darwin
import Foundation

/// A storefront's dedicated installation, recorded per account once every file succeeded.
protocol InstallReceipt: Codable, Equatable, Sendable {
    var directory: String { get }
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

    var errorDescription: String? {
        switch self {
        case .unsafeDirectory: L10n.string("model.game.unsafeInstallation")
        case .busy: L10n.string("model.game.installationBusy")
        }
    }
}

/// Removes only a storefront's dedicated installation, using the same lock as Rust.
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

    static func removeInstallation(directory: String, expectedDirectoryName: String, protecting libraryDirectory: URL) throws {
        let manager = FileManager.default
        guard directory.hasPrefix("/"), !expectedDirectoryName.isEmpty,
              !expectedDirectoryName.contains("/"),
              !(directory as NSString).pathComponents.contains("..") else {
            throw GameInstallationFailure.unsafeDirectory
        }
        let candidate = URL(fileURLWithPath: directory, isDirectory: true).standardizedFileURL
        let resolved = candidate.resolvingSymlinksInPath()
        let home = manager.homeDirectoryForCurrentUser.resolvingSymlinksInPath()
        let library = libraryDirectory.resolvingSymlinksInPath()
        let attributes = try manager.attributesOfItem(atPath: candidate.path)
        guard candidate.lastPathComponent == expectedDirectoryName,
              attributes[.type] as? FileAttributeType == .typeDirectory,
              resolved.lastPathComponent == expectedDirectoryName,
              !isAncestor(resolved, of: home), !isAncestor(resolved, of: library),
              resolved.pathComponents.count > 2 else {
            throw GameInstallationFailure.unsafeDirectory
        }
        // Foundation may retain /var or /tmp even after resolving their system
        // links. Normalize both spellings, while rejecting custom redirects.
        guard systemPath(candidate) == systemPath(resolved) else { throw GameInstallationFailure.unsafeDirectory }

        let lockURL = resolved.appending(path: ".forge-install.lock")
        let descriptor = Darwin.open(lockURL.path, O_RDWR | O_CREAT | O_NOFOLLOW | O_NONBLOCK, S_IRUSR | S_IWUSR)
        guard descriptor >= 0 else { throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
        defer { Darwin.close(descriptor) }
        var lockInfo = stat()
        guard fstat(descriptor, &lockInfo) == 0, lockInfo.st_mode & S_IFMT == S_IFREG, lockInfo.st_nlink == 1 else {
            throw GameInstallationFailure.unsafeDirectory
        }
        guard flock(descriptor, LOCK_EX | LOCK_NB) == 0 else { throw GameInstallationFailure.busy }
        defer { flock(descriptor, LOCK_UN) }
        try Task.checkCancellation()
        try manager.removeItem(at: resolved)
    }

    private static func isAncestor(_ ancestor: URL, of descendant: URL) -> Bool {
        descendant.path == ancestor.path || descendant.path.hasPrefix(ancestor.path + "/") || ancestor.path == "/"
    }

    private static func systemPath(_ url: URL) -> String {
        for (alias, target) in [("/var", "/private/var"), ("/tmp", "/private/tmp")] where url.path.hasPrefix(alias + "/") {
            return target + url.path.dropFirst(alias.count)
        }
        return url.path
    }
}
