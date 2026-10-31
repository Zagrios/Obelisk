import AppKit
import SwiftUI
import XCTest
@testable import Obelisk

final class GameOverlayTests: XCTestCase {
    @MainActor
    func testEpicSocialPanelReturnsFocusBeforePostingItsShortcut() async throws {
        var posted: [OverlayApplication] = []
        let fixture = OverlayFixture(postEpicSocialShortcut: { application in posted.append(application); return true })
        let overlay = fixture.overlay
        fixture.frontmost = Self.game
        fixture.epicSessions.fake.show(.epic(account: "epic-account", namespace: "50118b7f954e450f8823df1614b24e80",
            appName: "0a2d9f6403244d12969e11da6713137b", id: "fall-guys"))
        try await eventually { overlay.hasEpicSocialOverlay }
        overlay.present(.friends)
        overlay.openEpicSocialOverlay()
        XCTAssertFalse(overlay.isPresented)
        XCTAssertEqual(fixture.presenter.hidden.last!, Self.game)
        XCTAssertTrue(posted.isEmpty)
        try await eventually { posted == [Self.game] }
        XCTAssertFalse(overlay.epicSocialNeedsShortcut)
        overlay.present(.friends)
        overlay.openEpicSocialOverlay()
        fixture.epicSessions.fake.show(nil)
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(posted, [Self.game], "A game ending cancels pending input")
    }

    @MainActor
    func testEpicSocialPanelOffersTheManualShortcutWhenPostingIsUnavailable() async throws {
        let fixture = OverlayFixture()
        let overlay = fixture.overlay
        fixture.frontmost = Self.game
        fixture.epicSessions.fake.show(.epic(account: "epic-account", namespace: "50118b7f954e450f8823df1614b24e80",
            appName: "0a2d9f6403244d12969e11da6713137b", id: "fall-guys"))
        try await eventually { overlay.hasEpicSocialOverlay }
        overlay.present(.friends)
        overlay.openEpicSocialOverlay()
        try await eventually { overlay.isPresented && overlay.epicSocialNeedsShortcut }
        overlay.hide()
    }
    private static let game = OverlayApplication(pid: 4_242, bundleIdentifier: "com.zagrios.obelisk.game.620")
    private static let finder = OverlayApplication(pid: 4_343, bundleIdentifier: "com.apple.finder",
                                                   executable: URL(fileURLWithPath: "/System/Library/CoreServices/Finder.app/Contents/MacOS/Finder"))
    private static let obelisk = OverlayApplication(pid: ProcessInfo.processInfo.processIdentifier,
                                                    bundleIdentifier: "com.zagrios.obelisk")

    @MainActor
    func testTheOverlayShowsOnlyOverARunningGameAndGivesItsFocusBack() async throws {
        let fixture = OverlayFixture()
        let overlay = fixture.overlay
        var visibility: [Bool] = []
        overlay.onVisibilityChanged = { visibility.append($0) }
        XCTAssertFalse(overlay.present(.home), "No game runs to show it over")
        XCTAssertFalse(overlay.isPresented)

        fixture.steamSessions.fake.show(.steam(appID: 620, id: "portal", phase: .preparing))
        try await eventually { fixture.steamSessions.active != nil }
        XCTAssertNil(overlay.session, "A game still preparing has no overlay yet")
        fixture.frontmost = Self.game
        fixture.steamSessions.fake.show(.steam(appID: 620, id: "portal"))
        try await eventually { overlay.session?.game == .steam(appId: 620) }
        XCTAssertTrue(fixture.hotKey.isRegistered, "The game was already in front")

        XCTAssertTrue(overlay.present(.home))
        XCTAssertTrue(overlay.isPresented)
        XCTAssertEqual(overlay.request, .home)
        XCTAssertEqual(fixture.presenter.shown, [Self.game], "Over the game's screen")
        XCTAssertEqual(visibility, [true])

        // Its pages open inside it; Back returns home.
        fixture.frontmost = Self.obelisk
        let store = URL(string: "https://store.steampowered.com/app/620")!
        overlay.present(.webPage(store))
        XCTAssertEqual(overlay.request, .webPage(store))
        overlay.show(.achievements)
        XCTAssertEqual(overlay.request, .achievements)
        XCTAssertEqual(overlay.achievements?.storeID, "steam")
        XCTAssertTrue(overlay.back())
        XCTAssertEqual(overlay.request, .home)
        XCTAssertFalse(overlay.back(), "Home is the first page")
        overlay.present(.user(steamId: 76_561_198_000_000_001))
        XCTAssertEqual(overlay.request, .webPage(URL(string: "https://steamcommunity.com/profiles/76561198000000001")!),
                       "Someone who is no friend shows their profile")
        XCTAssertEqual(fixture.presenter.shown.count, 1, "It was already open")
        XCTAssertEqual(visibility, [true])

        overlay.hide()
        XCTAssertFalse(overlay.isPresented)
        XCTAssertEqual(overlay.request, .home)
        XCTAssertEqual(fixture.presenter.hidden, [Self.game], "The game comes back in front")
        XCTAssertEqual(visibility, [true, false])

        // Shift-Tab toggles it.
        fixture.frontmost = Self.game
        overlay.applicationActivated(Self.game)
        fixture.hotKey.press()
        XCTAssertTrue(overlay.isPresented)
        XCTAssertEqual(overlay.request, .home)
        fixture.hotKey.press()
        XCTAssertFalse(overlay.isPresented)
        XCTAssertEqual(visibility, [true, false, true, false])
    }

