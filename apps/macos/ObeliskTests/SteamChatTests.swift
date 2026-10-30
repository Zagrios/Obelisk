import AppKit
import Foundation
import SwiftUI
import XCTest
@testable import Obelisk

final class SteamChatIntegrationTests: XCTestCase {
    private let friend = ChatBridgeTestSession.friendID

    @MainActor
    func testWorkspaceSelectionLeavesTheFriendsListInPlace() async throws {
        let session = ChatBridgeTestSession()
        let model = friendsTestModel(connect: { _ in session })
        await model.start()
        defer { model.signOut() }
        try await eventually { model.friends?.connection == .connected }
        let state = try XCTUnwrap(model.friends)
        let friend = try XCTUnwrap(state.friends.first)
        let focus = FriendsPanelFocus()
        var selected: FriendIdentity?
        let panel = FriendsPanel(state: state, avatars: FriendsAvatarCache(), chat: model.chat,
            closable: false, openConversation: { selected = $0 }, focus: focus,
            dismiss: {}, retry: {}, signIn: {})
        let window = ChatTestWindow(contentRect: NSRect(x: -20000, y: -20000, width: 400, height: 550),
            styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = NSHostingView(rootView: panel)
        window.orderFrontRegardless()
        defer { window.close() }
        try await eventually { focus.activate != nil }
        XCTAssertTrue(focus.activate?(.friend(friend.id)) ?? false)
        XCTAssertEqual(selected, friend.id)
        XCTAssertNil(focus.conversation, "The friends window must not navigate before opening Chat")
    }

    @MainActor
    func testRecentConversationMetadataAndPreviewLoadWithoutReadingMessages() async throws {
        let session = ChatBridgeTestSession()
        session.sessions = [SteamChatSession(friendSteamId: friend, lastMessage: 50,
            lastView: 40, unreadMessageCount: 3)]
        session.history = { _, _ in
            SteamChatHistory(messages: [SteamChatHistoryMessage(fromMe: false, timestamp: 50,
                ordinal: 1, message: "[b]hello[/b]\nthere")], moreAvailable: false)
        }
        let model = friendsTestModel(connect: { _ in session })
        model.chat.applicationActivityChanged(false)
        await model.start()
        defer { model.signOut() }
        try await eventually { model.chat.preview(for: self.friend) == "hello there" }
        XCTAssertEqual(model.chat.lastActivity(for: friend), 50)
        XCTAssertEqual(model.chat.unread[friend], 3)
        XCTAssertNil(model.chat.visibleFriend)
        XCTAssertTrue(session.acks.isEmpty)
        session.chat.incomingMessage(friendSteamId: friend, message: "newest", timestamp: 100,
            ordinal: 0, fromMe: false)
        try await eventually { model.chat.preview(for: self.friend) == "newest" }
        XCTAssertEqual(model.chat.lastActivity(for: friend), 100)
    }

    @MainActor
    func testNonactivatingOverlayClearsUnreadAndClosingRestoresUnreadCounting() async throws {
        let session = ChatBridgeTestSession()
        let model = friendsTestModel(connect: { _ in session })
        await model.start()
        defer { model.signOut() }
        let chat = model.chat
        chat.applicationActivityChanged(false)
        session.chat.incomingMessage(friendSteamId: friend, message: "hello", timestamp: 100, ordinal: 0, fromMe: false)
        try await eventually { chat.unread[self.friend] == 1 }
        chat.show(friend)
        chat.isOverlayPresented = true
        try await eventually { session.acks.last?.timestamp == 100 }
        XCTAssertNil(chat.unread[friend])
        chat.isOverlayPresented = false
        session.chat.incomingMessage(friendSteamId: friend, message: "again", timestamp: 110, ordinal: 0, fromMe: false)
        try await eventually { chat.unread[self.friend] == 1 }
    }

    @MainActor
    func testIncomingMessagesCountAsUnreadUntilReadInActiveApp() async throws {
        let session = ChatBridgeTestSession()
        let model = friendsTestModel(connect: { _ in session })
        await model.start()
        defer { model.signOut() }
        let chat = model.chat
        chat.applicationActivityChanged(false)

        // Fixture events cross the production decoders and the bridge records.
        session.chat.incomingMessage(friendSteamId: friend, message: "Salut \\[toi]", timestamp: 100, ordinal: 0, fromMe: false)
        try await eventually { chat.unread[self.friend] == 1 }
        session.chat.incomingMessage(friendSteamId: friend, message: "Salut \\[toi]", timestamp: 100, ordinal: 0, fromMe: false)
        session.chat.incomingMessage(friendSteamId: friend, message: "encore", timestamp: 101, ordinal: 0, fromMe: false)
        session.chat.incomingMessage(friendSteamId: friend, message: "depuis le téléphone", timestamp: 102, ordinal: 0, fromMe: true)
        try await eventually { chat.conversations[self.friend]?.lines.count == 3 }
        XCTAssertEqual(chat.unread[friend], 2, "A repeated (timestamp, ordinal) and the account's own echo are not unread")
        XCTAssertEqual(chat.totalUnread, 2)
        XCTAssertEqual(chat.conversations[friend]?.lines.first?.content, .message(fromMe: false, bbcode: "Salut \\[toi]"))
        XCTAssertEqual(chat.conversations[friend]?.lines.last?.fromMe, true)

        // On screen while Obelisk is in the background: still unread.
        chat.show(friend)
        try await eventually { chat.conversations[self.friend]?.isLoaded == true }
        XCTAssertEqual(session.acks, [])
        XCTAssertEqual(chat.unread[friend], 2)
        chat.applicationActivityChanged(true)
        try await eventually { session.acks == [ChatBridgeTestSession.Ack(friend: self.friend, timestamp: 102)] }
        XCTAssertNil(chat.unread[friend])

        // A message arriving while read is acknowledged at once.
        session.chat.typing(friendSteamId: friend)
        try await eventually { chat.conversations[self.friend]?.isFriendTyping == true }
        session.chat.incomingMessage(friendSteamId: friend, message: "là ?", timestamp: 110, ordinal: 0, fromMe: false)
        try await eventually { session.acks.last?.timestamp == 110 }
        XCTAssertEqual(chat.conversations[friend]?.isFriendTyping, false, "A message ends the friend's typing")
        XCTAssertEqual(chat.totalUnread, 0)

        // Read in another session of the account.
        chat.hide(friend)
        session.chat.incomingMessage(friendSteamId: friend, message: "?", timestamp: 120, ordinal: 0, fromMe: false)
        try await eventually { chat.unread[self.friend] == 1 }
        session.chat.readElsewhere(friendSteamId: friend, timestamp: 120)
        try await eventually { chat.unread[self.friend] == nil }
    }

    @MainActor
    func testSteamUnreadCountsShowAtConnectionAndClearOnceRead() async throws {
        let session = ChatBridgeTestSession()
        session.sessions = [SteamChatSession(friendSteamId: friend, lastMessage: 50, lastView: 40, unreadMessageCount: 3)]
        session.history = { _, _ in
            SteamChatHistory(messages: [SteamChatHistoryMessage(fromMe: false, timestamp: 50, ordinal: 1, message: "hello")],
                             moreAvailable: false)
        }
        let model = friendsTestModel(connect: { _ in session })
        await model.start()
        defer { model.signOut() }
        let chat = model.chat
        try await eventually { chat.totalUnread == 3 }
        chat.applicationActivityChanged(true)
        chat.show(friend)
        try await eventually { session.acks == [ChatBridgeTestSession.Ack(friend: self.friend, timestamp: 50)] }
        XCTAssertEqual(chat.totalUnread, 0)
        XCTAssertEqual(chat.conversations[friend]?.lines.map(\.id), [.message(timestamp: 50, ordinal: 1)])
        // Showing it again sends nothing new.
        chat.hide(friend)
        chat.show(friend)
        try await Task.sleep(for: .milliseconds(50))
        XCTAssertEqual(session.acks.count, 1)
    }

    @MainActor
    func testFallingBehindTheEventRingReloadsCountsAndOpenConversation() async throws {
        let session = ChatBridgeTestSession()
        let model = friendsTestModel(connect: { _ in session })
        await model.start()
        defer { model.signOut() }
        let chat = model.chat
        chat.applicationActivityChanged(false)
        chat.show(friend)
        try await eventually {
            session.historyRequests == 1 && session.sessionRequests == 1 && chat.conversations[self.friend]?.isLoaded == true
        }

        await session.gate.close()
        session.chat.incomingMessage(friendSteamId: friend, message: "1", timestamp: 1, ordinal: 0, fromMe: false)
        try await eventually { chat.conversations[self.friend]?.lines.count == 1 }
        // The ring keeps 512 events: these overrun the model's cursor.
        for index in 0..<600 {
            session.chat.incomingMessage(friendSteamId: friend, message: "\(index)", timestamp: 10 + UInt32(index),
                                         ordinal: 0, fromMe: false)
        }
        session.sessions = [SteamChatSession(friendSteamId: friend, lastMessage: 609, lastView: 0, unreadMessageCount: 7)]
        session.history = { _, _ in
            SteamChatHistory(messages: [SteamChatHistoryMessage(fromMe: false, timestamp: 609, ordinal: 0, message: "599")],
                             moreAvailable: true)
        }
        await session.gate.open()
        try await eventually { session.historyRequests == 2 && session.sessionRequests == 2 }
        try await eventually { chat.unread[self.friend] == 7 }
        XCTAssertEqual(chat.conversations[friend]?.hasOlder, true)
        XCTAssertEqual(chat.conversations[friend]?.lines.last?.id, .message(timestamp: 609, ordinal: 0))
    }

    @MainActor
    func testInvitationOfferedOnlyWhileOwnGameTakesOneAndFriendIsOnline() async throws {
        let session = ChatBridgeTestSession(games: [SteamGame(appId: 620, name: "Portal 2", iconHash: "", lastPlayed: 0, playtimeMinutes: 0)])
        let model = friendsTestModel(connect: { _ in session })
        await model.start()
        defer { model.signOut() }
        let chat = model.chat
        try await eventually { model.friends?.connection == .connected && !model.library.games.isEmpty }
        let alice = try XCTUnwrap(model.friends?.friends.first)
        XCTAssertEqual(alice.steamID, friend)
        let offline = FriendPresentation(id: alice.id, name: alice.name, avatarURL: nil, presence: .offline, activity: nil)

        try await eventually { chat.ownGame != nil }
        XCTAssertNil(chat.inviteOffer(for: alice), "No game, no invitation")
        session.chat.ownGame(appId: 620, lobbyId: 0, connect: nil)
        try await eventually { chat.ownGame?.appId == 620 }
        XCTAssertNil(chat.inviteOffer(for: alice), "A game with neither lobby nor connect string takes no invitation")

        session.chat.ownGame(appId: 620, lobbyId: 0, connect: "+connect 10.0.0.1")
        try await eventually { chat.ownGame?.invite == .connect(connect: "+connect 10.0.0.1") }
        XCTAssertEqual(chat.inviteOffer(for: alice), SteamInviteOffer(appId: 620, gameName: "Portal 2"))
        XCTAssertNil(chat.inviteOffer(for: offline))
        XCTAssertEqual(chat.inviteOffer(for: alice)?.menuTitle, L10n.string("ui.chat.inviteToJoin", "Portal 2"))

        session.chat.ownGame(appId: 620, lobbyId: 109_775_241_058_543_776, connect: "+connect 10.0.0.1")
        try await eventually { chat.ownGame?.invite == .lobby(lobbyId: 109_775_241_058_543_776) }
        let offer = try XCTUnwrap(chat.inviteOffer(for: alice))
        chat.invite(friend, to: offer)
        try await eventually { chat.conversations[self.friend]?.lines.last?.content == .invitationSent(appId: 620) }
        XCTAssertEqual(session.invites, [ChatBridgeTestSession.Invite(friend: friend, appId: 620)])

        session.inviteFailure = SteamError.Failure(reason: "backend.steam.invite.unavailable")
        chat.invite(friend, to: offer)
        try await eventually { chat.conversations[self.friend]?.error == L10n.string("backend.steam.invite.unavailable") }
        XCTAssertEqual(chat.conversations[friend]?.isInviting, false)

        session.chat.ownGame(appId: 0, lobbyId: 0, connect: nil)
        try await eventually { chat.ownGame?.appId == 0 }
        XCTAssertNil(chat.inviteOffer(for: alice))
    }

    private static let peak = SteamGame(appId: 3_527_290, name: "PEAK", iconHash: "", lastPlayed: 0, playtimeMinutes: 0)
    private static let peakLobby: UInt64 = 109_775_244_421_126_053
    /// The lobby PEAK created after its first run, in the same session.
    private static let nextPeakLobby: UInt64 = 109_775_244_423_207_541

    /// PEAK's session as its supervisor shows it once the game asked for its
    /// invite dialog `sequence` times, with the engine showing it in `lobby`.
    private func peakSession(requests sequence: UInt64, invite: SteamGameInvite = .lobby(lobbyId: peakLobby),
                             lobby: UInt64? = nil, phase: GamePhase = .running) -> GameSessionState {
        var state = GameSessionState.steam(appID: Self.peak.appId, accountID: FriendsWireFixture.accountID,
                                           id: "peak", phase: phase)
        state.inviteRequest = sequence == 0 ? nil : SteamInviteRequest(sequence: sequence, invite: invite)
        state.lobbyId = lobby
        return state
    }

    private func pickerOffer(_ request: UInt64, invite: SteamGameInvite = .lobby(lobbyId: peakLobby)) -> SteamInviteOffer {
        SteamInviteOffer(appId: Self.peak.appId, gameName: "PEAK", invite: invite, request: request)
    }

    @MainActor
    func testLobbyTheEngineShowsIsOfferedWhenThePersonaShowsNone() async throws {
        let session = ChatBridgeTestSession(games: [Self.peak])
        let sessions = SteamGameSessions()
        let model = friendsTestModel(connect: { _ in session }, gameSessions: sessions)
        let overlay = OverlayFixture(steam: model).overlay
        await model.start()
        defer { model.signOut() }
        let chat = model.chat
        try await eventually { model.friends?.connection == .connected && !model.library.games.isEmpty && chat.ownGame != nil }
        let alice = try XCTUnwrap(model.friends?.friends.first)
        let offline = FriendPresentation(id: alice.id, name: alice.name, avatarURL: nil, presence: .offline, activity: nil)

        // PEAK's lobby is private: the persona shows the game, never its lobby.
        session.chat.ownGame(appId: Self.peak.appId, lobbyId: 0, connect: nil)
        sessions.fake.show(peakSession(requests: 0))
        try await eventually { chat.ownGame?.appId == Self.peak.appId && sessions.active != nil }
        XCTAssertNil(chat.inviteOffer(for: alice), "Nothing tells the lobby yet")

        // The lobby the game's Invite button named is the picker's alone.
        sessions.fake.show(peakSession(requests: 1))
        try await eventually { chat.invitePicker != nil }
        XCTAssertNil(chat.inviteOffer(for: alice), "The rows never offer a dialog's lobby the engine has not confirmed")
        overlay.hide()
        XCTAssertNil(chat.inviteOffer(for: alice), "Nor once its picker closed")

        // The engine shows the game in its lobby.
        sessions.fake.show(peakSession(requests: 1, lobby: Self.peakLobby))
        try await eventually { chat.inviteOffer(for: alice) != nil }
        let expected = SteamInviteOffer(appId: Self.peak.appId, gameName: "PEAK", invite: .lobby(lobbyId: Self.peakLobby))
        XCTAssertEqual(chat.inviteOffer(for: alice), expected)
        XCTAssertNil(chat.inviteOffer(for: offline))
        XCTAssertEqual(chat.inviteOffer(for: alice)?.buttonTitle, L10n.string("ui.chat.inviteTo", "PEAK"))

        chat.invite(friend, to: expected)
        try await eventually { chat.conversations[self.friend]?.lines.last?.content == .invitationSent(appId: Self.peak.appId) }
        XCTAssertEqual(session.reportedInvites, [ChatBridgeTestSession.ReportedInvite(
            friend: friend, appId: Self.peak.appId, invite: .lobby(lobbyId: Self.peakLobby))])
        XCTAssertEqual(session.invites, [], "The persona path is not asked")

        // A new run is a new lobby; back at the title screen there is none.
        sessions.fake.show(peakSession(requests: 1, lobby: Self.nextPeakLobby))
        try await eventually { chat.inviteOffer(for: alice)?.invite == .lobby(lobbyId: Self.nextPeakLobby) }
        sessions.fake.show(peakSession(requests: 1))
        try await eventually { chat.inviteOffer(for: alice) == nil }

        // A lobby the persona shows comes first, as in Steam's own menu.
        sessions.fake.show(peakSession(requests: 1, lobby: Self.peakLobby))
        session.chat.ownGame(appId: Self.peak.appId, lobbyId: 109_775_244_421_126_054, connect: nil)
        try await eventually { chat.ownGame?.invite == .lobby(lobbyId: 109_775_244_421_126_054) && chat.gameLobby != nil }
        XCTAssertEqual(chat.inviteOffer(for: alice), SteamInviteOffer(appId: Self.peak.appId, gameName: "PEAK"))
        // A persona showing another game refuses the engine's lobby.
        session.chat.ownGame(appId: 730, lobbyId: 0, connect: nil)
        try await eventually { chat.ownGame?.appId == 730 }
        XCTAssertNil(chat.inviteOffer(for: alice))

        // The game ends: its lobby goes with it.
        session.chat.ownGame(appId: 0, lobbyId: 0, connect: nil)
        sessions.fake.show(peakSession(requests: 1, lobby: Self.peakLobby, phase: .finalizing))
        try await eventually { chat.ownGame?.appId == 0 && sessions.active?.running == false }
        XCTAssertNil(chat.inviteOffer(for: alice))
        XCTAssertNil(chat.gameLobby)
    }

    @MainActor
    func testADialogsLobbyIsOfferedInRowsAndHeaderOnlyWhileTheEngineConfirmsIt() async throws {
        let session = ChatBridgeTestSession(games: [Self.peak])
        let sessions = SteamGameSessions()
        let model = friendsTestModel(connect: { _ in session }, gameSessions: sessions)
        let overlay = OverlayFixture(steam: model).overlay
        await model.start()
        defer { model.signOut() }
        let chat = model.chat
        try await eventually { model.friends?.connection == .connected && !model.library.games.isEmpty && chat.ownGame != nil }
        let alice = try XCTUnwrap(model.friends?.friends.first)
        let offline = FriendPresentation(id: alice.id, name: alice.name, avatarURL: nil, presence: .offline, activity: nil)

        // PEAK's private lobby: the persona shows the game alone. Its invite
        // kiosk names the lobby; the engine has not confirmed it yet.
        session.chat.ownGame(appId: Self.peak.appId, lobbyId: 0, connect: nil)
        sessions.fake.show(peakSession(requests: 0))
        try await eventually { chat.ownGame?.appId == Self.peak.appId && sessions.active != nil }
        sessions.fake.show(peakSession(requests: 1))
        try await eventually { chat.invitePicker != nil }
        overlay.hide()
        XCTAssertNil(chat.inviteOffer(for: alice))

        // The engine's matchmaking shows the account in the named lobby: the
        // friends list rows and the conversation header both offer it.
        sessions.fake.show(peakSession(requests: 1, lobby: Self.peakLobby))
        try await eventually { chat.inviteOffer(for: alice) != nil }
        let expected = SteamInviteOffer(appId: Self.peak.appId, gameName: "PEAK", invite: .lobby(lobbyId: Self.peakLobby))
        XCTAssertEqual(chat.inviteOffer(for: alice), expected)
        XCTAssertEqual(chat.inviteOffer(for: alice)?.buttonTitle, L10n.string("ui.chat.inviteTo", "PEAK"))
        XCTAssertNil(chat.inviteOffer(for: offline))
        XCTAssertNil(chat.invitePicker, "A confirmation opens no picker")

        // The game left that lobby, still running: the engine no longer
        // confirms it, though the last dialog named it.
        sessions.fake.show(peakSession(requests: 1))
        try await eventually { chat.inviteOffer(for: alice) == nil }
        XCTAssertNil(chat.gameLobby)

        // Its next lobby, once the kiosk named it and the engine confirmed it.
        let next = SteamGameInvite.lobby(lobbyId: Self.nextPeakLobby)
        sessions.fake.show(peakSession(requests: 2, invite: next))
        try await eventually { chat.invitePicker == self.pickerOffer(2, invite: next) }
        overlay.hide()
        XCTAssertNil(chat.inviteOffer(for: alice))
        sessions.fake.show(peakSession(requests: 2, invite: next, lobby: Self.nextPeakLobby))
        try await eventually { chat.inviteOffer(for: alice)?.invite == next }
    }

    @MainActor
    func testARequestTheSessionShowedWhenFirstSeenOpensNoPicker() async throws {
        let session = ChatBridgeTestSession(games: [Self.peak])
        let sessions = SteamGameSessions()
        // A session recovered from a previous Obelisk, its game long in its lobby.
        sessions.fake.show(peakSession(requests: 3, lobby: Self.peakLobby))
        let model = friendsTestModel(connect: { _ in session }, gameSessions: sessions)
        let fixture = OverlayFixture(steam: model)
        await model.start()
        defer { model.signOut() }
        try await eventually { model.friends?.connection == .connected && !model.library.games.isEmpty }
        let alice = try XCTUnwrap(model.friends?.friends.first)
        try await eventually { model.chat.inviteOffer(for: alice) != nil }
        XCTAssertEqual(model.chat.inviteOffer(for: alice)?.invite, .lobby(lobbyId: Self.peakLobby))
        XCTAssertNil(model.chat.invitePicker)
        XCTAssertFalse(fixture.overlay.isPresented)
        XCTAssertEqual(fixture.presenter.shown, [])
        XCTAssertFalse(model.isFriendsPresented)
    }

    @MainActor
    func testWithoutAnOverlayToShowItTheGamesRequestOpensNoPicker() async throws {
        let session = ChatBridgeTestSession(games: [Self.peak])
        let sessions = SteamGameSessions()
        let model = friendsTestModel(connect: { _ in session }, gameSessions: sessions)
        await model.start()
        defer { model.signOut() }
        try await eventually { model.friends?.connection == .connected && !model.library.games.isEmpty }
        sessions.fake.show(peakSession(requests: 0))
        try await eventually { model.chat.gameLobby == nil && sessions.active != nil }
        sessions.fake.show(peakSession(requests: 1, lobby: Self.peakLobby))
        try await eventually { model.chat.gameLobby != nil }
        XCTAssertNil(model.chat.invitePicker, "Nothing shows the picker: it closes")
        XCTAssertFalse(model.isFriendsPresented, "The library window's friends list stays closed")
    }

    @MainActor
    func testTheGamesInviteButtonOpensThePickerInTheOverlayWhichInvitesWithoutLeavingIt() async throws {
        let session = ChatBridgeTestSession(games: [Self.peak])
        let sessions = SteamGameSessions()
        let model = friendsTestModel(connect: { _ in session }, gameSessions: sessions)
        let fixture = OverlayFixture(steam: model, showsPanel: true)
        let overlay = fixture.overlay
        await model.start()
        defer { model.signOut() }
        let chat = model.chat
        try await eventually { model.friends?.connection == .connected && !model.library.games.isEmpty }
        let state = try XCTUnwrap(model.friends)
        let alice = try XCTUnwrap(state.friends.first)
        sessions.fake.show(peakSession(requests: 0))
        try await eventually { sessions.active != nil && overlay.session != nil }
        XCTAssertFalse(overlay.isPresented)

        sessions.fake.show(peakSession(requests: 1))
        try await eventually { chat.invitePicker != nil }
        XCTAssertTrue(overlay.isPresented)
        XCTAssertEqual(overlay.request, .invite(pickerOffer(1)))
        XCTAssertEqual(overlay.invitePicker, pickerOffer(1))
        XCTAssertEqual(fixture.presenter.shown.count, 1, "The overlay opens once")
        XCTAssertFalse(model.isFriendsPresented, "The library window's friends list stays closed")
        XCTAssertNil(fixture.app.presentedFriends)
        XCTAssertEqual(chat.invitePicker, pickerOffer(1))
        XCTAssertEqual(chat.pickerOffer(for: alice), pickerOffer(1))
        XCTAssertEqual(pickerOffer(1).pickerTitle, L10n.string("ui.chat.invitePicker.title", "PEAK"))

        // Return on a friend's row invites; the picker stays for the next one.
        let panel = try XCTUnwrap(fixture.presenter.panel)
        let host = try XCTUnwrap(panel.contentView)
        func settle() async {
            for _ in 0..<8 {
                await drainMainQueue()
                host.layoutSubtreeIfNeeded()
                panel.displayIfNeeded()
            }
        }
        func pressReturn(on friend: FriendPresentation) async {
            overlay.focus.target = .friend(friend.id)
            await settle()
            NSApp.sendEvent(NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [],
                                             timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: panel.windowNumber,
                                             context: nil, characters: "\r", charactersIgnoringModifiers: "\r",
                                             isARepeat: false, keyCode: 36)!)
            await settle()
        }
        await settle()
        XCTAssertTrue(overlay.focus.controls.contains(.friend(alice.id)), "The overlay's friends list takes Tab")
        XCTAssertFalse(overlay.focus.controls.contains(.close), "The overlay closes itself")
        await pressReturn(on: alice)
        try await eventually { chat.pickerInvited.contains(self.friend) }
        XCTAssertEqual(session.reportedInvites, [ChatBridgeTestSession.ReportedInvite(
            friend: friend, appId: Self.peak.appId, invite: .lobby(lobbyId: Self.peakLobby))])
        XCTAssertNil(overlay.focus.conversation, "The picker stays on the list")
        XCTAssertTrue(overlay.isPresented)

        // A conversation opened from the picker gives way to the next press,
        // even for the same lobby.
        overlay.focus.conversation = alice.id
        await settle()
        sessions.fake.show(peakSession(requests: 2))
        try await eventually { chat.invitePicker?.request == 2 }
        await settle()
        XCTAssertEqual(chat.invitePicker, pickerOffer(2))
        XCTAssertEqual(overlay.request, .invite(pickerOffer(2)))
        XCTAssertNil(overlay.focus.conversation, "Back on the picker's list")
        XCTAssertTrue(chat.pickerInvited.isEmpty, "Nobody invited yet")
        XCTAssertEqual(fixture.presenter.shown.count, 1, "Still the same overlay")

        // A failed invitation shows on its row only; the conversation keeps
        // its own errors, and the next picker starts clean.
        session.inviteFailure = SteamError.Failure(reason: "backend.steam.invite.unavailable")
        chat.invite(friend, to: pickerOffer(2))
        try await eventually { chat.pickerFailures[self.friend] != nil }
        XCTAssertEqual(chat.pickerFailures[friend], L10n.string("backend.steam.invite.unavailable"))
        XCTAssertNil(chat.conversations[friend]?.error)
        session.inviteFailure = nil

        // Closing the overlay ends the picker; the same request seen again
        // opens nothing.
        overlay.hide()
        XCTAssertNil(chat.invitePicker)
        XCTAssertTrue(chat.pickerInvited.isEmpty)
        XCTAssertTrue(chat.pickerFailures.isEmpty)
        XCTAssertFalse(chat.followGame(session: "peak", appId: Self.peak.appId,
                                       request: SteamInviteRequest(sequence: 2, invite: .lobby(lobbyId: Self.peakLobby)),
                                       lobby: nil))
        // Through the model too: the session changes, its request does not.
        sessions.fake.show(peakSession(requests: 2, lobby: Self.peakLobby))
        try await eventually { chat.gameLobby?.lobbyId == Self.peakLobby }
        XCTAssertNil(chat.invitePicker)
        XCTAssertFalse(overlay.isPresented)

        // The next press opens it again, with its own target.
        sessions.fake.show(peakSession(requests: 3, invite: .connect(connect: "+connect_lobby 1"), lobby: Self.peakLobby))
        try await eventually { chat.invitePicker != nil }
        XCTAssertEqual(chat.invitePicker, pickerOffer(3, invite: .connect(connect: "+connect_lobby 1")))
        XCTAssertTrue(overlay.isPresented)
        XCTAssertEqual(fixture.presenter.shown.count, 2)
        await settle()

        // Leaving the picker for another page of the overlay ends it.
        overlay.show(.achievements)
        XCTAssertNil(chat.invitePicker)
        overlay.show(.home)
        sessions.fake.show(peakSession(requests: 4, lobby: Self.peakLobby))
        try await eventually { chat.invitePicker?.request == 4 }
        XCTAssertEqual(overlay.request, .invite(pickerOffer(4)))
        await settle()

        // While Steam's chat reconnects, an online friend's row waits instead
        // of opening a conversation.
        chat.suspend()
        XCTAssertNil(chat.pickerOffer(for: alice))
        XCTAssertNotNil(chat.invitePicker)
        await pressReturn(on: alice)
        XCTAssertNil(overlay.focus.conversation)

        // The game's Steam connection drops: its request, and the picker, go.
        sessions.fake.show(peakSession(requests: 0, lobby: Self.peakLobby))
        try await eventually { chat.invitePicker == nil }
        XCTAssertNil(overlay.invitePicker)

        // The game ends: everything goes with it, the overlay too.
        sessions.fake.show(nil)
        try await eventually { chat.gameLobby == nil && !overlay.isPresented }
        XCTAssertNil(chat.inviteOffer(for: alice))
    }

