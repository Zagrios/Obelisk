import XCTest
@testable import Obelisk

/// Defaults of the test's own, gone once it ends: a status the test picks
/// never reaches Obelisk's.
@MainActor
func isolatedDefaults() -> UserDefaults {
    let suite = "com.zagrios.obelisk.tests.\(UUID().uuidString)"
    let defaults = UserDefaults(suiteName: suite)!
    defaults.removePersistentDomain(forName: suite)
    return defaults
}

@MainActor
final class OnlineStatusTests: XCTestCase {
    func testTheChoiceIsKeptPerStorefrontAndToldAtOnceThenOnEachChange() {
        let defaults = isolatedDefaults()
        let steam = OnlineStatusChoice(storeID: "steam", defaults: defaults)
        XCTAssertEqual(steam.status, .online)
        var told: [OnlineStatus] = []
        steam.follow { told.append($0) }

        steam.choose(.away)
        steam.choose(.away)
        steam.choose(.invisible)

        XCTAssertEqual(told, [.online, .away, .invisible])
        XCTAssertEqual(OnlineStatusChoice(storeID: "steam", defaults: defaults).status, .invisible)
        XCTAssertEqual(OnlineStatusChoice(storeID: "epic", defaults: defaults).status, .online)
    }

    func testOnlyStorefrontsWithFriendsOfferAStatus() {
        XCTAssertTrue(EpicGames.Presence.showsStatus)
        XCTAssertFalse(GogGames.Presence.showsStatus)
    }

    func testEpicFriendsRegisterTheChosenStatusBeforeTheyConnect() async throws {
        let friends = EpicFriends()
        friends.setStatus(.invisible)
        let session = EpicFriendsTestSession()

        friends.connect(session, account: "epic-account")
        try await eventually { session.starts == 1 }
        friends.setStatus(.away)

        XCTAssertEqual(session.social, ["status:invisible", "start", "status:away"])
        friends.revoke()
    }
}
