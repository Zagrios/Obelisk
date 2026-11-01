import AppKit
import Foundation
import Observation

/// Obelisk's library as Rust moves it.
@MainActor
protocol LibraryFolderBackend: AnyObject {
    /// What moving the library into the folder the player picked would do. It reads the disk.
    func inspect(_ folder: String) -> LibraryFolderInspection
    /// What moving the library to exactly `destination` would do. It reads the disk.
    func inspectDestination(_ destination: String) -> LibraryFolderInspection
    /// Moves the library to `destination` and returns where it went.
    func move(to destination: String) async throws -> String
    func progress() -> LibraryMoveProgress
    func cancel()
}

/// The library in Rust.
final class RustLibraryFolder: LibraryFolderBackend {
    private let core: LibraryFolder

    init(library: URL) {
        core = LibraryFolder(library: library.path)
    }

    func inspect(_ folder: String) -> LibraryFolderInspection { core.inspect(chosen: folder) }
    func inspectDestination(_ destination: String) -> LibraryFolderInspection {
        core.inspectDestination(destination: destination)
    }
    func move(to destination: String) async throws -> String { try await core.moveTo(destination: destination) }
    func progress() -> LibraryMoveProgress { core.progress() }
    func cancel() { core.cancel() }
}

/// A folder the player picked for the library, and what moving it there
/// does, before they agree.
struct LibraryFolderProposal: Identifiable, Equatable {
    let id = UUID()
    /// The folder the player picked; nil for Obelisk's own on this Mac.
    let chosen: URL?
    /// Where the library goes.
    let path: String
    /// What the move copies to another disk.
    let copiedBytes: UInt64
    let volume: String
    let availableBytes: UInt64?
    /// The disk's format when it may not keep everything the library holds, such as ExFAT.
    let format: String?
    let isReadOnly: Bool

    var lacksSpace: Bool { availableBytes.map { $0 < copiedBytes } ?? false }
    /// The library holds links and permissions only Mac formats keep.
    var canChoose: Bool { !isReadOnly && !lacksSpace && format == nil }
}

/// Where Obelisk keeps its library, and the player's move of it: the folder
/// they pick, what moving there does, then the move, after which Obelisk
/// restarts in the new library.
@MainActor @Observable
final class LibraryFolderModel {
    /// The library Obelisk runs with.
    let path: String
    /// Obelisk's own folder on this Mac.
    let isDefault: Bool
    /// Whether the player may move it: not while `OBELISK_LIBRARY_DIRECTORY`
    /// names the library, which wins over the folder a move keeps.
    let isMovable: Bool
    /// The folder the player picked, waiting for them to agree, then moving.
    var proposal: LibraryFolderProposal?
    /// The bytes copied of the total while the library moves.
    private(set) var moving: LibraryMoveProgress?
    /// Why the last move was not made.
    var failure: String?

    /// Quits Obelisk and opens it again, in the new library.
    @ObservationIgnored var restart: () -> Void = {}
    @ObservationIgnored private let backend: any LibraryFolderBackend
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private let defaultDirectory: URL

    init(library: URL, defaults: UserDefaults = .standard,
         defaultDirectory: URL = AppIdentity.defaultLibraryDirectory,
         environment: [String: String] = ProcessInfo.processInfo.environment,
         backend: (any LibraryFolderBackend)? = nil) {
        self.backend = backend ?? RustLibraryFolder(library: library)
        self.defaults = defaults
        self.defaultDirectory = defaultDirectory
        path = library.path
        isDefault = library.standardizedFileURL.path == defaultDirectory.standardizedFileURL.path
        isMovable = (environment["OBELISK_LIBRARY_DIRECTORY"] ?? "").isEmpty
    }

    /// Asks the player to move the library into the folder they picked.
    func propose(_ folder: URL) {
        propose(chosen: folder, backend.inspect(folder.path))
    }

    /// Asks the player to move the library back to Obelisk's own folder on this Mac.
    func proposeDefault() {
        propose(chosen: nil, backend.inspectDestination(defaultDirectory.path))
    }

    private func propose(chosen: URL?, _ inspection: LibraryFolderInspection) {
        failure = nil
        switch inspection {
        case let .ready(path, copiedBytes):
            let volume = GamesFolderProposal.volume(of: chosen ?? defaultDirectory.deletingLastPathComponent())
            proposal = LibraryFolderProposal(
                chosen: chosen, path: path, copiedBytes: copiedBytes, volume: volume.name,
                availableBytes: volume.available, format: volume.format, isReadOnly: volume.readOnly)
        case .missing:
            failure = L10n.string("model.libraryFolder.missing")
        case .inside:
            failure = L10n.string("model.libraryFolder.inside", AppIdentity.name)
        case .occupied:
            failure = L10n.string("model.libraryFolder.occupied")
        case .holdsGames:
            failure = L10n.string("model.libraryFolder.holdsGames", AppIdentity.name)
        }
    }

