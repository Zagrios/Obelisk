import AppKit
import SwiftUI
import XCTest
@testable import SailOver

final class FriendsPresentationTests: XCTestCase {
    func testPlayingAndAwayStayDistinctWhileGroupingAndCounting() {
        let state = fixtureState([
            friend("8", "Zoë", .online),
            friend("5", "Charlie", .offline),
            friend("3", "Émile", .away, game: FriendGameActivity(id: "9", name: "A game")),
            friend("1", "Alice", .busy, game: FriendGameActivity(id: "2", name: nil)),
            friend("2", "Bob", .online),
            friend("6", "Unknown", .unknown),
        ])
        let groups = state.sections(matching: "")
        XCTAssertEqual(state.onlineCount, 4)
        XCTAssertEqual(groups.playing.map(\.name), ["Alice", "Émile"])
        XCTAssertEqual(groups.online.map(\.name), ["Bob", "Zoë"])
        XCTAssertEqual(groups.offline.map(\.name), ["Charlie"])
        XCTAssertEqual(groups.unknown.map(\.name), ["Unknown"])
        XCTAssertEqual(groups.playing.last?.presence, .away)
        XCTAssertTrue(groups.playing.last?.status.contains(L10n.string("ui.friends.presence.away")) == true)
        XCTAssertEqual(groups.playing.first?.activity?.title, L10n.string("ui.friends.inGameUnknown"))
    }

    func testSearchMatchesCaseAndAccentsAndRevealsCollapsedOfflineSection() {
        let state = fixtureState([friend("2", "ÉMILE", .offline), friend("1", "Other", .online)])
        let all = state.sections(matching: "")
        XCTAssertFalse(all.showsOffline(isExpanded: false))
        let result = state.sections(matching: "  emile \n")
        XCTAssertEqual(result.offline.map(\.name), ["ÉMILE"])
        XCTAssertTrue(result.showsOffline(isExpanded: false))
        XCTAssertFalse(result.hasOnline)
        XCTAssertEqual(state.onlineCount, 1, "Filtering cannot change the account menu count")
        XCTAssertTrue(state.sections(matching: "missing").isEmpty)
    }

    func testMissingNamesAndStableIdentityDoNotDependOnActivityOrStore() {
        let unnamed = friend("2", " \n", .unknown)
        XCTAssertEqual(unnamed.displayName, L10n.string("ui.friends.unknownName"))
        XCTAssertEqual(unnamed.initials, "")
        XCTAssertNil(unnamed.avatarURL)
        let a = FriendIdentity(account: account, friendID: "2")
        let b = FriendIdentity(account: FriendsAccountIdentity(storeID: "other", accountID: "fixture"), friendID: "2")
        let c = FriendIdentity(account: FriendsAccountIdentity(storeID: "steam", accountID: "other-account"), friendID: "2")
        XCTAssertEqual(Set([a, b, c]).count, 3)
        XCTAssertEqual(fixtureState([friend("2", "Same", .online), friend("1", "Same", .online)])
            .sections(matching: "").online.map(\.id.friendID), ["1", "2"])
    }

    func testLoadingInterruptedFailureAndEmptyDoNotPublishAReliableZero() {
        let states: [FriendsConnectionState] = [.loading, .interrupted, .failed("model.friends.unavailable"), .authenticationRequired]
        let connected = fixtureState([])
        XCTAssertEqual(connected.menuTitle, L10n.string("ui.friends.totalCount", Int64(0)))
        for connection in states {
            let state = fixtureState([], connection: connection)
            XCTAssertNotEqual(state.menuTitle, connected.menuTitle)
            XCTAssertNotEqual(state.connection, .connected)
        }
        let cached = fixtureState([friend("1", "Alice", .online)], connection: .interrupted)
        XCTAssertEqual(cached.onlineCount, 1)
        XCTAssertEqual(cached.friends.first?.presence, .online)
        XCTAssertEqual(cached.summary, L10n.string("ui.friends.stale"))
    }

