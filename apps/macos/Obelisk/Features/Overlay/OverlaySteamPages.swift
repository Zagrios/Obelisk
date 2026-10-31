import SwiftUI

/// Friends' invitations while a Steam game runs: the player joins one into
/// that game, back in it at once, or dismisses one.
struct OverlayInvitations: View {
    let overlay: GameOverlayController
    /// The friends column's narrow rows put their buttons under the text.
    var compact = false

    var body: some View {
        let invites = overlay.receivedInvites
        if overlay.hasEpicSocialOverlay {
            VStack(alignment: .leading, spacing: 8) {
                Text(L10n.string("ui.overlay.epicSocial.detail"))
                    .font(.caption).foregroundStyle(.secondary)
                Text(L10n.string("ui.overlay.epicSocial.shortcut"))
                    .font(.caption)
                    .foregroundStyle(overlay.epicSocialNeedsShortcut ? Color.orange : Color.secondary)
                if overlay.epicSocialNeedsShortcut {
                    Button(L10n.string("ui.overlay.epicSocial.return"), action: overlay.hide)
                        .buttonStyle(.bordered)
                } else {
                    Button(L10n.string("ui.overlay.epicSocial.invite"), action: overlay.openEpicSocialOverlay)
                        .buttonStyle(.borderedProminent)
                }
            }
            .accessibilityIdentifier("epic-social-invitations")
        }
        if !invites.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                Text(L10n.string("ui.overlay.invites.title"))
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(.secondary)
                    .accessibilityAddTraits(.isHeader)
                ForEach(invites, id: \.self) { invite in
                    OverlayInvitationRow(overlay: overlay, invite: invite, compact: compact)
                }
            }
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("game-overlay-invites")
        }
    }
}

private struct OverlayInvitationRow: View {
    let overlay: GameOverlayController
    let invite: SteamReceivedInvite
    let compact: Bool

    var body: some View {
        let friend = overlay.sender(of: invite)
        let key = SteamReceivedInvitations.Key(invite)
        let invitations = overlay.steam.invitations
        let accepting = invitations.accepting == key
        let failure = invitations.failures[key]
        let availability = invitations.availability(appId: invite.appId)
        let name = friend?.displayName ?? L10n.string("ui.overlay.invites.someone")
        let summary = HStack(spacing: 12) {
            if let friend {
                FriendAvatar(friend: friend, avatars: overlay.avatars, size: 36)
            } else {
                Image(systemName: "person.fill")
                    .font(.system(size: 16))
                    .foregroundStyle(.secondary)
                    .frame(width: 36, height: 36)
                    .background(.primary.opacity(0.08), in: RoundedRectangle(cornerRadius: 9))
                    .accessibilityHidden(true)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(name)
                    .font(.system(size: 13, weight: .semibold))
                    .lineLimit(1)
                TimelineView(.everyMinute) { context in
                    Text(L10n.string("ui.overlay.invites.subtitle", overlay.steam.gameName(appId: invite.appId) ?? L10n.string("ui.invites.unknownGame"),
                                     Self.received(invite, now: context.date)))
                }
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
                .lineLimit(2)
                if let failure {
                    Label(failure, systemImage: "exclamationmark.triangle.fill")
                        .font(.system(size: 11))
                        .foregroundStyle(.red)
                        .lineLimit(2)
                        .accessibilityIdentifier("game-overlay-invite-error-\(invite.friendSteamId)")
                }
            }
            Spacer(minLength: 0)
        }
        let buttons = HStack(spacing: 8) {
            Button { overlay.dismiss(invite) } label: {
                Text(L10n.string("ui.overlay.invites.dismiss"))
                    .font(.system(size: 12, weight: .semibold))
                    .padding(.horizontal, 12).frame(height: 28)
                    .background(.primary.opacity(0.08), in: Capsule())
                    .contentShape(Capsule())
            }
            .obeliskButtonStyle(.plain)
            .disabled(accepting)
            .accessibilityLabel(L10n.string("ui.overlay.invites.dismissNamed", name))
            .accessibilityIdentifier("game-overlay-invite-dismiss-\(invite.friendSteamId)")
            Button { overlay.accept(invite) } label: {
                HStack(spacing: 6) {
                    if accepting { ProgressView().controlSize(.mini).tint(.white) }
                    else { Image(systemName: "gamecontroller.fill") }
                    Text(L10n.string(failure != nil ? "ui.friends.retry" : "ui.overlay.invites.join"))
                }
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(.white)
                .padding(.horizontal, 12).frame(height: 28)
                .background(LinearGradient(colors: ObeliskStyle.launchGradient, startPoint: .leading, endPoint: .trailing),
                            in: Capsule())
                .contentShape(Capsule())
            }
            .obeliskButtonStyle(.plain)
            .disabled(invitations.accepting != nil || !availability.canJoin)
            .help(availability.reason ?? "")
            .accessibilityLabel(L10n.string("ui.overlay.invites.joinNamed", name))
            .accessibilityIdentifier("game-overlay-invite-join-\(invite.friendSteamId)")
        }
        Group {
            if compact {
                VStack(alignment: .trailing, spacing: 8) {
                    summary
                    buttons
                }
            } else {
                HStack(spacing: 8) {
                    summary
                    buttons
                }
            }
        }
        .padding(10)
        .background(.primary.opacity(0.05), in: RoundedRectangle(cornerRadius: 12))
    }

