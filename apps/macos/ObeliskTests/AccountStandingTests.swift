import Foundation
import XCTest
@testable import Obelisk

/// How a storefront's account stands, as Home's tile, Settings' row and
/// Home's sign-in card tell it: its library shows with the account, its games
/// and its installed games, or its saved session restores, or it is signed
/// out, connecting or not. AppModel reads it from the account and from the
/// storefronts its launch restoration still lists.
@MainActor
final class AccountStandingTests: XCTestCase {
    private func standings(_ app: AppModel) -> [AccountStanding] {
        LibrarySection.storefronts.map(app.standing(of:))
    }

    private func twoGames() -> StorefrontsFixture.Setup {
        StorefrontsFixture.Setup(
            steam: [StorefrontsFixture.portal, StorefrontsFixture.counterStrike],
            epic: [StorefrontsFixture.epicGame("one", title: "Epic One"), StorefrontsFixture.epicGame("two", title: "Epic Two")],
            gog: [StorefrontsFixture.gogGame("1207658924", title: "GOG One"), StorefrontsFixture.gogGame("1", title: "GOG Two")]
        )
    }

    /// Lets the held restorations end.
    private func finishRestoring(_ restoration: Task<Void, Never>, _ gates: [LibrarySection: GameSessionGate<Void>]) async {
        for gate in gates.values { await gate.resolve(()) }
        await restoration.value
    }

    // MARK: Before and while an account connects

    func testAStorefrontNotYetRestoredAtLaunchStandsRestoringThoughItsAccountDoesNotRestoreYet() {
        let fixture = StorefrontsFixture(twoGames())
        defer { fixture.finish() }

        XCTAssertEqual(standings(fixture.app), [.restoring, .restoring, .restoring])
        XCTAssertEqual(fixture.app.homeContent, .restoring, "Home waits for all three at launch")
        for account in fixture.app.accounts {
            XCTAssertEqual(account.standing, .signedOut(connecting: false), "\(account.section): nothing restores yet")
            XCTAssertFalse(account.showsLibrary, "\(account.section)")
        }
    }

    func testASignedOutStorefrontOnceRestoredStandsSignedOutAndHomeOffersItsSignIn() async {
        let fixture = StorefrontsFixture()
        defer { fixture.finish() }

        await fixture.signIn()

        XCTAssertEqual(standings(fixture.app), Array(repeating: .signedOut(connecting: false), count: 3))
        XCTAssertEqual(fixture.app.homeContent, .signIn)
    }

    func testARestoringStorefrontWithoutACachedLibraryStandsRestoring() async throws {
        let fixture = StorefrontsFixture(StorefrontsFixture.Setup(steam: [], epic: [], gog: [], held: [.steam, .epic, .gog]))
        defer { fixture.finish() }
        let restoration = Task { await fixture.app.restoreStorefronts() }
        let gates = fixture.restoreGates
        try await eventually {
            let requested = [await gates[.steam]?.requests, await gates[.epic]?.requests, await gates[.gog]?.requests]
            return requested == [1, 1, 1]
        }

        XCTAssertEqual(standings(fixture.app), [.restoring, .restoring, .restoring])
        XCTAssertEqual(fixture.app.homeContent, .restoring)
        for account in fixture.app.accounts {
            XCTAssertEqual(account.standing, .restoring, "\(account.section): its own session restores")
            XCTAssertFalse(account.showsLibrary, "\(account.section)")
        }
        await finishRestoring(restoration, gates)
    }

    func testARestoringStorefrontStandsSignedInWithItsCachedLibraryBeforeItsAccountIsKnown() async throws {
        var setup = StorefrontsFixture.Setup(steam: [], epic: [], gog: [], held: [.steam, .epic, .gog])
        setup.cache = StorefrontsFixture.CachedLibraries(
            steam: [CachedSteamGame(game: StorefrontsFixture.portal, platforms: nil),
                    CachedSteamGame(game: StorefrontsFixture.counterStrike, platforms: nil)],
            epic: [CachedEpicGame(game: StorefrontsFixture.epicGame("one"), playtimeSeconds: nil),
                   CachedEpicGame(game: StorefrontsFixture.epicGame("two"), playtimeSeconds: nil)],
            gog: [CachedGogGame(game: StorefrontsFixture.gogGame("1207658924"), playtimeMinutes: nil),
                  CachedGogGame(game: StorefrontsFixture.gogGame("1"), playtimeMinutes: nil)]
        )
        let fixture = StorefrontsFixture(setup)
        defer { fixture.finish() }
        let restoration = Task { await fixture.app.restoreStorefronts() }
        let gates = fixture.restoreGates
        try await eventually {
            let requested = [await gates[.steam]?.requests, await gates[.epic]?.requests, await gates[.gog]?.requests]
            return requested == [1, 1, 1]
        }

        let cached = AccountStanding.signedIn(account: nil, avatarURL: nil, games: 2, installed: 0)
        XCTAssertEqual(standings(fixture.app), [cached, cached, cached],
                       "The library shows from the cache, the account is not known yet and no installation is")
        XCTAssertEqual(fixture.app.homeContent, .libraries)
        await finishRestoring(restoration, gates)
    }

    func testAWebStorefrontSigningInStandsSignedOutAndConnectingNotRestoring() async throws {
        let fixture = StorefrontsFixture()
        defer { fixture.finish() }
        await fixture.signIn()

        fixture.epic.signIn()
        fixture.gog.signIn()

        XCTAssertEqual(fixture.app.standing(of: .epic), .signedOut(connecting: true))
        XCTAssertEqual(fixture.app.standing(of: .gog), .signedOut(connecting: true))
        XCTAssertEqual(fixture.app.standing(of: .steam), .signedOut(connecting: false))
        XCTAssertEqual(fixture.app.homeContent, .signIn)
        XCTAssertEqual(LibrarySection.storefronts.map { fixture.app.standing(of: $0).isBusy }, [false, true, true])
        fixture.epic.cancelSignIn()
        fixture.gog.cancelSignIn()
        XCTAssertEqual(fixture.app.standing(of: .epic), .signedOut(connecting: false))
        XCTAssertFalse(fixture.app.standing(of: .epic).isBusy)
    }

