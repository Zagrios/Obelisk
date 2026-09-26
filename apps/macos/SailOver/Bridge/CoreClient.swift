import Foundation

/// Serializes disk operations and keeps generated Rust types out of the UI.
actor CoreClient {
    private let storageDirectory: String
    private var library: Library?

    init(storageDirectory: String) {
        self.storageDirectory = storageDirectory
    }

    func games() throws -> [LibraryGame] {
        try withLibrary { try $0.games().map(Self.map) }
    }

    func addLocalGame(path: String) throws -> LibraryGame {
        try withLibrary { try Self.map($0.addLocalGame(path: path)) }
    }

    func removeGame(id: String) throws {
        try withLibrary { try $0.removeGame(id: id) }
    }

    func recordLaunch(id: String) throws {
        try withLibrary { try $0.recordLaunch(id: id) }
    }

    private func withLibrary<T>(_ operation: (Library) throws -> T) throws -> T {
        do {
            return try operation(openLibrary())
        } catch let error as CoreError {
            throw LibraryFailure(error: error)
        }
    }

    private func openLibrary() throws -> Library {
        if let library { return library }
        let opened = try Library(storageDir: storageDirectory)
        library = opened
        return opened
    }

    private static func map(_ game: Game) -> LibraryGame {
        LibraryGame(
            id: game.id,
            title: game.title,
            path: game.path,
            addedAt: Date(timeIntervalSince1970: TimeInterval(game.addedAt)),
            lastPlayedAt: game.lastPlayedAt.map { Date(timeIntervalSince1970: TimeInterval($0)) },
            kind: game.kind == .nativeMac ? .nativeMac : .windowsExecutable
        )
    }
}

private struct LibraryFailure: LocalizedError {
    let error: CoreError

    var errorDescription: String? {
        switch error {
        case .InvalidGame:
            L10n.string("model.library.invalidGame")
        case .Storage:
            L10n.string("model.library.storage")
        case .CorruptLibrary:
            L10n.string("model.library.corruptLibrary")
        case .NotFound:
            L10n.string("model.library.notFound")
        }
    }
}