    @MainActor
    func testShiftTabIsTheOverlaysOnlyWhileTheGameOrTheOverlayIsInFront() async throws {
        let fixture = OverlayFixture()
        let overlay = fixture.overlay
        fixture.frontmost = Self.finder
        fixture.epicSessions.fake.show(.epic(account: "epic-account", namespace: "ns", appName: "fortnite", id: "fn"))
        try await eventually { overlay.session?.game == .epic("ns:fortnite") }
        XCTAssertFalse(fixture.hotKey.isRegistered, "Another app is in front")
        XCTAssertNil(overlay.friends, "Epic's friends show once its account is connected")

        // The Linux engine's game, then Rosetta Wine's.
        overlay.applicationActivated(Self.game)
        XCTAssertTrue(fixture.hotKey.isRegistered)
        overlay.applicationActivated(Self.finder)
        XCTAssertFalse(fixture.hotKey.isRegistered)
        let wine = OverlayApplication(pid: 4_444, executable: fixture.voile.appending(path: "current/bin/wine"))
        overlay.applicationActivated(wine)
        XCTAssertTrue(fixture.hotKey.isRegistered)

        // Obelisk in front for the overlay keeps it, so Shift-Tab closes it.
        fixture.frontmost = wine
        fixture.hotKey.press()
        XCTAssertTrue(overlay.isPresented)
        overlay.applicationActivated(Self.obelisk)
        XCTAssertTrue(fixture.hotKey.isRegistered)
        fixture.hotKey.press()
        XCTAssertFalse(overlay.isPresented)
        XCTAssertEqual(fixture.presenter.hidden, [wine])
        overlay.applicationActivated(Self.obelisk)
        XCTAssertFalse(fixture.hotKey.isRegistered, "Obelisk's own window takes Shift-Tab back")
    }

    @MainActor
    func testWineTellsOfShiftTabWhileItsGameCapturesTheDisplays() async throws {
        let fixture = OverlayFixture()
        let overlay = fixture.overlay
        let wine = OverlayApplication(pid: 4_444, executable: fixture.voile.appending(path: "current/bin/wine"))
        fixture.frontmost = wine
        overlay.overlayKeyPressed(in: wine)
        XCTAssertFalse(overlay.isPresented, "No game runs")

        fixture.steamSessions.fake.show(.steam(appID: 620, id: "portal"))
        try await eventually { overlay.session?.game == .steam(appId: 620) }
        overlay.overlayKeyPressed(in: Self.finder)
        XCTAssertFalse(overlay.isPresented, "Only the game's Wine is heard")
        overlay.overlayKeyPressed(in: wine)
        XCTAssertTrue(overlay.isPresented)
        XCTAssertEqual(fixture.presenter.shown, [wine], "Over the game's screen")
        overlay.overlayKeyPressed(in: wine)
        XCTAssertTrue(overlay.isPresented, "Obelisk's own Shift-Tab closes it")
    }