    func testTheSignInCardIsBusyWhileALibraryIsOnItsWayOrASignInRuns() {
        XCTAssertTrue(AccountStanding.restoring.isBusy)
        XCTAssertTrue(AccountStanding.signedOut(connecting: true).isBusy)
        XCTAssertFalse(AccountStanding.signedOut(connecting: false).isBusy)
        XCTAssertFalse(AccountStanding.signedIn(account: nil, avatarURL: nil, games: 0, installed: 0).isBusy)
    }

    func testOnlyAStorefrontSectionHasAnAccount() {
        let fixture = StorefrontsFixture()
        defer { fixture.finish() }
        let app = fixture.app

        XCTAssertEqual(app.accounts.map(\.section), LibrarySection.storefronts, "In the sidebar's order")
        XCTAssertTrue(app.account(.steam) === fixture.steam)
        XCTAssertTrue(app.account(.epic) === fixture.epic)
        XCTAssertTrue(app.account(.gog) === fixture.gog)
        XCTAssertNil(app.account(.home))
        XCTAssertNil(app.account(.settings))
        XCTAssertEqual(app.standing(of: .home), .signedOut(connecting: false))
        XCTAssertEqual(app.standing(of: .settings), .signedOut(connecting: false))
    }

    // MARK: Signed in

    func testASignedInStorefrontTellsItsAccountItsAvatarItsGamesAndItsInstalledGames() async throws {
        var setup = twoGames()
        setup.steamProfile = SteamProfile(displayName: "Gaben", avatarUrl: "https://avatars.example/gaben.jpg")
        setup.steamReceipts = [.steam(appId: 620, name: "Portal 2", directory: "/Games/steam-620", platform: .mac)]
        setup.epicReceipts = [.epic(namespace: "fixture", appName: "one", name: "Epic One", directory: "/Games/epic-one",
                                    platform: .windows)]
        setup.gogReceipts = [StorefrontsFixture.gogReceipt("1207658924", directory: "/Games/gog-1207658924")]
        let fixture = StorefrontsFixture(setup)
        defer { fixture.finish() }

        await fixture.signIn()
        try await eventually { fixture.steam.displayName == "Gaben" }

        XCTAssertEqual(fixture.app.standing(of: .steam), .signedIn(
            account: "Gaben", avatarURL: URL(string: "https://avatars.example/gaben.jpg"), games: 2, installed: 1))
        XCTAssertEqual(fixture.app.standing(of: .epic), .signedIn(
            account: "Name of epic-account", avatarURL: nil, games: 2, installed: 1))
        XCTAssertEqual(fixture.app.standing(of: .gog), .signedIn(
            account: "Name of gog-account", avatarURL: GogGames.expectedAvatar, games: 2, installed: 1))
        XCTAssertEqual(fixture.app.homeContent, .libraries)
        for account in fixture.app.accounts {
            XCTAssertEqual(account.standing, fixture.app.standing(of: account.section), "\(account.section)")
        }
    }

    // MARK: Installed games

    /// Each storefront's library lists two games. Its first is installed for
    /// both platforms; the receipts also hold a game its library does not list.
    private func installedSetup() -> StorefrontsFixture.Setup {
        var setup = twoGames()
        setup.steamReceipts = [
            .steam(appId: 620, name: "Portal 2", directory: "/Games/steam-620-macos", platform: .mac),
            .steam(appId: 620, name: "Portal 2", directory: "/Games/steam-620-windows", platform: .windows),
            .steam(appId: 999, name: "Delisted", directory: "/Games/steam-999", platform: .windows),
        ]
        setup.epicReceipts = [
            .epic(namespace: "fixture", appName: "one", name: "Epic One", directory: "/Games/epic-one-mac", platform: .mac),
            .epic(namespace: "fixture", appName: "one", name: "Epic One", directory: "/Games/epic-one-windows",
                  platform: .windows),
            .epic(namespace: "fixture", appName: "delisted", name: "Delisted", directory: "/Games/epic-delisted",
                  platform: .windows),
        ]
        setup.gogReceipts = [
            StorefrontsFixture.gogReceipt("1207658924", platform: .mac, directory: "/Games/gog-1207658924-mac"),
            StorefrontsFixture.gogReceipt("1207658924", platform: .windows, directory: "/Games/gog-1207658924-windows"),
            StorefrontsFixture.gogReceipt("999", directory: "/Games/gog-999"),
        ]
        return setup
    }

    /// Home's tile and Settings' row count the library's games that have an
    /// Installation receipt: a game installed for two platforms counts once,
    /// and one the library does not list does not count. Settings used to
    /// count the receipts' distinct games, the unlisted ones too.
    func testAStorefrontCountsInstalledGamesFromItsLibraryAlone() async {
        let fixture = StorefrontsFixture(installedSetup())
        defer { fixture.finish() }

        await fixture.signIn()

        for section in LibrarySection.storefronts {
            guard case let .signedIn(_, _, games, installed) = fixture.app.standing(of: section) else {
                XCTFail("\(section) is signed in")
                continue
            }
            XCTAssertEqual(games, 2, "\(section)")
            XCTAssertEqual(installed, 1, "\(section): the game of two platforms once, and not the delisted one")
        }
    }
}
