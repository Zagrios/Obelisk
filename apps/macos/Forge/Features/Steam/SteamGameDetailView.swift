import SwiftUI

struct SteamGameDetailView: View {
    @Bindable var model: SteamModel
    let game: SteamGame
    @Environment(\.colorScheme) private var colorScheme

    // This view is only shown for `model.selectedGame`, so the receipts agree.
    private var receipt: SteamInstallReceipt? { model.selectedInstallation }

    private var estimatedBytes: UInt64 {
        model.platform == "macos" ? model.details?.macosBytes ?? 0 : model.details?.windowsBytes ?? 0
    }

    private var gameTransfer: SteamTransfer? {
        model.transfer.flatMap { $0.appId == game.appId ? $0 : nil }
    }

    var body: some View {
        @Bindable var achievements = model.achievements
        VStack(alignment: .leading, spacing: 23) {
            GameDetailHeader(
                title: game.name,
                libraryTitle: L10n.string("ui.steam.library"),
                playtime: SteamFormatting.playtime(game.playtimeMinutes),
                lastPlayed: SteamFormatting.lastPlayed(game.lastPlayed),
                size: sizeText,
                back: model.backToLibrary,
                additionalMetrics: { GameAchievementsButton(model: model.achievements) }
            ) {
                SteamArtwork(appId: game.appId, portrait: false, iconHash: game.iconHash)
            } action: {
                HStack(spacing: 10) {
                    let cancelsSynchronization = model.cloud.canCancelSynchronization(for: game.appId)
                    SteamGameActionButton(
                        title: model.primaryActionTitle,
                        symbol: model.primaryActionSymbol,
                        transfer: gameTransfer,
                        isTransferring: model.activeTransferAppId == game.appId,
                        isAvailable: model.primaryActionAvailable,
                        action: model.primaryAction,
                        cancel: {
                            if cancelsSynchronization { model.cloud.respond(.cancel) }
                            else { model.cancelDownload() }
                        },
                        isCancellable: cancelsSynchronization,
                        isIndeterminate: model.isSynchronizingSaves
                    )
                    if let receipt {
                        GameManagementButton(
                            gameName: game.name,
                            isAvailable: model.canUninstallSelectedGame,
                            uninstall: { model.uninstall(receipt) }
                        )
                        .id(receipt.id + receipt.directory)
                    }
                }
            }

            if let message = model.gameSessionMessage {
                Text(message).font(.system(size: 12)).foregroundStyle(.secondary)
            }

            let cloudMessage = model.cloud.message(for: game.appId)
            let cloudPending = model.cloud.hasPending(for: game.appId)
            if !model.isSynchronizingSaves && (cloudMessage != nil || cloudPending) {
                HStack(spacing: 8) {
                    if let cloudMessage {
                        Text(cloudMessage).font(.system(size: 12)).foregroundStyle(.secondary)
                    }
                    if cloudPending {
                        Button(L10n.string("ui.common.retry")) {
                            model.cloud.retryPending(appId: game.appId)
                        }
                        .forgeButtonStyle(.borderless)
                        .controlSize(.small)
                    }
                }
            }

            if model.isLoadingDetails {
                ProgressView(L10n.string("ui.steam.finding_files")).controlSize(.small)
            } else if let error = model.detailsError {
                HStack {
                    Text(L10n.message(error)).font(.system(size: 12)).foregroundStyle(.secondary)
                    Button(L10n.string("ui.common.retry"), action: model.reloadDetails)
                        .forgeButtonStyle(.borderless)
                }
            }

            if let transfer = gameTransfer, transfer.state == .failed || transfer.state == .cancelled {
                Text(transfer.message.isEmpty ? transfer.activityTitle : L10n.message(transfer.message))
                    .font(.system(size: 12)).foregroundStyle(.secondary)
            } else if let activeAppId = model.activeTransferAppId, activeAppId != game.appId {
                Text(L10n.string("ui.steam.other_game_operation"))
                    .font(.system(size: 12)).foregroundStyle(.secondary)
            }

            newsSection
        }
        .sheet(isPresented: $achievements.isPresented) {
            GameAchievementsView(model: model.achievements, gameName: game.name, retry: model.reloadAchievements)
        }
    }

    private var sizeText: String {
        let bytes = receipt?.totalBytes ?? estimatedBytes
        return bytes > 0 ? SteamFormatting.bytes(bytes) : "—"
    }

    private var newsSection: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(L10n.string("ui.game.news")).font(.system(size: 20, weight: .semibold))
            if model.isLoadingNews {
                ProgressView(L10n.string("ui.game.loading_news")).controlSize(.small)
            } else if let error = model.newsError {
                Text(L10n.message(error)).font(.system(size: 12)).foregroundStyle(.secondary)
            } else if model.news.isEmpty {
                Text(L10n.string("ui.game.no_recent_news")).font(.system(size: 12)).foregroundStyle(.secondary)
            } else {
                ForEach(model.news, id: \.id) { article in
                    VStack(alignment: .leading, spacing: 9) {
                        HStack(spacing: 8) {
                            Text(article.source)
                            Text("·")
                            Text(Date(timeIntervalSince1970: TimeInterval(article.date)), format: .dateTime.day().month(.abbreviated).year().locale(L10n.locale))
                        }
                        .font(.system(size: 10)).foregroundStyle(.secondary)
                        Text(article.title).font(.system(size: 15, weight: .semibold))
                        if !article.excerpt.isEmpty {
                            Text(article.excerpt).font(.system(size: 12)).foregroundStyle(.secondary).lineLimit(5)
                        }
                        if let url = URL(string: article.url), url.scheme == "https", url.host != nil {
                            Link(destination: url) { Label(L10n.string("ui.game.read_article"), systemImage: "arrow.up.right") }
                                .font(.system(size: 11, weight: .medium))
                                .pointerCursor()
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(17)
                    .background(ForgePalette(colorScheme: colorScheme).surface, in: RoundedRectangle(cornerRadius: 10))
                }
            }
        }
        .padding(.top, 5)
    }

}

struct SteamGameActionButton: View {
    let title: String
    let symbol: String
    let transfer: (any StorefrontTransfer)?
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
                    SteamButtonProgress(fraction: mode == .indeterminate ? nil : transfer?.progressFraction)
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

private struct SteamButtonProgress: View {
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