    @MainActor
    func testTheGameEndingClosesTheOverlayAndReleasesShiftTab() async throws {
        let fixture = OverlayFixture()
        let overlay = fixture.overlay
        var visibility: [Bool] = []
        overlay.onVisibilityChanged = { visibility.append($0) }
        fixture.frontmost = Self.game
        fixture.gogSessions.fake.show(GameSessionState(id: "gog", game: .gog(accountId: "gog-account", productId: "1207658924"),
                                                       phase: .running, reconnecting: false, cloud: .idle, host: nil,
                                                       observed: true, runtimeInstall: nil))
        try await eventually { overlay.session?.game == .gog("1207658924") }
        XCTAssertNil(overlay.friends, "GOG shows no friends")
        XCTAssertNil(overlay.chat)
        overlay.present(.achievements)
        XCTAssertEqual(overlay.achievements?.storeID, "gog")
        XCTAssertTrue(fixture.hotKey.isRegistered)

        fixture.gogSessions.fake.show(nil)
        try await eventually { overlay.session == nil }
        XCTAssertFalse(overlay.isPresented)
        XCTAssertFalse(fixture.hotKey.isRegistered)
        XCTAssertNil(overlay.achievements)
        XCTAssertEqual(fixture.presenter.hidden, [nil], "The game that ended gets no focus")
        XCTAssertEqual(visibility, [true, false])
        XCTAssertFalse(overlay.present(.home))
        overlay.applicationActivated(Self.game)
        XCTAssertFalse(fixture.hotKey.isRegistered)
    }