    @MainActor
    func testSendingKeepsTheServerMessageAndShowsLocalizedErrors() async throws {
        let session = ChatBridgeTestSession()
        let model = friendsTestModel(connect: { _ in session })
        await model.start()
        defer { model.signOut() }
        let chat = model.chat
        chat.setDraft("  [b]salut[/b] ", for: friend)
        chat.send(to: friend)
        XCTAssertEqual(chat.draft(for: friend), "")
        try await eventually { chat.conversations[self.friend]?.lines.count == 1 }
        XCTAssertEqual(session.sent, ["[b]salut[/b]"])
        XCTAssertEqual(chat.conversations[friend]?.lines.first?.content, .message(fromMe: true, bbcode: "\\[b]salut\\[/b]"))
        XCTAssertEqual(chat.conversations[friend]?.isSending, false)

        session.sendFailure = SteamError.Failure(reason: "backend.steam.chat.rateLimited")
        chat.setDraft("encore", for: friend)
        chat.send(to: friend)
        try await eventually { chat.conversations[self.friend]?.error != nil }
        XCTAssertEqual(chat.conversations[friend]?.error, L10n.string("backend.steam.chat.rateLimited"))
        XCTAssertEqual(chat.draft(for: friend), "encore", "A failed message goes back to the composer")
        chat.dismissError(for: friend)
        XCTAssertNil(chat.conversations[friend]?.error)

        model.signOut()
        XCTAssertTrue(chat.conversations.isEmpty)
        XCTAssertNil(chat.ownGame)
    }

