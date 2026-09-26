import Foundation
import XCTest
@testable import SailOver

final class CoreIntegrationTests: XCTestCase {
    func testCorruptCatalogPreservesDataAndExplainsFailure() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let file = root.appending(path: "library.json")
        let original = Data("invalid JSON".utf8)
        try original.write(to: file)
        let core = CoreClient(storageDirectory: root.path)
        do {
            _ = try await core.games()
            XCTFail("A corrupt catalog must never appear as an empty library")
        } catch {
            XCTAssertEqual(error.localizedDescription, L10n.string("model.library.corruptLibrary"))
        }
        XCTAssertEqual(try Data(contentsOf: file), original)
    }

    func testLibraryRoundTripAcrossRustBridge() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let executable = root.appending(path: "A game.exe")
        try Data([0x4d, 0x5a]).write(to: executable)
        let directory = root.appending(path: "catalog").path
        let first = CoreClient(storageDirectory: directory)

        let added = try await first.addLocalGame(path: executable.path)
        XCTAssertEqual(added.title, "A game")
        XCTAssertEqual(added.kind, .windowsExecutable)
        XCTAssertFalse(added.canLaunch)

        let reopened = CoreClient(storageDirectory: directory)
        let reloaded = try await reopened.games()
        XCTAssertEqual(reloaded.map(\.id), [added.id])

        try await reopened.removeGame(id: added.id)
        let remaining = try await reopened.games()
        XCTAssertTrue(remaining.isEmpty)
        XCTAssertTrue(FileManager.default.fileExists(atPath: executable.path))
    }

    @MainActor
    func testImportSearchAndStoreIsolation() async throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let executable = root.appending(path: "Étoiles.exe")
        try Data([0x4d, 0x5a]).write(to: executable)
        let suite = "SailOverTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let model = AppModel(directory: root.appending(path: "catalog"), defaults: defaults)

        await model.addGame(at: executable)
        XCTAssertNil(model.errorMessage)
        XCTAssertEqual(model.selection, .other)
        model.searchText = "etoile"
        XCTAssertEqual(model.filteredGames.count, 1)
        model.searchText = "missing"
        XCTAssertTrue(model.filteredGames.isEmpty)
        model.selection = .steam
        XCTAssertTrue(model.filteredGames.isEmpty)
        XCTAssertTrue(model.searchText.isEmpty)
        model.appearance = .dark
        let reopened = AppModel(directory: root.appending(path: "catalog"), defaults: defaults)
        XCTAssertEqual(reopened.appearance, .dark)
    }
}
