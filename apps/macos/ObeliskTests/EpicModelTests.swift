import Foundation
import XCTest
@testable import Obelisk

/// What only Epic's adapter does. WebAccountModelTests covers the account
/// lifecycle, library and game page Epic shares with GOG.
@MainActor
final class EpicModelTests: XCTestCase {
    func testOpeningAGameAsksEpicForItsDetails() async throws {
        var owned = EpicGames.game("one", title: "One")
        owned.macos = true
        let session = EpicTestSession(credentials: EpicGames.credentials("current"), games: [owned])
        let model = await connectedModel(session)
        defer { model.signOut() }

        model.open(owned)
        try await eventually { !model.isLoadingGameDetails }
        let requests = await session.detailsCalls
        let product = GameOffer.epic(namespace: "fixture", catalogItemId: "one", appName: "one", title: "One")
        XCTAssertEqual(requests, [WebDetailsRequest(offer: product, language: "en-US")])
    }

    func testAchievementsAskForTheGameInABareLanguage() async throws {
        let owned = EpicGames.game("one", title: "One")
        let session = EpicTestSession(credentials: EpicGames.credentials("current"), games: [owned])
        let model = await connectedModel(session, language: "fr-FR")
        defer { model.signOut() }

        model.open(owned)
        try await eventually { model.achievements.items != nil }
        let reads = await session.achievementReads
        XCTAssertTrue(reads.contains(AchievementRead(ids: ["fixture:one"], locale: "fr", fresh: false)))
        XCTAssertTrue(reads.allSatisfy { $0.locale == "fr" })
    }

    func testTheStoreSearchKeepsAPlusApartFromASpace() {
        XCTAssertEqual(EpicGames.storeSearchURL("C++ Tycoon").absoluteString,
                       "https://store.epicgames.com/browse?q=C%2B%2B%20Tycoon&sortBy=relevancy&sortDir=DESC")
    }
}
