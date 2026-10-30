import AppKit
import ImageIO
import SwiftUI

struct FriendsPanel: View {
    let state: FriendsPresentationState
    let avatars: FriendsAvatarCache
    /// Steam's chat; without it the list only shows presence.
    var chat: SteamChatModel?
    /// The game's invite dialog, in the overlay: the list picks friends into
    /// its target.
    var picker: SteamInviteOffer?
    /// The overlay closes itself: the panel then has no close button.
    var closable = true
    /// Shown above the friends, such as the invitations into the overlay's game.
    var accessory: AnyView?
    /// A workspace opens chats beside this list instead of pushing over it.
    var openConversation: ((FriendIdentity) -> Void)?
    let focus: FriendsPanelFocus
    let dismiss: @MainActor () -> Void
    let retry: @MainActor () -> Void
    let signIn: @MainActor () -> Void
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var conversation: (friend: FriendPresentation, steamID: UInt64, chat: SteamChatModel)? {
        guard openConversation == nil, let chat, let identity = focus.conversation,
              let friend = state.friends.first(where: { $0.id == identity }), let steamID = friend.steamID else { return nil }
        return (friend, steamID, chat)
    }

    var body: some View {
        ZStack {
            if let conversation {
                FriendConversationView(friend: conversation.friend, steamID: conversation.steamID, chat: conversation.chat,
                                       avatars: avatars, focus: focus, dismiss: dismiss, closable: closable)
                    .transition(.move(edge: .trailing).combined(with: .opacity))
            } else {
                FriendsList(state: state, avatars: avatars, chat: chat, picker: picker, closable: closable,
                            accessory: accessory, openConversation: openConversation,
                            focus: focus, dismiss: dismiss, retry: retry, signIn: signIn)
                    .transition(.move(edge: .leading).combined(with: .opacity))
            }
        }
        .animation(reduceMotion ? .easeInOut(duration: 0.15) : .spring(response: 0.32, dampingFraction: 0.88),
                   value: conversation?.steamID)
        // The game asked to invite friends: the list is its picker. Each
        // request is another offer, even for the same lobby.
        .onChange(of: picker) { _, picker in
            if picker != nil && focus.conversation != nil {
                focus.target = .search
                focus.conversation = nil
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background {
            if reduceTransparency { ObeliskPalette(colorScheme: colorScheme).background }
            else {
                Rectangle().fill(.regularMaterial)
                    .overlay { ObeliskPalette(colorScheme: colorScheme).background.opacity(0.96) }
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: 18))
        .overlay { RoundedRectangle(cornerRadius: 18).strokeBorder(ObeliskPalette(colorScheme: colorScheme).border, lineWidth: 1) }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(state.title)
        .accessibilityIdentifier("friends-panel")
    }
}

private struct FriendsList: View {
    let state: FriendsPresentationState
    let avatars: FriendsAvatarCache
    let chat: SteamChatModel?
    /// The invitation the game's picker sends, while it shows.
    let picker: SteamInviteOffer?
    let closable: Bool
    let accessory: AnyView?
    let openConversation: ((FriendIdentity) -> Void)?
    let focus: FriendsPanelFocus
    let dismiss: @MainActor () -> Void
    let retry: @MainActor () -> Void
    let signIn: @MainActor () -> Void
    @State private var search = ""
    @State private var offlineExpanded = false
    @FocusState private var focusedControl: FriendsPanelFocus.Control?

    var body: some View {
        let recentIDs = Set(recent.map(\.id))
        let remaining = FriendsPresentationState(store: state.store, account: state.account,
            friends: state.friends.filter { !recentIDs.contains($0.id) }, connection: state.connection,
            lastKnownOnlineCount: state.lastKnownOnlineCount)
        let sections = remaining.sections(matching: search)
        let rowControls = rowControls(sections)
        VStack(spacing: 0) {
            header
            searchField.padding(.horizontal, 22).padding(.bottom, 16)
            Divider()
            if state.connection != .connected && state.connection != .loading {
                HStack(alignment: .center, spacing: 12) {
                    Label(state.summary, systemImage: "exclamationmark.circle")
                        .frame(maxWidth: .infinity, alignment: .leading)
                    if state.connection == .authenticationRequired {
                        Button(L10n.string("ui.friends.signIn"), action: signIn)
                            .buttonStyle(GlassCapsuleStyle())
                            .pointerCursor()
                            .focused($focusedControl, equals: .reconnect)
                    } else {
                        Button(L10n.string("ui.friends.retry"), action: retry)
                            .buttonStyle(GlassCapsuleStyle())
                            .pointerCursor()
                            .focused($focusedControl, equals: .reconnect)
                    }
                }
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 22).padding(.vertical, 12)
                    .background(ObeliskStyle.accent.opacity(0.06))
                    .accessibilityIdentifier("friends-connection-status")
            }
            if let invitations = chat?.invitations { joinStatus(invitations) }
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 4) {
                    if let accessory, picker == nil {
                        accessory.padding(.bottom, 10)
                    }
                    if !recent.isEmpty {
                        sectionHeading(L10n.string("ui.chat.recent"))
                        rows(recent)
                    }
                    if state.friends.isEmpty {
                        if state.connection == .loading {
                            ProgressView(L10n.string("ui.friends.loading"))
                                .frame(maxWidth: .infinity, minHeight: 200)
                        } else if state.connection == .connected {
                            emptyMessage("ui.friends.empty", symbol: "person.2")
                        } else {
                            emptyMessage("ui.friends.unavailable", symbol: "wifi.exclamationmark")
                        }
                    } else if sections.isEmpty && recent.isEmpty {
                        emptyMessage("ui.friends.noResults", symbol: "magnifyingglass")
                    } else {
                        if !sections.hasOnline && !sections.isSearching && state.connection == .connected {
                            Text(L10n.string("ui.friends.noOnline"))
                                .font(.system(size: 13)).foregroundStyle(.secondary)
                                .padding(.horizontal, 10).padding(.vertical, 14)
                        }
                        rows(sections.playing)
                        rows(sections.online)
                        if !sections.unknown.isEmpty {
                            sectionHeading(L10n.string("ui.friends.presence.unknown"))
                            rows(sections.unknown)
                        }
                        if !sections.offline.isEmpty {
                            Button {
                                offlineExpanded.toggle()
                            } label: {
                                HStack(spacing: 7) {
                                    Image(systemName: sections.showsOffline(isExpanded: offlineExpanded) ? "chevron.down" : "chevron.right")
                                        .font(.system(size: 10, weight: .semibold)).frame(width: 12)
                                    Text(L10n.string("ui.friends.offlineCount", Int64(sections.offline.count)))
                                    Spacer()
                                }
                                .font(.system(size: 12, weight: .semibold))
                                .foregroundStyle(.secondary)
                                .padding(.horizontal, 10).padding(.vertical, 12)
                                .contentShape(Rectangle())
                            }
                            .obeliskButtonStyle(.plain)
                            .focused($focusedControl, equals: .offline)
                            .disabled(sections.isSearching)
                            .accessibilityValue(L10n.string(sections.showsOffline(isExpanded: offlineExpanded) ? "ui.friends.expanded" : "ui.friends.collapsed"))
                            .accessibilityIdentifier("friends-offline-toggle")
                            if sections.showsOffline(isExpanded: offlineExpanded) { rows(sections.offline) }
                        }
                    }
                }
                .padding(.horizontal, 12).padding(.vertical, 10)
            }
            .accessibilityIdentifier("friends-scroll")
        }
        .onAppear { synchronizeFocus(sections: sections) }
        .onChange(of: search) { _, _ in synchronizeFocus(sections: sections) }
        .onChange(of: sections.offline.isEmpty) { _, _ in synchronizeFocus(sections: sections) }
        .onChange(of: rowControls) { _, _ in synchronizeFocus(sections: sections) }
        .onChange(of: state.connection) { _, _ in synchronizeFocus(sections: sections) }
        .onChange(of: focus.target) { _, target in focusedControl = target }
        .onChange(of: focusedControl) { _, target in if let target { focus.target = target } }
        .onExitCommand(perform: dismiss)
    }

    /// Why joining a friend's game could not, until the player reads it.
    @ViewBuilder
    private func joinStatus(_ invitations: SteamReceivedInvitations) -> some View {
        if let failure = invitations.friendJoinFailure {
            HStack(spacing: 10) {
                Label(failure, systemImage: "exclamationmark.circle").frame(maxWidth: .infinity, alignment: .leading)
                Button { invitations.friendJoinFailure = nil } label: { Image(systemName: "xmark") }
                    .obeliskButtonStyle(.plain)
                    .accessibilityLabel(L10n.string("ui.overlay.invites.dismiss"))
            }
            .font(.system(size: 12))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 22).padding(.vertical, 12)
            .background(ObeliskStyle.accent.opacity(0.06))
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("friends-join-status")
        }
    }

    private var header: some View {
        HStack(spacing: 14) {
            if let section = LibrarySection(rawValue: state.store.id) {
                StoreTile(section: section, size: 44)
            } else {
                Image(state.store.logoAssetName).resizable().scaledToFit()
                    .frame(width: 32, height: 32).accessibilityHidden(true)
            }
            VStack(alignment: .leading, spacing: 4) {
                if let picker {
                    Text(picker.pickerTitle).font(.system(size: 20, weight: .bold)).tracking(-0.2).lineLimit(2)
                        .accessibilityIdentifier("friends-invite-picker-title")
                    Text(L10n.string("ui.chat.invitePicker.subtitle"))
                        .font(.system(size: 13)).foregroundStyle(.secondary).lineLimit(2)
                } else {
                    Text(state.title).font(.system(size: 20, weight: .bold)).tracking(-0.2)
                    Text(state.headerSummary)
                        .font(.system(size: 13)).foregroundStyle(.secondary).lineLimit(2)
                }
            }
            Spacer(minLength: 12)
            if closable {
                FriendsPanelCloseButton(action: dismiss)
                    .focused($focusedControl, equals: .close)
            }
        }
        .padding(22).padding(.bottom, 2)
        .modifier(OverlayDraggableHeader())
    }

    private var searchField: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass").foregroundStyle(.secondary).accessibilityHidden(true)
            TextField(L10n.string("ui.friends.search"), text: $search)
                .textFieldStyle(.plain)
                .focused($focusedControl, equals: .search)
                .accessibilityLabel(L10n.string("ui.friends.search"))
                .accessibilityIdentifier("friends-search")
            if !search.isEmpty {
                Button { search = ""; focus.target = .search } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
                }
                .obeliskButtonStyle(.plain)
                .focused($focusedControl, equals: .clear)
                .accessibilityLabel(L10n.string("ui.search.clear"))
            }
        }
        .font(.system(size: 13))
        .padding(.horizontal, 12).frame(height: 38)
        .modifier(SignInWell(isFocused: focusedControl == .search))
    }

    @ViewBuilder
    private func rows(_ friends: [FriendPresentation]) -> some View {
        ForEach(friends) { friend in
            if let chat, let steamID = friend.steamID {
                FriendChatRow(friend: friend, steamID: steamID, avatars: avatars, chat: chat, picking: picker != nil,
                              open: { open(friend) })
                    .focused($focusedControl, equals: .friend(friend.id))
            } else {
                FriendRow(friend: friend, avatars: avatars)
            }
        }
    }

    private func open(_ friend: FriendPresentation) { open(friend.id) }

    private func open(_ identity: FriendIdentity) {
        if let openConversation { openConversation(identity); return }
        focus.target = .composer
        focus.conversation = identity
    }

    /// Only rows that can open a conversation take focus, and the panel
    /// resolves the identity against the friends it shows by then. In the
    /// game's picker, Return invites instead; an online friend the picker
    /// cannot invite yet, while Steam's chat reconnects, does nothing.
    private func activate(_ control: FriendsPanelFocus.Control) -> Bool {
        guard case .friend(let identity) = control, let chat else { return false }
        if picker != nil, let friend = state.friends.first(where: { $0.id == identity }), let steamID = friend.steamID {
            if let offer = chat.pickerOffer(for: friend) {
                if !chat.pickerInvited.contains(steamID) { chat.invite(steamID, to: offer) }
                return true
            }
            if friend.presence.isOnline { return true }
        }
        open(identity)
        return true
    }

    private func sectionHeading(_ title: String) -> some View {
        Text(title).font(.system(size: 12, weight: .semibold)).foregroundStyle(.secondary)
            .padding(.horizontal, 10).padding(.top, 14).padding(.bottom, 6)
    }

    private func emptyMessage(_ key: String, symbol: String) -> some View {
        VStack(spacing: 12) {
            Image(systemName: symbol).font(.system(size: 30)).accessibilityHidden(true)
            Text(L10n.string(key)).font(.system(size: 13)).multilineTextAlignment(.center)
        }
        .foregroundStyle(.secondary)
        .frame(maxWidth: .infinity, minHeight: 200)
        .accessibilityIdentifier("friends-empty-state")
    }

    /// Rows that open a conversation take part in Tab, in their visual order.
    private func rowControls(_ sections: FriendsSections) -> [FriendsPanelFocus.Control] {
        recent.map { .friend($0.id) } + onlineRowControls(sections) + offlineRowControls(sections)
    }

    private var recent: [FriendPresentation] {
        guard let chat, picker == nil else { return [] }
        return state.friends.filter {
            guard let id = $0.steamID else { return false }
            return chat.conversations[id]?.latestTimestamp != nil
                && (search.isEmpty || $0.displayName.localizedStandardContains(search))
        }.sorted {
            let left = chat.conversations[$0.steamID ?? 0]?.latestTimestamp ?? 0
            let right = chat.conversations[$1.steamID ?? 0]?.latestTimestamp ?? 0
            return left == right ? $0.id.friendID < $1.id.friendID : left > right
        }
    }

    private func onlineRowControls(_ sections: FriendsSections) -> [FriendsPanelFocus.Control] {
        chat == nil ? [] : (sections.playing + sections.online + sections.unknown).map { .friend($0.id) }
    }

    private func offlineRowControls(_ sections: FriendsSections) -> [FriendsPanelFocus.Control] {
        chat == nil || !sections.showsOffline(isExpanded: offlineExpanded) ? [] : sections.offline.map { .friend($0.id) }
    }

    private func synchronizeFocus(sections: FriendsSections) {
        focus.activate = activate
        focus.controls = (closable ? [.close] : []) + [.search] + (search.isEmpty ? [] : [.clear])
            + (state.connection != .connected && state.connection != .loading ? [.reconnect] : [])
            + recent.map { .friend($0.id) }
            + onlineRowControls(sections)
            + (!sections.offline.isEmpty && !sections.isSearching ? [.offline] : [])
            + offlineRowControls(sections)
        if !focus.controls.contains(focus.target) { focus.target = .search }
        focusedControl = focus.target
    }
}

