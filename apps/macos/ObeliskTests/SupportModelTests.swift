import XCTest
@testable import Obelisk

@MainActor
final class SupportModelTests: XCTestCase {
    /// The model one start of Obelisk makes over `defaults`: GitHub tells
    /// `stars`, Discord `members`, and nothing leaves the test.
    private func start(_ defaults: UserDefaults, stars: Int? = nil, members: Int? = nil) -> SupportModel {
        SupportModel(defaults: defaults, stars: { stars }, members: { members })
    }

    func testTheSheetIsDueAtTheThirdStart() {
        let defaults = isolatedDefaults()
        for count in 1...2 {
            let model = start(defaults)
            model.countStart()
            model.offer()
            XCTAssertFalse(model.isDue, "start \(count)")
            XCTAssertFalse(model.isPresented, "start \(count)")
        }

        let third = start(defaults)
        third.countStart()
        XCTAssertTrue(third.isDue)
        third.offer()
        XCTAssertTrue(third.isPresented)
    }

    func testAStartThatCouldNotShowTheSheetLeavesItDueAndItShowsByItselfOnce() {
        let defaults = isolatedDefaults()
        for _ in 1...4 { start(defaults).countStart() }
        XCTAssertEqual(defaults.integer(forKey: SupportModel.startsKey), SupportModel.offeredAtStart,
                       "starts are counted up to the one the sheet shows at")

        let fourth = start(defaults)
        XCTAssertTrue(fourth.isDue, "the third start did not show it")
        fourth.offer()
        XCTAssertTrue(fourth.isPresented)

        let next = start(defaults)
        next.countStart()
        next.offer()
        XCTAssertFalse(next.isDue)
        XCTAssertFalse(next.isPresented)
    }

    func testTheSheetShowsTheCountsGitHubAndDiscordTellAndNoneWhenTheyDoNotAnswer() async throws {
        let told = start(isolatedDefaults(), stars: 1284, members: 2410)
        told.present()
        try await eventually { told.stars == 1284 && told.members == 2410 }

        let silent = start(isolatedDefaults(), members: 2410)
        silent.present()
        try await eventually { silent.members == 2410 }
        XCTAssertNil(silent.stars, "one count does not wait for the other")
    }

    func testAnAnswerGivesTheCountItHoldsUnderItsKey() {
        let repository = Data(#"{"full_name":"Zagrios/obelisk","stargazers_count":1284}"#.utf8)
        XCTAssertEqual(SupportModel.count("stargazers_count", in: repository), 1284)
        let invite = Data(#"{"code":"h5a7F4rZ6f","approximate_member_count":2410,"approximate_presence_count":310}"#.utf8)
        XCTAssertEqual(SupportModel.count("approximate_member_count", in: invite), 2410)
        XCTAssertNil(SupportModel.count("stargazers_count", in: Data(#"{"message":"API rate limit exceeded"}"#.utf8)))
        XCTAssertNil(SupportModel.count("stargazers_count", in: Data("<html>".utf8)))
    }

    // MARK: At Obelisk's start

    /// A player signed in to Steam, whose library shows, at Obelisk's third start.
    private func thirdStart(signedIn: Bool = true) async -> StorefrontsFixture {
        let fixture = StorefrontsFixture(StorefrontsFixture.Setup(steam: signedIn ? [StorefrontsFixture.portal] : nil))
        await fixture.signIn()
        fixture.defaults.set(SupportModel.offeredAtStart, forKey: SupportModel.startsKey)
        return fixture
    }

    func testTheThirdStartOffersSupportToAPlayerWhoseLibraryShows() async {
        let fixture = await thirdStart()
        defer { fixture.finish() }

        await fixture.app.offerSupport(after: .zero)

        XCTAssertTrue(fixture.app.support.isPresented)
    }

    func testAStartWithSomethingOverTheWindowLeavesSupportForLater() async {
        let fixture = await thirdStart()
        defer { fixture.finish() }
        fixture.app.downloads.isPresented = true

        await fixture.app.offerSupport(after: .zero)
        XCTAssertFalse(fixture.app.support.isPresented)
        XCTAssertTrue(fixture.app.support.isDue)

        // The window opens again, or Obelisk starts once more.
        fixture.app.downloads.isPresented = false
        await fixture.app.offerSupport(after: .zero)
        XCTAssertTrue(fixture.app.support.isPresented)
    }

    func testAPlayerWhoNeverSignedInIsNotAskedToSupportObelisk() async {
        let fixture = await thirdStart(signedIn: false)
        defer { fixture.finish() }

        await fixture.app.offerSupport(after: .zero)

        XCTAssertFalse(fixture.app.support.isPresented)
        XCTAssertTrue(fixture.app.support.isDue)
    }
}
