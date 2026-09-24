import AppKit
import SwiftUI

struct GameAchievementsButton: View {
    let model: GameAchievementsModel
    @State private var isHovered = false

    var body: some View {
        if model.hasContent {
            Button { model.isPresented = true } label: {
                GameMetricLabel(caption: L10n.string("ui.game.achievements.title"), isHighlighted: isHovered) {
                    GameMetricSymbol(name: "trophy")
                } value: {
                    if model.isLoading {
                        ProgressView().controlSize(.mini)
                    } else {
                        Text(model.countText).monospacedDigit()
                    }
                }
            }
            .forgeButtonStyle(.plain)
            .onHover { isHovered = $0 }
            .fixedSize()
            .accessibilityLabel(L10n.string("ui.game.achievements.title"))
            .accessibilityValue(model.isLoading ? L10n.string("ui.game.achievements.loading") : model.error ?? model.countText)
            .accessibilityIdentifier("\(model.storeID)-achievements-button")
            .help(L10n.string("ui.game.achievements.show"))
        }
    }
}

struct GameAchievementsView: View {
    let model: GameAchievementsModel
    let gameName: String
    let retry: () -> Void
    @Environment(\.dismiss) private var dismiss
    @Environment(\.displayScale) private var displayScale

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            VStack(alignment: .leading, spacing: 18) {
                header
                if let items = model.items, !items.isEmpty {
                    HStack(spacing: 12) {
                        Text(model.countText).monospacedDigit()
                        ProgressView(value: model.fraction ?? 0)
                    }
                    .accessibilityElement(children: .combine)
                }
                Divider()
            }
            .padding(.horizontal, 24)
            .padding(.top, 24)
            content
        }
        .frame(width: 560, height: 540)
        .background { AchievementSheetDismissal { dismiss() } }
    }

    private var header: some View {
        HStack {
            VStack(alignment: .leading, spacing: 5) {
                Label(L10n.string("ui.game.achievements.title"), systemImage: "trophy")
                    .font(.title2.weight(.semibold))
                Text(gameName).font(.subheadline).foregroundStyle(.secondary)
            }
            Spacer()
            Button { dismiss() } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(.secondary)
                    .frame(width: 28, height: 28)
                    .background(.primary.opacity(0.08), in: Circle())
                    .contentShape(Circle())
            }
            .forgeButtonStyle(.plain)
            .keyboardShortcut(.cancelAction)
            .accessibilityLabel(L10n.string("ui.common.close"))
            .accessibilityIdentifier("\(model.storeID)-achievements-close")
            .help(L10n.string("ui.common.close"))
        }
    }

    @ViewBuilder
    private var content: some View {
        if model.isLoading {
            ProgressView(L10n.string("ui.game.achievements.loading"))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if let error = model.error {
            VStack(spacing: 12) {
                Text(error).foregroundStyle(.secondary)
                Button(L10n.string("ui.common.retry"), action: retry)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if let items = model.items, !items.isEmpty {
            ScrollView {
                LazyVStack(spacing: 0) {
                    ForEach(items, id: \.id) { item in
                        row(item)
                        Divider()
                    }
                }
                .padding(.horizontal, 24)
                .padding(.bottom, 24)
            }
        } else {
            ContentUnavailableView(L10n.string("ui.game.achievements.empty"), systemImage: "trophy")
        }
    }

    private func row(_ item: GameAchievement) -> some View {
        HStack(alignment: .top, spacing: 14) {
            CachedArtwork(url: URL(string: item.iconUrl), maxPixelSize: Int((48 * displayScale).rounded(.up))) { image in
                image.resizable().scaledToFit()
            } placeholder: {
                Image(systemName: item.hidden ? "lock" : "trophy")
                    .font(.title2).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(.primary.opacity(0.05))
            }
            .frame(width: 48, height: 48)
            .saturation(item.unlocked ? 1 : 0)
            .clipShape(RoundedRectangle(cornerRadius: 7))
            .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 5) {
                Text(item.hidden ? L10n.string("ui.game.achievements.hidden") : item.name)
                    .font(.system(size: 14, weight: .semibold))
                if !item.description.isEmpty {
                    Text(item.description).font(.system(size: 12)).foregroundStyle(.secondary)
                }
                if let date = item.unlockDate {
                    Text(date, format: .dateTime.day().month(.abbreviated).year().locale(L10n.locale))
                        .font(.system(size: 11)).foregroundStyle(.secondary)
                } else if !item.unlocked {
                    Text(L10n.string("ui.game.achievements.locked"))
                        .font(.system(size: 11)).foregroundStyle(.secondary)
                }
            }
            Spacer(minLength: 0)
            if item.unlocked {
                Image(systemName: "checkmark.circle.fill").foregroundStyle(.tint)
                    .accessibilityLabel(L10n.string("ui.game.achievements.unlocked"))
            }
        }
        .padding(.vertical, 13)
        .accessibilityElement(children: .combine)
    }
}

/// Keep the native sheet's keyboard/focus behavior, and consume clicks on its
/// parent window so dismissing cannot also activate a control underneath it.
private struct AchievementSheetDismissal: NSViewRepresentable {
    let dismiss: () -> Void

    func makeNSView(context: Context) -> OutsideClickView {
        let view = OutsideClickView()
        view.dismiss = dismiss
        return view
    }

    func updateNSView(_ view: OutsideClickView, context: Context) {
        view.dismiss = dismiss
    }

    static func dismantleNSView(_ view: OutsideClickView, coordinator: ()) {
        view.stopMonitoring()
    }

    final class OutsideClickView: NSView {
        var dismiss: (() -> Void)?
        private var monitor: Any?

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            stopMonitoring()
            guard window != nil else { return }
            monitor = NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown]) { [weak self] event in
                let consumed = MainActor.assumeIsolated {
                    guard let self, let sheet = self.window,
                          let parent = sheet.sheetParent, event.window === parent else { return false }
                    self.dismiss?()
                    return true
                }
                return consumed ? nil : event
            }
        }

        func stopMonitoring() {
            if let monitor { NSEvent.removeMonitor(monitor) }
            monitor = nil
        }
    }
}
