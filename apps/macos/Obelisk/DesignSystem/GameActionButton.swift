import SwiftUI

/// A game's Game action as its page and its library's hero show it. It reads
/// the game's progress in its own body, so a progress tick redraws the button
/// alone.
struct GameActionButton: View {
    let action: GameAction
    /// Names the game a Force Quit asks about.
    let gameName: String
    /// A line under the title while the button waits to be pressed, such as
    /// the space a download needs.
    var subtitle: String? = nil
    /// Lets more of the glass show through its color, as on a game's page,
    /// over the page's artwork.
    var translucent = false
    /// The Force Quit its player is asked to confirm.
    @State private var asking: GameAction?

    /// A step outranks a byte-counted transfer: it has no per-file title to show.
    /// A closing game shows as a step, and so does a wait in the library, as
    /// its cards show one busy.
    private enum Mode { case idle, transferring, step }
    private var mode: Mode {
        switch action.kind {
        case .step, .running(.closing): .step
        case .waiting where action.place == .library: .step
        case .transferring: .transferring
        default: .idle
        }
    }

    /// Set while the game runs: the button then ends it.
    private var quitStage: GameQuitStage? {
        if case let .running(stage) = action.kind { stage } else { nil }
    }

    private var colors: [Color] {
        switch quitStage {
        case .quit, .closing: ObeliskStyle.quitGradient
        case .forceQuit: ObeliskStyle.forceQuitGradient
        case nil: action.launches ? ObeliskStyle.launchGradient : ObeliskStyle.installGradient
        }
    }

    private var symbol: String {
        if quitStage != nil { return "stop.fill" }
        return action.launches ? "play.fill" : "arrow.down.to.line"
    }

    private func showsProgress(_ transfer: GameTransfer?) -> Bool {
        switch mode {
        case .step: true
        case .transferring: transfer?.isInProgress ?? true
        case .idle: false
        }
    }
    private func helpText(_ transfer: GameTransfer?) -> String {
        switch quitStage {
        case .closing: L10n.string("ui.game.closing_help")
        case .forceQuit: L10n.string("ui.game.force_quit_help")
        case .quit, nil: mode == .step ? action.title : transfer?.progressSummary ?? action.title
        }
    }
    private func accessibilityText(_ transfer: GameTransfer?) -> String {
        switch mode {
        case .step: action.title
        case .transferring: transfer?.progressSummary ?? L10n.string("ui.transfer.preparing_accessibility")
        case .idle: subtitle ?? ""
        }
    }

    var body: some View {
        let transfer = action.transfer()
        HStack(spacing: 8) {
            Button { action.run(asking: $asking) } label: {
                Group {
                    switch mode {
                    case .step:
                        Text(action.title)
                    case .transferring:
                        Text(transfer?.buttonTitle ?? L10n.string("ui.transfer.preparing_short"))
                    case .idle:
                        if let subtitle {
                            HStack(spacing: 9) {
                                Image(systemName: symbol)
                                VStack(alignment: .leading, spacing: 0) {
                                    Text(action.title)
                                    Text(subtitle)
                                        .font(.system(size: 10.5, weight: .semibold))
                                        .textCase(nil)
                                        .opacity(0.8)
                                        // Read as the button's value instead.
                                        .accessibilityHidden(true)
                                }
                            }
                        } else {
                            Label(action.title, systemImage: symbol)
                        }
                    }
                }
                .lineLimit(1)
                .minimumScaleFactor(0.8)
                .frame(width: 150)
            }
            .buttonStyle(GameActionButtonStyle(colors: colors, isBusy: mode != .idle, translucent: translucent))
            .overlay(alignment: .bottom) {
                if showsProgress(transfer) {
                    GameButtonProgress(fraction: mode == .step ? action.stepFraction() : transfer?.progressFraction)
                        .frame(height: 3)
                        .padding(.horizontal, 24)
                        .padding(.bottom, 6)
                        .allowsHitTesting(false)
                        .accessibilityHidden(true)
                }
            }
            .disabled(!action.isAvailable || mode != .idle)
            .help(helpText(transfer))
            .accessibilityValue(accessibilityText(transfer))

            if action.isCancellable {
                Button(action: action.cancel) {
                    StorefrontGlassIcon(systemName: "xmark", size: 12)
                }
                .obeliskButtonStyle(.plain)
                .foregroundStyle(.secondary)
                .accessibilityLabel(L10n.string("ui.transfer.cancel"))
                .help(L10n.string("ui.transfer.cancel"))
                .transition(.scale(scale: 0.5).combined(with: .opacity))
            }
        }
        .fixedSize()
        .forceQuitConfirmation(gameName, for: action, asking: $asking)
    }
}

