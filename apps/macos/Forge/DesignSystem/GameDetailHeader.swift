import SwiftUI

struct GameManagementButton: View {
    let gameName: String
    let isAvailable: Bool
    let uninstall: () -> Void
    @State private var confirmsUninstall = false

    var body: some View {
        Menu {
            Button(L10n.string("ui.game.uninstall"), systemImage: "trash", role: .destructive) {
                confirmsUninstall = true
            }
        } label: {
            Image(systemName: "gearshape.fill")
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(.primary)
                .frame(width: 44, height: 44)
                .background(.primary.opacity(0.1), in: RoundedRectangle(cornerRadius: 10))
                .contentShape(RoundedRectangle(cornerRadius: 10))
        }
        .menuStyle(.button)
        .forgeButtonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .disabled(!isAvailable)
        .accessibilityLabel(L10n.string("ui.game.manage_named", gameName))
        .accessibilityIdentifier("game-management-menu")
        .help(L10n.string("ui.game.manage"))
        .alert(L10n.string("ui.game.uninstall_title", gameName), isPresented: $confirmsUninstall) {
            Button(L10n.string("ui.common.cancel"), role: .cancel) {}
            Button(L10n.string("ui.game.uninstall"), role: .destructive, action: uninstall)
                .disabled(!isAvailable)
        } message: {
            Text(L10n.string("ui.game.uninstall_message"))
        }
    }
}

struct GameDetailHeader<Artwork: View, Action: View>: View {
    let title: String
    let libraryTitle: String
    let playtime: String
    let lastPlayed: String
    let size: String
    let back: () -> Void
    private let artwork: Artwork
    private let action: Action

    init(
        title: String,
        libraryTitle: String,
        playtime: String,
        lastPlayed: String,
        size: String,
        back: @escaping () -> Void,
        @ViewBuilder artwork: () -> Artwork,
        @ViewBuilder action: () -> Action
    ) {
        self.title = title
        self.libraryTitle = libraryTitle
        self.playtime = playtime
        self.lastPlayed = lastPlayed
        self.size = size
        self.back = back
        self.artwork = artwork()
        self.action = action()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 23) {
            Button(action: back) {
                Label(libraryTitle, systemImage: "chevron.left")
                    .font(.system(size: 12, weight: .medium))
            }
            .forgeButtonStyle(.borderless)

            Color.clear
                .frame(height: 280)
                .overlay { artwork }
                .overlay(alignment: .bottom) {
                    LinearGradient(colors: [.clear, .black.opacity(0.84)], startPoint: .top, endPoint: .bottom)
                }
                .overlay(alignment: .bottomLeading) {
                    Text(title)
                        .font(.system(size: 30, weight: .bold)).tracking(-0.7)
                        .foregroundStyle(.white).shadow(radius: 3).padding(24)
                }
                .clipShape(RoundedRectangle(cornerRadius: 13))
                // The scaled artwork can extend past its visual clip and cover nearby controls.
                .allowsHitTesting(false)

            HStack(spacing: 24) {
                metric(L10n.string("ui.game.metric.playtime"), value: playtime, symbol: "clock")
                metric(L10n.string("ui.game.metric.last_played"), value: lastPlayed, symbol: "calendar")
                metric(L10n.string("ui.game.metric.size"), value: size, symbol: "internaldrive")
                Spacer(minLength: 8)
                action
            }
        }
    }

    private func metric(_ title: String, value: String, symbol: String) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            Label(title, systemImage: symbol).font(.system(size: 10, weight: .semibold)).foregroundStyle(.secondary).lineLimit(1)
            Text(value).font(.system(size: 14, weight: .medium)).lineLimit(1)
        }
    }
}
