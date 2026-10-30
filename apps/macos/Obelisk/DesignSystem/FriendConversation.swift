import SwiftUI

/// A Steam friend's conversation, pushed over the friends list inside the panel.
struct FriendConversationView: View {
    let friend: FriendPresentation
    let steamID: UInt64
    let chat: SteamChatModel
    let avatars: FriendsAvatarCache
    let focus: FriendsPanelFocus
    let dismiss: @MainActor () -> Void
    /// The overlay closes itself: the conversation then has no close button.
    var closable = true
    @FocusState private var focusedControl: FriendsPanelFocus.Control?
    @State private var position = ScrollPosition(edge: .bottom)
    @State private var isAtBottom = true

    private var conversation: SteamConversation { chat.conversations[steamID] ?? SteamConversation() }

    var body: some View {
        let offer = chat.inviteOffer(for: friend)
        VStack(spacing: 0) {
            header(offer: offer)
            Divider()
            messages
            if let error = conversation.error {
                banner(error) { chat.dismissError(for: steamID) }
            } else if !chat.isConnected {
                banner(L10n.string("ui.chat.unavailable"), dismiss: nil)
            }
            composer
        }
        .onAppear {
            chat.show(steamID)
            synchronizeFocus(invites: offer != nil)
        }
        .onDisappear { chat.hide(steamID) }
        .onChange(of: offer != nil) { _, invites in synchronizeFocus(invites: invites) }
        // Return reads the friend's presence when it decides on an invitation.
        .onChange(of: friend) { _, _ in focus.activate = activate }
        .onChange(of: focus.target) { _, target in focusedControl = target }
        .onChange(of: focusedControl) { _, target in if let target { focus.target = target } }
        .onExitCommand { _ = focus.back() }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(L10n.string("ui.chat.conversation", friend.displayName))
        .accessibilityIdentifier("friends-conversation")
    }

