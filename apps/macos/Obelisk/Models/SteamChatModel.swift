import AppKit
import Foundation
import Observation

/// One line of a conversation: a message Steam stored, or a note Obelisk
/// adds locally, such as an invitation it sent.
struct SteamChatLine: Identifiable, Equatable, Sendable {
    enum ID: Hashable, Sendable {
        /// Steam identifies and orders messages by both.
        case message(timestamp: UInt32, ordinal: UInt32)
        case note(Int)
    }

    enum Content: Equatable, Sendable {
        case message(fromMe: Bool, bbcode: String)
        case invitationSent(appId: UInt32)
    }

    let id: ID
    /// Steam's clock, in seconds since 1970.
    let timestamp: UInt32
    let content: Content
    /// A message's BBCode as it shows, rendered once its conversation keeps it.
    fileprivate(set) var contents: [SteamChatContent] = []
    /// The game and lobby or `connect` string of the invitation it carries,
    /// read as Steam reads it.
    fileprivate(set) var joinTarget: SteamJoinTarget?

    init(id: ID, timestamp: UInt32, content: Content) {
        self.id = id
        self.timestamp = timestamp
        self.content = content
    }

    var date: Date { Date(timeIntervalSince1970: TimeInterval(timestamp)) }

    /// The games the line names.
    var appIds: [UInt32] {
        switch content {
        case .message: contents.compactMap { if case .invitation(let invitation) = $0 { invitation.appId } else { nil } }
        case .invitationSent(let appId): [appId]
        }
    }

    var fromMe: Bool {
        if case .message(let fromMe, _) = content { fromMe } else { true }
    }

    var position: SteamChatPosition? {
        if case .message(let timestamp, let ordinal) = id { SteamChatPosition(timestamp: timestamp, ordinal: ordinal) } else { nil }
    }

    /// A note follows every message of its second.
    fileprivate var order: (UInt32, UInt32, Int) {
        switch id {
        case .message(let timestamp, let ordinal): (timestamp, ordinal, 0)
        case .note(let number): (timestamp, .max, number)
        }
    }

    static func message(timestamp: UInt32, ordinal: UInt32, fromMe: Bool, bbcode: String) -> Self {
        SteamChatLine(id: .message(timestamp: timestamp, ordinal: ordinal), timestamp: timestamp,
                      content: .message(fromMe: fromMe, bbcode: bbcode))
    }
}

struct SteamConversation: Equatable, Sendable {
    /// Oldest first, each message once.
    private(set) var lines: [SteamChatLine] = []
    /// The latest page of history arrived; events alone may precede it.
    var isLoaded = false
    var isLoading = false
    var hasOlder = false
    var isLoadingOlder = false
    var isSending = false
    var isInviting = false
    var isFriendTyping = false
    var error: String?

    /// Whether any line was new. The same message can arrive as an event, in
    /// a history page and as the answer to sending it.
    @discardableResult
    mutating func merge(_ added: [SteamChatLine]) -> Bool {
        let known = Set(lines.map(\.id))
        let new = added.filter { !known.contains($0.id) }.map { line in
            var line = line
            if case .message(_, let bbcode) = line.content {
                line.contents = SteamBBCode.render(bbcode)
                if line.contents.contains(where: { if case .invitation = $0 { true } else { false } }) {
                    line.joinTarget = steamChatInvite(bbcode: bbcode)
                }
            }
            return line
        }
        guard !new.isEmpty else { return false }
        lines = (lines + new).sorted { $0.order < $1.order }
        return true
    }

    /// Keeps `page`, the local notes from its first message on and the
    /// messages that arrived after it.
    mutating func replace(with page: [SteamChatLine]) {
        let start = page.first?.timestamp ?? 0
        let end = page.last?.order ?? (0, 0, 0)
        lines = lines.filter {
            if case .note = $0.id { $0.timestamp >= start } else { $0.order > end }
        }
        merge(page)
    }

    var oldestPosition: SteamChatPosition? { lines.lazy.compactMap(\.position).first }
    var latestTimestamp: UInt32? { lines.last { $0.position != nil }?.timestamp }
}