struct FriendsPanelCloseButton: View {
    let action: @MainActor () -> Void

    var body: some View {
        Button(action: action) {
            StorefrontGlassIcon(systemName: "xmark", size: 13)
                .foregroundStyle(.primary)
        }
        .obeliskButtonStyle(.plain)
        .accessibilityLabel(L10n.string("ui.friends.close"))
        .accessibilityIdentifier("friends-close")
        .help(L10n.string("ui.friends.close"))
    }
}

struct FriendRow: View {
    let friend: FriendPresentation
    let avatars: FriendsAvatarCache
    var unread = 0

    var body: some View {
        HStack(spacing: 12) {
            FriendAvatar(friend: friend, avatars: avatars, size: 42)
            VStack(alignment: .leading, spacing: 3) {
                Text(friend.displayName).font(.system(size: 13, weight: .semibold)).lineLimit(1)
                HStack(spacing: 5) {
                    Circle().fill(friend.presenceColor).frame(width: 5, height: 5).accessibilityHidden(true)
                    Text(friend.presence.title).font(.system(size: 11)).foregroundStyle(.secondary)
                }
                if let activity = friend.activity {
                    Text(activity.title).font(.system(size: 11)).foregroundStyle(friend.presenceColor).lineLimit(1)
                }
            }
            Spacer(minLength: 0)
            if unread > 0 { UnreadBadge(count: unread) }
        }
        .padding(.horizontal, 10).padding(.vertical, 9)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(friend.displayName)
        .accessibilityValue(([friend.status] + (unread > 0 ? [L10n.string("ui.chat.unreadCount", Int64(unread))] : []))
            .joined(separator: " · "))
        .help([friend.displayName, friend.status].joined(separator: "\n"))
    }
}

