import XCTest
@testable import Obelisk

/// A library that moves as the test says, and remembers each call.
@MainActor
final class FakeLibraryFolder: LibraryFolderBackend {
    var inspection: LibraryFolderInspection = .missing
    var failure: Error?
    var moved: [String] = []
    var cancelled = false

    func inspect(_ folder: String) -> LibraryFolderInspection { inspection }
    func inspectDestination(_ destination: String) -> LibraryFolderInspection { inspection }

    func move(to destination: String) async throws -> String {
        moved.append(destination)
        if let failure { throw failure }
        return destination
    }

    func progress() -> LibraryMoveProgress { LibraryMoveProgress(copiedBytes: 0, totalBytes: 0) }
    func cancel() { cancelled = true }
}

@MainActor
final class LibraryFolderModelTests: XCTestCase {
    private let mac = URL(filePath: "/Users/player/Library/Application Support/com.zagrios.obelisk",
                          directoryHint: .isDirectory)
    private var defaults: UserDefaults!
    private var suite: String!

    override func setUp() async throws {
        suite = "LibraryFolderModelTests.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
    }

    override func tearDown() async throws {
        defaults.removePersistentDomain(forName: suite)
    }

    private func model(_ backend: FakeLibraryFolder, library: URL? = nil) -> LibraryFolderModel {
        LibraryFolderModel(library: library ?? mac, defaults: defaults, defaultDirectory: mac, environment: [:],
                           backend: backend)
    }

    func testALibraryTheDevelopmentOverrideNamesDoesNotMove() async {
        let backend = FakeLibraryFolder()
        let model = LibraryFolderModel(library: mac, defaults: defaults, defaultDirectory: mac,
                                       environment: ["OBELISK_LIBRARY_DIRECTORY": mac.path], backend: backend)
        XCTAssertFalse(model.isMovable)
        model.proposal = LibraryFolderProposal(chosen: URL(filePath: "/Volumes/Disk"), path: "/Volumes/Disk/Obelisk Library",
                                               copiedBytes: 0, volume: "Disk", availableBytes: nil, format: nil,
                                               isReadOnly: false)
        await model.confirm()
        XCTAssertTrue(backend.moved.isEmpty)
        XCTAssertNil(defaults.string(forKey: AppIdentity.libraryDirectoryKey))
    }

    func testTheLibraryIsObelisksOwnUntilThePlayerMovesItAndTheOverrideWinsOverBoth() {
        let resolve = { (environment: [String: String]) in
            AppIdentity.libraryDirectory(environment: environment, defaults: self.defaults).path
        }
        XCTAssertEqual(resolve([:]), AppIdentity.defaultLibraryDirectory.path)
        defaults.set("/Volumes/Disk/Obelisk Library", forKey: AppIdentity.libraryDirectoryKey)
        XCTAssertEqual(resolve([:]), "/Volumes/Disk/Obelisk Library")
        XCTAssertTrue(LibraryStartup.isMoved(environment: [:], defaults: defaults))
        XCTAssertEqual(resolve(["OBELISK_LIBRARY_DIRECTORY": "/tmp/library"]), "/tmp/library")
        XCTAssertFalse(LibraryStartup.isMoved(environment: ["OBELISK_LIBRARY_DIRECTORY": "/tmp/library"],
                                              defaults: defaults))
    }

    func testAPickedFolderShowsWhereTheLibraryGoesThenObeliskRestartsThere() async {
        let backend = FakeLibraryFolder()
        let model = model(backend)
        XCTAssertTrue(model.isDefault)
        var restarts = 0
        model.restart = { restarts += 1 }
        backend.inspection = .ready(path: "/Volumes/Disk/Obelisk Library", copiedBytes: 1_000)
        model.propose(URL(filePath: "/Volumes/Disk", directoryHint: .isDirectory))
        XCTAssertEqual(model.proposal?.path, "/Volumes/Disk/Obelisk Library")
        XCTAssertEqual(model.proposal?.copiedBytes, 1_000)
        await model.confirm()
        XCTAssertEqual(backend.moved, ["/Volumes/Disk/Obelisk Library"])
        XCTAssertEqual(defaults.string(forKey: AppIdentity.libraryDirectoryKey), "/Volumes/Disk/Obelisk Library")
        XCTAssertEqual(defaults.string(forKey: AppIdentity.leftLibraryKey), mac.path)
        XCTAssertNil(model.proposal)
        XCTAssertNil(model.moving)
        XCTAssertEqual(restarts, 1)
    }