    @MainActor
    func testAccountShowsOnlineAgainWhenItsSteamGameSessionEnds() async throws {
        let session = ChatBridgeTestSession()
        let sessions = SteamGameSessions()
        let model = friendsTestModel(connect: { _ in session }, gameSessions: sessions)
        await model.start()
        defer { model.signOut() }
        try await eventually { model.isConnected }

        sessions.fake.show(.steam(appID: 620, accountID: FriendsWireFixture.accountID + 1))
        try await eventually { sessions.active != nil }
        sessions.fake.show(nil)
        try await eventually { sessions.active == nil }
        try await Task.sleep(for: .milliseconds(50))
        XCTAssertEqual(session.onlineRequests, 0, "Another account's game leaves this one alone")

        sessions.fake.show(.steam(appID: 620, accountID: FriendsWireFixture.accountID))
        try await eventually { sessions.active != nil }
        XCTAssertEqual(session.onlineRequests, 0)
        sessions.fake.show(nil)
        try await eventually { session.onlineRequests == 1 }
    }

    @MainActor
    func testRequestsInFlightEndWithTheirSession() async throws {
        let session = ChatBridgeTestSession()
        let chat = SteamChatModel(isApplicationActive: true)
        let account = FriendsWireFixture.accountID
        chat.connect(session, account: account)
        defer { chat.reset() }
        chat.show(friend)
        try await eventually { chat.conversations[self.friend]?.isLoaded == true }

        await session.sending.close()
        session.sendFailure = SteamError.Failure(reason: "backend.steam.chat.rateLimited")
        chat.setDraft("hey", for: friend)
        chat.send(to: friend)
        XCTAssertEqual(chat.conversations[friend]?.isSending, true)
        chat.suspend()
        chat.connect(session, account: account)
        XCTAssertEqual(chat.conversations[friend]?.isSending, false, "A reconnection does not wait on the old session")
        await session.sending.open()
        try await eventually { chat.draft(for: self.friend) == "hey" }
        XCTAssertNil(chat.conversations[friend]?.error, "The old session's failure is not this one's")

        session.sendFailure = nil
        chat.send(to: friend)
        try await eventually { session.sent == ["hey"] }
    }