    @MainActor
    func testEachEnginesGameIsToldApartFromOtherApps() throws {
        let root = FileManager.default.temporaryDirectory.appending(path: "ObeliskOverlay-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let voile = root.appending(path: "Voile")
        let development = root.appending(path: "voile-build")
        let game = root.appending(path: "Games/Thronefall")
        for folder in [voile.appending(path: "26.3.0-12/bin"), development.appending(path: "bin"), game] {
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        }
        try FileManager.default.createSymbolicLink(at: voile.appending(path: "development"), withDestinationURL: development)
        let match = GameApplicationMatch(voile: voile.path, native: [game.path])
        func app(_ executable: URL?, bundle: String? = nil) -> OverlayApplication {
            OverlayApplication(pid: 1, bundleIdentifier: bundle, executable: executable)
        }

        XCTAssertTrue(match.contains(app(nil, bundle: "com.zagrios.obelisk.game.thronefall")), "The Linux engine's wsd")
        XCTAssertTrue(match.contains(app(voile.appending(path: "26.3.0-12/bin/wine"))), "Rosetta Wine")
        XCTAssertTrue(match.contains(app(development.appending(path: "bin/wine"))), "A linked development runtime")
        XCTAssertTrue(match.contains(app(game.appending(path: "Thronefall.app/Contents/MacOS/Thronefall"))), "A Mac game")
        XCTAssertFalse(match.contains(app(nil, bundle: "com.zagrios.obelisk")), "Obelisk itself")
        XCTAssertFalse(match.contains(app(root.appending(path: "Games/Thronefall 2/Game"))), "A neighbouring folder")
        XCTAssertFalse(match.contains(app(URL(fileURLWithPath: "/Applications/Safari.app/Contents/MacOS/Safari"))))
    }

    @MainActor
    func testRenderOverlayWhenRequested() async throws {
        guard let path = ProcessInfo.processInfo.environment["OBELISK_RENDER_PREVIEWS"], !path.isEmpty else {
            throw XCTSkip("Set OBELISK_RENDER_PREVIEWS to render the overlay in an isolated native window")
        }
        let output = URL(fileURLWithPath: path)
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        let session = ChatBridgeTestSession(games: [SteamGame(appId: 620, name: "Portal 2", iconHash: "", lastPlayed: 0,
                                                              playtimeMinutes: 0)])
        let sessions = SteamGameSessions()
        let model = friendsTestModel(connect: { _ in session }, gameSessions: sessions)
        let fixture = OverlayFixture(steam: model, showsPanel: true)
        await model.start()
        defer { model.signOut() }
        try await eventually { model.friends?.connection == .connected && !model.library.games.isEmpty }
        var state = GameSessionState.steam(appID: 620, accountID: FriendsWireFixture.accountID, id: "portal")
        state.startedAt = UInt64(Date().timeIntervalSince1970) - 3_900
        sessions.fake.show(state)
        session.chat.ownGame(appId: 620, lobbyId: 1, connect: nil)
        try await eventually { fixture.overlay.session != nil && model.chat.ownGame != nil }
        fixture.overlay.present(.home)
        let panel = try XCTUnwrap(fixture.presenter.panel)
        func render(_ name: String) async throws {
            let host = try XCTUnwrap(panel.contentView)
            for _ in 0..<12 {
                await drainMainQueue()
                host.layoutSubtreeIfNeeded()
                panel.displayIfNeeded()
            }
            try await Task.sleep(for: .milliseconds(500))
            let bitmap = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds))
            host.cacheDisplay(in: host.bounds, to: bitmap)
            try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).write(to: output.appending(path: "overlay-\(name).png"))
        }
        try await render("home")
        state.inviteRequest = SteamInviteRequest(sequence: 1, invite: .lobby(lobbyId: 1))
        sessions.fake.show(state)
        try await eventually { model.chat.invitePicker != nil }
        try await render("invite")
        fixture.overlay.hide()

        let now = UInt64(Date().timeIntervalSince1970)
        state.receivedInvites = [
            SteamReceivedInvite(friendSteamId: ChatBridgeTestSession.friendID, appId: 620, invite: .lobby(lobbyId: 1),
                                receivedAt: now - 120),
            SteamReceivedInvite(friendSteamId: 76_561_198_000_000_009, appId: 620, invite: .connect(connect: "+connect 1"),
                                receivedAt: now - 3_600),
        ]
        sessions.fake.show(state)
        try await eventually { fixture.overlay.receivedInvites.count == 2 }
        fixture.overlay.present(.home)
        try await render("invitations")
        fixture.overlay.show(.achievements)
        try await render("invitations-column")
        fixture.overlay.hide()

        let toast = OverlayToastPanel()
        toast.sharingType = .none
        toast.show(OverlayToastView(toast: OverlayToast(friend: model.friends?.friends.first,
                                                        message: L10n.string("ui.overlay.invites.toast", "Alice", "Portal 2"),
                                                        hint: L10n.string("ui.overlay.invites.toastHint")),
                                    avatars: fixture.overlay.avatars, open: {}))
        let size = try XCTUnwrap(toast.contentView?.fittingSize)
        toast.setFrame(NSRect(x: -20_000, y: -20_000, width: size.width, height: size.height), display: true)
        toast.orderFrontRegardless()
        for _ in 0..<12 { await drainMainQueue(); toast.displayIfNeeded() }
        try await Task.sleep(for: .milliseconds(300))
        let toastHost = try XCTUnwrap(toast.contentView)
        let toastBitmap = try XCTUnwrap(toastHost.bitmapImageRepForCachingDisplay(in: toastHost.bounds))
        toastHost.cacheDisplay(in: toastHost.bounds, to: toastBitmap)
        try XCTUnwrap(toastBitmap.representation(using: .png, properties: [:])).write(to: output.appending(path: "overlay-toast.png"))
        toast.orderOut(nil)

        state.microTxn = SteamMicroTxn(orderId: 4_242, appId: 620)
        sessions.fake.show(state)
        try await eventually { fixture.overlay.request == .microTxn(orderId: 4_242) }
        try await render("purchase")
        fixture.overlay.hide()
    }
}