/// What the account can invite a friend into right now.
struct SteamInviteOffer: Equatable, Sendable {
    let appId: UInt32
    let gameName: String?
    /// The lobby or connect string the running game named itself, as its own
    /// Invite button does. Without it, the account's Steam persona decides.
    var invite: SteamGameInvite? = nil
    /// The game's invite request that opened this picker; 0 outside one.
    /// Every press of the game's button is another picker.
    var request: UInt64 = 0
}

/// The Steam account's 1:1 chat and game invitations, through the UI session.
/// It follows the account's session across reconnections; signing out
/// forgets every conversation.
@MainActor @Observable
final class SteamChatModel {
    var requestedFriend: UInt64?
    @ObservationIgnored var onIncomingMessage: ((UInt64) -> Void)?
    private(set) var conversations: [UInt64: SteamConversation] = [:]
    /// What the account is writing to each friend, apart from the
    /// conversations so that typing redraws none of their messages.
    private(set) var drafts: [UInt64: String] = [:]
    /// Unread messages per friend, as Steam counts them.
    private(set) var unread: [UInt64: Int] = [:]
    private(set) var recentActivity: [UInt64: UInt32] = [:]

    func lastActivity(for friend: UInt64) -> UInt32? {
        let timestamp = max(recentActivity[friend] ?? 0, conversations[friend]?.latestTimestamp ?? 0)
        return timestamp > 0 ? timestamp : nil
    }

    func preview(for friend: UInt64) -> String? {
        guard let line = conversations[friend]?.lines.last(where: { $0.position != nil }) else { return nil }
        return line.contents.compactMap { content in
            if case .text(let text) = content { return String(text.characters) }
            return nil
        }.joined(separator: " ").replacingOccurrences(of: "\n", with: " ")
    }

    func loadPreview(for friend: UInt64) {
        guard conversations[friend]?.isLoaded != true else { return }
        loadLatest(friend, replacing: false)
    }
    private(set) var ownGame: SteamOwnGame?
    private(set) var isConnected = false
    /// The conversation on screen; read as messages arrive while Obelisk is active.
    private(set) var visibleFriend: UInt64?
    private(set) var isApplicationActive: Bool
    /// A nonactivating overlay can receive chat input while the game remains
    /// the active application.
    var isOverlayPresented = false {
        didSet {
            if isOverlayPresented, let visibleFriend { acknowledgeIfReading(visibleFriend) }
        }
    }
    var totalUnread: Int { unread.values.reduce(0, +) }
    /// The lobby the account's running Steam game is in now, as the engine
    /// hosting it shows it: Steam may never show this session the game's
    /// lobby, a private one above all. The engine also tells the lobby the
    /// game's invite dialog named, while its matchmaking shows the account in
    /// it. It goes as soon as the game leaves it.
    private(set) var gameLobby: (appId: UInt32, lobbyId: UInt64)?
    /// The game asked to invite friends: the friends list picks them, into
    /// the target the game named then. Nothing else offers that target.
    private(set) var invitePicker: SteamInviteOffer?
    /// Friends invited since the picker opened.
    private(set) var pickerInvited: Set<UInt64> = []
    /// Why inviting a friend from the picker failed, since it opened.
    private(set) var pickerFailures: [UInt64: String] = [:]

    /// Names the account's game, from its library or a friend playing it.
    @ObservationIgnored var gameName: @MainActor (UInt32) -> String? = { _ in nil }
    /// The invitations friends sent, which a conversation and a friend's row join.
    @ObservationIgnored var invitations: SteamReceivedInvitations?
    @ObservationIgnored private var session: (any SteamSessionProtocol)?
    @ObservationIgnored private var account: UInt64?
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var observations: [Task<Void, Never>] = []
    @ObservationIgnored private var typingExpiries: [UInt64: Task<Void, Never>] = [:]
    /// The latest message each conversation is known read up to.
    @ObservationIgnored private var acknowledged: [UInt64: UInt32] = [:]
    @ObservationIgnored private var notes = 0
    @ObservationIgnored private let typingTimeout: Duration
    @ObservationIgnored private var activity: ApplicationActivityObservation?
    /// The running game's session and the last invite request it showed.
    @ObservationIgnored private var followedRequest = SessionSequence()