/// A Steam friend's row: it opens their conversation, and its button and
/// menu offer the game the account can invite them into, or join the one
/// they invited it into. Its menu joins the game they play, as Steam's "Join
/// Game" does. In the game's picker, the row itself invites, and tells once
/// it did.
private struct FriendChatRow: View {
    let friend: FriendPresentation
    let steamID: UInt64
    let avatars: FriendsAvatarCache
    let chat: SteamChatModel
    let picking: Bool
    let open: @MainActor () -> Void

    var body: some View {
        let offer = picking ? chat.pickerOffer(for: friend) : chat.inviteOffer(for: friend)
        let invited = picking && chat.pickerInvited.contains(steamID)
        // Online, but Steam's chat reconnects: the picker waits for it.
        let waiting = picking && offer == nil && friend.presence.isOnline
        let trailing = trailing(offer: offer, invited: invited, waiting: waiting)
        let canJoinGame = joinable.map { chat.invitations?.availability(appId: $0.appId).canJoin == true } ?? false
        Button {
            // A friend the picker cannot invite, offline for one, still opens.
            if picking, let offer {
                if !invited { chat.invite(steamID, to: offer) }
            } else if !waiting { open() }
        } label: {
            FriendRow(friend: friend, avatars: avatars, unread: chat.unread[steamID] ?? 0)
                .padding(.trailing, trailing.width)
        }
        .buttonStyle(FriendRowButtonStyle())
        .pointerCursor()
        .overlay(alignment: .trailing) {
            Group {
                switch trailing {
                case .join(let invite, let invitations):
                    InvitationJoinButton(invite: invite, invitations: invitations, friendName: friend.displayName,
                                         identifier: "friends-row-join-\(steamID)")
                        .focusable(false)
                case .invite(let offer): inviteButton(offer)
                case .picker(let offer, let invited): pickerControl(offer, invited: invited)
                case .unavailable: unavailableControl
                case .empty: EmptyView()
                }
            }
            .padding(.trailing, 10)
        }
        .contextMenu {
            Button(L10n.string("ui.chat.sendMessage"), systemImage: "bubble.left", action: open)
            if let offer, !invited {
                Button(offer.menuTitle, systemImage: "gamecontroller") { invite(offer) }
            }
            if let joinable {
                Button(L10n.string("ui.invites.joinGame"), systemImage: "play") { join(joinable) }
                    .disabled(!canJoinGame)
                    .help(chat.invitations?.availability(appId: joinable.appId).reason ?? "")
            }
        }
        .accessibilityActions {
            Button(L10n.string("ui.chat.sendMessage"), action: open)
            if let offer, !invited { Button(offer.menuTitle) { invite(offer) } }
            if let joinable, canJoinGame {
                Button(L10n.string("ui.invites.joinGame")) { join(joinable) }
            }
        }
        .accessibilityIdentifier("friends-row-\(steamID)")
    }