    @MainActor
    func testConversationOpenedWhileDisconnectedLoadsOnceConnected() async throws {
        let session = ChatBridgeTestSession()
        session.history = { _, _ in
            SteamChatHistory(messages: [SteamChatHistoryMessage(fromMe: false, timestamp: 50, ordinal: 0, message: "hello")],
                             moreAvailable: true)
        }
        let chat = SteamChatModel(isApplicationActive: false)
        chat.connect(session, account: FriendsWireFixture.accountID)
        defer { chat.reset() }
        // Reconnecting after a wake: the cached friends stay on screen.
        chat.suspend()
        chat.show(friend)
        XCTAssertEqual(session.historyRequests, 0)
        chat.connect(session, account: FriendsWireFixture.accountID)
        try await eventually { chat.conversations[self.friend]?.isLoaded == true }
        XCTAssertEqual(chat.conversations[friend]?.hasOlder, true)
        XCTAssertEqual(chat.conversations[friend]?.lines.map(\.id), [.message(timestamp: 50, ordinal: 0)])
    }

    @MainActor
    func testEscapeLeavesTheConversationBeforeClosingThePanel() async throws {
        let session = ChatBridgeTestSession()
        let model = friendsTestModel(connect: { _ in session })
        await model.start()
        defer { model.signOut() }
        try await eventually { model.friends?.connection == .connected }
        let state = try XCTUnwrap(model.friends)
        let alice = try XCTUnwrap(state.friends.first)
        session.chat.incomingMessage(friendSteamId: friend, message: "[lobbyinvite appid=\"620\" lobbyid=\"1\"][/lobbyinvite]",
                                     timestamp: 100, ordinal: 0, fromMe: false)
        try await eventually { model.chat.unread[self.friend] == 1 }

        let presented = ChatModalPresentation()
        let host = NSHostingView(rootView: ChatModalHost(state: state, chat: model.chat, presentation: presented))
        let window = ChatTestWindow(contentRect: NSRect(x: -20_000, y: -20_000, width: 860, height: 600),
                                    styleMask: .borderless, backing: .buffered, defer: false)
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
        func container(_ view: NSView) -> FriendsModalContainer? {
            (view as? FriendsModalContainer) ?? view.subviews.compactMap(container).first
        }
        func press(_ keyCode: UInt16, _ characters: String) {
            NSApp.sendEvent(NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [],
                                             timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber,
                                             context: nil, characters: characters, charactersIgnoringModifiers: characters,
                                             isARepeat: false, keyCode: keyCode)!)
        }
        await settle()
        let modal = try XCTUnwrap(container(host))
        XCTAssertTrue(modal.focus.controls.contains(.friend(alice.id)), "Rows that open a conversation take part in Tab")