    /// How long ago the game's Steam received it.
    static func received(_ invite: SteamReceivedInvite, now: Date) -> String {
        let date = min(Date(timeIntervalSince1970: TimeInterval(invite.receivedAt)), now)
        return date.formatted(.relative(presentation: .named).locale(L10n.locale))
    }
}

/// The in-game purchase the Steam game asks Steam to authorize. The game
/// tells Steam no price Obelisk can show: the game's own offer tells it.
struct OverlayPurchasePage: View {
    let overlay: GameOverlayController
    let purchase: SteamPurchaseAuthorization

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Label(L10n.string("ui.overlay.purchase.title"), systemImage: "creditcard")
                .font(.title2.weight(.semibold))
                .accessibilityAddTraits(.isHeader)
            Text(L10n.string("ui.overlay.purchase.message", overlay.gameTitle))
                .font(.system(size: 14))
                .fixedSize(horizontal: false, vertical: true)
            Text(L10n.string("ui.overlay.purchase.charge"))
                .font(.system(size: 13))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if let pending = purchase.pending {
                Text(L10n.string("ui.overlay.purchase.order", String(pending.orderId)))
                    .font(.system(size: 11))
                    .foregroundStyle(.tertiary)
                    .textSelection(.enabled)
            }
            if let error = purchase.error {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .font(.system(size: 12))
                    .foregroundStyle(.red)
                    .accessibilityIdentifier("game-overlay-purchase-error")
            }
            HStack(spacing: 12) {
                Spacer()
                if purchase.isAnswering { ProgressView().controlSize(.small) }
                Button { overlay.answerPurchase(approve: false) } label: {
                    Text(L10n.string("ui.overlay.purchase.deny"))
                        .font(.system(size: 13, weight: .semibold))
                        .padding(.horizontal, 18).frame(height: 34)
                        .background(.primary.opacity(0.08), in: Capsule())
                        .contentShape(Capsule())
                }
                .obeliskButtonStyle(.plain)
                .disabled(purchase.isAnswering)
                .accessibilityIdentifier("game-overlay-purchase-deny")
                Button { overlay.answerPurchase(approve: true) } label: {
                    Text(L10n.string("ui.overlay.purchase.approve"))
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(.white)
                        .padding(.horizontal, 18).frame(height: 34)
                        .background(LinearGradient(colors: ObeliskStyle.launchGradient, startPoint: .leading,
                                                   endPoint: .trailing), in: Capsule())
                        .contentShape(Capsule())
                }
                .obeliskButtonStyle(.plain)
                .disabled(purchase.isAnswering)
                .accessibilityIdentifier("game-overlay-purchase-approve")
            }
        }
        .padding(28)
        .frame(maxWidth: 560, alignment: .leading)
        .modifier(StorefrontPanelGlass(cornerRadius: 24))
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("game-overlay-purchase-page")
    }
}
