import Foundation
import Observation

extension OverlayRequest {
    /// What Obelisk's overlay shows for a Steam game's request to Steam's
    /// overlay, by `account` while playing `appId`. Nil for its invite
    /// dialogs, which the friends list picks for, and for a page it cannot show.
    static func steam(_ kind: SteamOverlayKind, appId: UInt32, account: UInt64?) -> OverlayRequest? {
        switch kind {
        case .dialog(let dialog):
            switch dialog.lowercased() {
            case "friends", "players", "community":
                return .friends
            case let name where name.hasPrefix("chatroomgroup"):
                return .friends
            case "achievements", "stats":
                return .achievements
            case "officialgamegroup":
                return .webPage(GameOverlayController.communityURL(appId: appId))
            default:
                // "settings" and what Obelisk has no page for: the overlay itself.
                return .home
            }
        case .user(let dialog, let steamId):
            switch dialog.lowercased() {
            case "steamid", "chat":
                return .user(steamId: steamId)
            case "achievements", "stats":
                if steamId == account { return .achievements }
                return .webPage(GameOverlayController.achievementsURL(steamId: steamId, appId: appId))
            default:
                // Trading and friend requests happen on the person's profile.
                return .webPage(GameOverlayController.profileURL(steamId: steamId))
            }
        case .webPage(let url, _):
            guard let url = URL(string: url), let scheme = url.scheme?.lowercased(),
                  scheme == "https" || scheme == "http", url.host() != nil else { return nil }
            return .webPage(url)
        case .store(let appId, _):
            // Obelisk has no cart: the page lets the player add it there.
            return .webPage(GameOverlayController.storeURL(appId: appId))
        case .lobbyInvite, .connectInvite:
            return nil
        case .remotePlayTogetherInvite, .remotePlayTogether:
            return .friends
        }
    }
}

/// Follows the latest request a running Steam game's session shows, by its
/// sequence, which only grows within a session.
struct SessionSequence {
    private var followed: (session: String, sequence: UInt64)?

    /// Whether `session`'s request `sequence` is new. A request the session
    /// already showed when first seen is not; a request that went (nil)
    /// keeps its sequence.
    mutating func advance(session: String, sequence: UInt64?) -> Bool {
        let previous = followed?.session == session ? followed : nil
        followed = (session, sequence ?? previous?.sequence ?? 0)
        guard let previous, let sequence else { return false }
        return sequence > previous.sequence
    }
}

/// How the player can join a game a friend invited them into, or why not now.
enum SteamJoinAvailability: Equatable {
    /// The account's running game takes it in.
    case running
    /// The game starts, joining it.
    case launch
    /// No build of the game is installed.
    case notInstalled
    /// Another game runs or starts, or Steam is offline: Play is unavailable too.
    case unavailable

    var canJoin: Bool { self == .running || self == .launch }

    /// Why the player cannot join now.
    var reason: String? {
        switch self {
        case .running, .launch: nil
        case .notInstalled: L10n.string("ui.invites.notInstalled")
        case .unavailable: L10n.string("ui.invites.unavailable")
        }
    }
}

/// Invitations friends sent the account, as its Steam session and its running
/// game's Steam received them: the player joins one, or dismisses it here
/// only. Steam never withdraws one, so as Valve's chat does, an invitation
/// goes after six hours, once its friend plays something else or goes
/// offline, and when that friend sends another.
@MainActor @Observable
final class SteamReceivedInvitations {
    /// An invitation, whenever its friend sent it.
    struct Key: Hashable {
        let friend: UInt64
        let appId: UInt32
        let invite: SteamGameInvite

        init(_ invite: SteamReceivedInvite) {
            friend = invite.friendSteamId
            appId = invite.appId
            self.invite = invite.invite
        }
    }

    /// How long an invitation lasts, as in Valve's chat.
    static let lifetime: TimeInterval = 6 * 60 * 60
    /// How long the same invitation from the same friend counts as one: the
    /// account's session and the running game's Steam both receive it.
    static let repeatWindow: UInt64 = 120

