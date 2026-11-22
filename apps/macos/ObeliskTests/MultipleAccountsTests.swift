import Foundation
import XCTest
@testable import Obelisk

/// Several accounts of one storefront, each on a slot of its own: the player
/// adds a slot from an account's menu, signs another account in there, and
/// signs any of them out. Epic stands for every storefront; Steam's own
/// sign-in is checked where it differs.
@MainActor
final class MultipleAccountsTests: XCTestCase {
    // MARK: Slots

    func testAStorefrontStartsWithItsOriginalSlotAndKeepsTheSlotsThePlayerAdds() async throws {
        let fixture = AccountsFixture()
        defer { fixture.finish() }
        let app = fixture.app

        XCTAssertEqual(app.epic.accounts.map(\.slot), [AccountSlot(.epic)])
        XCTAssertEqual(AccountSlot(.epic).id, "epic", "The original slot is named as its storefront")

        app.addAccount(to: .epic)
        let added = try XCTUnwrap(app.epic.accounts.last?.slot)
        XCTAssertEqual(app.epic.accounts.count, 2)
        XCTAssertNotEqual(added, AccountSlot(.epic))
        XCTAssertEqual(added.section, .epic)

        let restarted = fixture.restarted()
        XCTAssertEqual(restarted.epic.accounts.map(\.slot), [AccountSlot(.epic), added], "In the order they were added")
        XCTAssertEqual(restarted.steam.accounts.map(\.slot), [AccountSlot(.steam)])
        XCTAssertEqual(restarted.gog.accounts.map(\.slot), [AccountSlot(.gog)])
    }

    func testEachSlotKeepsASessionOfItsOwnInTheKeychain() async throws {
        let service = "com.zagrios.obelisk.tests.\(UUID().uuidString)"
        let added = AccountSlot.added(to: .steam)
        let original = SteamKeychain(service: service)
        let other = SteamKeychain(slot: added, service: service)
        defer {
            Task {
                try? await original.remove()
                try? await other.remove()
            }
        }

        try await original.save(SteamCredentials(accountName: "first", steamId: 1, refreshToken: "fixture-only-first"))
        try await other.save(SteamCredentials(accountName: "second", steamId: 2, refreshToken: "fixture-only-second"))

        let first = try await original.load()
        let second = try await other.load()
        XCTAssertEqual(first?.steamId, 1)
        XCTAssertEqual(second?.steamId, 2)
        try await other.remove()
        let kept = try await original.load()
        let gone = try await SteamKeychain(slot: added, service: service).load()
        XCTAssertEqual(kept?.steamId, 1, "Removing a slot's session leaves the others")
        XCTAssertNil(gone)
    }

    // MARK: Adding an account

    func testAddingAnAccountShowsItsSignInOnATabOfItsOwn() async throws {
        let fixture = AccountsFixture(saved: ["first"])
        defer { fixture.finish() }
        let app = fixture.app
        await app.restoreStorefronts()
        let first = try XCTUnwrap(app.epic.accounts.first)
        XCTAssertTrue(first.isConnected)
        XCTAssertEqual(app.title(of: first), LibrarySection.epic.title, "A storefront's only account needs no name")

        app.addAccount(to: .epic)

        let added = try XCTUnwrap(app.epic.accounts.last)
        XCTAssertFalse(added === first)
        XCTAssertEqual(app.selection, added.tab)
        XCTAssertEqual(app.standing(of: added.slot), .signedOut(connecting: false))
        XCTAssertEqual(app.accounts.map(\.section), [.steam, .epic, .epic, .gog], "Beside its storefront's other account")

        fixture.nextAccount = "second"
        added.signIn()
        try await eventually { added.isConnected }

        XCTAssertEqual(added.accountID, "second")
        XCTAssertTrue(first.isConnected, "The first account stays signed in")
        XCTAssertEqual(app.title(of: first), L10n.string("ui.account.store_named", LibrarySection.epic.title, "Name of first"))
        XCTAssertEqual(app.title(of: added), L10n.string("ui.account.store_named", LibrarySection.epic.title, "Name of second"))
        XCTAssertEqual(app.title(of: app.steam.first), LibrarySection.steam.title)
    }

