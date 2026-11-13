import AppKit
import SwiftUI

/// What a game's management menu deletes: its installation, or the files of
/// a download that stopped before it was installed.
enum GameRemoval {
    case uninstall, download

    /// The menu item and the confirmation's button.
    var action: String {
        L10n.string(self == .uninstall ? "ui.game.uninstall" : "ui.game.delete_download")
    }

    func confirmation(_ gameName: String) -> String {
        L10n.string(self == .uninstall ? "ui.game.uninstall_title" : "ui.game.delete_download_title", gameName)
    }

    var message: String {
        L10n.string(self == .uninstall ? "ui.game.uninstall_message" : "ui.game.delete_download_message")
    }
}

struct GameManagementButton: View {
    let gameName: String
    let canVerify: Bool
    let canRemove: Bool
    /// Verifies the installed game's files, repairing or updating them; nil
    /// for a download that stopped first.
    let verify: (() -> Void)?
    let remove: () -> Void
    /// The game's directory, revealed in Finder from the menu.
    let directory: String
    var removal = GameRemoval.uninstall
    /// Opens a Windows build's launch settings; nil for a Mac build.
    var launchSettings: (() -> Void)?
    @State private var confirmsRemoval = false

    /// The menu opens while either of its actions may start, and whenever a
    /// Windows build's launch settings may change.
    private var isAvailable: Bool { canVerify || canRemove || launchSettings != nil }

    var body: some View {
        Menu {
            if let launchSettings {
                Button(L10n.string("ui.launch_settings.title"), systemImage: "slider.horizontal.3",
                       action: launchSettings)
                Divider()
            }
            Button(L10n.string("ui.game.show_files"), systemImage: "folder") {
                NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: directory)
            }
            if let verify {
                Button(L10n.string("ui.game.verify_files"), systemImage: "checkmark.shield", action: verify)
                    .disabled(!canVerify)
            }
            Divider()
            Button(removal.action, systemImage: "trash", role: .destructive) {
                confirmsRemoval = true
            }
            .disabled(!canRemove)
        } label: {
            StorefrontGlassIcon(systemName: "gearshape.fill", size: 15)
                .foregroundStyle(.primary)
        }
        .menuStyle(.button)
        .obeliskButtonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .disabled(!isAvailable)
        .accessibilityLabel(L10n.string("ui.game.manage_named", gameName))
        .accessibilityIdentifier("game-management-menu")
        .help(L10n.string("ui.game.manage"))
        .removalConfirmation(removal, gameName, isPresented: $confirmsRemoval, canRemove: canRemove,
                             remove: remove)
    }
}

/// The builds of a game its storefront offers both of, beside its launch
/// button: the page plays, installs and manages the one chosen, and a check
/// marks each installed one. The switch holds still while a Transfer of the
/// game runs.
struct GameBuildSwitch: View {
    let choice: GameBuildChoice
    let choose: (GamePlatform) -> Void
    @Namespace private var selection
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        HStack(spacing: 2) {
            segment(.windows, title: GamePlatform.windows.name)
            segment(.mac, title: GamePlatform.mac.name)
        }
        .padding(3)
        .frame(height: 36)
        .modifier(StorefrontControlGlass(interactive: false))
        .accessibilityElement(children: .contain)
        .accessibilityLabel(L10n.string("ui.game.build"))
        .accessibilityIdentifier("game-build-switch")
    }

    private func segment(_ build: GamePlatform, title: String) -> some View {
        let isChosen = choice.chosen == build
        let isInstalled = choice.installed.contains(build)
        return Button {
            withAnimation(.snappy(duration: 0.2)) { choose(build) }
        } label: {
            HStack(spacing: 5) {
                if isInstalled {
                    Image(systemName: "checkmark.circle.fill")
                        .font(.system(size: 12, weight: .bold))
                        // The gradient's deeper green where the chosen build's pill is light.
                        .foregroundStyle(ObeliskStyle.launchGradient[isChosen && colorScheme == .dark ? 1 : 0])
                        .accessibilityHidden(true)
                }
                Text(title).font(.system(size: 12, weight: .semibold))
            }
            // The chosen build reads as a solid pill, its text in the page's color.
            .foregroundStyle(isChosen ? AnyShapeStyle(Color(nsColor: .windowBackgroundColor))
                : AnyShapeStyle(.secondary))
            .padding(.horizontal, 12)
            .frame(maxHeight: .infinity)
            .background {
                if isChosen {
                    // Not `.primary`, which here follows the label's own text color.
                    Capsule().fill(Color(nsColor: .labelColor).opacity(0.92))
                        .matchedGeometryEffect(id: "chosen", in: selection)
                }
            }
            .contentShape(.capsule)
        }
        .obeliskButtonStyle(.plain)
        .disabled(choice.isLocked)
        .opacity(choice.isLocked && !isChosen ? 0.45 : 1)
        .accessibilityAddTraits(isChosen ? .isSelected : [])
        .accessibilityValue(isInstalled ? L10n.string("ui.game.build_installed") : "")
        .help(isInstalled ? L10n.string("ui.game.build_installed") : title)
    }
}