    /// The invitations the player can still join or dismiss, newest first,
    /// at most one per friend.
    private(set) var invites: [SteamReceivedInvite] = []
    /// The invitation being joined.
    private(set) var accepting: Key?
    /// Why joining an invitation failed.
    private(set) var failures: [Key: String] = [:]
    /// The account's running game's session.
    private(set) var session: String?
    /// Why joining a friend's game from the friends list failed, until the
    /// player reads it.
    var friendJoinFailure: String?
    /// Invitations that just showed for the first time, newest first, for
    /// their notice.
    @ObservationIgnored var onReceived: (@MainActor ([SteamReceivedInvite]) -> Void)?
    /// Invitations that no longer show: joined, dismissed, expired or gone
    /// with their account.
    @ObservationIgnored var onGone: (@MainActor ([SteamReceivedInvite]) -> Void)?
    /// Whether the friends list lets a friend still play an app. Valve's
    /// chat drops an invitation once its friend plays another game; the list
    /// may lag behind the invitation, so a friend it does not know yet, or
    /// shows online without a game, still counts.
    @ObservationIgnored var friendMayPlay: (@MainActor (_ friend: UInt64, _ appId: UInt32) -> Bool)?

    @ObservationIgnored private let sessions: SteamGameSessions
    @ObservationIgnored private let installations: SteamInstallations
    @ObservationIgnored private let now: () -> Date
    @ObservationIgnored private var account: UInt64?
    /// Invitations that arrived but have not shown yet.
    @ObservationIgnored private var unannounced: Set<Key> = []
    @ObservationIgnored private var observation: Task<Void, Never>?
    @ObservationIgnored private var expiry: Task<Void, Never>?
    /// When `expiry` shows the list again: its oldest invitation's receipt.
    @ObservationIgnored private var expiryOf: UInt64?
    /// What the account's session received, and the running game's Steam.
    @ObservationIgnored private var received: [SteamReceivedInvite] = []
    @ObservationIgnored private var inGame: [SteamReceivedInvite] = []
    /// When each dismissed or joined invitation was received: sent again, it
    /// shows again.
    @ObservationIgnored private var dismissed: [Key: UInt64] = [:]

    init(sessions: SteamGameSessions, installations: SteamInstallations, now: @escaping () -> Date = Date.init) {
        self.sessions = sessions
        self.installations = installations
        self.now = now
    }

    // MARK: Sources

    /// Follows the invitations `session` of `account` receives; a new
    /// session of the same account keeps the ones already known.
    func connect(_ session: any SteamSessionProtocol, account: UInt64) {
        if self.account != account { reset() }
        self.account = account
        observation?.cancel()
        observation = Task { [weak self] in
            var revision: UInt64 = 0
            while !Task.isCancelled {
                let snapshot = await session.nextInvites(afterRevision: revision)
                guard let self, !Task.isCancelled else { return }
                revision = snapshot.revision
                self.receive(snapshot.invites)
                guard snapshot.connected else { return }
            }
        }
    }

    /// The session ended; its replacement keeps the invitations.
    func suspend() {
        observation?.cancel()
        observation = nil
    }

    /// Forgets every invitation, at sign-out or account change.
    func reset() {
        suspend()
        account = nil
        received = []
        dismissed = [:]
        unannounced = []
        if !failures.isEmpty { failures = [:] }
        if friendJoinFailure != nil { friendJoinFailure = nil }
        leaveSession()
        show()
    }

    private func receive(_ invites: [SteamReceivedInvite]) {
        let known = Dictionary((received + inGame).map { (Key($0), $0.receivedAt) }, uniquingKeysWith: max)
        let arrived = invites.filter { invite in
            known[Key(invite)].map { $0 + Self.repeatWindow < invite.receivedAt } ?? true
        }
        // The session's list keeps one per friend; one that left it stays.
        received = Self.newestPerFriend(invites + received)
        arrive(arrived)
        show()
    }

    /// Follows the running game's session `id` and the invitations its Steam
    /// received. Those it did not show before are told of, except when the
    /// session is first seen, such as one recovered.
    func follow(session id: String?, invites: [SteamReceivedInvite]) {
        guard let id else {
            // Called on every change of the Game sessions, a runtime's
            // progress included: nothing changes while no game ran.
            guard session != nil else { return }
            leaveSession()
            show()
            return
        }
        let firstSight = session != id
        let known = Set((inGame + received).map(Key.init))
        if firstSight {
            session = id
            if accepting != nil { accepting = nil }
        }
        inGame = invites
        if !firstSight { arrive(invites.filter { !known.contains(Key($0)) }) }
        show()
    }