    func testAddingAnAccountAgainShowsTheSlotStillWaitingForOne() async throws {
        let fixture = AccountsFixture(saved: ["first"])
        defer { fixture.finish() }
        let app = fixture.app
        await app.restoreStorefronts()

        app.addAccount(to: .epic)
        let added = try XCTUnwrap(app.epic.accounts.last)
        try await eventually { added.isVacant }
        app.addAccount(to: .epic)

        XCTAssertEqual(app.epic.accounts.count, 2, "One slot at a time waits for its account")
        XCTAssertEqual(app.selection, added.tab)
    }

    func testAnAddedAccountThePlayerLeavesWithoutSigningInGoes() async throws {
        let fixture = AccountsFixture(saved: ["first"])
        defer { fixture.finish() }
        let app = fixture.app
        await app.restoreStorefronts()

        app.addAccount(to: .epic)
        let added = try XCTUnwrap(app.epic.accounts.last)
        try await eventually { added.isVacant }
        XCTAssertEqual(app.epic.accounts.count, 2, "It stays while its tab shows")

        app.show(.home)

        XCTAssertEqual(app.epic.accounts.map(\.slot), [AccountSlot(.epic)])
        XCTAssertEqual(fixture.restarted().epic.accounts.map(\.slot), [AccountSlot(.epic)])
    }

    func testAnAccountAlreadySignedInIsRefusedOnAnotherSlot() async throws {
        let fixture = AccountsFixture(saved: ["first"])
        defer { fixture.finish() }
        let app = fixture.app
        await app.restoreStorefronts()
        let first = try XCTUnwrap(app.epic.accounts.first)

        app.addAccount(to: .epic)
        let added = try XCTUnwrap(app.epic.accounts.last)
        // Its sign-in shows once it found nothing saved to restore.
        try await eventually { added.isVacant }
        fixture.nextAccount = "first"
        added.signIn()
        try await eventually { added.errorMessage != nil }

        XCTAssertEqual(added.errorMessage, L10n.string("model.account.alreadySignedIn"))
        XCTAssertFalse(added.isConnected)
        XCTAssertNil(added.accountID)
        XCTAssertTrue(first.isConnected, "The account stays signed in where it was")
        XCTAssertEqual(first.accountID, "first")

        app.show(.home)
        XCTAssertEqual(app.epic.accounts.count, 2, "A slot keeps a failure the player has not read")
        added.errorMessage = nil
        app.show(first.tab)
        XCTAssertEqual(app.epic.accounts.count, 1)
    }

    func testASteamAccountAlreadySignedInIsRefusedOnAnotherSlot() async throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "ObeliskAccounts-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let suite = "ObeliskTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let sessions = SteamGameSessions()
        let list = AccountList<SteamModel>(.steam, defaults: defaults) { slot in
            offlineSteam(directory: directory, slot: slot, gameSessions: sessions) { _ in
                StubSteamSession(games: [], profile: SteamProfile(displayName: nil, avatarUrl: nil))
            }
        }
        let first = list.first
        let second = list.add()
        defer {
            first.signOut()
            second.signOut()
        }
        func credentials(_ steamId: UInt64) -> SteamCredentials {
            SteamCredentials(accountName: "fixture-\(steamId)", steamId: steamId, refreshToken: "fixture-only-never-sent")
        }

        await first.connect(credentials(42), generation: 0, persist: false)
        XCTAssertEqual(first.accountID, "42")
        await second.connect(credentials(42), generation: 0, persist: false)

        XCTAssertFalse(second.isConnected)
        XCTAssertEqual(second.errorMessage, L10n.string("model.account.alreadySignedIn"))
        XCTAssertTrue(first.isConnected)

