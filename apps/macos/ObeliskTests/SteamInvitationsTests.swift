import XCTest
@testable import Obelisk

/// Friends' invitations the account's Steam session receives, and joining
/// them or a friend's game.
@MainActor
final class SteamInvitationsTests: XCTestCase {
    private static let peak = SteamGame(appId: 3_527_290, name: "PEAK", iconHash: "", lastPlayed: 0, playtimeMinutes: 0)
    private static let lobby: UInt64 = 109_775_244_421_126_053
    private static let alice = ChatBridgeTestSession.friendID
    private static let tag = "[lobbyinvite appid=\"3527290\" lobbyid=\"109775244421126053\"][/lobbyinvite]"

    func testAFriendsInvitationIsToldOnceAndJoinsByStartingTheInstalledGame() async throws {
        let receipt = InstallationReceipt.steam(appId: Self.peak.appId, name: "PEAK", directory: "/tmp/peak",
                                                platform: .windows)
        let (model, session, backend, notifier) = try await signedIn(receipts: [receipt])
        defer { model.signOut() }

        // Steam writes it into the chat and sends it apart: one invitation.
        let now = UInt32(Date().timeIntervalSince1970)
        session.chat.incomingInvitation(friendSteamId: Self.alice, message: Self.tag, timestamp: now, ordinal: 1)
        session.chat.lobbyInvite(friendSteamId: Self.alice, lobbyId: Self.lobby, appId: Self.peak.appId)
        try await eventually { notifier.notices.count == 1 }
        await drainMainQueue(times: 3)
        XCTAssertEqual(notifier.notices.count, 1)
        let notice = try XCTUnwrap(notifier.notices.first)
        XCTAssertEqual(notice.friend, "Alice")
        XCTAssertEqual(notice.game, "PEAK")
        XCTAssertEqual(notice.invite.invite, .lobby(lobbyId: Self.lobby))
        XCTAssertEqual(model.invitations.invites.map(\.friendSteamId), [Self.alice])
        XCTAssertEqual(model.invitations.availability(appId: Self.peak.appId), .launch)

        // Join from the notice: the game starts joining the lobby, and the
        // invitation goes.
        notifier.answer?(.join, notice.invite)
        try await eventually { backend.actions.contains(.join(receipt, .lobby(lobbyId: Self.lobby))) }
        try await eventually { model.invitations.invites.isEmpty }
        XCTAssertEqual(notifier.withdrawn.map(\.friendSteamId), [Self.alice])

        // Its notice, clicked again once the launch ended, joins nothing.
        backend.publish {
            $0.launching = false
            $0.launchGame = nil
            $0.gate = .open
        }
        notifier.answer?(.join, notice.invite)
        await drainMainQueue(times: 3)
        XCTAssertEqual(backend.actions.filter { if case .join = $0 { true } else { false } }.count, 1)
    }

    func testAnInvitationIntoAGameNotInstalledOrWhilePlayIsUnavailableWaits() async throws {
        let (model, session, backend, notifier) = try await signedIn(receipts: [])
        defer { model.signOut() }
        session.chat.connectInvite(friendSteamId: Self.alice, appId: Self.peak.appId, connect: "+connect 10.0.0.1")
        try await eventually { model.invitations.invites.count == 1 }
        XCTAssertEqual(notifier.notices.count, 1)
        let invite = model.invitations.invites[0]
        XCTAssertEqual(model.invitations.availability(appId: Self.peak.appId), .notInstalled)
        let joined = await model.invitations.accept(invite)
        XCTAssertFalse(joined)
        XCTAssertFalse(backend.actions.contains { if case .join = $0 { true } else { false } })

        // Installed, but another launch holds Play.
        let receipt = InstallationReceipt.steam(appId: Self.peak.appId, name: "PEAK", directory: "/tmp/peak",
                                                platform: .windows)
        backend.publish {
            $0.receipts = [receipt]
            $0.gate = .action(620)
        }
        try await eventually { model.installations.receipts == [receipt] }
        XCTAssertEqual(model.invitations.availability(appId: Self.peak.appId), .unavailable)
        XCTAssertNotNil(SteamJoinAvailability.unavailable.reason)
        backend.publish { $0.gate = .open }
        try await eventually { model.invitations.availability(appId: Self.peak.appId) == .launch }

        // Dismissed, it stays hidden until the friend sends another, and its
        // notice goes.
        model.invitations.dismiss(invite)
        XCTAssertTrue(model.invitations.invites.isEmpty)
        XCTAssertEqual(notifier.withdrawn, [invite])
    }