        // Return on the friend's row opens the conversation.
        modal.focus.target = .friend(alice.id)
        await settle()
        press(36, "\r")
        await settle()
        XCTAssertEqual(modal.focus.conversation, alice.id)
        XCTAssertEqual(modal.focus.target, .composer)
        XCTAssertEqual(model.chat.visibleFriend, friend)

        press(53, "\u{1b}")
        await settle()
        XCTAssertNil(modal.focus.conversation)
        XCTAssertEqual(modal.focus.target, .friend(alice.id))
        XCTAssertTrue(presented.isPresented)
        try await eventually { model.chat.visibleFriend == nil }

        press(53, "\u{1b}")
        await settle()
        XCTAssertFalse(presented.isPresented)
    }

    @MainActor
    func testRenderConversationWhenRequested() async throws {
        guard let path = ProcessInfo.processInfo.environment["OBELISK_RENDER_PREVIEWS"], !path.isEmpty else {
            throw XCTSkip("Set OBELISK_RENDER_PREVIEWS to render the conversation in isolated native windows")
        }
        let output = URL(fileURLWithPath: path)
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        let session = ChatBridgeTestSession(games: [SteamGame(appId: 620, name: "Portal 2", iconHash: "", lastPlayed: 0, playtimeMinutes: 0)])
        let now = UInt32(Date().timeIntervalSince1970)
        session.history = { _, _ in
            SteamChatHistory(messages: [
                SteamChatHistoryMessage(fromMe: false, timestamp: now - 90_000, ordinal: 0, message: "On se fait une partie ce soir ?"),
                SteamChatHistoryMessage(fromMe: true, timestamp: now - 89_900, ordinal: 0,
                                        message: "Carrément, regarde [url=https://example.com]ce mod[/url] d’abord"),
                SteamChatHistoryMessage(fromMe: false, timestamp: now - 3_000, ordinal: 0,
                                        message: "[lobbyinvite appid=\"620\" lobbyid=\"1\"][/lobbyinvite]"),
                SteamChatHistoryMessage(fromMe: true, timestamp: now - 2_900, ordinal: 0,
                                        message: "J’arrive, je finis un truc [emoticon]steamhappy[/emoticon]"),
            ], moreAvailable: false)
        }
        let sessions = SteamGameSessions()
        let model = friendsTestModel(connect: { _ in session }, gameSessions: sessions)
        await model.start()
        defer { model.signOut() }
        try await eventually { model.friends?.connection == .connected && !model.library.games.isEmpty }
        let game = GameSessionState.steam(appID: 620, accountID: FriendsWireFixture.accountID, id: "portal")
        sessions.fake.show(game)
        session.chat.ownGame(appId: 620, lobbyId: 1, connect: nil)
        try await eventually { model.chat.ownGame?.invite == .lobby(lobbyId: 1) }
        session.chat.incomingMessage(friendSteamId: friend, message: "Tu viens ?", timestamp: now - 60, ordinal: 0, fromMe: false)
        try await eventually { model.chat.unread[self.friend] == 1 }
        let state = try XCTUnwrap(model.friends)
        let alice = try XCTUnwrap(state.friends.first)
        for scheme in [ColorScheme.dark, .light] {
            let presented = ChatModalPresentation()
            let host = NSHostingView(rootView: AnyView(ChatModalHost(state: state, chat: model.chat, presentation: presented)
                .environment(\.colorScheme, scheme)))
            let window = ChatTestWindow(contentRect: NSRect(x: -20_000, y: -20_000, width: 860, height: 600),
                                        styleMask: .borderless, backing: .buffered, defer: false)
            window.isReleasedWhenClosed = false
            window.sharingType = .none
            window.contentView = host
            window.makeKeyAndOrderFront(nil)
            func settle() async {
                for _ in 0..<12 {
                    await withCheckedContinuation { continuation in DispatchQueue.main.async { continuation.resume() } }
                    host.layoutSubtreeIfNeeded()
                    window.displayIfNeeded()
                }
            }
            func container(_ view: NSView) -> FriendsModalContainer? {
                (view as? FriendsModalContainer) ?? view.subviews.compactMap(container).first
            }
            func render(_ name: String) throws {
                let bitmap = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds))
                host.cacheDisplay(in: host.bounds, to: bitmap)
                try XCTUnwrap(bitmap.representation(using: .png, properties: [:]))
                    .write(to: output.appending(path: "chat-\(name)-\(scheme).png"))
            }
            await settle()
            try render("list")
            try XCTUnwrap(container(host)).focus.conversation = alice.id
            try await eventually { model.chat.conversations[self.friend]?.isLoaded == true }
            session.chat.typing(friendSteamId: friend)
            try await eventually { model.chat.conversations[self.friend]?.isFriendTyping == true }
            try await Task.sleep(for: .milliseconds(400))
            await settle()
            try render("conversation")
            container(host)?.finish()
            window.close()
        }
    }
}

