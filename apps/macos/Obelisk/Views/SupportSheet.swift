import AppKit
import SwiftUI

/// The ways to support Obelisk, each a card: a star on GitHub and the Discord
/// server open their page, beside how many stars and members they have, and
/// sharing copies the link a friend gets Obelisk from.
struct SupportSheet: View {
    let model: SupportModel
    let close: () -> Void
    /// Whether the link was copied since the sheet opened.
    @State private var copiedLink = false

    /// Discord's own color, top to bottom.
    private static let discordColors = [Color(red: 0.45, green: 0.5, blue: 0.98), Color(red: 0.345, green: 0.396, blue: 0.949)]

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            SheetHeader(title: L10n.string("ui.support.title", AppIdentity.name),
                        subtitle: L10n.string("ui.support.message", AppIdentity.name),
                        symbol: "heart.fill", colors: TileColors.red, close: close, closeIdentifier: "support-close")
            VStack(spacing: 10) {
                SupportWay(title: L10n.string("ui.support.star"),
                           detail: L10n.string("ui.support.star_description", AppIdentity.name),
                           identifier: "support-star") {
                    NSWorkspace.shared.open(AppIdentity.repository)
                } icon: {
                    LogoTile(asset: "GitHubLogo", colors: TileColors.graphite, size: 36)
                } accessory: {
                    if let stars = model.stars {
                        SupportCount(symbol: "star.fill", count: stars)
                            .transition(.opacity)
                    }
                    Image(systemName: "arrow.up.right")
                }
                .accessibilityValue(model.stars.map { L10n.string("ui.support.stars", Int64($0)) } ?? "")
                SupportWay(title: L10n.string("ui.support.discord"),
                           detail: L10n.string("ui.support.discord_description"),
                           identifier: "support-discord") {
                    NSWorkspace.shared.open(SupportModel.discord)
                } icon: {
                    LogoTile(asset: "DiscordLogo", colors: Self.discordColors, size: 36)
                } accessory: {
                    if let members = model.members {
                        SupportCount(symbol: "person.2.fill", count: members)
                            .transition(.opacity)
                    }
                    Image(systemName: "arrow.up.right")
                }
                .accessibilityValue(model.members.map { L10n.string("ui.support.members", Int64($0)) } ?? "")
                SupportWay(title: L10n.string("ui.support.share"),
                           detail: L10n.string("ui.support.share_description"),
                           identifier: "support-share") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(SupportModel.site.absoluteString, forType: .string)
                    copiedLink = true
                } icon: {
                    SymbolTile(name: "square.and.arrow.up", colors: TileColors.violet, size: 36)
                } accessory: {
                    if copiedLink {
                        Text(L10n.string("ui.support.link_copied"))
                            .transition(.opacity)
                    }
                    Image(systemName: copiedLink ? "checkmark" : "doc.on.doc")
                }
                .accessibilityValue(copiedLink ? L10n.string("ui.support.link_copied") : "")
            }
        }
        .padding(24)
        .frame(width: 480)
        .tint(ObeliskStyle.accent)
        .animation(.easeInOut(duration: 0.2), value: model.stars)
        .animation(.easeInOut(duration: 0.2), value: model.members)
        .animation(.easeInOut(duration: 0.2), value: copiedLink)
        .accessibilityIdentifier("support-sheet")
    }
}

/// A way to support Obelisk, as a card to press: its tile, what it is over
/// what it brings, then what pressing it does.
private struct SupportWay<Icon: View, Accessory: View>: View {
    let title: String
    let detail: String
    let identifier: String
    let action: () -> Void
    @ViewBuilder let icon: Icon
    @ViewBuilder let accessory: Accessory

    var body: some View {
        ChoiceCard(identifier: identifier, action: action) {
            ObeliskRow(title: title) {
                icon
            } detail: {
                Text(detail)
            } accessory: {
                HStack(spacing: 8) { accessory }
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(.secondary)
            }
            .multilineTextAlignment(.leading)
        }
        .accessibilityLabel(title)
        .accessibilityHint(detail)
    }
}

/// How many stars or members a way has, on the gradient getobelisk.app shows
/// them on.
private struct SupportCount: View {
    let symbol: String
    let count: Int

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: symbol)
                .font(.system(size: 10, weight: .semibold))
            Text(count.formatted(.number.locale(L10n.locale)))
                .font(.system(size: 12, weight: .bold))
                .monospacedDigit()
        }
        .foregroundStyle(.white)
        .padding(.horizontal, 9)
        .frame(height: 22)
        .background(LinearGradient(colors: [Color(red: 0.639, green: 0.294, blue: 1),
                                            Color(red: 0.133, green: 0.784, blue: 0.91)],
                                   startPoint: .leading, endPoint: .trailing), in: .capsule)
        .accessibilityHidden(true)
    }
}