    func testAFriendsGameJoinsFromTheLobbySteamPushesOfThem() async throws {
        let receipt = InstallationReceipt.steam(appId: Self.peak.appId, name: "PEAK", directory: "/tmp/peak",
                                                platform: .windows)
        let (model, session, backend, _) = try await signedIn(receipts: [receipt])
        defer { model.signOut() }
        func alice() -> FriendPresentation? { model.friends?.friends.first { $0.steamID == Self.alice } }
        XCTAssertNil(alice()?.steamJoin, "Online without a game")

        // Steam pushes Alice in PEAK's lobby: her row's menu can join it.
        session.friends.friendPlaying(friendSteamId: Self.alice, appId: Self.peak.appId, lobbyId: Self.lobby,
                                      connect: nil)
        try await eventually { alice()?.steamJoin != nil }
        let target = try XCTUnwrap(alice()?.steamJoin)
        XCTAssertEqual(target, SteamJoinTarget(appId: Self.peak.appId, invite: .lobby(lobbyId: Self.lobby)))
        model.invitations.joinFriend(Self.alice, target: target)
        try await eventually { backend.actions.contains(.join(receipt, .lobby(lobbyId: Self.lobby))) }
        XCTAssertNil(model.invitations.friendJoinFailure)

        // A launch Rust refuses says why.
        backend.publish {
            $0.launching = false
            $0.launchGame = nil
            $0.gate = .open
        }
        backend.onCall = { _ in }
        try await eventually { model.invitations.availability(appId: Self.peak.appId) == .launch }
        model.invitations.joinFriend(Self.alice, target: target)
        try await eventually { model.invitations.friendJoinFailure != nil }
        XCTAssertEqual(model.invitations.friendJoinFailure, L10n.string("ui.invites.unavailable"))

        // A game without lobby or connect string offers nothing to join.
        session.friends.friendPlaying(friendSteamId: Self.alice, appId: Self.peak.appId, lobbyId: 0, connect: nil)
        try await eventually { alice()?.steamJoin == nil }
    }

    func testAnInvitationGoesAfterSixHoursOrOnceItsFriendPlaysAnotherGame() async throws {
        let sessions = SteamGameSessions()
        let installations = SteamInstallations(sessions: sessions, cloud: SteamCloudModel(sessions: sessions))
        var clock = Date(timeIntervalSince1970: 2_000_000_000)
        let invitations = SteamReceivedInvitations(sessions: sessions, installations: installations, now: { clock })
        // nil: online without a game.
        var playing: UInt32?
        invitations.friendMayPlay = { _, appId in playing.map { $0 == appId } ?? true }
        let invite = SteamReceivedInvite(friendSteamId: Self.alice, appId: Self.peak.appId,
                                         invite: .lobby(lobbyId: Self.lobby), receivedAt: 2_000_000_000)
        invitations.follow(session: "peak", invites: [invite])
        XCTAssertEqual(invitations.invites, [invite], "A friend the list shows without a game may still invite")

        playing = 620
        invitations.friendsChanged()
        XCTAssertTrue(invitations.invites.isEmpty, "Its friend plays another game")
        XCTAssertFalse(invitations.isCurrent(invite))
        playing = Self.peak.appId
        invitations.friendsChanged()
        XCTAssertEqual(invitations.invites, [invite])

        clock = clock.addingTimeInterval(SteamReceivedInvitations.lifetime + 1)
        invitations.friendsChanged()
        XCTAssertTrue(invitations.invites.isEmpty, "Six hours later")
    }

    func testAChatMessageCarryingAnInvitationNamesItsLobby() {
        var conversation = SteamConversation()
        conversation.merge([.message(timestamp: 10, ordinal: 1, fromMe: false, bbcode: Self.tag),
                            .message(timestamp: 11, ordinal: 1, fromMe: false, bbcode: "hello")])
        XCTAssertEqual(conversation.lines[0].joinTarget,
                       SteamJoinTarget(appId: Self.peak.appId, invite: .lobby(lobbyId: Self.lobby)))
        XCTAssertNil(conversation.lines[1].joinTarget)
    }

    // MARK: Fixture

    private func signedIn(receipts: [InstallationReceipt<UInt32>]) async throws
        -> (SteamModel, ChatBridgeTestSession, FakeInstallations<SteamGames>, FakeInviteNotifier) {
        let session = ChatBridgeTestSession(games: [Self.peak])
        let backend = FakeInstallations<SteamGames>(receipts: receipts)
        // Rust starts a launch at once.
        backend.onCall = { [weak backend] call in
            guard case let .join(receipt, _) = call else { return }
            backend?.publish {
                $0.launching = true
                $0.launchGame = receipt.gameID
                $0.gate = .action(receipt.gameID)
            }
        }
        let notifier = FakeInviteNotifier()
        let model = friendsTestModel(connect: { _ in session }, installations: SteamModel.installations(backend),
                                     gameSessions: SteamGameSessions())
        model.notifier = notifier
        // The app routes the notices' answers to the account they are for.
        notifier.answer = { [weak model] in model?.answerNotice($0, $1) }
        await model.start()
        try await eventually { model.friends?.connection == .connected && !model.library.games.isEmpty }
        return (model, session, backend, notifier)
    }
}

@MainActor
final class FakeInviteNotifier: SteamInviteNotifying {
    struct Notice: Equatable {
        let invite: SteamReceivedInvite
        let friend: String
        let game: String?
    }

    private(set) var notices: [Notice] = []
    var answer: (@MainActor (SteamInviteAnswer, SteamReceivedInvite) -> Void)?

    private(set) var withdrawn: [SteamReceivedInvite] = []

    func notify(_ invite: SteamReceivedInvite, friend: String, game: String?) {
        notices.append(Notice(invite: invite, friend: friend, game: game))
    }

    func withdraw(_ invites: [SteamReceivedInvite]) {
        withdrawn += invites
    }
}