    func testInterruptedInitialHydrationDoesNotInventAHistoricalOnlineCount() {
        var state = fixtureState([friend("1", "Alice", .unknown)], connection: .interrupted)
        XCTAssertNil(state.lastKnownOnlineCount)
        XCTAssertEqual(state.headerSummary, state.summary)
        XCTAssertNotEqual(state.headerSummary, L10n.string("ui.friends.lastKnownCount", state.onlineSummary))
        state.lastKnownOnlineCount = 3
        XCTAssertEqual(state.headerSummary, L10n.string("ui.friends.lastKnownCount", L10n.string("ui.friends.onlineCount", Int64(3))))
    }

    @MainActor
    func testFocusOrderWrapsAndSupportsReverseTraversal() {
        let focus = FriendsPanelFocus()
        focus.controls = [.close, .search, .offline]
        focus.advance(backward: false)
        XCTAssertEqual(focus.target, .offline)
        focus.advance(backward: false)
        XCTAssertEqual(focus.target, .close)
        focus.advance(backward: true)
        XCTAssertEqual(focus.target, .offline)
    }

    @MainActor
    func testAvatarCacheReusesImageAndDropsLateImagesFromPreviousAccount() async throws {
        let requests = AvatarRequests()
        let cache = FriendsAvatarCache(load: { await requests.load($0) })
        cache.activate(account: account)
        let url = try XCTUnwrap(URL(string: "https://example.invalid/avatar.png"))
        let imageTask = Task { await cache.image(for: FriendIdentity(account: account, friendID: "1"), url: url) }
        await requests.waitUntilRequested()
        cache.activate(account: FriendsAccountIdentity(storeID: "steam", accountID: "next"))
        await requests.release(avatarPNG)
        let stale = await imageTask.value
        XCTAssertNil(stale)
        cache.activate(account: account)
        let first = await cache.image(for: FriendIdentity(account: account, friendID: "1"), url: url)
        let second = await cache.image(for: FriendIdentity(account: account, friendID: "1"), url: url)
        XCTAssertNotNil(first)
        XCTAssertTrue(first === second)
        let count = await requests.count
        XCTAssertEqual(count, 2)
    }

    @MainActor
    func testModalCentersAtMinimumWindowAndConsumesBackgroundClicks() async throws {
        let fixture = FriendsModalFixture(state: fixtureState(sampleFriends))
        defer { fixture.close() }
        await fixture.settle()
        let modal = try XCTUnwrap(fixture.modal)
        XCTAssertEqual(modal.host.frame.width, 520, accuracy: 1)
        XCTAssertEqual(modal.host.frame.midX, fixture.host.bounds.midX, accuracy: 1)
        XCTAssertEqual(modal.host.frame.midY, fixture.host.bounds.midY, accuracy: 1)
        XCTAssertGreaterThanOrEqual(modal.host.frame.minY, 24)
        XCTAssertLessThanOrEqual(modal.host.frame.maxY, 576)
        let target = fixture.host.hitTest(NSPoint(x: 20, y: 20))
        XCTAssertTrue(target === modal, "The scrim must consume hits outside the panel")
        XCTAssertEqual(modal.focus.target, .search)
        fixture.press(keyCode: 48, characters: "\t")
        await fixture.settle()
        XCTAssertEqual(modal.focus.target, .offline)
        fixture.press(keyCode: 48, characters: "\t")
        await fixture.settle()
        XCTAssertEqual(modal.focus.target, .close)
        XCTAssertTrue(fixture.state.isPresented)
        fixture.press(keyCode: 53, characters: "\u{1b}")
        await fixture.settle()
        XCTAssertFalse(fixture.state.isPresented)
        XCTAssertNil(fixture.modal)
    }