        second.errorMessage = nil
        await second.connect(credentials(43), generation: 0, persist: false)
        XCTAssertEqual(second.accountID, "43", "Another account signs in beside the first")
        XCTAssertTrue(first.isConnected)
    }

    // MARK: Signing out

    func testSigningOutOneOfSeveralAccountsTakesItsSlotAwayAndShowsTheStorefrontsFirst() async throws {
        let fixture = AccountsFixture(saved: ["first"])
        defer { fixture.finish() }
        let app = fixture.app
        await app.restoreStorefronts()
        let first = try XCTUnwrap(app.epic.accounts.first)
        app.addAccount(to: .epic)
        let added = try XCTUnwrap(app.epic.accounts.last)
        fixture.nextAccount = "second"
        added.signIn()
        try await eventually { added.isConnected }
        app.homeGame = HomeGame.ID(slot: added.slot, game: "fixture:one")
        let store = fixture.store(of: added.slot)
        try await eventually { await store.value?.accountId == "second" }

        app.signOut(added)

        XCTAssertEqual(app.epic.accounts.map(\.slot), [first.slot])
        XCTAssertEqual(app.selection, first.tab, "The storefront's remaining account shows")
        XCTAssertNil(app.homeGame)
        XCTAssertFalse(added.isConnected)
        XCTAssertTrue(first.isConnected)
        XCTAssertEqual(app.title(of: first), LibrarySection.epic.title, "Alone again, it needs no name")
        XCTAssertNil(app.account(added.slot))
        XCTAssertEqual(fixture.restarted().epic.accounts.map(\.slot), [first.slot])
        // Its saved session goes with it.
        try await eventually { await store.value == nil }
    }

    func testSigningOutTheFirstOfSeveralAccountsLeavesTheOtherAsTheStorefronts() async throws {
        let fixture = AccountsFixture(saved: ["first"])
        defer { fixture.finish() }
        let app = fixture.app
        await app.restoreStorefronts()
        let first = try XCTUnwrap(app.epic.accounts.first)
        app.addAccount(to: .epic)
        let added = try XCTUnwrap(app.epic.accounts.last)
        fixture.nextAccount = "second"
        added.signIn()
        try await eventually { added.isConnected }
        app.selection = .home

        app.signOut(first)

        XCTAssertEqual(app.epic.accounts.map(\.slot), [added.slot])
        XCTAssertEqual(app.selection, .home, "Another tab stays where it was")
        XCTAssertTrue(added.isConnected)
        let restarted = fixture.restarted()
        XCTAssertEqual(restarted.epic.accounts.map(\.slot), [added.slot], "The added slot is now the storefront's")
    }

    func testTheLastAccountOfAStorefrontKeepsItsSlotOnceSignedOut() async throws {
        let fixture = AccountsFixture(saved: ["first"])
        defer { fixture.finish() }
        let app = fixture.app
        await app.restoreStorefronts()
        let first = try XCTUnwrap(app.epic.accounts.first)
        app.selection = first.tab

        app.signOut(first)
        try await eventually { !first.isSigningOut }

        XCTAssertEqual(app.epic.accounts.map(\.slot), [AccountSlot(.epic)])
        XCTAssertEqual(app.selection, first.tab)
        XCTAssertEqual(app.standing(of: first.slot), .signedOut(connecting: false))
        app.show(.home)
        XCTAssertEqual(app.epic.accounts.count, 1, "A storefront's last slot offers its sign-in")
    }

    // MARK: Restoring

    func testEveryAccountRestoresAtLaunchAndASlotWithNothingSavedGoes() async throws {
        let fixture = AccountsFixture(saved: ["first", "second", nil])
        defer { fixture.finish() }
        let app = fixture.app
        XCTAssertEqual(app.epic.accounts.count, 3)
        XCTAssertEqual(app.homeContent, .restoring)

        await app.restoreStorefronts()

        XCTAssertEqual(app.epic.accounts.map(\.accountID), ["first", "second"])
        XCTAssertEqual(app.homeContent, .libraries)
        XCTAssertEqual(app.accounts.map(\.section), [.steam, .epic, .epic, .gog])
        for account in app.epic.accounts {
            guard case .signedIn = app.standing(of: account.slot) else { return XCTFail("\(account.slot) is signed in") }
        }
    }

    func testASlotWhoseRestorationFailedStaysForItsSavedSession() async throws {
        let fixture = AccountsFixture(saved: ["first", "second"], failing: ["second"])
        defer { fixture.finish() }
        let app = fixture.app

        await app.restoreStorefronts()

        XCTAssertEqual(app.epic.accounts.count, 2, "Its saved session restores at the next launch")
        let second = try XCTUnwrap(app.epic.accounts.last)
        XCTAssertFalse(second.isConnected)
        XCTAssertFalse(second.isVacant)
        second.errorMessage = nil
        app.show(.settings)
        XCTAssertEqual(app.epic.accounts.count, 2)
    }

    func testASlotWhoseSavedSessionItsStorefrontRefusesGoesOnceThePlayerReadWhy() async throws {
        let fixture = AccountsFixture(saved: ["first", "second"], refused: ["second"])
        defer { fixture.finish() }
        let app = fixture.app

        await app.restoreStorefronts()

        let second = try XCTUnwrap(app.epic.accounts.last)
        XCTAssertEqual(app.epic.accounts.count, 2, "It stays to tell why")
        XCTAssertEqual(second.errorMessage, EpicGames.unauthorizedMessage)
        XCTAssertTrue(second.isVacant)
        let saved = await fixture.store(of: second.slot).value
        XCTAssertNil(saved, "A refused session never restores")

        second.errorMessage = nil
        app.show(.settings)

        XCTAssertEqual(app.epic.accounts.map(\.slot), [AccountSlot(.epic)])
    }

    func testASteamSessionSteamRefusesIsForgottenAndLeavesItsSlotVacant() async throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "ObeliskAccounts-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        func model(_ store: MemoryStore<SteamCredentials>, fails error: SteamError) -> SteamModel {
            offlineSteam(directory: directory, keychain: store) { _ in throw error }
        }
        let credentials = SteamCredentials(accountName: "fixture", steamId: 42, refreshToken: "fixture-only-never-sent")

        let refusedStore = MemoryStore<SteamCredentials>(credentials)
        let refused = model(refusedStore, fails: .AuthenticationRequired)
        await refused.start(signIn: false)
        let forgotten = await refusedStore.value
        XCTAssertNil(forgotten)
        XCTAssertTrue(refused.isVacant)
        XCTAssertEqual(refused.errorMessage, L10n.string("backend.steam.friends.authenticationRequired"))

        let unreachableStore = MemoryStore<SteamCredentials>(credentials)
        let unreachable = model(unreachableStore, fails: .Failure(reason: "Fixture: no network"))
        await unreachable.start(signIn: false)
        let kept = await unreachableStore.value
        XCTAssertEqual(kept?.steamId, 42, "A session that could not connect yet restores next time")
        XCTAssertFalse(unreachable.isVacant)
    }

    func testTheSidebarNamesAnAccountAsItLastShowedWhileItsSessionRestores() async throws {
        let fixture = AccountsFixture(saved: ["first", "second"])
        defer { fixture.finish() }
        await fixture.app.restoreStorefronts()
        try await eventually { fixture.app.epic.rememberedName(of: AccountSlot(.epic)) == "Name of first" }

        let restarted = fixture.restarted()

        XCTAssertNil(restarted.epic.first.accountName)
        XCTAssertEqual(restarted.epic.accounts.map(restarted.title(of:)), [
            L10n.string("ui.account.store_named", LibrarySection.epic.title, "Name of first"),
            L10n.string("ui.account.store_named", LibrarySection.epic.title, "Name of second"),
        ])
        XCTAssertEqual(restarted.storeTitle(of: restarted.epic.accounts[1].slot),
                       L10n.string("ui.account.store_named", LibrarySection.epic.storeTitle, "Name of second"),
                       "Home names the storefront in full")
        XCTAssertEqual(restarted.storeTitle(of: .steam), LibrarySection.steam.storeTitle,
                       "A storefront's only account is not named")
        let single = AccountsFixture(saved: ["first"])
        defer { single.finish() }
        XCTAssertEqual(single.app.storeTitle(of: .epic), LibrarySection.epic.storeTitle)
    }

    // MARK: Across the app

    func testAQueuedTransferBelongsToTheAccountItNames() async throws {
        let game = EpicGames.game("one", title: "Epic One")
        let fixture = AccountsFixture(saved: ["first", "second"], games: ["second": [game]])
        defer { fixture.finish() }
        let app = fixture.app
        await app.restoreStorefronts()
        let (first, second) = (app.epic.accounts[0], app.epic.accounts[1])
        try await eventually { second.library.game("fixture:one") != nil }
        fixture.backend(of: second.slot).publish {
            $0.transferGame = "fixture:one"
            $0.transfer = GameTransfer(game: "one", state: .downloading, completedBytes: 20, totalBytes: 100,
                                       networkBytes: 20, contentBytes: 20, currentFile: "", message: "")
        }
        try await eventually { second.installations.transfer != nil }
        let ofSecond = DownloadItem.queued(.epic, game: "fixture:one", account: "second")
        let ofFirst = DownloadItem.queued(.epic, game: "fixture:one", account: "first")

        XCTAssertEqual(app.downloadTransfer(ofSecond)?.completedBytes, 20)
        XCTAssertNil(app.downloadTransfer(ofFirst), "The other account downloads nothing")
        XCTAssertEqual(app.downloadGame(ofSecond).title, "Epic One")
        XCTAssertEqual(app.downloadGame(ofFirst).title, "Queued name", "Its library does not list the game")
        XCTAssertEqual(app.downloadGame(.queued(.epic, game: "fixture:one", account: "gone")).title, "Queued name")
        XCTAssertEqual(app.downloadGame(ofSecond).source,
                       L10n.string("ui.account.store_named", LibrarySection.epic.title, "Name of second"),
                       "The queue names the account that downloads")
        XCTAssertEqual(app.downloadGame(.queued(.epic, game: "fixture:one", account: "gone")).source,
                       LibrarySection.epic.title)

        app.selection = .home
        app.openDownload(ofSecond)
        XCTAssertEqual(app.selection, second.tab)
        XCTAssertEqual(second.selectedGame?.id, "fixture:one")
        XCTAssertNil(first.selectedGame)
    }

    func testAPromptNamesTheAccountThatAsks() async throws {
        let fixture = AccountsFixture(saved: ["first", "second"])
        defer { fixture.finish() }
        let app = fixture.app
        await app.restoreStorefronts()
        let second = app.epic.accounts[1]

        fixture.backend(of: second.slot).publish { $0.cloudSync = .exit("fixture:one", question: .conflict(1)) }

        try await eventually { app.presentedPrompt == AccountPrompt(slot: second.slot, kind: .cloud) }
    }
}

