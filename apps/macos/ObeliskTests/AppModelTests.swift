import Foundation
import XCTest
@testable import Obelisk

@MainActor
final class AppModelTests: XCTestCase {
    func testAnotherSectionClearsTheSearchAndTheAppearanceIsRemembered() throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let suite = "ObeliskTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let model = AppModel(directory: root, defaults: defaults)

        model.searchText = "portal"
        model.selection = .steam
        XCTAssertTrue(model.searchText.isEmpty)

        model.appearance = .dark
        XCTAssertEqual(AppModel(directory: root, defaults: defaults).appearance, .dark)
    }

    func testTheChosenLanguageIsObelisksOwnAppleLanguagesUntilThePlayerFollowsTheMacAgain() throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let suite = "ObeliskTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let model = AppModel(directory: root, defaults: defaults, defaultsDomain: suite)
        XCTAssertEqual(model.language, .system)

        let other = L10n.languageCode == "fr" ? "en" : "fr"
        model.language = .language(other)
        XCTAssertEqual(defaults.persistentDomain(forName: suite)?["AppleLanguages"] as? [String], [other])
        XCTAssertTrue(model.languageNeedsRestart)
        XCTAssertEqual(AppModel(directory: root, defaults: defaults, defaultsDomain: suite).language, .language(other))

        model.language = .language(L10n.languageCode)
        XCTAssertFalse(model.languageNeedsRestart)

        model.language = .system
        XCTAssertNil(defaults.persistentDomain(forName: suite)?["AppleLanguages"])
        XCTAssertEqual(AppModel(directory: root, defaults: defaults, defaultsDomain: suite).language, .system)
    }

    func testALanguageMacOSChoseForObeliskReadsAsTheTranslationItShows() throws {
        let suite = "ObeliskTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        defaults.set(["fr-CA"], forKey: "AppleLanguages")
        XCTAssertEqual(AppLanguage(defaults: defaults, domain: suite), .language("fr"))
        defaults.set(["zh-Hans-CN"], forKey: "AppleLanguages")
        XCTAssertEqual(AppLanguage(defaults: defaults, domain: suite), .language("zh-Hans"))
        defaults.set(["ko-KR"], forKey: "AppleLanguages")
        XCTAssertEqual(AppLanguage(defaults: defaults, domain: suite), .language("en"))
    }

    func testGamesUpdateAutomaticallyUnlessThePlayerTurnsItOffInEveryStorefront() throws {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let suite = "ObeliskTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let model = AppModel(directory: root, defaults: defaults)
        XCTAssertTrue(model.updatesGamesAutomatically)
        XCTAssertTrue(model.steam.installations.updatesAutomatically)

        model.updatesGamesAutomatically = false
        XCTAssertFalse(model.steam.installations.updatesAutomatically)
        XCTAssertFalse(model.epic.installations.updatesAutomatically)
        XCTAssertFalse(model.gog.installations.updatesAutomatically)
        let restarted = AppModel(directory: root, defaults: defaults)
        XCTAssertFalse(restarted.updatesGamesAutomatically)
        XCTAssertFalse(restarted.gog.installations.updatesAutomatically)
    }
}