    private func inviteButton(_ offer: SteamInviteOffer) -> some View {
        Button { invite(offer) } label: {
            Group {
                if chat.conversations[steamID]?.isInviting == true { ProgressView().controlSize(.mini).tint(.white) }
                else { Image(systemName: "gamecontroller.fill").font(.system(size: 12, weight: .semibold)) }
            }
            .foregroundStyle(.white)
            .frame(width: 30, height: 30)
            .background(LinearGradient(colors: ObeliskStyle.launchGradient, startPoint: .leading, endPoint: .trailing),
                        in: Circle())
            .contentShape(Circle())
        }
        .obeliskButtonStyle(.plain)
        .focusable(false)
        .disabled(chat.conversations[steamID]?.isInviting == true)
        .help(offer.buttonTitle)
        .accessibilityLabel(offer.buttonTitle)
        .accessibilityIdentifier("friends-row-invite-\(steamID)")
    }

    /// Invite, its progress, its failure, or the invitation handed to Steam,
    /// which never confirms delivery.
    @ViewBuilder
    private func pickerControl(_ offer: SteamInviteOffer, invited: Bool) -> some View {
        let inviting = chat.conversations[steamID]?.isInviting == true
        let failure = chat.pickerFailures[steamID]
        if invited {
            Label(L10n.string("ui.chat.invitePicker.sent"), systemImage: "checkmark")
                .font(.system(size: 12, weight: .semibold)).foregroundStyle(.secondary)
                .help(L10n.string("ui.chat.invitePicker.sentHelp"))
                .accessibilityHint(L10n.string("ui.chat.invitePicker.sentHelp"))
                .accessibilityIdentifier("friends-row-invited-\(steamID)")
        } else {
            Button { chat.invite(steamID, to: offer) } label: {
                HStack(spacing: 6) {
                    if inviting { ProgressView().controlSize(.mini).tint(.white) }
                    else if failure != nil { Image(systemName: "exclamationmark.triangle.fill") }
                    else { Image(systemName: "gamecontroller.fill") }
                    Text(L10n.string(failure != nil ? "ui.friends.retry" : "ui.chat.invitePicker.invite"))
                }
                .font(.system(size: 12, weight: .semibold)).foregroundStyle(.white)
                .padding(.horizontal, 12).frame(height: 28)
                .background(LinearGradient(colors: ObeliskStyle.launchGradient, startPoint: .leading, endPoint: .trailing),
                            in: Capsule())
                .contentShape(Capsule())
            }
            .obeliskButtonStyle(.plain)
            .focusable(false)
            .disabled(inviting)
            .help(failure ?? offer.buttonTitle)
            .accessibilityLabel(L10n.string("ui.chat.invitePicker.inviteNamed", friend.displayName))
            .accessibilityIdentifier("friends-row-invite-\(steamID)")
        }
    }