    /// Tests tell activity themselves instead of following the application.
    init(typingTimeout: Duration = .seconds(11), isApplicationActive: Bool? = nil) {
        self.typingTimeout = typingTimeout
        self.isApplicationActive = isApplicationActive ?? NSApplication.shared.isActive
        guard isApplicationActive == nil else { return }
        activity = ApplicationActivityObservation { [weak self] active in
            Task { @MainActor [weak self] in self?.applicationActivityChanged(active) }
        }
    }

    /// Follows `session`. A new session of the same account keeps the
    /// conversations and reloads what they may have missed meanwhile.
    func connect(_ session: any SteamSessionProtocol, account: UInt64) {
        if self.account != account { reset() }
        stopObserving()
        self.account = account
        self.session = session
        isConnected = true
        let current = generation
        observations = [observeChat(session, generation: current), observeOwnGame(session, generation: current)]
        reload(session, generation: current)
    }

    /// The session ended; its replacement keeps the conversations.
    func suspend() {
        stopObserving()
        session = nil
        isConnected = false
        ownGame = nil
    }

    /// Forgets the account's conversations, at sign-out or account change.
    func reset() {
        suspend()
        followGame(session: nil, appId: 0, request: nil, lobby: nil)
        account = nil
        conversations = [:]
        drafts = [:]
        unread = [:]
        recentActivity = [:]
        visibleFriend = nil
        requestedFriend = nil
        acknowledged = [:]
    }

    /// Requests in flight end with their generation, and their flags with it.
    private func stopObserving() {
        generation += 1
        observations.forEach { $0.cancel() }
        observations = []
        typingExpiries.values.forEach { $0.cancel() }
        typingExpiries = [:]
        for (friend, conversation) in conversations {
            var stopped = conversation
            stopped.isFriendTyping = false
            stopped.isLoading = false
            stopped.isLoadingOlder = false
            stopped.isSending = false
            stopped.isInviting = false
            if stopped != conversation { conversations[friend] = stopped }
        }
    }

    /// Shows the account Online again, as a game's own Steam session can leave
    /// its presence behind when it ends.
    func setOnline() {
        guard let session else { return }
        Task { try? await session.setOnline() }
    }

    // MARK: Observation

    private func observeChat(_ session: any SteamSessionProtocol, generation current: Int) -> Task<Void, Never> {
        Task { [weak self] in
            var cursor: UInt64 = 0
            while !Task.isCancelled {
                let batch = await session.nextChat(afterSequence: cursor)
                guard let self, current == self.generation, !Task.isCancelled else { return }
                cursor = batch.lastSequence
                // Events past a gap are partial: Steam's own counts replace them.
                for entry in batch.events { self.apply(entry.event, counting: !batch.gap) }
                if batch.gap { self.reload(session, generation: current) }
                guard batch.connected else { return }
            }
        }
    }

    private func observeOwnGame(_ session: any SteamSessionProtocol, generation current: Int) -> Task<Void, Never> {
        Task { [weak self] in
            var revision: UInt64 = 0
            while !Task.isCancelled {
                let game = await session.nextOwnGame(afterRevision: revision)
                guard let self, current == self.generation, !Task.isCancelled else { return }
                revision = game.revision
                self.ownGame = game.connected ? game : nil
                guard game.connected else { return }
            }
        }
    }