final class SteamChatPresentationTests: XCTestCase {
    func testBBCodeKeepsTextLinksAndInvitationsAndStripsOtherTags() {
        let rendered = SteamBBCode.render(
            "Regarde [url=https://example.com/a]ça[/url] \\[pas une balise] [b]gras[/b] [spoiler]x[/spoiler]"
        )
        guard case .text(let text)? = rendered.first, rendered.count == 1 else { return XCTFail("\(rendered)") }
        XCTAssertEqual(String(text.characters), "Regarde ça [pas une balise] gras x")
        let link = text.runs.first { $0.link != nil }
        XCTAssertEqual(link?.link, URL(string: "https://example.com/a"))
        XCTAssertEqual(link.map { String(text[$0.range].characters) }, "ça")

        XCTAssertEqual(SteamBBCode.render("[lobbyinvite appid=\"620\" lobbyid=\"109775241058543776\"]fallback[/lobbyinvite]"),
                       [.invitation(SteamChatInvitation(kind: .lobby, appId: 620))])
        XCTAssertEqual(SteamBBCode.render("salut [gameinvite appid=730 connect=\"+connect 1\"][/gameinvite] !"),
                       [.text(AttributedString("salut ")), .invitation(SteamChatInvitation(kind: .game, appId: 730)),
                        .text(AttributedString(" !"))])
        XCTAssertEqual(plainText(of: "[emoticon]steamhappy[/emoticon] [sticker type=\"Corgi\" limit=\"0\"][/sticker]"),
                       ":steamhappy: Corgi")
        XCTAssertEqual(plainText(of: "[url]https://store.steampowered.com[/url] [1] [unclosed"),
                       "https://store.steampowered.com [1] [unclosed")
        guard case .text(let unsafe)? = SteamBBCode.render("[url=file:///etc/passwd]x[/url]").first else { return XCTFail() }
        XCTAssertTrue(unsafe.runs.allSatisfy { $0.link == nil }, "Only web links open from a conversation")
        guard case .text(let bare)? = SteamBBCode.render("vu sur https://example.org/b ?").first else { return XCTFail() }
        XCTAssertEqual(bare.runs.compactMap(\.link), [URL(string: "https://example.org/b")!])
    }