    @MainActor
    func testModalRestoresFocusToPreviousControlOnClose() async throws {
        let fixture = FriendsModalFixture(state: fixtureState(sampleFriends), presented: false)
        defer { fixture.close() }
        await fixture.settle()
        let previous = NSTextField(string: "")
        previous.frame = NSRect(x: 15, y: 15, width: 140, height: 22)
        // NSHostingView owns its subviews. A native sibling provides the
        // previously focused control without inserting into SwiftUI's tree.
        let wrapper = NSView(frame: fixture.host.frame)
        fixture.window.contentView = wrapper
        wrapper.addSubview(previous)
        fixture.host.frame = wrapper.bounds
        fixture.host.autoresizingMask = [.width, .height]
        wrapper.addSubview(fixture.host)
        fixture.window.makeFirstResponder(previous)
        fixture.state.isPresented = true
        await fixture.settle()
        XCTAssertNotNil(fixture.modal)
        fixture.state.isPresented = false
        await fixture.settle()
        XCTAssertTrue(fixture.window.firstResponder === previous.currentEditor() || fixture.window.firstResponder === previous)
    }

    @MainActor
    func testNativeSearchFindsOfflineFriendAndScrollViewHasOverflow() async throws {
        let fixture = FriendsModalFixture(state: fixtureState(sampleFriends))
        defer { fixture.close() }
        await fixture.settle()
        let scroll = try XCTUnwrap(fixture.scrollView)
        XCTAssertGreaterThan(try XCTUnwrap(scroll.documentView).bounds.height, scroll.contentSize.height)
        let modal = try XCTUnwrap(fixture.modal)
        XCTAssertTrue(modal.focus.controls.contains(.offline))
        try fixture.enterSearch("Camille")
        await fixture.settle()
        XCTAssertEqual(fixture.searchField?.stringValue, "Camille")
        XCTAssertFalse(modal.focus.controls.contains(.offline), "Search reveals offline results without requiring the collapsed section toggle")
        fixture.press(keyCode: 53, characters: "\u{1b}")
        await fixture.settle()
        XCTAssertFalse(fixture.state.isPresented, "Escape closes the modal even when the search field contains text")
    }

