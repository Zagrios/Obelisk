import AppKit
import Foundation
import SwiftUI
import XCTest
@testable import Obelisk

/// What AppModel does across the three storefronts, over accounts the test
/// signs in: the prompts it shows over the whole app, which storefront's
/// friends show, how Obelisk restores the saved sessions, and how it reaches a
/// queued Transfer's storefront.
@MainActor
final class AppModelStorefrontsTests: XCTestCase {
    // MARK: Prompts

    private func prompt(_ section: LibrarySection, _ kind: AccountPrompt.Kind) -> AccountPrompt {
        AccountPrompt(section: section, kind: kind)
    }

    /// Every prompt waiting at once: three storefronts, a newer Voile, Rosetta
    /// and a Cloud question each, but GOG's, which asks none.
    private func askEverything(_ fixture: StorefrontsFixture) async throws {
        let steam = GameSessionState.steam(appID: 620, accountID: StorefrontsFixture.steamAccount,
                                           phase: .preparing, observed: false).askingForVoile()
        fixture.steamSessions.fake.show(steam)
        fixture.steamSessions.fake.requireRosetta(decisionID: 1)
        fixture.steamSessions.refresh()
        fixture.steamBackend.publish { $0.cloudSync = .exit(620, question: .conflict(1)) }

        let epic = GameSessionState.epic(account: StorefrontsFixture.epicAccount, namespace: "fixture", appName: "one",
                                         phase: .preparing, observed: false).askingForVoile()
        fixture.epicSessions.fake.show(epic)
        fixture.epicSessions.fake.requireRosetta(decisionID: 2)
        fixture.epicSessions.refresh()
        fixture.epicBackend.publish { $0.cloudSync = .exit("fixture:one", question: .conflict(2)) }

        let gog = GameSessionState.gog(phase: .preparing, observed: false).askingForVoile()
        fixture.gogSessions.fake.show(gog)
        fixture.gogSessions.fake.requireRosetta(decisionID: 3)
        fixture.gogSessions.refresh()

        try await eventually { fixture.steam.cloud.question != nil && fixture.epic.cloud.question != nil }
    }

