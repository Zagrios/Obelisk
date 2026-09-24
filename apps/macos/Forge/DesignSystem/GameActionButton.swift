import SwiftUI

struct GameActionButton: View {
    let title: String
    let symbol: String
    let transfer: GameTransfer?
    let isTransferring: Bool
    let isAvailable: Bool
    let action: () -> Void
    let cancel: () -> Void
    var isCancellable = false
    var isIndeterminate = false

    /// An indeterminate step (a pre-launch Cloud sync) outranks a byte-counted
    /// transfer: it has no fraction and no per-file title to show.
    private enum Mode { case idle, transferring, indeterminate }
    private var mode: Mode {
        if isIndeterminate { .indeterminate } else if isTransferring { .transferring } else { .idle }
    }

    private var showsProgress: Bool {
        switch mode {
        case .indeterminate: true
        case .transferring: transfer?.isInProgress ?? true
        case .idle: false
        }
    }
    private var helpText: String {
        mode == .indeterminate ? title : transfer?.progressSummary ?? title
    }
    private var accessibilityText: String {
        switch mode {
        case .indeterminate: title
        case .transferring: transfer?.progressSummary ?? L10n.string("ui.transfer.preparing_accessibility")
        case .idle: ""
        }
    }

    var body: some View {
        HStack(spacing: 8) {
            if isTransferring || isCancellable {
                Button(action: cancel) {
                    Image(systemName: "xmark")
                        .font(.system(size: 10, weight: .semibold))
                        .frame(width: 24, height: 24)
                        .background(.primary.opacity(0.06), in: Circle())
                        .contentShape(Circle())
                }
                .forgeButtonStyle(.plain)
                .foregroundStyle(.secondary)
                .accessibilityLabel(L10n.string("ui.transfer.cancel"))
                .help(L10n.string("ui.transfer.cancel"))
            }

            Button(action: action) {
                Group {
                    switch mode {
                    case .indeterminate:
                        Text(title)
                    case .transferring:
                        Text(transfer?.buttonTitle ?? L10n.string("ui.transfer.preparing_short"))
                    case .idle:
                        Label(title, systemImage: symbol)
                    }
                }
                .lineLimit(1)
                .frame(width: 152)
            }
            .buttonStyle(ForgePrimaryButtonStyle(scale: 1.25))
            .overlay(alignment: .bottom) {
                if showsProgress {
                    GameButtonProgress(fraction: mode == .indeterminate ? nil : transfer?.progressFraction)
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
        }
        .fixedSize()
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