    /// The text a message shows, without its invitations.
    private func plainText(of bbcode: String) -> String {
        SteamBBCode.render(bbcode).compactMap {
            if case .text(let text) = $0 { String(text.characters) } else { nil }
        }.joined(separator: " ")
    }

    func testConversationKeepsEachMessageOnceInSteamOrder() {
        var conversation = SteamConversation()
        XCTAssertTrue(conversation.merge([.message(timestamp: 10, ordinal: 1, fromMe: false, bbcode: "b"),
                                          .message(timestamp: 10, ordinal: 0, fromMe: true, bbcode: "a")]))
        XCTAssertFalse(conversation.merge([.message(timestamp: 10, ordinal: 1, fromMe: false, bbcode: "b")]))
        conversation.merge([SteamChatLine(id: .note(1), timestamp: 10, content: .invitationSent(appId: 620)),
                            .message(timestamp: 5, ordinal: 0, fromMe: false, bbcode: "old")])
        XCTAssertEqual(conversation.lines.map(\.id), [.message(timestamp: 5, ordinal: 0), .message(timestamp: 10, ordinal: 0),
                                                      .message(timestamp: 10, ordinal: 1), .note(1)])
        XCTAssertEqual(conversation.lines.first?.contents, [.text(AttributedString("old"))], "Rendered as it is kept")
        XCTAssertEqual(conversation.oldestPosition, SteamChatPosition(timestamp: 5, ordinal: 0))
        XCTAssertEqual(conversation.latestTimestamp, 10)

        conversation.merge([.message(timestamp: 30, ordinal: 0, fromMe: false, bbcode: "after")])
        conversation.replace(with: [.message(timestamp: 10, ordinal: 1, fromMe: false, bbcode: "b"),
                                    .message(timestamp: 20, ordinal: 0, fromMe: false, bbcode: "missed")])
        XCTAssertEqual(conversation.lines.map(\.id), [.message(timestamp: 10, ordinal: 1), .note(1),
                                                      .message(timestamp: 20, ordinal: 0), .message(timestamp: 30, ordinal: 0)])
    }

    func testSeparatorsMarkNewDaysAndLongPauses() {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        let day: UInt32 = 1_760_000_000 - 1_760_000_000 % 86_400
        let lines: [SteamChatLine] = [
            .message(timestamp: day + 100, ordinal: 0, fromMe: false, bbcode: "a"),
            .message(timestamp: day + 200, ordinal: 0, fromMe: true, bbcode: "b"),
            .message(timestamp: day + 200 + 15 * 60, ordinal: 0, fromMe: false, bbcode: "c"),
            .message(timestamp: day + 86_400 + 10, ordinal: 0, fromMe: false, bbcode: "d"),
        ]
        let items = ConversationItem.items(lines, calendar: calendar)
        let separators = items.compactMap { item -> Bool? in
            if case .separator(_, _, let showsDay) = item { showsDay } else { nil }
        }
        XCTAssertEqual(separators, [true, false, true])
        XCTAssertEqual(items.count, 7)
    }
}