extension View {
    /// Asks before `removal` deletes `gameName`'s files.
    func removalConfirmation(_ removal: GameRemoval, _ gameName: String, isPresented: Binding<Bool>,
                             canRemove: Bool, remove: @escaping () -> Void) -> some View {
        alert(removal.confirmation(gameName), isPresented: isPresented) {
            Button(L10n.string("ui.common.cancel"), role: .cancel) {}
            Button(removal.action, role: .destructive, action: remove)
                .disabled(!canRemove)
        } message: {
            Text(removal.message)
        }
    }
}

/// A game page's launch row: its launch and management buttons, then its
/// metrics, then Obelisk's note on the game, when it has one.
struct GameLaunchRow<Buttons: View>: View {
    let metrics: GameMetrics
    var note: String?
    @ViewBuilder let buttons: Buttons

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 10) { buttons }
            GameMetricsRow(metrics: metrics)
            if let note {
                GameNote(text: note)
            }
        }
    }
}

/// What Obelisk tells players about a game, such as what works of it and
/// what does not yet: a metric's caption over a few lines.
struct GameNote: View {
    let text: String

    var body: some View {
        HStack(alignment: .top, spacing: 9) {
            GameMetricSymbol(name: "info.circle")
            VStack(alignment: .leading, spacing: 4) {
                GameMetricCaption(text: L10n.string("ui.game.note"))
                Text(text)
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: 560, alignment: .leading)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("game-note")
    }
}

/// What a game page's metrics report. The row orders them as Steam does and,
/// when it runs out of width, drops the least useful ones first.
struct GameMetrics {
    var isInstalled: Bool
    var size: String?
    var lastPlayed: String?
    var playtime: String?
    var cloud: CloudIndicatorButton?
    var achievements: GameAchievementsModel
    /// How the game runs on this Mac's chip, as other players measured it.
    var estimate: PerformanceEstimates.Estimate?
}

/// A game page's metrics, under its launch button.
struct GameMetricsRow: View {
    let metrics: GameMetrics