/// The game's launch button: an uppercase title on a gradient capsule seen
/// through glass, green to launch, blue to install, grey to quit and red to
/// force quit, as the library's cards.
private struct GameActionButtonStyle: ButtonStyle {
    let colors: [Color]
    /// A button showing progress keeps its color while it cannot be pressed.
    let isBusy: Bool
    let translucent: Bool

    func makeBody(configuration: Configuration) -> some View {
        Appearance(configuration: configuration, colors: colors, isBusy: isBusy, translucent: translucent)
    }

    private struct Appearance: View {
        let configuration: Configuration
        let colors: [Color]
        let isBusy: Bool
        let translucent: Bool
        @Environment(\.isEnabled) private var isEnabled
        @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
        @State private var isHovered = false

        private var isDimmed: Bool { !isEnabled && !isBusy }

        var body: some View {
            let label = configuration.label
                .font(.system(size: 14, weight: .bold))
                .textCase(.uppercase)
                .tracking(0.5)
                .foregroundStyle(.white.opacity(isDimmed ? 0.6 : 1))
                .shadow(color: .black.opacity(0.2), radius: 1.5, y: 0.5)
                .padding(.horizontal, 18)
                .frame(height: 44)
                .background {
                    Capsule()
                        .fill(LinearGradient(colors: colors, startPoint: .leading, endPoint: .trailing))
                        .saturation(isDimmed ? 0.35 : 1)
                        .opacity(isDimmed ? 0.5 : reduceTransparency ? 1 : translucent ? 0.45 : 0.85)
                        .brightness(configuration.isPressed ? -0.08 : isHovered && isEnabled ? 0.06 : 0)
                }
            Group {
                if reduceTransparency {
                    label
                } else {
                    label.glassEffect(.regular.interactive(isEnabled), in: .capsule)
                }
            }
            .contentShape(.capsule)
            .onHover { isHovered = $0 }
            .pointerCursor()
        }
    }
}

private struct GameButtonProgress: View {
    let fraction: Double?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        GeometryReader { geometry in
            ZStack(alignment: .leading) {
                Capsule().fill(.white.opacity(0.25))
                if let fraction {
                    Capsule().fill(.white)
                        .frame(width: geometry.size.width * fraction)
                } else {
                    // Keyframes move the offset alone. A repeating animation
                    // started on appear also repeated the page's layout changes
                    // of that moment, and the bar swung outside the button.
                    Capsule().fill(.white)
                        .frame(width: geometry.size.width * 0.3)
                        .keyframeAnimator(initialValue: 0.0, repeating: !reduceMotion) { bar, travel in
                            bar.offset(x: geometry.size.width * 0.7 * travel)
                        } keyframes: { _ in
                            LinearKeyframe(1, duration: 0.9, timingCurve: .easeInOut)
                            LinearKeyframe(0, duration: 0.9, timingCurve: .easeInOut)
                        }
                }
            }
        }
    }
}

extension GameAction {
    /// Runs the action, or hands it to `asking` when its player confirms it first.
    func run(asking: Binding<GameAction?>) {
        if needsConfirmation { asking.wrappedValue = self } else { perform() }
    }
}

extension View {
    /// Asks before forcing `gameName` to quit, as macOS's own Force Quit does:
    /// what the game did not save is lost. The confirmation runs the action
    /// `asking` holds. The question goes away once `action` no longer needs
    /// it, as when the game exits by itself first.
    func forceQuitConfirmation(_ gameName: String, for action: GameAction,
                               asking: Binding<GameAction?>) -> some View {
        // Read now: the alert may clear `asking` before its button runs.
        let asked = asking.wrappedValue
        return onChange(of: action.needsConfirmation) { _, needs in
            if !needs { asking.wrappedValue = nil }
        }
        .alert(L10n.string("ui.game.force_quit_title", gameName), isPresented: Binding(
            get: { asking.wrappedValue != nil },
            set: { if !$0 { asking.wrappedValue = nil } }
        )) {
            Button(L10n.string("ui.common.cancel"), role: .cancel) {}
            Button(L10n.string("model.game.forceQuit"), role: .destructive) { asked?.perform() }
        } message: {
            Text(L10n.string("ui.game.force_quit_message"))
        }
    }
}
