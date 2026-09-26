import Darwin
import Foundation
import XCTest
@testable import SailOver

final class GameInstallationManagerTests: XCTestCase {
    func testRemovalDeletesOnlyDedicatedGameAndUpdatesCapturedReceipts() async throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let game = try installation(in: root)
        let sibling = try installation(in: root, name: "steam-570-macos")
        let receiptFile = root.appending(path: "receipts.json")
        try Data("old".utf8).write(to: receiptFile)
        try await GameInstallationManager.uninstall(directory: game.path, expectedDirectoryName: game.lastPathComponent,
                                                    protecting: root, receiptFile: receiptFile, remaining: [String]())
        XCTAssertFalse(FileManager.default.fileExists(atPath: game.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: sibling.appending(path: "game.bin").path))
        XCTAssertEqual(try String(contentsOf: receiptFile, encoding: .utf8), "[]")
    }

    func testRefusesUnexpectedDirectoryAndPreservesReceipts() async throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let game = try installation(in: root, name: "Imported folder")
        let receipts = root.appending(path: "receipts.json")
        try Data("original".utf8).write(to: receipts)
        do {
            try await GameInstallationManager.uninstall(directory: game.path, expectedDirectoryName: "steam-620-macos",
                                                        protecting: root, receiptFile: receipts, remaining: [String]())
            XCTFail("An arbitrary imported folder must never be removed")
        } catch { XCTAssertTrue(error is GameInstallationFailure) }
        XCTAssertTrue(FileManager.default.fileExists(atPath: game.appending(path: "game.bin").path))
        XCTAssertEqual(try String(contentsOf: receipts, encoding: .utf8), "original")
    }

    func testRefusesHomeRootLibraryAndParentDirectories() throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let library = root.appending(path: "Library")
        try FileManager.default.createDirectory(at: library, withIntermediateDirectories: true)
        for candidate in [URL(fileURLWithPath: "/"), FileManager.default.homeDirectoryForCurrentUser, root, library] {
            XCTAssertThrowsError(try GameInstallationManager.removeInstallation(
                directory: candidate.path, expectedDirectoryName: candidate.lastPathComponent, protecting: library
            ))
            XCTAssertTrue(FileManager.default.fileExists(atPath: candidate.path))
        }
    }

    func testRefusesSymlinkRootAndParentButDoesNotFollowLinksInsideGame() throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let outside = try installation(in: root, name: "outside")
        let link = root.appending(path: "steam-620-macos")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: outside)
        XCTAssertThrowsError(try GameInstallationManager.removeInstallation(
            directory: link.path, expectedDirectoryName: link.lastPathComponent, protecting: root
        ))
        try FileManager.default.removeItem(at: link)
        let redirect = root.appending(path: "redirect")
        try FileManager.default.createSymbolicLink(at: redirect, withDestinationURL: outside)
        let redirectedGame = try installation(in: outside)
        XCTAssertThrowsError(try GameInstallationManager.removeInstallation(
            directory: redirect.appending(path: redirectedGame.lastPathComponent).path,
            expectedDirectoryName: redirectedGame.lastPathComponent, protecting: root
        ))
        let game = try installation(in: root)
        try FileManager.default.createSymbolicLink(at: game.appending(path: "external"), withDestinationURL: outside)
        try GameInstallationManager.removeInstallation(directory: game.path, expectedDirectoryName: game.lastPathComponent, protecting: root)
        XCTAssertTrue(FileManager.default.fileExists(atPath: outside.appending(path: "game.bin").path))
    }

    func testRefusesLockedInstallationAndCanRetryAfterRelease() throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let game = try installation(in: root)
        let descriptor = Darwin.open(game.appending(path: ".sailover-install.lock").path, O_RDWR | O_CREAT, S_IRUSR | S_IWUSR)
        XCTAssertGreaterThanOrEqual(descriptor, 0)
        defer { Darwin.close(descriptor) }
        XCTAssertEqual(flock(descriptor, LOCK_EX | LOCK_NB), 0)
        XCTAssertThrowsError(try GameInstallationManager.removeInstallation(
            directory: game.path, expectedDirectoryName: game.lastPathComponent, protecting: root
        )) { error in
            guard case GameInstallationFailure.busy = error else { return XCTFail("Expected installation lock error: \(error)") }
        }
        XCTAssertTrue(FileManager.default.fileExists(atPath: game.appending(path: "game.bin").path))
        XCTAssertEqual(flock(descriptor, LOCK_UN), 0)
        try GameInstallationManager.removeInstallation(directory: game.path, expectedDirectoryName: game.lastPathComponent, protecting: root)
        XCTAssertFalse(FileManager.default.fileExists(atPath: game.path))
    }

    func testEpicDirectoryNameMatchesRustJSONIdentityHash() throws {
        XCTAssertEqual(epicInstallationDirectoryName(namespace: "namespace", catalogItemId: "catalog", appName: "app/name", platform: "Mac"),
                       "epic-c5d3f0bff0a8491d3095fdd858bc870fe8416f60-Mac")
    }

    func testAcceptsBothSpellingsOfSystemTemporaryDirectory() throws {
        let component = "SailOverRemovalAliases-\(UUID().uuidString)"
        let temporaryPath = FileManager.default.temporaryDirectory.path
        let privateTemporaryPath = temporaryPath.hasPrefix("/var/") ? "/private" + temporaryPath : temporaryPath
        for parent in ["/tmp", "/private/tmp", temporaryPath, privateTemporaryPath] {
            let root = URL(fileURLWithPath: parent).appending(path: component)
            defer { try? FileManager.default.removeItem(at: root) }
            let game = try installation(in: root)
            try GameInstallationManager.removeInstallation(directory: game.path, expectedDirectoryName: game.lastPathComponent, protecting: root)
            XCTAssertFalse(FileManager.default.fileExists(atPath: game.path))
        }
    }

    private func temporaryDirectory() throws -> URL {
        let directory = FileManager.default.temporaryDirectory.resolvingSymlinksInPath().appending(path: "SailOverRemovalTests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory
    }

    private func installation(in root: URL, name: String = "steam-620-macos") throws -> URL {
        let directory = root.appending(path: name)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try Data("installed content".utf8).write(to: directory.appending(path: "game.bin"))
        return directory
    }
}