/// A Steam session whose friends, chat and own game come from the debug
/// fixtures, recording every chat request Swift makes. Nothing reaches Steam.
final class ChatBridgeTestSession: SteamSessionProtocol, @unchecked Sendable {
    struct Ack: Equatable { let friend: UInt64; let timestamp: UInt32 }
    struct Invite: Equatable { let friend: UInt64; let appId: UInt32 }
    struct ReportedInvite: Equatable { let friend: UInt64; let appId: UInt32; let invite: SteamGameInvite }
    static let friendID: UInt64 = 0x0110000100000001

    let friends: SteamFriendsFixture
    let chat: SteamChatFixture
    /// Holds chat observation between events, so a test can overrun the ring.
    let gate = ChatTestGate()
    /// Holds a message on its way to Steam.
    let sending = ChatTestGate()
    private let lock = NSLock()
    private let games: [SteamGame]
    private var _acks: [Ack] = []
    private var _invites: [Invite] = []
    private var _reportedInvites: [ReportedInvite] = []
    private var _sent: [String] = []
    private var _onlineRequests = 0
    private var _historyRequests = 0
    private var _sessionRequests = 0
    private var _sessions: [SteamChatSession] = []
    private var _history: @Sendable (UInt64, SteamChatPosition?) -> SteamChatHistory = { _, _ in
        SteamChatHistory(messages: [], moreAvailable: false)
    }
    private var _sendFailure: Error?
    private var _inviteFailure: Error?

    var acks: [Ack] { lock.withLock { _acks } }
    var invites: [Invite] { lock.withLock { _invites } }
    /// Invitations into what the game reported itself, which skip the persona.
    var reportedInvites: [ReportedInvite] { lock.withLock { _reportedInvites } }
    var sent: [String] { lock.withLock { _sent } }
    var onlineRequests: Int { lock.withLock { _onlineRequests } }
    var historyRequests: Int { lock.withLock { _historyRequests } }
    var sessionRequests: Int { lock.withLock { _sessionRequests } }
    var sessions: [SteamChatSession] {
        get { lock.withLock { _sessions } }
        set { lock.withLock { _sessions = newValue } }
    }
    var history: @Sendable (UInt64, SteamChatPosition?) -> SteamChatHistory {
        get { lock.withLock { _history } }
        set { lock.withLock { _history = newValue } }
    }
    var sendFailure: Error? {
        get { lock.withLock { _sendFailure } }
        set { lock.withLock { _sendFailure = newValue } }
    }
    var inviteFailure: Error? {
        get { lock.withLock { _inviteFailure } }
        set { lock.withLock { _inviteFailure = newValue } }
    }

    init(games: [SteamGame] = []) {
        self.games = games
        friends = SteamFriendsFixture(accountId: FriendsWireFixture.accountID)
        friends.receive(kind: 767, body: FriendsWireFixture.fullList)
        friends.receive(kind: 766, body: FriendsWireFixture.initialPersona)
        chat = SteamChatFixture(accountId: FriendsWireFixture.accountID)
    }

    func nextFriends(afterRevision: UInt64) async -> SteamFriendsSnapshot {
        await friends.nextFriends(afterRevision: afterRevision)
    }
    func shutdown() {
        friends.disconnect(authenticationRequired: false)
        chat.disconnect()
    }
    func games() async throws -> [SteamGame] { games }
    func profile() async throws -> SteamProfile { SteamProfile(displayName: nil, avatarUrl: nil) }
    func details(appId: UInt32, language: String) async throws -> SteamGameDetails { throw FriendsTestFailure.unexpected }

    func setOnline() async throws { lock.withLock { _onlineRequests += 1 } }
    func nextChat(afterSequence: UInt64) async -> SteamChatEvents {
        await gate.pass()
        return await chat.nextChat(afterSequence: afterSequence)
    }
    func nextOwnGame(afterRevision: UInt64) async -> SteamOwnGame { await chat.nextOwnGame(afterRevision: afterRevision) }
    func activeSessions() async throws -> [SteamChatSession] {
        lock.withLock { _sessionRequests += 1; return _sessions }
    }
    func recentMessages(friendSteamId: UInt64, before: SteamChatPosition?, count: UInt32) async throws -> SteamChatHistory {
        let history = lock.withLock { _historyRequests += 1; return _history }
        return history(friendSteamId, before)
    }
    func sendMessage(friendSteamId: UInt64, text: String) async throws -> SteamSentMessage {
        await sending.pass()
        if let failure = sendFailure { throw failure }
        let count = lock.withLock { _sent.append(text); return _sent.count }
        // Steam stores the text as Rust escaped it.
        return SteamSentMessage(timestamp: 1_000 + UInt32(count), ordinal: 0,
                                message: text.replacingOccurrences(of: "[", with: "\\["), plainMessage: text)
    }
    func ack(friendSteamId: UInt64, timestamp: UInt32) async throws {
        lock.withLock { _acks.append(Ack(friend: friendSteamId, timestamp: timestamp)) }
    }
    func inviteToGame(friendSteamId: UInt64, appId: UInt32) async throws {
        if let failure = inviteFailure { throw failure }
        lock.withLock { _invites.append(Invite(friend: friendSteamId, appId: appId)) }
    }
    func inviteIntoGame(friendSteamId: UInt64, appId: UInt32, invite: SteamGameInvite) async throws {
        if let failure = inviteFailure { throw failure }
        lock.withLock { _reportedInvites.append(ReportedInvite(friend: friendSteamId, appId: appId, invite: invite)) }
    }
    func nextInvites(afterRevision: UInt64) async -> SteamReceivedInvites {
        await chat.nextInvites(afterRevision: afterRevision)
    }
}

actor ChatTestGate {
    private var isOpen = true
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func pass() async {
        if isOpen { return }
        await withCheckedContinuation { waiters.append($0) }
    }

    func close() { isOpen = false }

    func open() {
        isOpen = true
        let pending = waiters
        waiters.removeAll()
        for waiter in pending { waiter.resume() }
    }
}

@MainActor @Observable
private final class ChatModalPresentation {
    var isPresented = true
}

private struct ChatModalHost: View {
    let state: FriendsPresentationState
    let chat: SteamChatModel
    let presentation: ChatModalPresentation
    @State private var avatars = FriendsAvatarCache()

    var body: some View {
        Color.clear.overlay {
            if presentation.isPresented {
                // At the size of the window's sheet.
                FriendsModal(state: state, avatars: avatars, chat: chat, dismiss: { presentation.isPresented = false },
                             retry: {}, signIn: {})
                    .frame(width: 520, height: 550)
            }
        }
    }
}

private final class ChatTestWindow: NSWindow {
    override var canBecomeKey: Bool { true }
}
