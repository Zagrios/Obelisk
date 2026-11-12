import CryptoKit
import Foundation
import XCTest
@testable import Obelisk

/// The beta releases choice, and the return of a pre-release to the latest
/// release that Sparkle would refuse as a downgrade.
@MainActor
final class ReleaseReturnTests: XCTestCase {
    private func release(_ version: String, beta: Bool = false) -> FeedRelease {
        FeedRelease(version: version, displayVersion: version, channel: beta ? ReleaseReturn.betaChannel : nil,
                    url: URL(string: "https://example.invalid/Obelisk-\(version).dmg")!, length: 3, signature: "")
    }

    func testABetaGoesBackToTheNewestReleaseOnlyWithoutBetaReleases() {
        let feed = [release("0.6.1"), release("0.6.2"), release("0.6.3.beta4", beta: true)]
        XCTAssertEqual(ReleaseReturn.target(running: "0.6.3.beta4", in: feed, takesBetaReleases: false, floor: nil),
                       release("0.6.2"))
        XCTAssertEqual(ReleaseReturn.target(running: "0.6.3.beta2", in: feed, takesBetaReleases: false, floor: nil),
                       release("0.6.2"), "the feed lists only the newest beta")
        XCTAssertEqual(ReleaseReturn.target(running: "0.7.0.beta1", in: [release("0.6.2")], takesBetaReleases: false,
                                            floor: "0.6.2"), release("0.6.2"))
        XCTAssertNil(ReleaseReturn.target(running: "0.6.3.beta4", in: feed, takesBetaReleases: true, floor: nil))
    }

    func testOnlyABetaByItsOwnNameGoesBack() {
        let feed = [release("0.6.2"), release("0.7.0", beta: true)]
        XCTAssertNil(ReleaseReturn.target(running: "0.7.0", in: feed, takesBetaReleases: false, floor: nil),
                     "the feed, which is not signed, never makes a version a beta")
        XCTAssertNil(ReleaseReturn.target(running: "0.6.2", in: feed, takesBetaReleases: false, floor: nil))
        XCTAssertNil(ReleaseReturn.target(running: "0.8.0", in: feed, takesBetaReleases: false, floor: nil),
                     "a build the feed never listed, such as a local one, stays")
        XCTAssertNil(ReleaseReturn.target(running: "0.6.3.beta2", in: [release("0.6.3"), release("0.6.2")],
                                          takesBetaReleases: false, floor: nil),
                     "its own release is Sparkle's update")
        XCTAssertNil(ReleaseReturn.target(running: "0.6.3.beta2", in: [release("0.6.3.beta2", beta: true)],
                                          takesBetaReleases: false, floor: nil),
                     "no release to go back to")
        XCTAssertTrue(ReleaseReturn.isBeta("0.6.3.beta2"))
        XCTAssertFalse(ReleaseReturn.isBeta("0.6.3"))
    }

    func testABetaIsNeverWhereABetaGoesBack() {
        XCTAssertNil(ReleaseReturn.target(running: "0.6.3.beta5", in: [release("0.6.3.beta1")], takesBetaReleases: false,
                                          floor: "0.6.2"), "a feed may not list a beta as a release")
        XCTAssertEqual(ReleaseReturn.target(running: "0.6.3.beta5", in: [release("0.6.3.beta1"), release("0.6.2")],
                                            takesBetaReleases: false, floor: "0.6.2"), release("0.6.2"))
    }

    func testAFeedNeverTakesObeliskBelowTheNewestReleaseItEverListed() {
        let feed = [release("0.5.0"), release("0.6.3.beta2", beta: true)]
        XCTAssertNil(ReleaseReturn.target(running: "0.6.3.beta2", in: feed, takesBetaReleases: false, floor: "0.6.2"))
        XCTAssertEqual(ReleaseReturn.target(running: "0.6.3.beta2", in: [release("0.6.2")], takesBetaReleases: false,
                                            floor: "0.6.2"), release("0.6.2"))
    }

    func testOnlyTheSignatureOfTheFeedsKeyHoldsTheDMG() {
        let key = Curve25519.Signing.PrivateKey()
        let dmg = Data("Obelisk".utf8)
        let signature = try! key.signature(for: dmg).base64EncodedString()
        let publicKey = key.publicKey.rawRepresentation.base64EncodedString()
        XCTAssertTrue(ReleaseReturn.signs(signature, dmg, key: publicKey))
        XCTAssertFalse(ReleaseReturn.signs(signature, Data("Obelisc".utf8), key: publicKey))
        let other = Curve25519.Signing.PrivateKey().publicKey.rawRepresentation.base64EncodedString()
        XCTAssertFalse(ReleaseReturn.signs(signature, dmg, key: other))
        XCTAssertFalse(ReleaseReturn.signs("not base64", dmg, key: publicKey))
    }

    func testAnAppThatCannotReplaceItselfGetsNoFolder() throws {
        let files = FileManager.default
        let root = files.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? files.removeItem(at: root) }
        XCTAssertThrowsError(try ReleaseReturn.folder(replacing: root.appending(path: "Obelisk.app"))) {
            XCTAssertEqual($0 as? ReleaseReturn.Failure, .unwritable, "a missing app cannot be replaced")
        }
        let app = root.appending(path: "Obelisk.app")
        try files.createDirectory(at: app, withIntermediateDirectories: true)
        let folder = try ReleaseReturn.folder(replacing: app)
        XCTAssertTrue(files.fileExists(atPath: folder.path))
        ReleaseReturn.abandon(folder)
        XCTAssertFalse(files.fileExists(atPath: folder.path))
    }

    func testTheStagedAppTakesThePlaceOfTheRunningOneInOneStep() throws {
        let files = FileManager.default
        let root = files.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? files.removeItem(at: root) }
        let app = root.appending(path: "Obelisk.app")
        let folder = root.appending(path: "staging")
        let staged = ReleaseReturn.staged(in: folder, replacing: app)
        try files.createDirectory(at: app, withIntermediateDirectories: true)
        try files.createDirectory(at: staged, withIntermediateDirectories: true)
        try Data("pre-release".utf8).write(to: app.appending(path: "version"))
        try Data("release".utf8).write(to: staged.appending(path: "version"))

        try ReleaseReturn.install(from: folder, over: app)

        XCTAssertEqual(try String(contentsOf: app.appending(path: "version"), encoding: .utf8), "release")
        XCTAssertFalse(files.fileExists(atPath: folder.path), "the beta goes with the staging folder")
        XCTAssertThrowsError(try ReleaseReturn.install(from: folder, over: app), "nothing staged is left to swap")
    }

    func testTheBetaChoiceIsRememberedAndReachesVoile() throws {
        let suite = "ObeliskTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        var voile: [Bool] = []
        let updates = AppUpdates(starts: false, defaults: defaults) { voile.append($0) }
        XCTAssertFalse(updates.takesBetaReleases)
        XCTAssertEqual(voile, [false], "Voile follows the choice from the start")

        updates.takesBetaReleases = true
        updates.takesBetaReleases = true
        XCTAssertEqual(voile, [false, true])
        XCTAssertTrue(defaults.bool(forKey: AppUpdates.betaReleasesKey))

        var reopened: [Bool] = []
        let again = AppUpdates(starts: false, defaults: defaults) { reopened.append($0) }
        XCTAssertTrue(again.takesBetaReleases)
        XCTAssertEqual(reopened, [true])
        XCTAssertNil(again.returnStanding)

        updates.takesBetaReleases = false
        XCTAssertEqual(voile, [false, true, false])
    }
}
