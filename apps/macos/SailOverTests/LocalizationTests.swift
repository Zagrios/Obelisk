import Foundation
import XCTest
@testable import SailOver

final class LocalizationTests: XCTestCase {
    func testSystemLanguageAndRegionalVariants() {
        for identifier in ["fr", "fr-FR", "fr_CA", "fr-BE"] {
            XCTAssertEqual(L10n.resolvedLanguage(preferredLanguages: [identifier]), "fr", identifier)
        }
        for identifier in ["en", "en-US", "en_GB", "de-DE", "ja-JP", "zh-Hant", ""] {
            XCTAssertEqual(L10n.resolvedLanguage(preferredLanguages: [identifier]), "en", identifier)
        }
        XCTAssertEqual(L10n.resolvedLanguage(preferredLanguages: []), "en")
        XCTAssertEqual(L10n.resolvedLanguage(preferredLanguages: ["de-DE", "fr-FR"]), "en")
    }

    func testBothTranslationsArePackagedAndUnsupportedLanguagesUseEnglish() {
        XCTAssertTrue(Bundle.main.localizations.contains("en"))
        XCTAssertTrue(Bundle.main.localizations.contains("fr"))
        XCTAssertEqual(Bundle.main.developmentLocalization, "en")
        XCTAssertEqual(LocalizedStrings(language: "en").string("app.library"), "Library")
        XCTAssertEqual(LocalizedStrings(language: "fr-CA").string("app.library"), "Bibliothèque")
        XCTAssertEqual(LocalizedStrings(language: "de-DE").string("app.library"), "Library")
    }

    func testGameCountsUseEachLanguagesPluralRules() {
        let english = LocalizedStrings(language: "en")
        let french = LocalizedStrings(language: "fr")
        for (count, en, fr) in [(0, "0 games", "0 jeu"), (1, "1 game", "1 jeu"), (2, "2 games", "2 jeux")] {
            XCTAssertEqual(english.string("ui.library.game_count", arguments: [Int64(count)]), en)
            XCTAssertEqual(french.string("ui.library.game_count", arguments: [Int64(count)]), fr)
        }
    }

    func testInterpolatedTextAndNumbersKeepTheirPlaceholders() {
        let english = LocalizedStrings(language: "en")
        let french = LocalizedStrings(language: "fr")
        XCTAssertEqual(english.string("ui.transfer.verified_files", arguments: ["12 MB", "30 MB"]), "Files verified: 12 MB of 30 MB")
        XCTAssertEqual(french.string("ui.transfer.verified_files", arguments: ["12 Mo", "30 Mo"]), "Fichiers vérifiés : 12 Mo sur 30 Mo")
        XCTAssertEqual(english.string("model.keychain.readFailed", arguments: ["Steam", Int64(-50)]), "Unable to read the Steam session in the macOS Keychain (-50).")
    }

    func testRustKeysAndStructuredArgumentsAreLocalized() throws {
        let english = LocalizedStrings(language: "en")
        let french = LocalizedStrings(language: "fr")
        XCTAssertEqual(english.message("backend.epic.unauthorized"), "Your Epic Games session has expired. Sign in again.")
        XCTAssertEqual(french.message("backend.epic.unauthorized"), "La session Epic Games a expiré. Reconnectez votre compte.")
        let payload = try JSONSerialization.data(withJSONObject: ["key": "backend.epic.http", "args": ["503"]])
        let encoded = try XCTUnwrap(String(data: payload, encoding: .utf8))
        XCTAssertEqual(english.message(encoded), "Epic Games is unavailable for this request (HTTP 503).")
        XCTAssertEqual(french.message(encoded), "Epic Games est indisponible pour cette demande (HTTP 503).")
        XCTAssertEqual(french.message("model.epic.recoveringSession"), "Reconnexion au jeu Epic en cours…")
        XCTAssertEqual(english.message("model.steam.recoveringSession"), "Recovering the Steam game session…")
        for raw in ["Game_Data/level1.bin", "An external diagnostic", "{malformed}", "model.bin"] {
            XCTAssertEqual(french.message(raw), raw)
        }
    }

    func testFormattingKeepsTheRegionAndUsesTheSelectedLanguage() {
        let locale = L10n.formattingLocale(language: "fr", region: "CA")
        XCTAssertEqual(locale.language.languageCode?.identifier, "fr")
        XCTAssertEqual(locale.region?.identifier, "CA")
        let date = Date(timeIntervalSince1970: 1_735_689_600)
        XCTAssertEqual(date.formatted(.dateTime.month(.wide).locale(Locale(identifier: "en"))), "January")
        XCTAssertEqual(date.formatted(.dateTime.month(.wide).locale(Locale(identifier: "fr"))), "janvier")
    }

    func testNestedRustErrorsAndLiteralPathArguments() throws {
        let nested = try JSONSerialization.data(withJSONObject: [
            "key": "backend.steam.cm.rejected",
            "args": ["backend.steam.cm.action.depot", "5"]
        ])
        let encoded = try XCTUnwrap(String(data: nested, encoding: .utf8))
        XCTAssertEqual(LocalizedStrings(language: "en").message(encoded), "Steam rejected depot access (result 5).")
        XCTAssertEqual(LocalizedStrings(language: "fr").message(encoded), "Steam a refusé l’accès au dépôt (résultat 5).")

        let filename = "Game Data/\"quoted\"/100%.bin"
        let path = try JSONSerialization.data(withJSONObject: ["key": "backend.steam.content.unsafePath", "args": [filename]])
        XCTAssertEqual(LocalizedStrings(language: "en").message(try XCTUnwrap(String(data: path, encoding: .utf8))), "Unsafe download path: \(filename)")
    }
}