    private func leaveSession() {
        if session != nil { session = nil }
        inGame = []
        if accepting != nil { accepting = nil }
    }

    /// The friends list changed: an invitation whose friend left its game goes.
    func friendsChanged() { show() }

    /// Invitations to tell of once they show; an old failure no longer holds.
    private func arrive(_ invites: [SteamReceivedInvite]) {
        for invite in invites {
            let key = Key(invite)
            unannounced.insert(key)
            if failures[key] != nil { failures[key] = nil }
        }
    }

    // MARK: The player's actions

    /// Hides `invite` until its friend sends it again. A chat card's copy of
    /// an invitation in the list hides that one too.
    func dismiss(_ invite: SteamReceivedInvite) {
        let key = Key(invite)
        let listed = (received + inGame).filter { Key($0) == key }.map(\.receivedAt)
        dismissed[key] = ([dismissed[key] ?? 0, invite.receivedAt] + listed).max()
        if failures[key] != nil { failures[key] = nil }
        show()
    }

    /// How the player joins a friend's game `appId` now.
    func availability(appId: UInt32) -> SteamJoinAvailability {
        if let session, let active = sessions.active, active.id == session, active.running, active.game == appId {
            return .running
        }
        guard installations.isConnected else { return .unavailable }
        guard installations.receiptToLaunch(for: appId, preferring: .mac) != nil else { return .notInstalled }
        return installations.canLaunch(appId) ? .launch : .unavailable
    }

    /// Whether `invite`, such as one a chat card shows, can still be joined.
    func isCurrent(_ invite: SteamReceivedInvite) -> Bool {
        now().timeIntervalSince1970 - TimeInterval(invite.receivedAt) < Self.lifetime
            && friendMayPlay?(invite.friendSteamId, invite.appId) ?? true
    }

    /// The listed invitation that `invite`, such as one a notice carries,
    /// names: nil once it went.
    func listed(_ invite: SteamReceivedInvite) -> SteamReceivedInvite? {
        invites.first { Key($0) == Key(invite) }
    }

    /// Joins `invite` while it lasts: the running game takes it in, or its
    /// game starts joining it. True once the game is told or starts.
    func accept(_ invite: SteamReceivedInvite) async -> Bool {
        guard isCurrent(invite) else { return false }
        return await join(invite)
    }

    /// Joins the game `friend` plays, as Steam's "Join Game" does: the
    /// invitation they sent into it, else `target`, the lobby or `connect`
    /// string Steam shows of them.
    func joinFriend(_ friend: UInt64, target: SteamJoinTarget) {
        guard accepting == nil, availability(appId: target.appId).canJoin else { return }
        friendJoinFailure = nil
        let invite = invites.first { $0.friendSteamId == friend && $0.appId == target.appId }
            ?? SteamReceivedInvite(friendSteamId: friend, appId: target.appId, invite: target.invite,
                                   receivedAt: Self.seconds(now()))
        Task { [weak self] in
            guard let self, !(await self.join(invite)) else { return }
            self.friendJoinFailure = self.failure(joining: invite)
        }
    }

    /// The invitation `friend` sent that the player can still join, if any.
    func invite(from friend: UInt64) -> SteamReceivedInvite? {
        invites.first { $0.friendSteamId == friend }
    }

    private func join(_ invite: SteamReceivedInvite) async -> Bool {
        guard accepting == nil else { return false }
        let key = Key(invite)
        switch availability(appId: invite.appId) {
        case .running:
            guard let session else { return false }
            accepting = key
            if failures[key] != nil { failures[key] = nil }
            do {
                try await sessions.acceptInvite(sessionId: session, friendSteamId: invite.friendSteamId, invite: invite.invite)
                guard self.session == session else { return false }
                accepting = nil
                dismiss(invite)
                return true
            } catch {
                guard self.session == session else { return false }
                accepting = nil
                failures[key] = SteamModel.message(error)
                return false
            }
        case .launch:
            guard let receipt = installations.receiptToLaunch(for: invite.appId, preferring: .mac) else {
                return false
            }
            if failures[key] != nil { failures[key] = nil }
            installations.join(receipt, invite: invite.invite)
            // Rust starts the launch at once, or refuses it.
            guard installations.isLaunching, installations.launchGame == invite.appId else {
                failures[key] = L10n.string("ui.invites.unavailable")
                return false
            }
            dismiss(invite)
            return true
        case .notInstalled, .unavailable:
            return false
        }
    }