    private func apply(_ event: SteamChatEvent, counting: Bool) {
        switch event {
        case .message(let message):
            let friend = message.friendSteamId
            var conversation = conversations[friend] ?? SteamConversation()
            let added = conversation.merge([.message(timestamp: message.timestamp, ordinal: message.ordinal,
                                                     fromMe: message.fromMe, bbcode: message.message)])
            if !message.fromMe {
                conversation.isFriendTyping = false
                typingExpiries.removeValue(forKey: friend)?.cancel()
            }
            conversations[friend] = conversation
            guard added, !message.fromMe else { return }
            if isReading(friend) { acknowledgeIfReading(friend) }
            else if counting {
                unread[friend, default: 0] += 1
                onIncomingMessage?(friend)
            }
        case .typing(let friend):
            conversations[friend, default: SteamConversation()].isFriendTyping = true
            typingExpiries[friend]?.cancel()
            let timeout = typingTimeout
            let current = generation
            typingExpiries[friend] = Task { [weak self] in
                try? await Task.sleep(for: timeout)
                guard let self, !Task.isCancelled, current == self.generation else { return }
                self.conversations[friend]?.isFriendTyping = false
                self.typingExpiries[friend] = nil
            }
        case .read(let friend, let timestamp):
            acknowledged[friend] = max(acknowledged[friend] ?? 0, timestamp)
            let later = conversations[friend]?.lines.filter { !$0.fromMe && $0.position != nil && $0.timestamp > timestamp }.count ?? 0
            unread[friend] = later > 0 ? later : nil
        }
    }

    /// Steam's unread counts, and the latest page of every open conversation.
    private func reload(_ session: any SteamSessionProtocol, generation current: Int) {
        Task { [weak self] in
            guard let sessions = try? await session.activeSessions() else { return }
            guard let self, current == self.generation else { return }
            var counts: [UInt64: Int] = [:]
            for entry in sessions {
                recentActivity[entry.friendSteamId] = max(recentActivity[entry.friendSteamId] ?? 0, entry.lastMessage)
                acknowledged[entry.friendSteamId] = max(acknowledged[entry.friendSteamId] ?? 0, entry.lastView)
                if entry.unreadMessageCount > 0 { counts[entry.friendSteamId] = Int(entry.unreadMessageCount) }
            }
            unread = counts
            // Metadata restores conversation ordering across launches. Fetch
            // recent previews without marking those conversations as read.
            for entry in sessions.sorted(by: { $0.lastMessage > $1.lastMessage }).prefix(20)
                where conversations[entry.friendSteamId]?.isLoaded != true {
                loadLatest(entry.friendSteamId, replacing: false)
            }
            if let visibleFriend { acknowledgeIfReading(visibleFriend) }
        }
        for (friend, conversation) in conversations where conversation.isLoaded {
            loadLatest(friend, replacing: true)
        }
        // Opened while disconnected, or its first page failed.
        if let visibleFriend, conversations[visibleFriend]?.isLoaded != true {
            conversations[visibleFriend]?.error = nil
            loadLatest(visibleFriend, replacing: false)
        }
    }

    // MARK: Conversations

    func show(_ friend: UInt64) {
        visibleFriend = friend
        if conversations[friend]?.isLoaded != true { loadLatest(friend, replacing: false) }
        acknowledgeIfReading(friend)
    }

    func hide(_ friend: UInt64) {
        if visibleFriend == friend { visibleFriend = nil }
    }

    func applicationActivityChanged(_ active: Bool) {
        isApplicationActive = active
        if active, let visibleFriend { acknowledgeIfReading(visibleFriend) }
    }

    private func isReading(_ friend: UInt64) -> Bool {
        visibleFriend == friend && (isApplicationActive || isOverlayPresented)
    }

    private func acknowledgeIfReading(_ friend: UInt64) {
        guard isReading(friend), let session else { return }
        unread[friend] = nil
        guard let latest = conversations[friend]?.latestTimestamp, latest > acknowledged[friend] ?? 0 else { return }
        acknowledged[friend] = latest
        Task { try? await session.ack(friendSteamId: friend, timestamp: latest) }
    }