    private func header(offer: SteamInviteOffer?) -> some View {
        HStack(spacing: 12) {
            Button { _ = focus.back() } label: {
                Image(systemName: "chevron.left").font(.system(size: 13, weight: .semibold))
                    .frame(width: 30, height: 30)
                    .background(.primary.opacity(0.07), in: Circle())
                    .contentShape(Circle())
            }
            .obeliskButtonStyle(.plain)
            .focused($focusedControl, equals: .back)
            .accessibilityLabel(L10n.string("ui.chat.back"))
            .accessibilityIdentifier("friends-conversation-back")
            .help(L10n.string("ui.chat.back"))
            FriendAvatar(friend: friend, avatars: avatars, size: 36)
            VStack(alignment: .leading, spacing: 2) {
                Text(friend.displayName).font(.system(size: 15, weight: .semibold)).lineLimit(1)
                HStack(spacing: 5) {
                    Circle().fill(friend.presenceColor).frame(width: 5, height: 5).accessibilityHidden(true)
                    Text(friend.status).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            .accessibilityElement(children: .combine)
            Spacer(minLength: 8)
            if let offer {
                Button { chat.invite(steamID, to: offer) } label: {
                    HStack(spacing: 6) {
                        if conversation.isInviting { ProgressView().controlSize(.mini).tint(.white) }
                        else { Image(systemName: "gamecontroller.fill") }
                        Text(offer.buttonTitle).lineLimit(1)
                    }
                }
                .buttonStyle(ChatInviteButtonStyle())
                .focused($focusedControl, equals: .invite)
                .disabled(conversation.isInviting)
                .help(offer.menuTitle)
                .accessibilityIdentifier("friends-conversation-invite")
                .transition(.scale(scale: 0.6).combined(with: .opacity))
            }
            if closable {
                FriendsPanelCloseButton(action: dismiss)
                    .focused($focusedControl, equals: .close)
            }
        }
        .padding(.horizontal, 18).padding(.vertical, 14)
        .modifier(OverlayDraggableHeader())
    }

    private var messages: some View {
        let conversation = conversation
        let items = ConversationItem.items(conversation.lines)
        return ScrollView {
            LazyVStack(spacing: 6) {
                if conversation.hasOlder {
                    ProgressView().controlSize(.small).frame(maxWidth: .infinity).padding(.vertical, 8)
                        .onAppear { chat.loadOlder(steamID) }
                        .accessibilityLabel(L10n.string("ui.chat.loadingOlder"))
                }
                if items.isEmpty {
                    Group {
                        if conversation.isLoaded || !chat.isConnected {
                            Text(L10n.string("ui.chat.empty", friend.displayName)).multilineTextAlignment(.center)
                        } else {
                            ProgressView(L10n.string("ui.chat.loading"))
                        }
                    }
                    .font(.system(size: 12)).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, minHeight: 220)
                }
                ForEach(items) { item in
                    switch item {
                    case .separator(_, let date, let showsDay):
                        ChatSeparator(date: date, showsDay: showsDay)
                    case .line(let line):
                        ChatLineView(line: line, friendName: friend.displayName, friendSteamID: steamID,
                                     gameNames: gameNames(of: line), invitations: chat.invitations)
                    }
                }
                if conversation.isFriendTyping {
                    Text(L10n.string("ui.chat.typing", friend.displayName))
                        .font(.system(size: 11)).foregroundStyle(.secondary).italic()
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 6)
                        .transition(.opacity)
                        .accessibilityIdentifier("friends-conversation-typing")
                }
            }
            .scrollTargetLayout()
            .padding(.horizontal, 16).padding(.vertical, 12)
        }
        .scrollPosition($position)
        .defaultScrollAnchor(.bottom)
        .onScrollGeometryChange(for: Bool.self) { geometry in
            geometry.contentOffset.y + geometry.containerSize.height >= geometry.contentSize.height - 40
        } action: { _, atBottom in
            isAtBottom = atBottom
        }
        // A new message follows the bottom unless the reader scrolled up;
        // the account's own always shows.
        .onChange(of: conversation.lines.last?.id) { _, _ in
            if isAtBottom || conversation.lines.last?.fromMe == true { position.scrollTo(edge: .bottom) }
        }
        .onChange(of: conversation.isFriendTyping) { _, typing in
            if typing && isAtBottom { position.scrollTo(edge: .bottom) }
        }
        // Older messages arrive above: keep the one that was on top in place.
        .onChange(of: conversation.lines.first?.id) { previous, _ in
            if let previous, !isAtBottom { position.scrollTo(id: ConversationItem.ID.line(previous), anchor: .top) }
        }
        .accessibilityIdentifier("friends-conversation-messages")
    }

    /// The names of the games `line` names, so that the line redraws only
    /// when they change.
    private func gameNames(of line: SteamChatLine) -> [UInt32: String] {
        Dictionary(line.appIds.compactMap { appId in chat.gameName(appId).map { (appId, $0) } },
                   uniquingKeysWith: { first, _ in first })
    }

    private func banner(_ message: String, dismiss: (() -> Void)?) -> some View {
        HStack(spacing: 10) {
            Label(message, systemImage: "exclamationmark.circle")
                .frame(maxWidth: .infinity, alignment: .leading)
            if let dismiss {
                Button(action: dismiss) { Image(systemName: "xmark").font(.system(size: 10, weight: .semibold)) }
                    .obeliskButtonStyle(.plain)
                    .accessibilityLabel(L10n.string("ui.chat.dismissError"))
                    .help(L10n.string("ui.chat.dismissError"))
            }
        }
        .font(.system(size: 12)).foregroundStyle(.secondary)
        .padding(.horizontal, 18).padding(.vertical, 9)
        .background(ObeliskStyle.accent.opacity(0.06))
        .accessibilityIdentifier("friends-conversation-error")
    }