    var body: some View {
        // The last level keeps only the controls. When even they do not fit,
        // the Cloud value truncates.
        ViewThatFits(in: .horizontal) {
            ForEach(0...4, id: \.self) { metricRow(dropping: $0) }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// Each level drops one more metric: an installed game's size, then the
    /// last played date, the playtime and the space a game needs, which matters
    /// most before it is installed. The Cloud and achievement metrics are
    /// controls and never leave.
    private func metricRow(dropping level: Int) -> some View {
        HStack(spacing: 2) {
            if let cloud = metrics.cloud { cloud }
            if !metrics.isInstalled, level < 4, let size = metrics.size {
                GameMetricLabel(caption: L10n.string("ui.game.metric.space_required")) {
                    GameMetricSymbol(name: "internaldrive")
                } value: {
                    Text(size)
                }
            }
            if level < 2, let lastPlayed = metrics.lastPlayed {
                GameMetricLabel(caption: L10n.string("ui.game.metric.last_played")) {
                    GameMetricSymbol(name: "calendar")
                } value: {
                    Text(lastPlayed)
                }
            }
            if level < 3, let playtime = metrics.playtime {
                GameMetricLabel(caption: L10n.string("ui.game.metric.playtime")) {
                    GameMetricSymbol(name: "clock")
                } value: {
                    Text(playtime)
                }
            }
            if level < 3, let estimate = metrics.estimate, let fps = estimate.fpsMedian {
                GameExpectedFPSButton(estimate: estimate, fps: fps)
            }
            GameAchievementsButton(model: metrics.achievements)
            // An installed game's size moves last, where it leaves first.
            if metrics.isInstalled, level < 1, let size = metrics.size {
                GameMetricLabel(caption: L10n.string("ui.game.metric.size")) {
                    GameMetricSymbol(name: "internaldrive")
                } value: {
                    Text(size)
                }
            }
        }
        // Aligns the first metric's symbol with the launch button's edge.
        .padding(.leading, -10)
    }
}

/// The frame rate other players measured on this Mac's chip, or on the
/// nearest weaker one. It opens the game's page on getobelisk.app, with
/// every chip's numbers.
struct GameExpectedFPSButton: View {
    let estimate: PerformanceEstimates.Estimate
    let fps: Double
    @State private var isHovered = false
    @Environment(\.openURL) private var openURL

    private var value: String {
        let rate = String(Int(fps.rounded()))
        guard estimate.measuredOn != MacHardware.current.chip else {
            return L10n.string("ui.game.metric.expected_fps_value", rate)
        }
        let name = estimate.measuredOn
        let chip = name.hasPrefix("Apple ") ? String(name.dropFirst("Apple ".count)) : name
        return L10n.string("ui.game.metric.expected_fps_value_other_chip", rate, chip)
    }

    var body: some View {
        Button { estimate.page.map { openURL($0) } } label: {
            GameMetricLabel(caption: L10n.string("ui.game.metric.expected_fps"), isHighlighted: isHovered) {
                GameMetricSymbol(name: "gauge.with.needle")
            } value: {
                Text(value).monospacedDigit()
            }
        }
        .obeliskButtonStyle(.plain)
        .onHover { isHovered = $0 }
        .fixedSize()
        .accessibilityLabel(L10n.string("ui.game.metric.expected_fps"))
        .accessibilityValue(value)
        .accessibilityIdentifier("game-expected-fps")
        .help(L10n.string("ui.game.metric.expected_fps_help", String(estimate.sessions), estimate.measuredOn))
    }
}

/// The uppercase caption over a game page metric, and over the game's note.
private struct GameMetricCaption: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.system(size: 10, weight: .semibold))
            .tracking(0.4)
            .textCase(.uppercase)
            .foregroundStyle(.secondary)
    }
}

/// A game page metric: an uppercase caption over its value, after an optional
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
                GameMetricCaption(text: caption)
                value
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(.primary)
            }
            .lineLimit(1)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(.primary.opacity(isHighlighted ? 0.08 : 0), in: .capsule)
        .contentShape(.capsule)
    }
}

extension GameMetricLabel where Icon == EmptyView {
    init(caption: String, @ViewBuilder value: () -> Value) {
        self.init(caption: caption, icon: { EmptyView() }, value: value)
    }
}

/// The symbol before a game page metric.
struct GameMetricSymbol: View {
    let name: String
    var isWarning = false

    var body: some View {
        Image(systemName: name)
            .font(.system(size: 18, weight: .regular))
            .foregroundStyle(isWarning ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
            .frame(width: 26, height: 26)
    }
}

extension GamePlatform {
    /// How a game's page and its cards name the build.
    var name: String { L10n.string(self == .mac ? "ui.game.build_mac" : "ui.game.build_windows") }
}