    /// The latest page. Replacing it drops older pages, which may no longer
    /// join the new one after missed events.
    private func loadLatest(_ friend: UInt64, replacing: Bool) {
        guard let session, conversations[friend]?.isLoading != true else { return }
        conversations[friend, default: SteamConversation()].isLoading = true
        let current = generation
        Task { [weak self] in
            do {
                let history = try await session.recentMessages(friendSteamId: friend, before: nil, count: 50)
                guard let self, current == self.generation else { return }
                var conversation = self.conversations[friend] ?? SteamConversation()
                let page = history.messages.map(Self.line)
                if replacing { conversation.replace(with: page) } else { conversation.merge(page) }
                if replacing || !conversation.isLoaded { conversation.hasOlder = history.moreAvailable }
                conversation.isLoaded = true
                conversation.isLoading = false
                self.conversations[friend] = conversation
                self.acknowledgeIfReading(friend)
            } catch {
                guard let self, current == self.generation else { return }
                self.conversations[friend]?.isLoading = false
                self.conversations[friend]?.error = SteamModel.message(error)
            }
        }
    }

    func loadOlder(_ friend: UInt64) {
        guard let session, let conversation = conversations[friend], conversation.isLoaded, conversation.hasOlder,
              !conversation.isLoadingOlder, let oldest = conversation.oldestPosition else { return }
        conversations[friend]?.isLoadingOlder = true
        let current = generation
        Task { [weak self] in
            do {
                let history = try await session.recentMessages(friendSteamId: friend, before: oldest, count: 50)
                guard let self, current == self.generation else { return }
                self.conversations[friend]?.merge(history.messages.map(Self.line))
                self.conversations[friend]?.hasOlder = history.moreAvailable
                self.conversations[friend]?.isLoadingOlder = false
            } catch {
                guard let self, current == self.generation else { return }
                self.conversations[friend]?.isLoadingOlder = false
                self.conversations[friend]?.error = SteamModel.message(error)
            }
        }
    }

    private static func line(_ message: SteamChatHistoryMessage) -> SteamChatLine {
        .message(timestamp: message.timestamp, ordinal: message.ordinal, fromMe: message.fromMe, bbcode: message.message)
    }

    // MARK: Composing

    func draft(for friend: UInt64) -> String { drafts[friend] ?? "" }

    /// Tells the friend the account is typing; Rust sends it at most every
    /// 10 seconds, as Valve's clients do.
    func setDraft(_ text: String, for friend: UInt64) {
        let previous = draft(for: friend)
        drafts[friend] = text
        guard let session, text.count > previous.count,
              !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        Task { try? await session.sendTyping(friendSteamId: friend) }
    }

    func send(to friend: UInt64) {
        guard let session, conversations[friend]?.isSending != true else { return }
        let text = draft(for: friend).trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        conversations[friend, default: SteamConversation()].isSending = true
        conversations[friend]?.error = nil
        drafts[friend] = ""
        let current = generation
        let account = account
        Task { [weak self] in
            do {
                let sent = try await session.sendMessage(friendSteamId: friend, text: text)
                guard let self, current == self.generation else { return }
                self.conversations[friend]?.merge([.message(timestamp: sent.timestamp, ordinal: sent.ordinal,
                                                            fromMe: true, bbcode: sent.message)])
                self.conversations[friend]?.isSending = false
                self.acknowledgeIfReading(friend)
            } catch {
                guard let self, account == self.account else { return }
                // A session that ended meanwhile cleared the flag; the text still goes back.
                if self.conversations[friend] != nil && self.draft(for: friend).isEmpty { self.drafts[friend] = text }
                guard current == self.generation else { return }
                self.conversations[friend]?.isSending = false
                self.conversations[friend]?.error = SteamModel.message(error)
            }
        }
    }

    func dismissError(for friend: UInt64) { conversations[friend]?.error = nil }

    // MARK: Invitations

    /// Offered only while the account plays a game that takes an invitation
    /// now, and to a friend who is online. Steam's own friend menu follows the
    /// account's persona; the lobby the engine shows the running game in
    /// comes next, unless the persona shows another game.
    func inviteOffer(for friend: FriendPresentation) -> SteamInviteOffer? {
        guard friend.presence.isOnline, isConnected else { return nil }
        if let game = ownGame, game.connected, game.appId != 0, game.invite != .none {
            return SteamInviteOffer(appId: game.appId, gameName: gameName(game.appId))
        }
        guard let lobby = gameLobby, (ownGame?.appId ?? 0) == 0 || ownGame?.appId == lobby.appId else { return nil }
        return SteamInviteOffer(appId: lobby.appId, gameName: gameName(lobby.appId),
                                invite: .lobby(lobbyId: lobby.lobbyId))
    }