    private var composer: some View {
        let draft = chat.draft(for: steamID)
        let canSend = chat.isConnected && !conversation.isSending
            && !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        return HStack(alignment: .bottom, spacing: 8) {
            TextField(L10n.string("ui.chat.composer", friend.displayName),
                      text: Binding(get: { chat.draft(for: steamID) }, set: { chat.setDraft($0, for: steamID) }),
                      axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(1...6)
                .font(.system(size: 13))
                .focused($focusedControl, equals: .composer)
                .onKeyPress(keys: [.return], phases: .down) { press in
                    guard !press.modifiers.contains(.shift),
                          (NSApp.keyWindow?.firstResponder as? NSTextView)?.hasMarkedText() != true else { return .ignored }
                    send()
                    return .handled
                }
                .padding(.horizontal, 13).padding(.vertical, 9)
                .background(.primary.opacity(0.05), in: RoundedRectangle(cornerRadius: 17))
                .overlay { RoundedRectangle(cornerRadius: 17).strokeBorder(.primary.opacity(0.08), lineWidth: 1) }
                .accessibilityLabel(L10n.string("ui.chat.composer", friend.displayName))
                .accessibilityIdentifier("friends-conversation-composer")
            Button(action: send) {
                Group {
                    if conversation.isSending { ProgressView().controlSize(.small).tint(.white) }
                    else { Image(systemName: "arrow.up").font(.system(size: 14, weight: .bold)) }
                }
                .foregroundStyle(.white)
                .frame(width: 34, height: 34)
                .background(ObeliskStyle.accent.opacity(canSend || conversation.isSending ? 1 : 0.35), in: Circle())
                .contentShape(Circle())
            }
            .obeliskButtonStyle(.plain)
            .focused($focusedControl, equals: .send)
            .disabled(!canSend)
            .accessibilityLabel(L10n.string("ui.chat.send"))
            .accessibilityIdentifier("friends-conversation-send")
            .help(L10n.string("ui.chat.send"))
        }
        .padding(.horizontal, 14).padding(.vertical, 12)
    }

    private func send() {
        chat.send(to: steamID)
        position.scrollTo(edge: .bottom)
    }

    private func activate(_ control: FriendsPanelFocus.Control) -> Bool {
        switch control {
        case .composer, .send: send()
        case .back: _ = focus.back()
        case .invite:
            guard let offer = chat.inviteOffer(for: friend) else { return false }
            chat.invite(steamID, to: offer)
        default: return false
        }
        return true
    }

    private func synchronizeFocus(invites: Bool) {
        focus.activate = activate
        focus.controls = [.back] + (invites ? [.invite] : []) + (closable ? [.close] : []) + [.composer, .send]
        if !focus.controls.contains(focus.target) { focus.target = .composer }
        focusedControl = focus.target
    }
}

/// The conversation's rows: its lines, with the day or time where it changes.
enum ConversationItem: Identifiable, Equatable {
    enum ID: Hashable {
        case separator(SteamChatLine.ID)
        case line(SteamChatLine.ID)
    }

    case separator(SteamChatLine.ID, Date, showsDay: Bool)
    case line(SteamChatLine)

    var id: ID {
        switch self {
        case .separator(let line, _, _): .separator(line)
        case .line(let line): .line(line.id)
        }
    }

    /// A new day starts with its date; a pause of a quarter hour shows the time.
    static func items(_ lines: [SteamChatLine], calendar: Calendar = .current) -> [ConversationItem] {
        var items: [ConversationItem] = []
        var previous: Date?
        for line in lines {
            let date = line.date
            if let previous, calendar.isDate(previous, inSameDayAs: date) {
                if date.timeIntervalSince(previous) >= 15 * 60 { items.append(.separator(line.id, date, showsDay: false)) }
            } else {
                items.append(.separator(line.id, date, showsDay: true))
            }
            items.append(.line(line))
            previous = date
        }
        return items
    }
}

private struct ChatSeparator: View {
    let date: Date
    let showsDay: Bool

    var body: some View {
        Text(title)
            .font(.system(size: 11, weight: .medium)).foregroundStyle(.secondary)
            .frame(maxWidth: .infinity)
            .padding(.top, 10).padding(.bottom, 4)
    }

