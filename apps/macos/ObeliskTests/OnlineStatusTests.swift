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

/// What a test's follower was told, in order.
@MainActor
private final class Told {
    var statuses: [OnlineStatus] = []
}

@MainActor
final class OnlineStatusTests: XCTestCase {
    func testTheChoiceIsKeptPerStorefrontAndToldAtOnceThenOnEachChange() {
        let defaults = isolatedDefaults()
        let steam = OnlineStatusChoice(storeID: "steam", defaults: defaults)
        XCTAssertEqual(steam.status, .online)
        let told = Told()
        steam.follow(told) { $0.statuses.append($1) }

        steam.choose(.away)
        steam.choose(.away)
        steam.choose(.invisible)

        XCTAssertEqual(told.statuses, [.online, .away, .invisible])
        XCTAssertEqual(OnlineStatusChoice(storeID: "steam", defaults: defaults).status, .invisible)
        XCTAssertEqual(OnlineStatusChoice(storeID: "epic", defaults: defaults).status, .online)
    }

    /// A storefront's accounts each ask the storefront's Game sessions to
    /// follow its status, and an account whose slot went follows no more.
    func testWhatFollowsTheStatusFollowsItOnceAndForAsLongAsItLives() {
        let choice = OnlineStatusChoice(storeID: "steam", defaults: isolatedDefaults())
        let shared = Told()
        choice.follow(shared) { $0.statuses.append($1) }
        choice.follow(shared) { $0.statuses.append($1) }
        var gone: Told? = Told()
        weak let released = gone
        choice.follow(gone!) { $0.statuses.append($1) }

        gone = nil
        choice.choose(.away)

        XCTAssertEqual(shared.statuses, [.online, .away], "Told once, however many asked")
        XCTAssertNil(released, "The status keeps nothing alive")
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