    /// The picker's Invite, greyed until Steam's chat is back.
    private var unavailableControl: some View {
        Label(L10n.string("ui.chat.invitePicker.invite"), systemImage: "gamecontroller.fill")
            .font(.system(size: 12, weight: .semibold)).foregroundStyle(.secondary)
            .padding(.horizontal, 12).frame(height: 28)
            .background(.primary.opacity(0.07), in: Capsule())
            .help(L10n.string("ui.chat.invitePicker.unavailable"))
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(L10n.string("ui.chat.invitePicker.inviteNamed", friend.displayName))
            .accessibilityValue(L10n.string("ui.chat.invitePicker.unavailable"))
            .accessibilityIdentifier("friends-row-invite-unavailable-\(steamID)")
    }

    /// The game the friend plays and how to join it, while Steam shows one,
    /// as its "Join Game" does.
    private var joinable: SteamJoinTarget? {
        picking ? nil : friend.steamJoin
    }

    private func join(_ target: SteamJoinTarget) {
        chat.invitations?.joinFriend(steamID, target: target)
    }

    /// What the row shows at its trailing edge, and the room it leaves for it.
    private enum Trailing {
        /// A friend's invitation, in place of the invite button while it can
        /// be joined, or while there is nothing to invite them into.
        case join(SteamReceivedInvite, SteamReceivedInvitations)
        case invite(SteamInviteOffer)
        case picker(SteamInviteOffer, invited: Bool)
        /// The picker's Invite while Steam's chat reconnects.
        case unavailable
        case empty