    private var title: String {
        let time = date.formatted(Date.FormatStyle(date: .omitted, time: .shortened, locale: L10n.locale))
        guard showsDay else { return time }
        let calendar = Calendar.current
        let day: String
        if calendar.isDateInToday(date) { day = L10n.string("ui.chat.today") }
        else if calendar.isDateInYesterday(date) { day = L10n.string("ui.chat.yesterday") }
        else {
            var style = Date.FormatStyle(locale: L10n.locale).weekday(.wide).day().month(.wide)
            if !calendar.isDate(date, equalTo: .now, toGranularity: .year) { style = style.year() }
            day = date.formatted(style)
        }
        return [day, time].joined(separator: " · ")
    }
}

private struct ChatLineView: View {
    let line: SteamChatLine
    let friendName: String
    let friendSteamID: UInt64
    /// The games the line names, when known.
    let gameNames: [UInt32: String]
    /// Joins the invitation the line carries.
    let invitations: SteamReceivedInvitations?

    var body: some View {
        switch line.content {
        case .message(let fromMe, _):
            HStack(spacing: 0) {
                if fromMe { Spacer(minLength: 64) }
                VStack(alignment: fromMe ? .trailing : .leading, spacing: 4) {
                    ForEach(Array(line.contents.enumerated()), id: \.offset) { _, content in
                        switch content {
                        case .text(let text):
                            ChatBubble(text: text, fromMe: fromMe)
                                // The sender shows only through side and color.
                                .accessibilityElement(children: .combine)
                                .accessibilityLabel(fromMe ? L10n.string("ui.chat.a11y.mine", String(text.characters))
                                                           : L10n.string("ui.chat.a11y.theirs", friendName, String(text.characters)))
                                .accessibilityValue(line.date.formatted(Date.FormatStyle(date: .abbreviated, time: .shortened,
                                                                                         locale: L10n.locale)))
                        case .invitation(let invitation):
                            ChatInvitationCard(invitation: invitation, fromMe: fromMe, friendName: friendName,
                                               gameName: invitation.appId.flatMap { gameNames[$0] },
                                               join: fromMe ? nil : joinable, invitations: invitations)
                        }
                    }
                }
                .help(line.date.formatted(Date.FormatStyle(date: .abbreviated, time: .shortened, locale: L10n.locale)))
                if !fromMe { Spacer(minLength: 64) }
            }
        case .invitationSent(let appId):
            Label(gameNames[appId].map { L10n.string("ui.chat.invite.sent", friendName, $0) }
                      ?? L10n.string("ui.chat.invite.sentUnknown", friendName),
                  systemImage: "paperplane")
                .font(.system(size: 11, weight: .medium)).foregroundStyle(.secondary)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 6)
                .accessibilityIdentifier("friends-conversation-invite-sent")
        }
    }
}

extension ChatLineView {
    /// The friend's invitation the line carries, as the player can join it.
    private var joinable: SteamReceivedInvite? {
        guard let target = line.joinTarget else { return nil }
        return SteamReceivedInvite(friendSteamId: friendSteamID, appId: target.appId, invite: target.invite,
                                   receivedAt: UInt64(line.timestamp))
    }
}

private struct ChatBubble: View {
    let text: AttributedString
    let fromMe: Bool

    /// Links keep an underline: on the account's bubble they share the text's color.
    private var underlinedText: AttributedString {
        var text = text
        for run in text.runs where run.link != nil { text[run.range].underlineStyle = .single }
        return text
    }

    var body: some View {
        Text(underlinedText)
            .font(.system(size: 13))
            .foregroundStyle(fromMe ? .white : .primary)
            .tint(fromMe ? .white : ObeliskStyle.accent)
            .textSelection(.enabled)
            .padding(.horizontal, 12).padding(.vertical, 8)
            .background {
                if fromMe {
                    RoundedRectangle(cornerRadius: 16)
                        .fill(LinearGradient(colors: [ObeliskStyle.accent, ObeliskStyle.accent.mix(with: .purple, by: 0.18)],
                                             startPoint: .topLeading, endPoint: .bottomTrailing))
                } else {
                    RoundedRectangle(cornerRadius: 16).fill(.primary.opacity(0.08))
                }
            }
    }
}