    func testPromptsShowOneAtATimeSteamsFirstThenEpicsThenGogsEachVoileThenRosettaThenCloud() async throws {
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: []))
        defer { fixture.finish() }
        await fixture.signIn()
        XCTAssertNil(fixture.app.presentedPrompt)

        try await askEverything(fixture)
        XCTAssertEqual(fixture.app.presentedPrompt, prompt(.steam, .voile))

        fixture.steamSessions.answerVoileUpdate(.skip)
        XCTAssertEqual(fixture.app.presentedPrompt, prompt(.steam, .rosetta))
        fixture.steamSessions.fake.requireRosetta(decisionID: nil)
        fixture.steamSessions.refresh()
        XCTAssertEqual(fixture.app.presentedPrompt, prompt(.steam, .cloud))
        fixture.steamBackend.publish { $0.cloudSync = nil }
        try await eventually { fixture.app.presentedPrompt == prompt(.epic, .voile) }

        fixture.epicSessions.answerVoileUpdate(.skip)
        XCTAssertEqual(fixture.app.presentedPrompt, prompt(.epic, .rosetta))
        fixture.epicSessions.fake.requireRosetta(decisionID: nil)
        fixture.epicSessions.refresh()
        XCTAssertEqual(fixture.app.presentedPrompt, prompt(.epic, .cloud))
        fixture.epicBackend.publish { $0.cloudSync = nil }
        try await eventually { fixture.app.presentedPrompt == prompt(.gog, .voile) }

        fixture.gogSessions.answerVoileUpdate(.skip)
        XCTAssertEqual(fixture.app.presentedPrompt, prompt(.gog, .rosetta))
        fixture.gogSessions.fake.requireRosetta(decisionID: nil)
        fixture.gogSessions.refresh()
        XCTAssertNil(fixture.app.presentedPrompt)
    }

    func testAPromptThatCameLaterStillGoesFirstWhenItsStorefrontIsEarlier() async throws {
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: []))
        defer { fixture.finish() }
        await fixture.signIn()

        fixture.gogSessions.fake.show(.gog(phase: .preparing, observed: false))
        fixture.gogSessions.fake.requireRosetta(decisionID: 3)
        fixture.gogSessions.refresh()
        XCTAssertEqual(fixture.app.presentedPrompt, prompt(.gog, .rosetta))

        fixture.epicBackend.publish { $0.cloudSync = .maintenance("fixture:one", question: .conflict(2)) }
        try await eventually { fixture.app.presentedPrompt == prompt(.epic, .cloud) }

        fixture.steamSessions.fake.show(.steam(appID: 620, accountID: StorefrontsFixture.steamAccount,
                                               phase: .preparing, observed: false))
        fixture.steamSessions.fake.requireRosetta(decisionID: 1)
        fixture.steamSessions.refresh()
        XCTAssertEqual(fixture.app.presentedPrompt, prompt(.steam, .rosetta))

        fixture.steamSessions.fake.show(GameSessionState.steam(appID: 620, accountID: StorefrontsFixture.steamAccount,
                                                               phase: .preparing, observed: false).askingForVoile())
        fixture.steamSessions.refresh()
        XCTAssertEqual(fixture.app.presentedPrompt, prompt(.steam, .voile), "A newer Voile comes before the same storefront's Rosetta")
    }

    func testAGogCloudQuestionIsNotAPromptOfTheApp() async throws {
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: []))
        defer { fixture.finish() }
        await fixture.signIn()

        fixture.gogBackend.publish { $0.cloudSync = .exit("1207658924", question: .conflict(1)) }
        try await eventually { fixture.gog.cloud.question != nil }

        XCTAssertNil(fixture.gog.pendingPrompt)
        XCTAssertNil(fixture.app.presentedPrompt, "GOG asks no Cloud question: Obelisk does not sync its saves")
    }

    /// The window presents the sheet of the prompt the app names, over
    /// whichever tab is open, a sheet of its own for each storefront's each
    /// question.
    func testTheWindowPresentsTheSheetOfThePromptTheAppNamesAndReplacesItWhenAnotherComesFirst() async throws {
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: []))
        defer { fixture.finish() }
        await fixture.signIn()
        fixture.app.selection = .settings
        let host = NSHostingView(rootView: RootView(model: fixture.app))
        let window = NSWindow(contentRect: NSRect(x: -20_000, y: -20_000, width: 1120, height: 760),
                              styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        window.orderFront(nil)
        defer { window.close() }
        func sheetSize() -> CGSize? {
            window.sheets.count == 1 ? window.sheets.first?.frame.size : nil
        }
        func isPresenting(_ prompt: AccountPrompt) async throws {
            try await eventually {
                host.layoutSubtreeIfNeeded()
                return fixture.app.presentedPrompt == prompt && (sheetSize().map { $0.width > 100 && $0.height > 100 } ?? false)
            }
        }
        await drainMainQueue(times: 5)
        XCTAssertTrue(window.sheets.isEmpty, "No question waits")

        fixture.gogSessions.fake.show(.gog(phase: .preparing, observed: false))
        fixture.gogSessions.fake.requireRosetta(decisionID: 3)
        fixture.gogSessions.refresh()
        try await isPresenting(prompt(.gog, .rosetta))
        let rosetta = sheetSize()

        fixture.steamSessions.fake.show(.steam(appID: 620, accountID: StorefrontsFixture.steamAccount,
                                               phase: .preparing, observed: false).askingForVoile())
        fixture.steamSessions.refresh()
        try await isPresenting(prompt(.steam, .voile))
        // The sheet is Steam's Voile when the window takes its size.
        try await eventually {
            host.layoutSubtreeIfNeeded()
            return sheetSize() != rosetta
        }

        fixture.steamSessions.answerVoileUpdate(.skip)
        fixture.steamSessions.fake.show(nil)
        fixture.steamSessions.refresh()
        try await isPresenting(prompt(.gog, .rosetta))
        try await eventually {
            host.layoutSubtreeIfNeeded()
            return sheetSize() == rosetta
        }

        fixture.gogSessions.fake.requireRosetta(decisionID: nil)
        fixture.gogSessions.refresh()
        try await eventually {
            host.layoutSubtreeIfNeeded()
            return fixture.app.presentedPrompt == nil && window.sheets.isEmpty
        }
    }

    // MARK: Failures and warnings

    func testEachAccountTellsItsFailureAndItsStorageWarningAndTheWindowDismissesThem() async throws {
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: []))
        defer { fixture.finish() }
        await fixture.signIn()
        let accounts = fixture.app.accounts
        for account in accounts {
            XCTAssertNil(account.errorMessage, "\(account.section)")
            XCTAssertNil(account.storageWarning, "\(account.section)")
        }

        fixture.steam.errorMessage = "Steam failed"
        fixture.epic.storageWarning = "Epic's receipts were not saved"
        XCTAssertEqual(accounts.map(\.errorMessage), ["Steam failed", nil, nil])
        XCTAssertEqual(accounts.map(\.storageWarning), [nil, "Epic's receipts were not saved", nil])

        for account in accounts {
            account.errorMessage = nil
            account.storageWarning = nil
        }
        XCTAssertNil(fixture.steam.errorMessage)
        XCTAssertNil(fixture.epic.storageWarning)
    }

    func testAnAccountWaitsForItsFirstGamesOnlyWhileItLoadsAnEmptyLibrary() async throws {
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: [StorefrontsFixture.gogGame()]))
        defer { fixture.finish() }
        await fixture.signIn()
        let accounts = fixture.app.accounts
        XCTAssertEqual(accounts.map(\.isFetchingFirstGames), [false, false, false])

        await fixture.epicWeb.holdNextGames()
        fixture.epic.refresh()
        try await eventually { fixture.app.account(.epic)?.isFetchingFirstGames == true }
        XCTAssertEqual(accounts.map(\.isFetchingFirstGames), [false, true, false])

        try await eventually { await fixture.epicWeb.waitingForGames }
        await fixture.epicWeb.completeGames([])
        try await eventually { fixture.app.account(.epic)?.isFetchingFirstGames == false }

        await fixture.gogSession.holdNextGames()
        fixture.gog.refresh()
        try await eventually { await fixture.gogSession.waitingForGames }
        XCTAssertTrue(fixture.gog.isLoadingGames)
        XCTAssertFalse(fixture.gog.isFetchingFirstGames, "Its games are on screen while it reloads")
        await fixture.gogSession.completeGames([StorefrontsFixture.gogGame()])
        try await eventually { !fixture.gog.isLoadingGames }
    }

    // MARK: Friends

    /// Opens a storefront's friends from its own menu, and waits for the app to
    /// present them.
    private func openFriends(of fixture: StorefrontsFixture, _ section: LibrarySection) async throws {
        switch section {
        case .steam: fixture.steam.openFriends()
        default: fixture.epic.openFriends()
        }
        try await eventually { fixture.app.activeFriendsAccount?.storeID == section.rawValue }
    }

    func testTheFriendsOfTheStorefrontJustOpenedShowAndTheOtherStorefrontsCloseTheirs() async throws {
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: []))
        defer { fixture.finish() }
        await fixture.signIn()
        XCTAssertNil(fixture.app.activeFriendsAccount)
        XCTAssertNil(fixture.app.presentedFriends)

        try await openFriends(of: fixture, .steam)
        XCTAssertEqual(fixture.app.activeFriendsAccount, fixture.steam.friends?.account)
        XCTAssertEqual(fixture.app.presentedFriends?.account.storeID, "steam")
        XCTAssertEqual(fixture.app.presentedFriends?.account.accountID, String(StorefrontsFixture.steamAccount))

        try await openFriends(of: fixture, .epic)
        XCTAssertEqual(fixture.app.activeFriendsAccount, fixture.epic.friends?.account)
        XCTAssertEqual(fixture.app.presentedFriends?.account.storeID, "epic")
        XCTAssertFalse(fixture.steam.isFriendsPresented, "Opening Epic's friends closes Steam's")
        XCTAssertTrue(fixture.epic.isFriendsPresented)

        try await openFriends(of: fixture, .steam)
        XCTAssertEqual(fixture.app.presentedFriends?.account.storeID, "steam")
        XCTAssertFalse(fixture.epic.isFriendsPresented, "Opening Steam's friends closes Epic's")
    }

    func testGogAndASignedOutStorefrontPresentNoFriends() async throws {
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: []))
        defer { fixture.finish() }
        await fixture.signIn()
        try await openFriends(of: fixture, .steam)
        let steam = fixture.steam.friends?.account

        XCTAssertNil(fixture.gog.friends)
        XCTAssertFalse(fixture.gog.isFriendsPresented)
        fixture.gog.openFriends()
        fixture.gog.retryFriends()
        XCTAssertFalse(fixture.gog.isFriendsPresented, "GOG has no friends to open")
        await drainMainQueue(times: 3)
        XCTAssertEqual(fixture.app.activeFriendsAccount, steam, "GOG shows no friends")
        XCTAssertEqual(fixture.app.presentedFriends?.account.storeID, "steam")

        fixture.epic.signOut()
        XCTAssertNil(fixture.epic.friends)
        fixture.epic.openFriends()
        XCTAssertFalse(fixture.epic.isFriendsPresented, "A signed-out Epic has no friends to open")
        await drainMainQueue(times: 3)
        XCTAssertEqual(fixture.app.activeFriendsAccount, steam, "A signed-out Epic has none to show")

        fixture.app.closeFriends()
        fixture.epic.openFriends()
        await drainMainQueue(times: 3)
        XCTAssertNil(fixture.app.activeFriendsAccount)
    }

    func testClosingTheFriendsOfAnotherStorefrontLeavesThePresentedOnesOpen() async throws {
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: []))
        defer { fixture.finish() }
        await fixture.signIn()
        try await openFriends(of: fixture, .steam)

        fixture.epic.closeFriends()
        fixture.gog.closeFriends()
        await drainMainQueue(times: 3)
        XCTAssertEqual(fixture.app.presentedFriends?.account.storeID, "steam")
        XCTAssertTrue(fixture.steam.isFriendsPresented)

        fixture.steam.closeFriends()
        try await eventually { fixture.app.activeFriendsAccount == nil }
        XCTAssertNil(fixture.app.presentedFriends)
        XCTAssertFalse(fixture.steam.isFriendsPresented)
    }

    func testClosingTheFriendsWithoutANameClosesEveryStorefrontsList() async throws {
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: []))
        defer { fixture.finish() }
        await fixture.signIn()
        try await openFriends(of: fixture, .steam)
        // Epic's list opens before the app heard of it.
        fixture.epic.openFriends()
        XCTAssertTrue(fixture.steam.isFriendsPresented)
        XCTAssertTrue(fixture.epic.isFriendsPresented)

        fixture.app.closeFriends()

        XCTAssertNil(fixture.app.activeFriendsAccount)
        XCTAssertFalse(fixture.steam.isFriendsPresented)
        XCTAssertFalse(fixture.epic.isFriendsPresented, "Epic's list closes though the app presented Steam's")
        await drainMainQueue(times: 3)
        XCTAssertNil(fixture.app.activeFriendsAccount)
    }

    func testTwoFriendsListsOpenedInOneTurnLeaveTheFirstStorefrontsPresented() async throws {
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: []))
        defer { fixture.finish() }
        await fixture.signIn()

        // Both menus open before the app looks at either.
        fixture.epic.openFriends()
        fixture.steam.openFriends()
        try await eventually { fixture.app.activeFriendsAccount != nil }
        await drainMainQueue(times: 5)

        XCTAssertEqual(fixture.app.presentedFriends?.account.storeID, "steam", "The sidebar's first storefront wins")
        XCTAssertTrue(fixture.steam.isFriendsPresented)
        XCTAssertFalse(fixture.epic.isFriendsPresented)
    }

    func testFriendsOfAnAccountThatSignedOutCloseOnTheirOwn() async throws {
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: []))
        defer { fixture.finish() }
        await fixture.signIn()
        try await openFriends(of: fixture, .steam)
        await drainMainQueue(times: 3)
        XCTAssertEqual(fixture.app.presentedFriends?.account.storeID, "steam", "Valid friends stay")

        fixture.steam.signOut()

        XCTAssertNil(fixture.steam.friends)
        XCTAssertNil(fixture.app.presentedFriends, "Nothing shows for an account without friends")
        try await eventually { fixture.app.activeFriendsAccount == nil }
        XCTAssertFalse(fixture.steam.isFriendsPresented)
    }

    func testRetryingThePresentedFriendsAsksThatStorefrontsSessionToConnectAgain() async throws {
        let epicSession = EpicFriendsTestSession(accountID: StorefrontsFixture.epicAccount)
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: [], epicSession: epicSession))
        defer { fixture.finish() }
        await fixture.signIn()
        try await eventually { epicSession.starts == 1 }

        fixture.app.retryPresentedFriends()
        await drainMainQueue(times: 3)
        XCTAssertEqual(fixture.steamSession.reconnects, 0, "Nothing is presented to retry")
        XCTAssertEqual(epicSession.reconnections, 0)

        try await openFriends(of: fixture, .epic)
        fixture.app.retryPresentedFriends()
        try await eventually { epicSession.reconnections == 1 }
        XCTAssertEqual(epicSession.reconnections, 1)
        XCTAssertEqual(fixture.steamSession.reconnects, 0)

        try await openFriends(of: fixture, .steam)
        fixture.app.retryPresentedFriends()
        XCTAssertEqual(fixture.steamSession.reconnects, 1)
        await drainMainQueue(times: 3)
        XCTAssertEqual(epicSession.reconnections, 1, "Steam's retry leaves Epic's socket alone")
    }

    func testSigningInFromThePresentedFriendsSignsThatStorefrontOutAndClosesThem() async throws {
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: []))
        defer { fixture.finish() }
        await fixture.signIn()

        fixture.app.signInPresentedFriends()
        XCTAssertTrue(fixture.steam.isConnected && fixture.epic.isConnected && fixture.gog.isConnected,
                      "Nothing presented, nothing signed out")

        try await openFriends(of: fixture, .epic)
        fixture.app.signInPresentedFriends()
        XCTAssertFalse(fixture.epic.isConnected)
        XCTAssertNil(fixture.app.activeFriendsAccount)
        XCTAssertTrue(fixture.steam.isConnected)
        XCTAssertTrue(fixture.gog.isConnected)

        try await openFriends(of: fixture, .steam)
        fixture.app.signInPresentedFriends()
        XCTAssertFalse(fixture.steam.isConnected)
        XCTAssertNil(fixture.app.activeFriendsAccount)
        XCTAssertTrue(fixture.gog.isConnected)
    }

    // MARK: Restoring the saved sessions

    /// The storefronts Home still tells as restoring.
    private func restoring(_ app: AppModel) -> [LibrarySection] {
        LibrarySection.storefronts.filter { app.standing(of: $0) == .restoring }
    }

    func testEveryStorefrontRestoresAtOnceAndLeavesTheRestoringListWhenItsOwnRestorationEnds() async throws {
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: [], held: [.steam, .epic, .gog]))
        defer { fixture.finish() }
        XCTAssertEqual(restoring(fixture.app), [.steam, .epic, .gog], "Home waits for all three at launch")
        XCTAssertEqual(fixture.app.homeContent, .restoring)

        let restoration = Task { await fixture.app.restoreStorefronts() }
        let gates = fixture.restoreGates
        try await eventually {
            let requested = [await gates[.steam]?.requests, await gates[.epic]?.requests, await gates[.gog]?.requests]
            return requested == [1, 1, 1]
        }
        XCTAssertTrue(fixture.steam.isRestoring && fixture.epic.isRestoring && fixture.gog.isRestoring,
                      "None waits for another to end")
        XCTAssertEqual(restoring(fixture.app), [.steam, .epic, .gog])

        await gates[.epic]?.resolve(())
        try await eventually { restoring(fixture.app) == [.steam, .gog] }
        XCTAssertTrue(fixture.epic.isConnected)

        await gates[.gog]?.resolve(())
        try await eventually { restoring(fixture.app) == [.steam] }
        XCTAssertTrue(fixture.gog.isConnected)

        await gates[.steam]?.resolve(())
        await restoration.value
        XCTAssertTrue(restoring(fixture.app).isEmpty)
        XCTAssertEqual(fixture.app.homeContent, .libraries)
        XCTAssertTrue(fixture.steam.isConnected)
        XCTAssertFalse(fixture.steam.isRestoring || fixture.epic.isRestoring || fixture.gog.isRestoring)
    }

    func testAStorefrontLeavesTheRestoringListWhenItsOwnRestorationEndsWithoutAnAccount() async throws {
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: [], held: [.steam, .epic, .gog],
                                               failing: [.steam, .epic, .gog]))
        defer { fixture.finish() }
        let restoration = Task { await fixture.app.restoreStorefronts() }
        let gates = fixture.restoreGates
        try await eventually {
            let requested = [await gates[.steam]?.requests, await gates[.epic]?.requests, await gates[.gog]?.requests]
            return requested == [1, 1, 1]
        }
        XCTAssertEqual(restoring(fixture.app), [.steam, .epic, .gog])

        await gates[.epic]?.resolve(())
        try await eventually { fixture.app.standing(of: .epic) == .signedOut(connecting: false) }
        XCTAssertEqual(restoring(fixture.app), [.steam, .gog], "Only the storefront whose restoration ended leaves")
        XCTAssertEqual(fixture.app.homeContent, .restoring)

        await gates[.steam]?.resolve(())
        try await eventually { fixture.app.standing(of: .steam) == .signedOut(connecting: false) }
        XCTAssertEqual(restoring(fixture.app), [.gog])
        XCTAssertEqual(fixture.app.homeContent, .restoring)

        await gates[.gog]?.resolve(())
        await restoration.value
        XCTAssertEqual(fixture.app.standing(of: .gog), .signedOut(connecting: false))
        XCTAssertEqual(fixture.app.homeContent, .signIn)
    }

    func testASignedOutStorefrontIsRestoredWithoutOfferingASignInAndLeavesTheList() async throws {
        let fixture = StorefrontsFixture()
        defer { fixture.finish() }

        await fixture.signIn()

        let logins = await fixture.logins.count
        XCTAssertEqual(logins, 0, "A signed-out Steam waits for its own tab to ask for a QR code")
        XCTAssertNil(fixture.steam.qrError)
        XCTAssertTrue(restoring(fixture.app).isEmpty)
        XCTAssertEqual(fixture.app.homeContent, .signIn)
        XCTAssertFalse(fixture.steam.isConnected || fixture.epic.isConnected || fixture.gog.isConnected)
        XCTAssertFalse(fixture.steam.showsLibrary || fixture.epic.showsLibrary || fixture.gog.showsLibrary)
        XCTAssertFalse(fixture.steam.isRestoring || fixture.epic.isRestoring || fixture.gog.isRestoring)
    }

    func testARestorationThatFailsStillLeavesTheListAndTellsItsStorefrontsError() async throws {
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: [], failing: [.steam, .epic, .gog]))
        defer { fixture.finish() }

        await fixture.signIn()

        XCTAssertTrue(restoring(fixture.app).isEmpty)
        XCTAssertEqual(fixture.app.homeContent, .signIn)
        XCTAssertNotNil(fixture.steam.errorMessage)
        XCTAssertEqual(fixture.epic.errorMessage, EpicGames.describe(EpicGames.network))
        XCTAssertEqual(fixture.gog.errorMessage, GogGames.describe(GogGames.network))
        XCTAssertFalse(fixture.steam.showsLibrary || fixture.epic.showsLibrary || fixture.gog.showsLibrary)
        XCTAssertFalse(fixture.steam.isRestoring || fixture.epic.isRestoring || fixture.gog.isRestoring)
    }

    // MARK: The download queue

    func testAQueuedTransfersProgressComesFromTheInstallationsOfItsOwnStorefront() async throws {
        let fixture = StorefrontsFixture(.init(steam: [], epic: [], gog: []))
        defer { fixture.finish() }
        await fixture.signIn()
        func transfer(_ game: String, bytes: UInt64) -> GameTransfer {
            GameTransfer(game: game, state: .downloading, completedBytes: bytes, totalBytes: 100, networkBytes: bytes,
                         contentBytes: bytes, currentFile: "", message: "")
        }
        fixture.steamBackend.publish {
            $0.transferGame = 620
            $0.transfer = transfer("620", bytes: 10)
        }
        fixture.epicBackend.publish {
            $0.transferGame = "fixture:one"
            $0.transfer = transfer("one", bytes: 20)
        }
        fixture.gogBackend.publish {
            $0.transferGame = "1207658924"
            $0.transfer = transfer("1207658924", bytes: 30)
        }
        try await eventually {
            fixture.steam.installations.transfer != nil && fixture.epic.installations.transfer != nil
                && fixture.gog.installations.transfer != nil
        }

        let app = fixture.app
        XCTAssertEqual(app.downloadTransfer(.queued(.steam, game: "620"))?.completedBytes, 10)
        XCTAssertEqual(app.downloadTransfer(.queued(.epic, game: "fixture:one"))?.completedBytes, 20)
        XCTAssertEqual(app.downloadTransfer(.queued(.gog, game: "1207658924"))?.completedBytes, 30)

        XCTAssertNil(app.downloadTransfer(.queued(.steam, game: "730")), "Another game of the storefront")
        XCTAssertNil(app.downloadTransfer(.queued(.epic, game: "620")), "Epic names its games by namespace")
        XCTAssertNil(app.downloadTransfer(.queued(.gog, game: "fixture:one")), "GOG names its games by product")
        XCTAssertNil(app.downloadTransfer(.queued(.steam, game: "1207658924")))
    }

    func testTheQueueShowsAGameByItsLibraryOrElseByTheNameItWasQueuedUnder() async throws {
        let fixture = StorefrontsFixture(.init(steam: [StorefrontsFixture.portal], epic: [StorefrontsFixture.epicGame()],
                                               gog: [StorefrontsFixture.gogGame()]))
        defer { fixture.finish() }
        await fixture.signIn()
        let app = fixture.app

        let known: [(DownloadItem, String)] = [
            (.queued(.steam, game: "620"), "Portal 2"),
            (.queued(.epic, game: "fixture:one"), "Epic One"),
            (.queued(.gog, game: "1207658924"), "GOG One"),
        ]
        for (item, title) in known {
            let game = app.downloadGame(item)
            XCTAssertEqual(game.title, title, "The library names the game, not the queue")
            XCTAssertNotNil(game.cover)
            XCTAssertNotNil(game.artwork)
        }

        let unknown: [DownloadItem] = [
            .queued(.steam, game: "730"), .queued(.epic, game: "fixture:two"), .queued(.gog, game: "1"),
            .queued(.steam, game: "1207658924"), .queued(.epic, game: "620"), .queued(.gog, game: "fixture:one"),
        ]
        for item in unknown {
            let game = app.downloadGame(item)
            XCTAssertEqual(game.title, "Queued name", "\(item.storefront) \(item.game)")
            XCTAssertNil(game.cover)
            XCTAssertNil(game.artwork)
        }
    }

    func testOpeningAQueuedDownloadClosesTheQueueAndOpensItsGamePageInItsOwnLibrary() async throws {
        let fixture = StorefrontsFixture(.init(steam: [StorefrontsFixture.portal], epic: [StorefrontsFixture.epicGame()],
                                               gog: [StorefrontsFixture.gogGame()]))
        defer { fixture.finish() }
        await fixture.signIn()
        let app = fixture.app

        func open(_ item: DownloadItem) {
            app.selection = .home
            app.searchText = "search"
            app.downloads.isPresented = true
            app.openDownload(item)
            XCTAssertFalse(app.downloads.isPresented)
        }

        open(.queued(.epic, game: "fixture:one"))
        XCTAssertEqual(app.selection, .epic)
        XCTAssertEqual(fixture.epic.selectedGame?.id, "fixture:one")
        XCTAssertNil(fixture.steam.selectedGame)
        XCTAssertNil(fixture.gog.selectedGame)
        XCTAssertEqual(app.searchText, "", "Another section starts without a search")

        open(.queued(.gog, game: "1207658924"))
        XCTAssertEqual(app.selection, .gog)
        XCTAssertEqual(fixture.gog.selectedGame?.id, "1207658924")

        open(.queued(.steam, game: "620"))
        XCTAssertEqual(app.selection, .steam)
        XCTAssertEqual(fixture.steam.selectedGame?.appId, 620)
    }

    func testAQueuedDownloadWhoseGameItsLibraryDoesNotListOnlyClosesTheQueue() async throws {
        let fixture = StorefrontsFixture(.init(steam: [StorefrontsFixture.portal], epic: nil, gog: []))
        defer { fixture.finish() }
        await fixture.signIn()
        let app = fixture.app
        app.selection = .home

        for item in [DownloadItem.queued(.steam, game: "730"), .queued(.epic, game: "fixture:one"),
                     .queued(.gog, game: "1207658924"), .queued(.gog, game: "fixture:one")] {
            app.downloads.isPresented = true
            app.openDownload(item)
            XCTAssertFalse(app.downloads.isPresented, "\(item.storefront) \(item.game)")
            XCTAssertEqual(app.selection, .home)
        }
        XCTAssertNil(fixture.steam.selectedGame)
        XCTAssertNil(fixture.epic.selectedGame)
        XCTAssertNil(fixture.gog.selectedGame)
    }

    func testAnAccountOpensAGameItsLibraryListsAndNothingElseWhoeverNamedIt() async throws {
        let fixture = gamesFixture()
        defer { fixture.finish() }
        await fixture.signIn()
        let accounts = fixture.app.accounts
        func noGamePage() {
            XCTAssertNil(fixture.steam.selectedGame)
            XCTAssertNil(fixture.epic.selectedGame)
            XCTAssertNil(fixture.gog.selectedGame)
        }

        let names = ["", "not a game", "730", "fixture:two", "1", "-1", "99999999999999999999", ":", "fixture:"]
        for account in accounts {
            for name in names {
                XCTAssertFalse(account.openGame(named: name), "\(account.section) \(name)")
                XCTAssertNil(account.transfer(named: name), "\(account.section) \(name)")
            }
        }
        XCTAssertFalse(fixture.steam.openGame(named: "fixture:one"), "Epic's game is no Steam game")
        XCTAssertFalse(fixture.epic.openGame(named: "620"), "Epic names its games by namespace")
        XCTAssertFalse(fixture.gog.openGame(named: "fixture:one"), "GOG names its games by product")
        XCTAssertFalse(fixture.steam.openGame(named: "1207658924"), "The library of another storefront")
        noGamePage()

        XCTAssertTrue(fixture.steam.openGame(named: "620"))
        XCTAssertEqual(fixture.steam.selectedGame?.appId, 620)
        XCTAssertTrue(fixture.epic.openGame(named: "fixture:one"))
        XCTAssertEqual(fixture.epic.selectedGame?.id, "fixture:one")
        XCTAssertTrue(fixture.gog.openGame(named: "1207658924"))
        XCTAssertEqual(fixture.gog.selectedGame?.id, "1207658924")

        for account in accounts { account.closeGamePage() }
        noGamePage()
        for account in accounts { account.closeGamePage() }
        noGamePage()
    }

    // MARK: Sections

    /// Every storefront on a game's page, and Home told it opened Epic's.
    private func openGames(_ fixture: StorefrontsFixture) {
        fixture.steam.open(StorefrontsFixture.portal)
        fixture.epic.open(StorefrontsFixture.epicGame())
        fixture.gog.open(StorefrontsFixture.gogGame())
        fixture.app.homeGame = HomeGame.ID(section: .epic, game: "fixture:one")
    }

    private func gamesFixture() -> StorefrontsFixture {
        StorefrontsFixture(.init(steam: [StorefrontsFixture.portal], epic: [StorefrontsFixture.epicGame()],
                                 gog: [StorefrontsFixture.gogGame()]))
    }

    func testShowingAStorefrontReturnsItToItsLibraryAndHomeToTheGameItOpened() async throws {
        let fixture = gamesFixture()
        defer { fixture.finish() }
        await fixture.signIn()
        let app = fixture.app
        openGames(fixture)

        app.show(.steam)
        XCTAssertEqual(app.selection, .steam)
        XCTAssertNil(fixture.steam.selectedGame, "A storefront shows its library, not the game left open in it")
        XCTAssertNotNil(fixture.epic.selectedGame)
        XCTAssertNotNil(fixture.gog.selectedGame)
        XCTAssertEqual(app.homeGame, HomeGame.ID(section: .epic, game: "fixture:one"),
                       "Another storefront's game page is Home's to close")

        app.show(.home)
        XCTAssertEqual(app.selection, .home)
        XCTAssertNil(fixture.epic.selectedGame, "Home returns from the game page it opened")
        XCTAssertNil(app.homeGame)
        XCTAssertNotNil(fixture.gog.selectedGame, "Only the storefront Home opened")

        app.show(.settings)
        XCTAssertEqual(app.selection, .settings)
        XCTAssertNotNil(fixture.gog.selectedGame)
        app.show(.gog)
        XCTAssertNil(fixture.gog.selectedGame)
    }

    func testShowingTheStorefrontHomeOpenedTheGameOfClearsHomesGameToo() async throws {
        let fixture = gamesFixture()
        defer { fixture.finish() }
        await fixture.signIn()
        openGames(fixture)

        fixture.app.show(.epic)

        XCTAssertNil(fixture.epic.selectedGame)
        XCTAssertNil(fixture.app.homeGame)
    }

    func testAskingForTheSameSectionAgainChangesNothing() async throws {
        let fixture = gamesFixture()
        defer { fixture.finish() }
        await fixture.signIn()
        let app = fixture.app

        app.show(.steam)
        app.searchText = "portal"
        app.show(.steam)
        XCTAssertEqual(app.searchText, "portal", "The sidebar asks twice for one click")

        app.show(.epic)
        XCTAssertEqual(app.searchText, "", "Another section clears the search")
    }

    func testRefreshReloadsTheOpenStorefrontsLibraryHomeEveryStorefrontsAndSettingsNone() async throws {
        let fixture = gamesFixture()
        defer { fixture.finish() }
        await fixture.signIn()
        let app = fixture.app

        func calls() async -> [Int] {
            let epic = await fixture.epicWeb.gameCalls
            let gog = await fixture.gogSession.gameCalls
            return [fixture.steamSession.gameCalls, epic, gog]
        }
        func reloaded(_ expected: [Int]) async throws {
            try await eventually {
                let now = await calls()
                return now == expected && !fixture.steam.isLoadingGames && !fixture.epic.isLoadingGames
                    && !fixture.gog.isLoadingGames
            }
        }
        var expected = await calls()
        try await reloaded(expected)

        app.selection = .settings
        app.refresh()
        await drainMainQueue(times: 5)
        let afterSettings = await calls()
        XCTAssertEqual(afterSettings, expected, "Settings has no library")

        app.selection = .epic
        app.refresh()
        expected[1] += 1
        try await reloaded(expected)
        await drainMainQueue(times: 5)
        let afterEpic = await calls()
        XCTAssertEqual(afterEpic, expected, "Only the open storefront reloads")

        app.selection = .gog
        app.refresh()
        expected[2] += 1
        try await reloaded(expected)

        app.selection = .steam
        app.refresh()
        expected[0] += 1
        try await reloaded(expected)

        app.selection = .home
        app.refresh()
        expected = expected.map { $0 + 1 }
        try await reloaded(expected)
    }

    // MARK: Home's games

    func testHomeNamesAGameByItsStorefrontAndItsLibraryIDAndOpensItsPageThere() async throws {
        let fixture = gamesFixture()
        defer { fixture.finish() }
        await fixture.signIn()
        func library<Model: LibraryStorefront>(_ model: Model, _ section: LibrarySection) -> HomeLibrary {
            model.homeLibrary(in: section, cover: { _ in EmptyView() }, heroArtwork: { _ in EmptyView() },
                              heroLogo: { _ in EmptyView() })
        }
        let steam = library(fixture.steam, .steam)
        let epic = library(fixture.epic, .epic)
        let gog = library(fixture.gog, .gog)

        XCTAssertEqual(steam.covers.map(\.id), [HomeGame.ID(section: .steam, game: "620")])
        XCTAssertEqual(epic.covers.map(\.id), [HomeGame.ID(section: .epic, game: "fixture:one")])
        XCTAssertEqual(gog.covers.map(\.id), [HomeGame.ID(section: .gog, game: "1207658924")])

        try XCTUnwrap(epic.covers.first).open()
        XCTAssertEqual(fixture.epic.selectedGame?.id, "fixture:one")
        XCTAssertNil(fixture.steam.selectedGame)
        XCTAssertNil(fixture.gog.selectedGame)
        try XCTUnwrap(gog.covers.first).open()
        XCTAssertEqual(fixture.gog.selectedGame?.id, "1207658924")
        try XCTUnwrap(steam.covers.first).open()
        XCTAssertEqual(fixture.steam.selectedGame?.appId, 620)
    }

    // MARK: Installed games outside the games folder

    func testGamesOutsideTheGamesFolderCountEachFolderOnceAcrossTheStorefronts() async throws {
        let folder = "/library/Games"
        let fixture = StorefrontsFixture(.init(
            steam: [], epic: [], gog: [],
            steamReceipts: [
                .steam(appId: 620, name: "Portal 2", directory: "\(folder)/steam-620", platform: .windows),
                .steam(appId: 730, name: "Counter-Strike 2", directory: "/Volumes/Old/steam-730", platform: .mac),
            ],
            epicReceipts: [
                .epic(namespace: "fixture", appName: "one", name: "Epic One", directory: "/Volumes/Old/steam-730",
                      platform: .windows),
                .epic(namespace: "fixture", appName: "two", name: "Epic Two", directory: "/Volumes/Old/epic-two",
                      platform: .mac),
            ],
            gogReceipts: [
                StorefrontsFixture.gogReceipt("1", directory: "/library/Games-old/gog-1"),
                StorefrontsFixture.gogReceipt("2", directory: "\(folder)/gog-2"),
            ],
            gamesFolder: folder
        ))
        defer { fixture.finish() }
        XCTAssertEqual(fixture.app.gamesOutsideFolder, 0, "Signed out, nothing is installed")

        await fixture.signIn()

        XCTAssertEqual(fixture.app.gamesOutsideFolder, 3,
                       "/Volumes/Old/steam-730 counts once, and a folder beside the games folder is outside it")
    }

    // MARK: Performance reports

    func testEachStorefrontsEndedGameIsReportedUnderItsStoreWithTheTitleItsLibraryKnowsIt() async throws {
        let fixture = StorefrontsFixture(.init(steam: [StorefrontsFixture.portal], epic: [StorefrontsFixture.epicGame()],
                                               gog: [StorefrontsFixture.gogGame()]))
        defer { fixture.finish() }
        await fixture.signIn()
        let played = GamePlayed(seconds: 2410, frames: 140_321, medianFps: 58.2, low1pctFps: 41, hitches: 3,
                                exitCode: 0, exitSignal: nil, endedByObelisk: false, engine: "linux",
                                graphics: "d3dmetal", voile: "v26.3.0-13")
        func end(_ state: GameSessionState, on fake: FakeSessions) {
            var ended = state
            ended.phase = .finished
            ended.played = played
            fake.publish { $0.last = ended }
        }

        end(.steam(appID: 730, accountID: StorefrontsFixture.steamAccount), on: fixture.steamSessions.fake)
        end(.epic(account: StorefrontsFixture.epicAccount, namespace: "fixture", appName: "ghost"),
            on: fixture.epicSessions.fake)
        fixture.steamSessions.refresh()
        fixture.epicSessions.refresh()
        await drainMainQueue(times: 5)
        let unknown = await fixture.sent.reports
        XCTAssertTrue(unknown.isEmpty, "A game its library does not know has no title to report")

        end(.steam(appID: 620, accountID: StorefrontsFixture.steamAccount), on: fixture.steamSessions.fake)
        end(.epic(account: StorefrontsFixture.epicAccount, namespace: "fixture", appName: "one"),
            on: fixture.epicSessions.fake)
        end(.gog(), on: fixture.gogSessions.fake)
        fixture.steamSessions.refresh()
        fixture.epicSessions.refresh()
        fixture.gogSessions.refresh()
        try await eventually { await fixture.sent.reports.count == 3 }

        let reports = await fixture.sent.reports
        let byStore = Dictionary(uniqueKeysWithValues: reports.map { ($0.store, $0) })
        XCTAssertEqual(byStore["steam"]?.storeGameID, "620")
        XCTAssertEqual(byStore["steam"]?.gameName, "Portal 2")
        XCTAssertEqual(byStore["epic"]?.storeGameID, "one", "Epic names a game by its app name")
        XCTAssertEqual(byStore["epic"]?.gameName, "Epic One")
        XCTAssertEqual(byStore["gog"]?.storeGameID, "1207658924")
        XCTAssertEqual(byStore["gog"]?.gameName, "GOG One")
        XCTAssertEqual(Set(byStore.keys), ["steam", "epic", "gog"])
    }
}