/// An AppModel whose Epic accounts the test signs in: each slot keeps its
/// saved session in a store of its own, restores as the account saved there,
/// and signs in as `nextAccount`. Steam and GOG stay signed out. Nothing
/// reaches a storefront, the Keychain or the player's library.
@MainActor
final class AccountsFixture {
    let root: URL
    let defaults: UserDefaults
    private let suite: String
    let sessions = EpicGameSessions()
    /// The account the next sign-in signs in.
    var nextAccount = "added"
    /// Obelisk as it first started.
    var app: AppModel { started }
    private var started: AppModel!
    private var stores: [AccountSlot: MemoryStore<WebAccountCredentials>] = [:]
    /// The account each slot still to be made restores, in the order made.
    private var saved: [String?]
    private var backends: [AccountSlot: FakeInstallations<EpicGames>] = [:]
    private let games: [String: [EpicGame]]
    private let failing: Set<String>
    private let refused: Set<String>
    private var apps: [AppModel] = []

    /// `saved` is the account each of Epic's slots restores at launch, in the
    /// sidebar's order, the original slot first: nil for a slot with nothing
    /// saved. An account of `failing` cannot reach Epic, and Epic refuses the
    /// saved session of one of `refused`.
    init(saved: [String?] = [nil], games: [String: [EpicGame]] = [:], failing: Set<String> = [],
         refused: Set<String> = []) {
        root = FileManager.default.temporaryDirectory.appending(path: "ObeliskAccounts-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        suite = "ObeliskTests.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)!
        self.games = games
        self.failing = failing
        self.refused = refused
        self.saved = saved
        started = restarted()
        // The slots after the original one, added as the player adds them.
        for _ in saved.dropFirst() { _ = started.epic.add() }
    }