        var width: CGFloat {
            switch self {
            case .join: 110
            case .invite: 40
            case .picker(_, let invited): invited ? 170 : 120
            case .unavailable: 120
            case .empty: 0
            }
        }
    }

    private func trailing(offer: SteamInviteOffer?, invited: Bool, waiting: Bool) -> Trailing {
        if !picking, let invitations = chat.invitations, let invite = invitations.invite(from: steamID),
           offer == nil || invitations.availability(appId: invite.appId).canJoin {
            return .join(invite, invitations)
        }
        if let offer { return picking ? .picker(offer, invited: invited) : .invite(offer) }
        return waiting ? .unavailable : .empty
    }

    /// Outside the picker, the conversation opens to show the outcome.
    private func invite(_ offer: SteamInviteOffer) {
        chat.invite(steamID, to: offer)
        if !picking { open() }
    }
}

/// Joins a friend's invitation; greyed, with why, while Play is unavailable.
struct InvitationJoinButton: View {
    let invite: SteamReceivedInvite
    let invitations: SteamReceivedInvitations
    let friendName: String
    let identifier: String

    var body: some View {
        let availability = invitations.availability(appId: invite.appId)
        let accepting = invitations.accepting == SteamReceivedInvitations.Key(invite)
        Button { Task { _ = await invitations.accept(invite) } } label: {
            HStack(spacing: 6) {
                if accepting { ProgressView().controlSize(.mini).tint(.white) }
                else { Image(systemName: "play.fill") }
                Text(L10n.string("ui.overlay.invites.join"))
            }
            .font(.system(size: 12, weight: .semibold)).foregroundStyle(.white)
            .padding(.horizontal, 12).frame(height: 28)
            .background(LinearGradient(colors: ObeliskStyle.launchGradient, startPoint: .leading, endPoint: .trailing),
                        in: Capsule())
            .opacity(availability.canJoin ? 1 : 0.45)
            .contentShape(Capsule())
        }
        .obeliskButtonStyle(.plain)
        .disabled(!availability.canJoin || invitations.accepting != nil)
        .help(availability.reason ?? "")
        .accessibilityLabel(L10n.string("ui.overlay.invites.joinNamed", friendName))
        .accessibilityIdentifier(identifier)
    }
}