    func testTheLibraryMovedBackToThisMacForgetsThePlayersFolder() async {
        let backend = FakeLibraryFolder()
        let disk = URL(filePath: "/Volumes/Disk/Obelisk Library", directoryHint: .isDirectory)
        defaults.set(disk.path, forKey: AppIdentity.libraryDirectoryKey)
        let model = model(backend, library: disk)
        XCTAssertFalse(model.isDefault)
        backend.inspection = .ready(path: mac.path, copiedBytes: 1_000)
        model.proposeDefault()
        XCTAssertNil(model.proposal?.chosen)
        await model.confirm()
        XCTAssertNil(defaults.string(forKey: AppIdentity.libraryDirectoryKey))
        XCTAssertEqual(defaults.string(forKey: AppIdentity.leftLibraryKey), disk.path)
    }

    func testAFailedOrCancelledMoveKeepsTheLibraryWhereItIs() async {
        let backend = FakeLibraryFolder()
        let model = model(backend)
        var restarts = 0
        model.restart = { restarts += 1 }
        backend.inspection = .ready(path: "/Volumes/Disk/Obelisk Library", copiedBytes: 0)
        backend.failure = GameSessionError.Failure(reason: "model.libraryFolder.refused")
        model.propose(URL(filePath: "/Volumes/Disk", directoryHint: .isDirectory))
        await model.confirm()
        XCTAssertEqual(model.failure,
                       L10n.string("model.libraryFolder.failed", L10n.string("model.libraryFolder.refused")))
        backend.failure = GameSessionError.Cancelled
        model.propose(URL(filePath: "/Volumes/Disk", directoryHint: .isDirectory))
        await model.confirm()
        XCTAssertNotNil(model.proposal, "a cancelled move leaves the sheet to the player")
        XCTAssertNil(model.moving)
        XCTAssertNil(defaults.string(forKey: AppIdentity.libraryDirectoryKey))
        XCTAssertNil(defaults.string(forKey: AppIdentity.leftLibraryKey))
        XCTAssertEqual(restarts, 0)
    }

    func testAFolderThatCannotHoldTheLibrarySaysWhyAndAsksNothing() {
        let backend = FakeLibraryFolder()
        let model = model(backend)
        for inspection: LibraryFolderInspection in [.missing, .inside, .occupied, .holdsGames, .games, .unwritable] {
            backend.inspection = inspection
            model.propose(URL(filePath: "/Volumes/Disk", directoryHint: .isDirectory))
            XCTAssertNil(model.proposal)
            XCTAssertNotNil(model.failure)
        }
    }

    func testADiskThatCannotKeepTheLibraryIsNotOffered() async {
        let backend = FakeLibraryFolder()
        let model = model(backend)
        model.proposal = LibraryFolderProposal(chosen: URL(filePath: "/Volumes/Stick"), path: "/Volumes/Stick/Obelisk Library",
                                               copiedBytes: 10, volume: "Stick", availableBytes: 100,
                                               format: "ExFAT", isReadOnly: false)
        XCTAssertFalse(model.proposal!.canChoose)
        await model.confirm()
        XCTAssertTrue(backend.moved.isEmpty)
    }

    func testACloudSyncedFolderIsNotOfferedAndAnExternalDisksLibraryIsToldApartFromAMissingDisk() async {
        let home = URL(filePath: "/Users/player", directoryHint: .isDirectory)
        for synced in ["Library/Mobile Documents/com~apple~CloudDocs/Games", "Library/CloudStorage/Dropbox"] {
            XCTAssertTrue(LibraryFolderProposal.sync(of: home.appending(path: synced), home: home).synced, synced)
        }
        XCTAssertFalse(LibraryFolderProposal.sync(of: URL(filePath: "/Volumes/Disk"), home: home).synced)
        XCTAssertFalse(LibraryFolderProposal.sync(of: home.appending(path: "Library/CloudStorageNotes"), home: home).synced)

        let backend = FakeLibraryFolder()
        let model = model(backend)
        model.proposal = LibraryFolderProposal(chosen: home.appending(path: "Library/CloudStorage/Dropbox"),
                                               path: "/Users/player/Library/CloudStorage/Dropbox/Obelisk Library",
                                               copiedBytes: 0, volume: "Macintosh HD", availableBytes: nil,
                                               format: nil, isReadOnly: false, isSynced: true)
        XCTAssertFalse(model.proposal!.canChoose)
        await model.confirm()
        XCTAssertTrue(backend.moved.isEmpty)

        XCTAssertFalse(LibraryStartup.isDiskConnected(URL(filePath: "/Volumes/NoSuchDisk/Obelisk Library")))
        XCTAssertTrue(LibraryStartup.isDiskConnected(URL(filePath: "/Users/player/Obelisk Library")))
        // A disk that is there, whose library folder is not.
        let mounted = try? FileManager.default.contentsOfDirectory(atPath: "/Volumes").first
        if let mounted {
            XCTAssertTrue(LibraryStartup.isDiskConnected(URL(filePath: "/Volumes/\(mounted)/No Obelisk Library")))
        }
    }
}
