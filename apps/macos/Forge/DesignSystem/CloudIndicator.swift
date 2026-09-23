import SwiftUI

/// What a game's Cloud indicator shows. Rust derives the persisted state;
/// this adds the live states: a sync in progress and a running game.
struct CloudIndicatorPresentation: Equatable {
    enum Tone: Equatable { case normal, warning, muted }

    /// `nil` shows progress instead of a symbol.
    let symbol: String?
    let value: String
    let tone: Tone

    /// `nil` hides the indicator. An uninstalled game shows it only while an
    /// upload is pending. A missing record means the game never synced.
    static func make(saved: CloudIndicator?, isSyncing: Bool, isPlaying: Bool, isInstalled: Bool,
                     now: Date = Date()) -> CloudIndicatorPresentation? {
        let saved = saved ?? CloudIndicator(state: .neverSynced, gameSession: false, syncedAt: nil,
                                            pendingUpload: false, installationRemoved: false)
        guard isInstalled || saved.pendingUpload else { return nil }
        if isSyncing {
            return CloudIndicatorPresentation(symbol: nil, value: L10n.string("ui.cloud.indicator.syncing"), tone: .normal)
        }
        let lastSync = saved.syncedAt.map { CloudSyncDate.text(Date(timeIntervalSince1970: TimeInterval($0)), now: now) }
        let pending = L10n.string("ui.cloud.indicator.pending")
        if saved.gameSession {
            if isPlaying {
                return CloudIndicatorPresentation(symbol: "cloud", value: lastSync ?? L10n.string("ui.cloud.indicator.never"),
                                                  tone: .normal)
            }
            // The next launch or maintenance recovers a guard nobody holds.
            return .warning(pending)
        }
        switch saved.state {
        case .recoveryNeeded, .uploadUncertain, .localChanges:
            return .warning(pending)
        case .conflict:
            return .warning(L10n.string("ui.cloud.indicator.conflict"))
        case .pending:
            return .warning(lastSync.map { L10n.string("ui.cloud.indicator.pending_since", $0) } ?? pending)
        case .unavailable:
            return CloudIndicatorPresentation(symbol: "icloud.slash", value: L10n.string("ui.cloud.indicator.unavailable"),
                                              tone: .normal)
        case .lastFailed:
            return .warning(L10n.string("ui.cloud.indicator.failed"))
        case .synced:
            let value = lastSync.map { L10n.string("ui.cloud.indicator.synced_at", $0) } ?? L10n.string("ui.cloud.indicator.synced")
            return CloudIndicatorPresentation(symbol: "icloud", value: value, tone: .normal)
        case .neverSynced:
            return CloudIndicatorPresentation(symbol: "icloud", value: L10n.string("ui.cloud.indicator.never"), tone: .muted)
        }
    }

    private static func warning(_ value: String) -> CloudIndicatorPresentation {
        CloudIndicatorPresentation(symbol: "exclamationmark.icloud", value: value, tone: .warning)
    }
}

/// A Last Cloud sync reads relative for a week, then as a day and month. The
/// year appears only when it is not the current one.
enum CloudSyncDate {
    static func text(_ date: Date, now: Date = Date(), locale: Locale = L10n.locale,
                     calendar: Calendar = .current) -> String {
        let elapsed = now.timeIntervalSince(date)
        if elapsed < 60 { return L10n.string("ui.cloud.date.just_now") }
        if elapsed < 7 * 86_400 {
            let formatter = RelativeDateTimeFormatter()
            formatter.locale = locale
            formatter.calendar = calendar
            formatter.unitsStyle = .short
            return formatter.localizedString(for: date, relativeTo: now)
        }
        let style = Date.FormatStyle(locale: locale, calendar: calendar, timeZone: calendar.timeZone).day().month(.abbreviated)
        let sameYear = calendar.component(.year, from: date) == calendar.component(.year, from: now)
        return date.formatted(sameYear ? style : style.year())
    }
}

/// The header metric beside achievements. Its menu starts Cloud maintenance
/// of the game, which is also how the player retries a failed one.
struct CloudIndicatorButton: View {
    let presentation: CloudIndicatorPresentation
    let canSync: Bool
    let sync: () -> Void
    var storefront = "steam"
    @State private var isHovered = false

    var body: some View {
        Menu {
            Button(L10n.string("ui.cloud.sync_now"), systemImage: "arrow.triangle.2.circlepath", action: sync)
                .disabled(!canSync)
        } label: {
            HStack(spacing: 10) {
                icon.frame(width: 28, height: 23)
                VStack(alignment: .leading, spacing: 7) {
                    Text(L10n.string("ui.cloud.indicator.caption"))
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(.secondary)
                    Text(presentation.value)
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(presentation.tone == .muted ? .secondary : .primary)
                        .lineLimit(1)
                }
            }
            .padding(.horizontal, 10).padding(.vertical, 8)
            .background(.primary.opacity(isHovered ? 0.15 : 0.1), in: RoundedRectangle(cornerRadius: 8))
            .contentShape(RoundedRectangle(cornerRadius: 8))
        }
        .menuStyle(.button)
        .forgeButtonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .disabled(!canSync)
        .onHover { isHovered = $0 }
        .accessibilityLabel(L10n.string("ui.cloud.indicator.caption"))
        .accessibilityValue(presentation.value)
        .accessibilityIdentifier("\(storefront)-cloud-indicator")
        .help(L10n.string("ui.cloud.indicator.help"))
    }

    @ViewBuilder private var icon: some View {
        if let symbol = presentation.symbol {
            Image(systemName: symbol)
                .font(.system(size: 23, weight: .regular))
                .foregroundStyle(presentation.tone == .warning ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
        } else {
            ProgressView().controlSize(.small)
        }
    }
}