private struct FriendRowButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        Appearance(configuration: configuration)
    }

    private struct Appearance: View {
        let configuration: Configuration
        @Environment(\.isFocused) private var isFocused
        @State private var isHovered = false

        var body: some View {
            configuration.label
                .background {
                    RoundedRectangle(cornerRadius: 12)
                        .fill(.primary.opacity(configuration.isPressed ? 0.1 : isHovered ? 0.05 : 0))
                }
                .overlay {
                    if isFocused {
                        RoundedRectangle(cornerRadius: 12).strokeBorder(ObeliskStyle.accent.opacity(0.7), lineWidth: 2)
                    }
                }
                .contentShape(RoundedRectangle(cornerRadius: 12))
                .onHover { isHovered = $0 }
        }
    }
}

struct FriendAvatar: View {
    let friend: FriendPresentation
    let avatars: FriendsAvatarCache
    let size: CGFloat
    @State private var avatar: NSImage?

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: size * 0.24).fill(.primary.opacity(0.08))
            if let avatar { Image(nsImage: avatar).resizable().scaledToFill() }
            else if friend.initials.isEmpty {
                Image(systemName: "person.fill").font(.system(size: size * 0.45)).foregroundStyle(.secondary)
            } else {
                Text(friend.initials).font(.system(size: size * 0.36, weight: .semibold)).foregroundStyle(.secondary)
            }
        }
        .frame(width: size, height: size).clipShape(RoundedRectangle(cornerRadius: size * 0.24))
        .accessibilityHidden(true)
        .task(id: FriendsAvatarCache.Key(identity: friend.id, url: friend.avatarURL)) {
            avatar = nil
            let image = await avatars.image(for: friend.id, url: friend.avatarURL)
            guard !Task.isCancelled else { return }
            avatar = image
        }
    }
}

struct UnreadBadge: View {
    let count: Int
    var compact = false

    var body: some View {
        Text(count > 99 ? "99+" : String(count))
            .font(.system(size: compact ? 9 : 10.5, weight: .bold)).monospacedDigit()
            .foregroundStyle(.white)
            .padding(.horizontal, compact ? 4 : 6)
            .frame(minWidth: compact ? 15 : 19, minHeight: compact ? 15 : 19)
            .background(ObeliskStyle.accent, in: Capsule())
            .accessibilityLabel(L10n.string("ui.chat.unreadCount", Int64(count)))
    }
}

extension FriendPresentation {
    var presenceColor: Color {
        if presence == .unknown { return .secondary }
        if !presence.isOnline { return .secondary }
        if activity != nil { return Color(red: 0.3, green: 0.68, blue: 0.4) }
        return Color(red: 0.32, green: 0.59, blue: 0.87)
    }

    /// Nil for a friend of another storefront.
    var steamID: UInt64? { id.account.storeID == "steam" ? UInt64(id.friendID) : nil }
}

extension FriendsPresentationState {
    /// The Steam friend `steamID`, when the list shows them.
    func friend(_ steamID: UInt64) -> FriendPresentation? {
        friends.first { $0.steamID == steamID }
    }
}

extension SteamInviteOffer {
    var menuTitle: String {
        gameName.map { L10n.string("ui.chat.inviteToJoin", $0) } ?? L10n.string("ui.chat.inviteToJoinUnknown")
    }

    var buttonTitle: String {
        gameName.map { L10n.string("ui.chat.inviteTo", $0) } ?? L10n.string("ui.chat.inviteToUnknown")
    }

    var pickerTitle: String {
        gameName.map { L10n.string("ui.chat.invitePicker.title", $0) } ?? L10n.string("ui.chat.invitePicker.titleUnknown")
    }
}

/// The panel's keyboard state, shared with its AppKit container, which
/// handles Tab, Return and Escape before SwiftUI sees them.
@MainActor @Observable
final class FriendsPanelFocus {
    enum Control: Hashable {
        case close, search, clear, offline, reconnect
        case friend(FriendIdentity)
        case back, invite, composer, send
    }
    var target: Control = .search
    var controls: [Control] = [.close, .search]
    /// The friend whose conversation replaces the list.
    var conversation: FriendIdentity?
    /// Performs Return on a control; false lets the key through.
    @ObservationIgnored var activate: (@MainActor (Control) -> Bool)?

