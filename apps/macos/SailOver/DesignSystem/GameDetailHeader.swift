import AppKit
import SwiftUI

struct GameManagementButton: View {
    let gameName: String
    let isAvailable: Bool
    let uninstall: () -> Void
    /// The installed game's directory, revealed in Finder from the menu.
    let directory: String
    @State private var confirmsUninstall = false
    @State private var isHovered = false

    var body: some View {
        Menu {
            Button(L10n.string("ui.game.show_files"), systemImage: "folder") {
                NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: directory)
            }
            Divider()
            Button(L10n.string("ui.game.uninstall"), systemImage: "trash", role: .destructive) {
                confirmsUninstall = true
            }
            .disabled(!isAvailable)
        } label: {
            Image(systemName: "gearshape.fill")
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(.primary)
                .frame(width: 36, height: 36)
                .background(.primary.opacity(isHovered ? 0.16 : 0.1), in: RoundedRectangle(cornerRadius: 6))
                .contentShape(RoundedRectangle(cornerRadius: 6))
        }
        .menuStyle(.button)
        .sailOverButtonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .disabled(!isAvailable)
        .onHover { isHovered = $0 }
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

/// What a game page's launch bar reports. The bar orders it as Steam does and,
/// when it runs out of width, drops the least useful metrics first.
struct GameMetrics {
    var isInstalled: Bool
    var size: String?
    var lastPlayed: String?
    var playtime: String?
    var cloud: CloudIndicatorButton?
    var achievements: GameAchievementsModel
}

struct GameDetailHeader<Artwork: View, Action: View, Tools: View>: View {
    let title: String
    let libraryTitle: String
    let metrics: GameMetrics
    let back: () -> Void
    private let artwork: Artwork
    private let action: Action
    private let tools: Tools

    init(
        title: String,
        libraryTitle: String,
        metrics: GameMetrics,
        back: @escaping () -> Void,
        @ViewBuilder artwork: () -> Artwork,
        @ViewBuilder action: () -> Action,
        @ViewBuilder tools: () -> Tools = { EmptyView() }
    ) {
        self.title = title
        self.libraryTitle = libraryTitle
        self.metrics = metrics
        self.back = back
        self.artwork = artwork()
        self.action = action()
        self.tools = tools()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 23) {
            Button(action: back) {
                Label(libraryTitle, systemImage: "chevron.left")
                    .font(.system(size: 12, weight: .medium))
            }
            .sailOverButtonStyle(.borderless)

            VStack(alignment: .leading, spacing: 0) {
                Text(title)
                    .font(.system(size: 30, weight: .bold)).tracking(-0.7)
                    .foregroundStyle(.white).shadow(radius: 3)
                    .padding(.horizontal, 24).padding(.bottom, 18)
                    .frame(maxWidth: .infinity, minHeight: 280, alignment: .bottomLeading)
                launchBar
            }
            .background {
                Color.clear
                    .overlay { artwork }
                    .overlay {
                        LinearGradient(colors: [.clear, .black.opacity(0.85)], startPoint: .top, endPoint: .bottom)
                    }
                    // The scaled artwork can extend past its visual clip and cover nearby controls.
                    .allowsHitTesting(false)
            }
            .clipShape(RoundedRectangle(cornerRadius: 13))
        }
    }

    /// The bar always sits on the darkened artwork, so it reads light on dark
    /// in both appearances.
    private var launchBar: some View {
        HStack(spacing: 0) {
            action
            // The last level keeps only the controls. When even they do not fit,
            // the Cloud value truncates.
            ViewThatFits(in: .horizontal) {
                ForEach(0...4, id: \.self) { metricRow(dropping: $0) }
            }
            .padding(.leading, 14)
            // Offered all the width the spacer does not need.
            .layoutPriority(1)
            Spacer(minLength: 12)
            tools
        }
        .foregroundStyle(.white)
        .padding(.horizontal, 14).padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.black.opacity(0.35))
        .overlay(alignment: .top) {
            Rectangle().fill(.white.opacity(0.08)).frame(height: 1)
        }
    }

    /// Each level drops one more metric: an installed game's size, then the
    /// last played date, the playtime and the space a game needs, which matters
    /// most before it is installed. The Cloud and achievement metrics are
    /// controls and never leave.
    private func metricRow(dropping level: Int) -> some View {
        HStack(spacing: 4) {
            if let cloud = metrics.cloud { cloud }
            if !metrics.isInstalled, level < 4, let size = metrics.size {
                GameMetricLabel(caption: L10n.string("ui.game.metric.space_required")) { Text(size) }
            }
            if level < 2, let lastPlayed = metrics.lastPlayed {
                GameMetricLabel(caption: L10n.string("ui.game.metric.last_played")) { Text(lastPlayed) }
            }
            if level < 3, let playtime = metrics.playtime {
                GameMetricLabel(caption: L10n.string("ui.game.metric.playtime")) {
                    GameMetricSymbol(name: "clock")
                } value: {
                    Text(playtime)
                }
            }
            GameAchievementsButton(model: metrics.achievements)
            // An installed game's size moves last, where it leaves first.
            if metrics.isInstalled, level < 1, let size = metrics.size {
                GameMetricLabel(caption: L10n.string("ui.game.metric.size")) { Text(size) }
            }
        }
    }
}

/// A launch bar metric: an uppercase caption over its value, after an optional
/// large symbol. A metric that is a control highlights under the pointer.
struct GameMetricLabel<Icon: View, Value: View>: View {
    let caption: String
    private let isHighlighted: Bool
    private let icon: Icon
    private let value: Value

    init(caption: String, isHighlighted: Bool = false, @ViewBuilder icon: () -> Icon,
         @ViewBuilder value: () -> Value) {
        self.caption = caption
        self.isHighlighted = isHighlighted
        self.icon = icon()
        self.value = value()
    }

    var body: some View {
        HStack(spacing: 9) {
            icon
            VStack(alignment: .leading, spacing: 4) {
                Text(caption)
                    .font(.system(size: 11, weight: .semibold))
                    .tracking(0.3)
                    .textCase(.uppercase)
                    .foregroundStyle(.primary.opacity(0.88))
                value
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
            }
            .lineLimit(1)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        // The bar is always dark, so its spinners must be too.
        .environment(\.colorScheme, .dark)
        .background(.primary.opacity(isHighlighted ? 0.1 : 0), in: RoundedRectangle(cornerRadius: 6))
        .contentShape(RoundedRectangle(cornerRadius: 6))
    }
}

extension GameMetricLabel where Icon == EmptyView {
    init(caption: String, @ViewBuilder value: () -> Value) {
        self.init(caption: caption, icon: { EmptyView() }, value: value)
    }
}

/// The large symbol before a launch bar metric.
struct GameMetricSymbol: View {
    let name: String
    var isWarning = false

    var body: some View {
        Image(systemName: name)
            .font(.system(size: 22, weight: .light))
            .foregroundStyle(isWarning ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
            .frame(width: 26, height: 26)
    }
}
