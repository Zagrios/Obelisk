import SwiftUI

struct GameActionButton: View {
    let title: String
    /// Whether the button launches the game rather than installing it.
    let launches: Bool
    let transfer: GameTransfer?
    let isTransferring: Bool
    let isAvailable: Bool
    let action: () -> Void
    let cancel: () -> Void
    var isCancellable = false
    /// A step outside any Transfer holds the button: a pre-launch Cloud sync,
    /// or Voile installing before a Windows launch.
    var isStep = false
    /// The step's progress, when it has one.
    var stepFraction: Double? = nil

    /// A step outranks a byte-counted transfer: it has no per-file title to show.
    private enum Mode { case idle, transferring, step }
    private var mode: Mode {
        if isStep { .step } else if isTransferring { .transferring } else { .idle }
    }

    private var showsProgress: Bool {
        switch mode {
        case .step: true
        case .transferring: transfer?.isInProgress ?? true
        case .idle: false
        }
    }
    private var helpText: String {
        mode == .step ? title : transfer?.progressSummary ?? title
    }
    private var accessibilityText: String {
        switch mode {
        case .step: title
        case .transferring: transfer?.progressSummary ?? L10n.string("ui.transfer.preparing_accessibility")
        case .idle: ""
        }
    }

    var body: some View {
        HStack(spacing: 8) {
            Button(action: action) {
                Group {
                    switch mode {
                    case .step:
                        Text(title)
                    case .transferring:
                        Text(transfer?.buttonTitle ?? L10n.string("ui.transfer.preparing_short"))
                    case .idle:
                        Label(title, systemImage: launches ? "play.fill" : "arrow.down.to.line")
                    }
                }
                .lineLimit(1)
                .minimumScaleFactor(0.8)
                .frame(width: 150)
            }
            .buttonStyle(GameActionButtonStyle(launches: launches, isBusy: mode != .idle))
            .overlay(alignment: .bottom) {
                if showsProgress {
                    GameButtonProgress(fraction: mode == .step ? stepFraction : transfer?.progressFraction)
                        .frame(height: 3)
                        .padding(.horizontal, 12)
                        .padding(.bottom, 5)
                        .allowsHitTesting(false)
                        .accessibilityHidden(true)
                }
            }
            .disabled(!isAvailable || mode != .idle)
            .help(helpText)
            .accessibilityValue(accessibilityText)

            if isTransferring || isCancellable {
                Button(action: cancel) {
                    Image(systemName: "xmark")
                        .font(.system(size: 11, weight: .semibold))
                        .frame(width: 28, height: 28)
                        .background(.primary.opacity(0.1), in: Circle())
                        .contentShape(Circle())
                }
                .sailOverButtonStyle(.plain)
                .foregroundStyle(.secondary)
                .accessibilityLabel(L10n.string("ui.transfer.cancel"))
                .help(L10n.string("ui.transfer.cancel"))
            }
        }
        .fixedSize()
    }
}

/// Steam's launch bar button: an uppercase title on a gradient, green to
/// launch and blue to install.
private struct GameActionButtonStyle: ButtonStyle {
    private static let launchGradient = [Color(red: 0.46, green: 0.77, blue: 0.27), Color(red: 0.32, green: 0.63, blue: 0.20)]
    private static let installGradient = [Color(red: 0.28, green: 0.55, blue: 0.98), Color(red: 0.40, green: 0.38, blue: 0.88)]

    let launches: Bool
    /// A button showing progress keeps its color while it cannot be pressed.
    let isBusy: Bool

    func makeBody(configuration: Configuration) -> some View {
        Appearance(configuration: configuration, colors: launches ? Self.launchGradient : Self.installGradient,
                   isBusy: isBusy)
    }

    private struct Appearance: View {
        let configuration: Configuration
        let colors: [Color]
        let isBusy: Bool
        @Environment(\.isEnabled) private var isEnabled
        @State private var isHovered = false

        private var isDimmed: Bool { !isEnabled && !isBusy }

        var body: some View {
            configuration.label
                .font(.system(size: 15, weight: .semibold))
                .textCase(.uppercase)
                .tracking(0.4)
                .foregroundStyle(.white.opacity(isDimmed ? 0.6 : 1))
                .padding(.horizontal, 14)
                .frame(height: 44)
                .background {
                    RoundedRectangle(cornerRadius: 6)
                        .fill(LinearGradient(colors: colors, startPoint: .leading, endPoint: .trailing))
                        .saturation(isDimmed ? 0.35 : 1)
                        .opacity(isDimmed ? 0.55 : 1)
                        .brightness(configuration.isPressed ? -0.08 : isHovered && isEnabled ? 0.05 : 0)
                }
                .contentShape(RoundedRectangle(cornerRadius: 6))
                .onHover { isHovered = $0 }
                .pointerCursor()
        }
    }
}

private struct GameButtonProgress: View {
    let fraction: Double?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var moving = false

    var body: some View {
        GeometryReader { geometry in
            ZStack(alignment: .leading) {
                Capsule().fill(.white.opacity(0.25))
                if let fraction {
                    Capsule().fill(.white)
                        .frame(width: geometry.size.width * fraction)
                } else {
                    Capsule().fill(.white)
                        .frame(width: geometry.size.width * 0.3)
                        .offset(x: moving && !reduceMotion ? geometry.size.width * 0.7 : 0)
                        .animation(reduceMotion ? nil : .easeInOut(duration: 0.9).repeatForever(autoreverses: true), value: moving)
                        .onAppear { moving = true }
                        .onDisappear { moving = false }
                }
            }
        }
    }
}