/// The overlay over test Game sessions of every storefront, with a hot key and
/// a presenter that record what the overlay asks of them. Its panel, when it
/// shows one, opens off screen.
@MainActor
final class OverlayFixture {
    let app: AppModel
    let hotKey = RecordingOverlayHotKey()
    let presenter = RecordingOverlayPresenter()
    let toaster = RecordingOverlayToastPresenter()
    let steamSessions: SteamGameSessions
    let epicSessions = EpicGameSessions()
    let gogSessions = GogGameSessions()
    let voile: URL
    /// The application in front, as the overlay asks.
    var frontmost: OverlayApplication? {
        get { front.application }
        set { front.application = newValue }
    }
    var overlay: GameOverlayController { app.overlay }
    private let front: Front

    @MainActor final class Front {
        var application: OverlayApplication?
    }

    init(steam: SteamModel? = nil, showsPanel: Bool = false,
         postEpicSocialShortcut: @escaping @MainActor (OverlayApplication) -> Bool = { _ in false }) {
        let directory = FileManager.default.temporaryDirectory.appending(path: "ObeliskOverlay-\(UUID().uuidString)")
        let voile = directory.appending(path: "Voile")
        try? FileManager.default.createDirectory(at: voile.appending(path: "current/bin"), withIntermediateDirectories: true)
        self.voile = voile
        let steam = steam ?? SteamModel(directory: directory, gameSessions: SteamGameSessions())
        steamSessions = steam.gameSessions
        let front = Front()
        self.front = front
        presenter.showsPanel = showsPanel
        app = AppModel(
            directory: directory, steam: steam,
            epic: EpicModel(directory: directory, keychain: MemoryStore<EpicCredentials>(nil), gameSessions: epicSessions),
            gog: GogModel(directory: directory, keychain: MemoryStore<GogCredentials>(nil), gameSessions: gogSessions),
            overlay: GameOverlayEnvironment(hotKey: hotKey, presenter: presenter, toaster: toaster,
                                            frontmost: { front.application }, voile: voile.path,
                                            observesActivations: false, postEpicSocialShortcut: postEpicSocialShortcut)
        )
    }
}

@MainActor
final class RecordingOverlayHotKey: OverlayHotKey {
    private(set) var isRegistered = false
    private var action: (@MainActor () -> Void)?

    func register(_ action: @escaping @MainActor () -> Void) {
        guard !isRegistered else { return }
        isRegistered = true
        self.action = action
    }

    func unregister() {
        isRegistered = false
        action = nil
    }

    /// The player presses Shift-Tab; nothing happens unless it is registered.
    func press() { action?() }
}

@MainActor
final class RecordingOverlayPresenter: OverlayPresenter {
    var showsPanel = false
    private(set) var panel: GameOverlayPanel?
    /// The game each opening covered.
    private(set) var shown: [OverlayApplication?] = []
    /// What each closing brought back in front.
    private(set) var hidden: [OverlayApplication?] = []

    func show(_ overlay: GameOverlayController, over game: OverlayApplication?) {
        shown.append(game)
        guard showsPanel else { return }
        let panel = self.panel ?? GameOverlayPanel(overlay: overlay)
        self.panel = panel
        panel.sharingType = .none
        panel.setFrame(NSRect(x: -20_000, y: -20_000, width: 1280, height: 800), display: true)
        panel.makeKeyAndOrderFront(nil)
    }

    func hide(returningTo application: OverlayApplication?) {
        hidden.append(application)
        panel?.orderOut(nil)
    }
}

@MainActor
final class RecordingOverlayToastPresenter: OverlayToastPresenter {
    /// Each toast shown, and the game whose screen it showed on.
    private(set) var shown: [(toast: OverlayToast, game: OverlayApplication?)] = []
    private(set) var hides = 0
    /// Whether a toast shows now.
    private(set) var isShowing = false
    private var open: (@MainActor () -> Void)?

    var toasts: [OverlayToast] { shown.map(\.toast) }

    func show(_ toast: OverlayToast, avatars: FriendsAvatarCache, over game: OverlayApplication?,
              open: @escaping @MainActor () -> Void) {
        shown.append((toast, game))
        isShowing = true
        self.open = open
    }

    func hide() {
        hides += 1
        isShowing = false
        open = nil
    }

    /// The player clicks the toast showing.
    func click() {
        let open = open
        hide()
        open?()
    }
}