    /// Moves the library where the player agreed, then restarts Obelisk
    /// there. The library stays where it was if the move fails or is
    /// cancelled.
    func confirm() async {
        guard isMovable, let proposal, proposal.canChoose, moving == nil else { return }
        moving = LibraryMoveProgress(copiedBytes: 0, totalBytes: proposal.copiedBytes)
        let polling = Task { [weak self, backend] in
            while (try? await Task.sleep(for: .milliseconds(250))) != nil {
                self?.moving = backend.progress()
            }
        }
        defer { polling.cancel() }
        do {
            let moved = try await backend.move(to: proposal.path)
            save(moved)
            moving = nil
            self.proposal = nil
            restart()
        } catch GameSessionError.Cancelled {
            moving = nil
        } catch {
            moving = nil
            self.proposal = nil
            failure = L10n.string("model.libraryFolder.failed", L10n.message(Self.reason(error)))
        }
    }

    /// Stops the copy: the library stays where it was.
    func cancel() {
        if moving != nil {
            backend.cancel()
        } else {
            proposal = nil
        }
    }

    /// Obelisk starts in `moved` from now on, and removes the library it
    /// left once it did.
    private func save(_ moved: String) {
        if URL(fileURLWithPath: moved).standardizedFileURL.path == defaultDirectory.standardizedFileURL.path {
            defaults.removeObject(forKey: AppIdentity.libraryDirectoryKey)
        } else {
            defaults.set(moved, forKey: AppIdentity.libraryDirectoryKey)
        }
        defaults.set(path, forKey: AppIdentity.leftLibraryKey)
    }

    private static func reason(_ error: Error) -> String {
        if case let GameSessionError.Failure(reason) = error { return reason }
        return error.localizedDescription
    }
}

/// Obelisk's start with a library the player moved: before anything opens
/// it, the library's disk must be there, and the library it left goes.
@MainActor
enum LibraryStartup {
    /// Whether the library Obelisk starts with is one the player moved, which
    /// Obelisk must never make anew.
    static func isMoved(environment: [String: String], defaults: UserDefaults) -> Bool {
        (environment["OBELISK_LIBRARY_DIRECTORY"] ?? "").isEmpty
            && defaults.string(forKey: AppIdentity.libraryDirectoryKey) != nil
    }

    /// Waits for the moved library's disk, as Obelisk would otherwise start
    /// with an empty library: the player connects it and tries again, quits,
    /// or starts anew on this Mac. False when they quit.
    static func awaitLibrary(_ library: URL, defaults: UserDefaults = .standard) -> Bool {
        guard isMoved(environment: ProcessInfo.processInfo.environment, defaults: defaults) else { return true }
        while !isFolder(library) {
            let alert = NSAlert()
            alert.alertStyle = .warning
            alert.messageText = L10n.string("app.library.missing.title", AppIdentity.name)
            alert.informativeText = L10n.string("app.library.missing.message", AppIdentity.name, library.path)
            alert.addButton(withTitle: L10n.string("app.library.missing.retry"))
            alert.addButton(withTitle: L10n.string("app.library.missing.quit", AppIdentity.name))
            alert.addButton(withTitle: L10n.string("app.library.missing.mac"))
            NSApplication.shared.activate()
            switch alert.runModal() {
            case .alertFirstButtonReturn:
                continue
            case .alertThirdButtonReturn:
                // The player's choice: the moved library stays on its disk.
                defaults.removeObject(forKey: AppIdentity.libraryDirectoryKey)
                defaults.removeObject(forKey: AppIdentity.leftLibraryKey)
                return true
            default:
                return false
            }
        }
        return true
    }

    /// Removes the library Obelisk left for `library` once it started there.
    static func removeLeftLibrary(_ library: URL, defaults: UserDefaults = .standard) {
        guard let left = defaults.string(forKey: AppIdentity.leftLibraryKey) else { return }
        let path = library.path
        guard isFolder(library), left != path else {
            defaults.removeObject(forKey: AppIdentity.leftLibraryKey)
            return
        }
        Task {
            let removed = await Task.detached(priority: .utility) {
                Obelisk.removeLeftLibrary(left: left, library: path)
            }.value
            // What stayed, such as a disk's mount point, goes at a later start.
            if removed { defaults.removeObject(forKey: AppIdentity.leftLibraryKey) }
        }
    }

    static func isFolder(_ url: URL) -> Bool {
        var isDirectory: ObjCBool = false
        return FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory) && isDirectory.boolValue
    }
}