    /// The picker's invitation, for a friend who is online.
    func pickerOffer(for friend: FriendPresentation) -> SteamInviteOffer? {
        guard friend.presence.isOnline, isConnected else { return nil }
        return invitePicker
    }

    /// Follows the account's running Steam Game session `id`, of `appId`: the
    /// lobby its game is in, and whether it just asked to invite friends.
    /// True when it did, and the picker opened. A request the session already
    /// showed when it was first seen, such as one recovered, opens nothing,
    /// and a request that went, with the game's Steam connection, closes it.
    @discardableResult
    func followGame(session id: String?, appId: UInt32, request: SteamInviteRequest?, lobby: UInt64?) -> Bool {
        guard let id, appId != 0 else {
            followedRequest = SessionSequence()
            if gameLobby != nil { gameLobby = nil }
            closeInvitePicker()
            return false
        }
        let isNew = followedRequest.advance(session: id, sequence: request?.sequence)
        let current = lobby.map { (appId: appId, lobbyId: $0) }
        if gameLobby?.appId != current?.appId || gameLobby?.lobbyId != current?.lobbyId { gameLobby = current }
        guard let request else {
            closeInvitePicker()
            return false
        }
        guard isNew else { return false }
        invitePicker = SteamInviteOffer(appId: appId, gameName: gameName(appId), invite: request.invite,
                                        request: request.sequence)
        pickerInvited = []
        pickerFailures = [:]
        return true
    }

    func closeInvitePicker() {
        if invitePicker != nil { invitePicker = nil }
        if !pickerInvited.isEmpty { pickerInvited = [] }
        if !pickerFailures.isEmpty { pickerFailures = [:] }
    }

    /// A failure in the picker stays on its row; elsewhere the conversation,
    /// which opens to show the outcome, tells it.
    func invite(_ friend: UInt64, to offer: SteamInviteOffer) {
        guard let session, conversations[friend]?.isInviting != true else { return }
        conversations[friend, default: SteamConversation()].isInviting = true
        let picking = offer.request != 0
        if picking { pickerFailures[friend] = nil } else { conversations[friend]?.error = nil }
        let current = generation
        Task { [weak self] in
            do {
                if let invite = offer.invite {
                    try await session.inviteIntoGame(friendSteamId: friend, appId: offer.appId, invite: invite)
                } else {
                    try await session.inviteToGame(friendSteamId: friend, appId: offer.appId)
                }
                guard let self, current == self.generation else { return }
                if self.invitePicker == offer { self.pickerInvited.insert(friend) }
                self.notes += 1
                let now = UInt32(clamping: Int(Date().timeIntervalSince1970))
                let timestamp = max(now, self.conversations[friend]?.lines.last?.timestamp ?? 0)
                self.conversations[friend]?.merge([SteamChatLine(id: .note(self.notes), timestamp: timestamp,
                                                                 content: .invitationSent(appId: offer.appId))])
                self.conversations[friend]?.isInviting = false
            } catch {
                guard let self, current == self.generation else { return }
                self.conversations[friend]?.isInviting = false
                if !picking { self.conversations[friend]?.error = SteamModel.message(error) }
                else if self.invitePicker == offer { self.pickerFailures[friend] = SteamModel.message(error) }
            }
        }
    }
}

/// NotificationCenter retains observer blocks, so removal follows the model's lifetime.
final class ApplicationActivityObservation: @unchecked Sendable {
    private let tokens: [NSObjectProtocol]
    init(action: @escaping @Sendable (Bool) -> Void) {
        let center = NotificationCenter.default
        tokens = [
            center.addObserver(forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main) { _ in action(true) },
            center.addObserver(forName: NSApplication.didResignActiveNotification, object: nil, queue: .main) { _ in action(false) },
        ]
    }
    deinit {
        for token in tokens { NotificationCenter.default.removeObserver(token) }
    }
}