    /// The saved session of `slot`'s account: the next of `saved`, for a
    /// slot just made.
    func store(of slot: AccountSlot) -> MemoryStore<WebAccountCredentials> {
        if let store = stores[slot] { return store }
        let account = saved.isEmpty ? nil : saved.removeFirst()
        let store = MemoryStore(account.map { EpicGames.credentials("saved", account: $0) })
        stores[slot] = store
        return store
    }

    /// The installations of `slot`'s account.
    func backend(of slot: AccountSlot) -> FakeInstallations<EpicGames> {
        if let backend = backends[slot] { return backend }
        let backend = FakeInstallations<EpicGames>()
        backends[slot] = backend
        return backend
    }

    /// Obelisk as it starts again over the same slots and saved sessions.
    func restarted() -> AppModel {
        let root = root
        let (games, failing, refused, sessions) = (games, failing, refused, sessions)
        let session: @Sendable (String) -> WebSession = { account in
            WebTestSession<EpicGames>(credentials: EpicGames.credentials("current", account: account),
                                      games: games[account] ?? [])
        }
        let app = AppModel(
            directory: root, defaults: defaults,
            steam: offlineSteam(directory: root),
            gog: GogModel(directory: root, keychain: MemoryStore<WebAccountCredentials>(nil),
                          gameSessions: GogGameSessions()),
            makeEpic: { [unowned self] slot in
                EpicModel(
                    directory: root, slot: slot, keychain: self.store(of: slot),
                    authorize: { "test-code" },
                    signIn: { [unowned self] _ in session(await self.nextAccount) },
                    restore: { credentials in
                        if failing.contains(credentials.accountId) { throw EpicGames.network }
                        if refused.contains(credentials.accountId) { throw EpicGames.unauthorized }
                        return session(credentials.accountId)
                    },
                    gameSessions: sessions,
                    installations: EpicModel.installations(self.backend(of: slot))
                )
            },
            downloads: DownloadsModel(backend: FakeDownloads(), defaults: defaults),
            gamesFolder: GamesFolderModel(library: root, backend: FakeGamesFolder(path: root.appending(path: "Games").path),
                                          workspace: NotificationCenter()),
            performance: PerformanceReports(isEnabled: false, mac: StorefrontsFixture.mac) { _ in },
            support: SupportModel(defaults: defaults, stars: { nil }, members: { nil })
        )
        apps.append(app)
        return app
    }

    /// Ends the accounts and the files the test made.
    func finish() {
        for app in apps {
            for account in app.accounts where account.accountID != nil { account.signOut() }
        }
        defaults.removePersistentDomain(forName: suite)
        try? FileManager.default.removeItem(at: root)
    }
}

/// A Steam account that reaches nothing: its sign-in is offline, its saved
/// session is `keychain`'s, and it connects as `connect` says.
@MainActor
func offlineSteam(
    directory: URL, slot: AccountSlot = AccountSlot(.steam),
    keychain: MemoryStore<SteamCredentials> = MemoryStore(nil),
    gameSessions: SteamGameSessions = SteamGameSessions(),
    connect: (@Sendable (SteamCredentials) async throws -> any SteamSessionProtocol)? = nil
) -> SteamModel {
    SteamModel(
        directory: directory, slot: slot, keychain: keychain, connect: connect, news: { _ in [] }, storePage: { _, _ in nil },
        webCookies: { _ in [] },
        beginLogin: { _ in throw SteamError.Failure(reason: "Fixture: sign-in is offline in tests") },
        gameSessions: gameSessions, installations: SteamModel.installations(FakeInstallations()),
        chat: SteamChatModel(isApplicationActive: false))
}