    private func failure(joining invite: SteamReceivedInvite) -> String {
        failures[Key(invite)] ?? availability(appId: invite.appId).reason ?? L10n.string("backend.steam.join.unavailable")
    }

    // MARK: What shows

    private func show() {
        let previous = invites
        let all = received + inGame
        let visible = Self.newestPerFriend(all).filter { invite in
            dismissed[Key(invite)].map { $0 < invite.receivedAt } ?? true
        }
        .filter(isCurrent)
        if invites != visible { invites = visible }
        scheduleExpiry()
        let shown = Set(visible.map(Key.init))
        let gone = previous.filter { !shown.contains(Key($0)) }
        if !gone.isEmpty { onGone?(gone) }
        // An invitation tells of itself once it shows, even if the friends
        // list caught up with its friend's game only later.
        let announced = visible.filter { unannounced.contains(Key($0)) }
        unannounced = unannounced.subtracting(announced.map(Key.init)).intersection(all.map(Key.init))
        if !announced.isEmpty { onReceived?(announced) }
    }

    /// Shows the list again as its oldest invitation expires.
    private func scheduleExpiry() {
        let oldest = invites.map(\.receivedAt).min()
        guard oldest != expiryOf else { return }
        expiryOf = oldest
        expiry?.cancel()
        guard let oldest else { expiry = nil; return }
        let delay = TimeInterval(oldest) + Self.lifetime - now().timeIntervalSince1970
        expiry = Task { [weak self] in
            try? await Task.sleep(for: .seconds(max(delay, 0) + 1))
            guard let self, !Task.isCancelled else { return }
            self.expiryOf = nil
            self.show()
        }
    }

    /// The newest invitation of each friend, newest first; the first of two
    /// received at once.
    private static func newestPerFriend(_ invites: [SteamReceivedInvite]) -> [SteamReceivedInvite] {
        var newest: [UInt64: SteamReceivedInvite] = [:]
        for invite in invites {
            if let known = newest[invite.friendSteamId], known.receivedAt >= invite.receivedAt { continue }
            newest[invite.friendSteamId] = invite
        }
        return newest.values.sorted { ($0.receivedAt, $0.friendSteamId) > ($1.receivedAt, $1.friendSteamId) }
    }

    private static func seconds(_ date: Date) -> UInt64 {
        UInt64(max(date.timeIntervalSince1970, 0))
    }
}

/// The in-game purchase the account's running Steam game asks Steam to
/// authorize, until the player answers it.
@MainActor @Observable
final class SteamPurchaseAuthorization {
    /// The purchase waiting on the player.
    private(set) var pending: SteamMicroTxn?
    private(set) var isAnswering = false
    /// Why the last answer failed.
    private(set) var error: String?
    /// The running game's session.
    private(set) var session: String?

    @ObservationIgnored private let sessions: SteamGameSessions

    init(sessions: SteamGameSessions) {
        self.sessions = sessions
    }

    /// Follows the running game's session `id` and the purchase it waits on.
    /// True when another purchase starts waiting, except when the session is
    /// first seen, such as one recovered: opening the overlay shows it then.
    @discardableResult
    func follow(session id: String?, microTxn: SteamMicroTxn?) -> Bool {
        let firstSight = session != id
        if firstSight {
            session = id
            if isAnswering { isAnswering = false }
        }
        let current = id == nil ? nil : microTxn
        guard current?.orderId != pending?.orderId || firstSight else { return false }
        pending = current
        if error != nil { error = nil }
        return !firstSight && current != nil
    }

    /// The player's answer; true once the game is told.
    func answer(approve: Bool) async -> Bool {
        guard let session, let pending, !isAnswering else { return false }
        isAnswering = true
        error = nil
        defer { if self.session == session { isAnswering = false } }
        do {
            try await sessions.authorizeMicroTxn(sessionId: session, orderId: pending.orderId, approve: approve)
            return self.session == session
        } catch {
            if self.session == session && self.pending?.orderId == pending.orderId { self.error = SteamModel.message(error) }
            return false
        }
    }
}