    func advance(backward: Bool) {
        guard !controls.isEmpty else { return }
        let index = controls.firstIndex(of: target) ?? 0
        target = controls[(index + (backward ? controls.count - 1 : 1)) % controls.count]
    }

    /// Leaves the conversation for the list, on its friend's row. False when
    /// the list already shows.
    func back() -> Bool {
        guard let conversation else { return false }
        target = .friend(conversation)
        self.conversation = nil
        return true
    }

    /// Tab, Return and Escape in the panel's window; true when handled.
    /// Escape leaves the conversation, otherwise it dismisses.
    func handle(_ event: NSEvent, dismiss: @MainActor () -> Void) -> Bool {
        // App switching and other system shortcuts must bypass chat's Tab
        // navigation, even while the overlay owns keyboard focus.
        guard event.modifierFlags.intersection([.command, .option, .control]).isEmpty else { return false }
        // Input methods own Return and Escape while they compose text.
        let editor = event.window?.firstResponder as? NSTextView
        if editor?.hasMarkedText() == true { return false }
        if event.keyCode == 53 {
            if !back() { dismiss() }
            return true
        }
        if event.keyCode == 48 {
            advance(backward: event.modifierFlags.contains(.shift))
            return true
        }
        if event.keyCode == 36 || event.keyCode == 76 {
            if target == .composer && event.modifierFlags.contains(.shift) {
                editor?.insertNewlineIgnoringFieldEditor(nil)
                return editor != nil
            }
            return activate?(target) ?? false
        }
        return false
    }
}

/// Images stay in memory for this account only. URLSession uses no disk cache.
@MainActor
final class FriendsAvatarCache {
    struct Key: Hashable { let identity: FriendIdentity; let url: URL? }
    private var account: FriendsAccountIdentity?
    private var images: [Key: NSImage] = [:]
    private var order: [Key] = []
    private var pending: [Key: Task<NSImage?, Never>] = [:]
    private let load: @Sendable (URL) async -> CGImage?
    private nonisolated static let session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.urlCache = nil
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        return URLSession(configuration: configuration)
    }()

    init(load: @escaping @Sendable (URL) async -> CGImage? = FriendsAvatarCache.fetch) {
        self.load = load
    }

    /// Downloads and decodes the avatar off the main actor.
    private nonisolated static func fetch(_ url: URL) async -> CGImage? {
        do {
            let (data, response) = try await session.data(for: URLRequest(url: url, timeoutInterval: 20))
            guard let response = response as? HTTPURLResponse, response.statusCode == 200,
                  data.count <= 2_097_152, !Task.isCancelled else { return nil }
            return ImageDecoding.thumbnail(from: data, maxPixelSize: 96)
        } catch { return nil }
    }
    private var generation = UUID()

    func activate(account: FriendsAccountIdentity?) {
        guard self.account != account else { return }
        pending.values.forEach { $0.cancel() }
        pending.removeAll()
        images.removeAll()
        order.removeAll()
        generation = UUID()
        self.account = account
    }

    func image(for identity: FriendIdentity, url: URL?) async -> NSImage? {
        guard account == identity.account, let url, url.scheme == "https" else { return nil }
        let key = Key(identity: identity, url: url)
        if let image = images[key] { return image }
        let requestGeneration = generation
        // Scrolling can cancel a row before its shared request finishes. Wait
        // for a real completion instead of creating unbounded network tasks.
        while pending[key] == nil && pending.count >= 8 {
            guard let active = pending.values.first else { break }
            _ = await active.value
            guard !Task.isCancelled, generation == requestGeneration, account == identity.account else { return nil }
            // Let the request owner remove its completed entry.
            await Task.yield()
        }
        guard !Task.isCancelled else { return nil }
        if let task = pending[key] {
            let image = await task.value
            return generation == requestGeneration && account == identity.account ? image : nil
        }
        let task = Task<NSImage?, Never> { [load] in
            guard let image = await load(url), !Task.isCancelled else { return nil }
            return NSImage(cgImage: image, size: NSSize(width: image.width, height: image.height))
        }
        pending[key] = task
        let image = await task.value
        guard generation == requestGeneration, account == identity.account else { return nil }
        pending.removeValue(forKey: key)
        if let image {
            images[key] = image
            order.append(key)
            while order.count > 128 { images.removeValue(forKey: order.removeFirst()) }
        }
        return image
    }
}