    @MainActor
    func testRootIntegratesModalAndKeepsItAcrossSectionChanges() async throws {
        let session = FriendsBridgeTestSession(hydrated: true)
        let steam = friendsTestModel(session: session)
        await steam.start()
        defer { steam.signOut() }
        await session.settle()
        let directory = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let model = AppModel(directory: directory, steam: steam)
        let host = NSHostingView(rootView: RootView(model: model))
        let window = FriendsFixtureWindow(contentRect: NSRect(x: -20_000, y: -20_000, width: 860, height: 600), styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.sharingType = .none
        window.contentView = host
        window.makeKeyAndOrderFront(nil)
        defer { window.close() }
        func settle() async {
            for _ in 0..<8 {
                await withCheckedContinuation { continuation in DispatchQueue.main.async { continuation.resume() } }
                host.layoutSubtreeIfNeeded()
                window.displayIfNeeded()
            }
        }
        func modal(_ view: NSView) -> FriendsModalContainer? {
            (view as? FriendsModalContainer) ?? view.subviews.compactMap(modal).first
        }
        steam.openFriends()
        await settle()
        let panel = try XCTUnwrap(modal(host))
        XCTAssertEqual(panel.host.frame.midX, 430, accuracy: 1)
        model.selection = .settings
        session.fixture.receive(kind: 766, body: FriendsWireFixture.playing)
        await session.settle()
        await settle()
        XCTAssertTrue(steam.isFriendsPresented)
        XCTAssertNotNil(modal(host))
        XCTAssertEqual(steam.friends?.friends.first?.activity?.name, "Portal 2")
        XCTAssertEqual(session.maximumSimultaneousWaits, 1)
        steam.closeFriends()
        await settle()
        XCTAssertNil(modal(host))
        steam.openFriends()
        await settle()
        XCTAssertNotNil(modal(host))
        XCTAssertEqual(session.maximumSimultaneousWaits, 1)
    }

    @MainActor
    func testRenderFriendsFixturesWhenRequested() async throws {
        guard let path = ProcessInfo.processInfo.environment["SAILOVER_RENDER_PREVIEWS"], !path.isEmpty else {
            throw XCTSkip("Set SAILOVER_RENDER_PREVIEWS to render the friends modal in isolated native windows")
        }
        let output = URL(fileURLWithPath: path)
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        for store in ["steam", "epic"] {
            var state = store == "epic" ? FriendsPresentationState.epic(accountID: "epic-fixture") : fixtureState(sampleFriends)
            state.connection = .connected
            state.friends = sampleFriends.map { friend in
                FriendPresentation(id: FriendIdentity(account: state.account, friendID: friend.id.friendID), name: friend.name,
                                   avatarURL: nil, presence: friend.presence, activity: friend.activity)
            }
            for scheme in [ColorScheme.dark, .light] {
                for size in [NSSize(width: 860, height: 600), NSSize(width: 1100, height: 760)] {
                    let fixture = FriendsModalFixture(state: state, size: size, scheme: scheme)
                    await fixture.settle()
                    for query in ["", "Camille", "no such friend"] {
                        if !query.isEmpty { try fixture.enterSearch(query); await fixture.settle() }
                        let bitmap = try XCTUnwrap(fixture.host.bitmapImageRepForCachingDisplay(in: fixture.host.bounds))
                        fixture.host.cacheDisplay(in: fixture.host.bounds, to: bitmap)
                        let png = try XCTUnwrap(bitmap.representation(using: .png, properties: [:]))
                        try png.write(to: output.appending(path: "friends-\(store)-\(scheme)-\(Int(size.width))x\(Int(size.height))-\(query.isEmpty ? "list" : query == "Camille" ? "search" : "empty").png"))
                    }
                    fixture.press(keyCode: 48, characters: "\t")
                    fixture.press(keyCode: 53, characters: "\u{1b}")
                    await fixture.settle()
                    XCTAssertFalse(fixture.state.isPresented)
                    fixture.close()
                }
            }
        }
    }
}

private let account = FriendsAccountIdentity(storeID: "steam", accountID: "fixture")
private func friend(_ id: String, _ name: String?, _ presence: FriendPresence, game: FriendGameActivity? = nil) -> FriendPresentation {
    FriendPresentation(id: FriendIdentity(account: account, friendID: id), name: name, avatarURL: nil, presence: presence, activity: game)
}
private func fixtureState(_ friends: [FriendPresentation], connection: FriendsConnectionState = .connected) -> FriendsPresentationState {
    FriendsPresentationState(store: FriendsStoreIdentity(id: "steam", name: L10n.string("ui.store.steam"), logoAssetName: "SteamLogo"),
                             account: account, friends: friends, connection: connection)
}
private let sampleFriends = [
    friend("1", "Alice", .online, game: FriendGameActivity(id: "1", name: "Portal 2")),
    friend("2", "Émile", .away, game: FriendGameActivity(id: "2", name: "A game with a much longer name than the available width allows")),
    friend("3", "Morgan with a very long display name that should truncate cleanly", .busy, game: FriendGameActivity(id: "3", name: nil)),
    friend("4", "Robin", .online),
    friend("5", nil, .snooze),
    friend("6", "Zoë", .unknown),
    friend("7", "Camille", .offline),
    friend("8", "Alex", .offline),
]
private let avatarPNG = Data(base64Encoded: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNIaPj/HwAGAgLfJrhpqwAAAABJRU5ErkJggg==")!

private actor AvatarRequests {
    private(set) var count = 0
    private var result: Data?
    private var pending: CheckedContinuation<Data, Never>?
    private var observer: CheckedContinuation<Void, Never>?
    func load(_ url: URL) async -> Data {
        count += 1
        observer?.resume(); observer = nil
        if let result { return result }
        return await withCheckedContinuation { pending = $0 }
    }
    func waitUntilRequested() async {
        if count > 0 { return }
        await withCheckedContinuation { observer = $0 }
    }
    func release(_ data: Data) {
        result = data
        pending?.resume(returning: data); pending = nil
    }
}

@MainActor @Observable
private final class FriendsFixtureState {
    var isPresented: Bool
    let friends: FriendsPresentationState
    init(friends: FriendsPresentationState, presented: Bool) { self.friends = friends; isPresented = presented }
}

@MainActor
private final class FriendsModalFixture {
    let window: FriendsFixtureWindow
    let host: NSHostingView<AnyView>
    let state: FriendsFixtureState
    private let directory: URL
    private let defaults: UserDefaults
    private let suite: String

    init(state friends: FriendsPresentationState, size: NSSize = NSSize(width: 860, height: 600), scheme: ColorScheme = .dark, presented: Bool = true) {
        directory = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        suite = "FriendsFixture.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)!
        let model = AppModel(directory: directory, defaults: defaults)
        let state = FriendsFixtureState(friends: friends, presented: presented)
        self.state = state
        host = NSHostingView(rootView: AnyView(FriendsFixtureView(model: model, state: state)
            .environment(\.colorScheme, scheme)))
        window = FriendsFixtureWindow(contentRect: NSRect(origin: NSPoint(x: -20_000, y: -20_000), size: size), styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.sharingType = .none
        window.contentView = host
        window.makeKeyAndOrderFront(nil)
    }

    var modal: FriendsModalContainer? { descendants(host).compactMap { $0 as? FriendsModalContainer }.first }
    var searchField: NSTextField? {
        descendants(host).compactMap { $0 as? NSTextField }.first { $0.placeholderString == L10n.string("ui.friends.search") }
    }
    var scrollView: NSScrollView? {
        guard let modal else { return nil }
        return descendants(modal.host).compactMap { $0 as? NSScrollView }.first
    }

    func enterSearch(_ value: String) throws {
        let field = try XCTUnwrap(searchField)
        window.makeFirstResponder(field)
        let editor = try XCTUnwrap(field.currentEditor() as? NSTextView)
        editor.insertText(value, replacementRange: NSRange(location: 0, length: editor.string.utf16.count))
    }

    func settle() async {
        for _ in 0..<8 {
            await withCheckedContinuation { continuation in DispatchQueue.main.async { continuation.resume() } }
            host.layoutSubtreeIfNeeded()
            window.displayIfNeeded()
        }
    }

    func press(keyCode: UInt16, characters: String) {
        let event = NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                                    windowNumber: window.windowNumber, context: nil, characters: characters, charactersIgnoringModifiers: characters,
                                    isARepeat: false, keyCode: keyCode)!
        NSApp.sendEvent(event)
    }

    func close() {
        modal?.finish()
        window.close()
        defaults.removePersistentDomain(forName: suite)
        try? FileManager.default.removeItem(at: directory)
    }

    private func descendants(_ view: NSView) -> [NSView] { [view] + view.subviews.flatMap(descendants) }
}

private struct FriendsFixtureView: View {
    let model: AppModel
    let state: FriendsFixtureState
    @State private var avatars = FriendsAvatarCache()
    var body: some View {
        RootView(model: model)
            .disabled(state.isPresented)
            .allowsHitTesting(!state.isPresented)
            .accessibilityHidden(state.isPresented)
            .overlay {
                if state.isPresented {
                    FriendsModal(state: state.friends, avatars: avatars, dismiss: { state.isPresented = false }, retry: {}, signIn: {})
                }
            }
    }
}

private final class FriendsFixtureWindow: NSWindow {
    override var canBecomeKey: Bool { true }
}
