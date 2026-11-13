import Foundation
import XCTest
@testable import Obelisk

/// The facts that name a storefront, which several types and raw strings
/// carry apart from each other today: they must agree for every storefront.
@MainActor
final class StorefrontIdentityTests: XCTestCase {
    private struct Facts {
        let section: LibrarySection
        let session: SessionStorefront
        let id: String
        let logo: String
        let github: String
    }

    private static let all = [
        Facts(section: .steam, session: .steam, id: "steam", logo: "SteamLogo", github: "Steam"),
        Facts(section: .epic, session: .epic, id: "epic", logo: "EpicGamesLogo", github: "Epic Games"),
        Facts(section: .gog, session: .gog, id: "gog", logo: "GOGLogo", github: "GOG"),
    ]

    func testEachStorefrontIsNamedTheSameWayByEveryLookupOfItsIdentity() async throws {
        XCTAssertEqual(LibrarySection.storefronts, [.steam, .epic, .gog], "The sidebar's order")
        XCTAssertEqual(LibrarySection.allCases.map(\.rawValue), ["home", "steam", "epic", "gog", "settings"],
                       "The accessibility identifiers are built from the sections' raw values")
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: []))
        defer { fixture.finish() }
        await fixture.signIn()

        for facts in Self.all {
            let name = facts.id
            XCTAssertEqual(facts.section.rawValue, name, "Raw strings name sections: \(name)")
            XCTAssertEqual(LibrarySection(rawValue: name), facts.section)
            XCTAssertTrue(facts.section.isStorefront)
            XCTAssertEqual(facts.session.section, facts.section, name)
            XCTAssertEqual(facts.session.siteName, name, name)
            XCTAssertEqual(facts.section.logoAssetName, facts.logo, name)
            XCTAssertEqual(facts.section.storeName, L10n.string("ui.store.\(name)"), name)
            XCTAssertEqual(facts.section.title, L10n.string("model.section.\(name).title"), name)
            let store = FriendsStoreIdentity(facts.section)
            XCTAssertEqual([store.id, store.name, store.logoAssetName],
                           [name, L10n.string("ui.store.\(name)"), facts.logo], name)
            XCTAssertEqual(facts.section.storeSubtitle, L10n.string("model.section.\(name).subtitle"), name)

            let report = BugReport(
                subject: .game(store: facts.session, id: "42", name: "Game"), description: "It crashes.", contact: "",
                mac: StorefrontsFixture.mac, obeliskVersion: "0.1.0", voileVersion: nil
            )
            let issue = try XCTUnwrap(URLComponents(url: GitHubIssues.bug(report, code: "K7F3-9QX2"),
                                                    resolvingAgainstBaseURL: false)?.queryItems)
            XCTAssertEqual(issue.first { $0.name == "game" }?.value, "Game (\(facts.github) 42)")
        }

        XCTAssertEqual(SteamGames.storefront, .steam)
        XCTAssertEqual(EpicGames.storefront, .epic)
        XCTAssertEqual(GogGames.storefront, .gog)
        XCTAssertEqual(EpicGames.storeID, "epic", "Receipts files and achievements")
        XCTAssertEqual(GogGames.storeID, "gog")
        XCTAssertEqual(EpicGames.storeID, LibrarySection.epic.rawValue)
        XCTAssertEqual(GogGames.storeID, LibrarySection.gog.rawValue)
        XCTAssertEqual(fixture.steam.achievements.storeID, "steam")
        XCTAssertEqual(fixture.epic.achievements.storeID, "epic")
        XCTAssertEqual(fixture.gog.achievements.storeID, "gog")

        XCTAssertEqual(OverlaySession(id: "a", game: .steam(appId: 1), startedAt: nil).section, .steam)
        XCTAssertEqual(OverlaySession(id: "b", game: .epic("ns:app"), startedAt: nil).section, .epic)
        XCTAssertEqual(OverlaySession(id: "c", game: .gog("1"), startedAt: nil).section, .gog)

        let steamStore = try XCTUnwrap(fixture.steam.friends?.store)
        XCTAssertEqual([steamStore.id, steamStore.name, steamStore.logoAssetName],
                       ["steam", L10n.string("ui.store.steam"), "SteamLogo"])
        let epicStore = try XCTUnwrap(fixture.epic.friends?.store)
        XCTAssertEqual([epicStore.id, epicStore.name, epicStore.logoAssetName],
                       ["epic", L10n.string("ui.store.epic"), "EpicGamesLogo"])
        XCTAssertEqual(fixture.steam.friends?.account.storeID, "steam")
        XCTAssertEqual(fixture.epic.friends?.account.storeID, "epic")
    }
}