/// An invitation Steam wrote into the conversation. A friend's joins their
/// game while it lasts, as Valve's chat lets it.
private struct ChatInvitationCard: View {
    let invitation: SteamChatInvitation
    let fromMe: Bool
    let friendName: String
    let gameName: String?
    /// The friend's invitation, to join.
    var join: SteamReceivedInvite?
    var invitations: SteamReceivedInvitations?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                Image(systemName: "gamecontroller.fill")
                    .font(.system(size: 15)).foregroundStyle(.white)
                    .frame(width: 34, height: 34)
                    .background(LinearGradient(colors: ObeliskStyle.launchGradient, startPoint: .leading, endPoint: .trailing),
                                in: RoundedRectangle(cornerRadius: 9))
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.system(size: 12, weight: .semibold)).fixedSize(horizontal: false, vertical: true)
                    if fromMe, let gameName {
                        Text(gameName).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                    }
                }
            }
            if let join, let invitations { joinControl(join, invitations: invitations) }
        }
        .padding(10)
        .frame(maxWidth: 280, alignment: .leading)
        .background(.primary.opacity(0.05), in: RoundedRectangle(cornerRadius: 14))
        .overlay { RoundedRectangle(cornerRadius: 14).strokeBorder(.primary.opacity(0.1), lineWidth: 1) }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("friends-conversation-invitation")
    }

    /// Join while the invitation lasts; its failure, or why Play is
    /// unavailable, shows under it.
    @ViewBuilder
    private func joinControl(_ invite: SteamReceivedInvite, invitations: SteamReceivedInvitations) -> some View {
        if invitations.isCurrent(invite) {
            VStack(alignment: .leading, spacing: 6) {
                InvitationJoinButton(invite: invite, invitations: invitations, friendName: friendName,
                                     identifier: "friends-conversation-invitation-join")
                if let message = invitations.failures[SteamReceivedInvitations.Key(invite)]
                    ?? invitations.availability(appId: invite.appId).reason {
                    Text(message).font(.system(size: 11)).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        } else {
            Text(L10n.string("ui.invites.expired")).font(.system(size: 11)).foregroundStyle(.secondary)
                .accessibilityIdentifier("friends-conversation-invitation-expired")
        }
    }

    private var title: String {
        if fromMe { return L10n.string("ui.chat.invite.sentCard") }
        return gameName.map { L10n.string("ui.chat.invite.received", friendName, $0) }
            ?? L10n.string("ui.chat.invite.receivedUnknown", friendName)
    }
}

/// The invitation's glass capsule, green as the game's launch button.
private struct ChatInviteButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        Appearance(configuration: configuration)
    }

    private struct Appearance: View {
        let configuration: Configuration
        @Environment(\.isEnabled) private var isEnabled
        @Environment(\.isFocused) private var isFocused
        @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
        @State private var isHovered = false

        var body: some View {
            let label = configuration.label
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(.white)
                .shadow(color: .black.opacity(0.2), radius: 1, y: 0.5)
                .padding(.horizontal, 13)
                .frame(height: 30)
                .background {
                    Capsule()
                        .fill(LinearGradient(colors: ObeliskStyle.launchGradient, startPoint: .leading, endPoint: .trailing))
                        .opacity(reduceTransparency ? 1 : 0.85)
                        .brightness(configuration.isPressed ? -0.08 : isHovered && isEnabled ? 0.06 : 0)
                }
                .overlay { if isFocused { Capsule().strokeBorder(.white.opacity(0.8), lineWidth: 2) } }
            Group {
                if reduceTransparency { label }
                else { label.glassEffect(.regular.interactive(isEnabled), in: .capsule) }
            }
            .contentShape(.capsule)
            .onHover { isHovered = $0 }
            .pointerCursor()
        }
    }
}
