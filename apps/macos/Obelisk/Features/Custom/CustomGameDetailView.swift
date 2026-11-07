import SwiftUI

/// Detail page for a standalone/custom Windows game.
struct CustomGameDetailView: View {
    @Bindable var model: CustomGamesModel
    let game: CustomGame
    @State private var showsBackdrop = false
    @State private var showsTitle = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.colorScheme) private var colorScheme

    private static var heroHeight: CGFloat { ObeliskStyle.libraryHeroHeight }
    private static var coverOverlap: CGFloat { 150 }

    var body: some View {
        StorefrontScrollView {
            VStack(alignment: .leading, spacing: 0) {
                hero
                header
                details
                    .padding(.top, 28)
                    .padding(.horizontal, ObeliskStyle.contentPadding)
                    .padding(.bottom, 48)
            }
        }
        .onScrollGeometryChange(for: Bool.self, of: { $0.contentOffset.y + $0.contentInsets.top > Self.heroHeight }) { _, under in
            showsBackdrop = under
        }
        .onScrollGeometryChange(for: Bool.self, of: { $0.contentOffset.y + $0.contentInsets.top > Self.heroHeight - 40 }) { _, under in
            showsTitle = under
        }
        .overlay(alignment: .top) {
            topBar
        }
        .ignoresSafeArea(.container, edges: .top)
    }

    private var hero: some View {
        HeroBackdrop {
            CustomGameBannerView(game: game)
        }
        .heroFrame()
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 28) {
            coverView
            VStack(alignment: .leading, spacing: 0) {
                Color.clear.frame(height: Self.coverOverlap)
                VStack(alignment: .leading, spacing: 14) {
                    Text(verbatim: game.title)
                        .font(.system(size: 26, weight: .bold))
                        .lineLimit(2)

                    HStack(spacing: 12) {
                        GameActionButton(
                            action: model.gameAction(for: game, place: .page),
                            gameName: game.title
                        )

                        StorefrontGlassButton(
                            systemName: "slider.horizontal.3",
                            size: 14,
                            label: L10n.string("ui.custom.launch_settings_title"),
                            identifier: "custom-detail-settings",
                            action: { model.settingsGame = game }
                        )

                        StorefrontGlassButton(
                            systemName: "pencil",
                            size: 14,
                            label: L10n.string("ui.custom.edit_game"),
                            identifier: "custom-detail-edit",
                            action: { model.editingGame = game }
                        )

                        Menu {
                            Button {
                                NSWorkspace.shared.selectFile(game.executablePath, inFileViewerRootedAtPath: "")
                            } label: {
                                Label(L10n.string("ui.custom.show_in_finder"), systemImage: "folder")
                            }

                            if let prefix = game.winePrefixPath, FileManager.default.fileExists(atPath: prefix) {
                                Button {
                                    NSWorkspace.shared.selectFile(prefix, inFileViewerRootedAtPath: "")
                                } label: {
                                    Label(L10n.string("ui.custom.show_prefix_in_finder"), systemImage: "folder.badge.gearshape")
                                }
                            }

                            Divider()

                            Button(role: .destructive) {
                                model.gamePendingRemoval = game
                            } label: {
                                Label(L10n.string("ui.custom.remove_game"), systemImage: "trash")
                            }
                        } label: {
                            StorefrontGlassIcon(systemName: "ellipsis", size: 14)
                                .foregroundStyle(.primary)
                        }
                        .menuStyle(.button)
                        .obeliskButtonStyle(.plain)
                        .menuIndicator(.hidden)
                        .fixedSize()
                        .accessibilityLabel(L10n.string("ui.common.more"))
                    }

                    metricsRow
                }
                .padding(.top, 18)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.horizontal, ObeliskStyle.contentPadding)
        .padding(.top, -Self.coverOverlap)
    }

    private var coverView: some View {
        let shape = RoundedRectangle(cornerRadius: ObeliskStyle.coverCornerRadius, style: .continuous)
        return Color.clear
            .aspectRatio(ObeliskStyle.coverAspectRatio, contentMode: .fit)
            .overlay {
                CustomGameCoverView(game: game)
            }
            .clipShape(shape)
            .overlay {
                shape.strokeBorder(.white.opacity(colorScheme == .dark ? 0.12 : 0.3), lineWidth: 1)
                    .allowsHitTesting(false)
            }
            .frame(width: 190)
            .shadow(color: .black.opacity(colorScheme == .dark ? 0.5 : 0.3), radius: 24, y: 14)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(game.title)
            .accessibilityAddTraits(.isHeader)
    }

    private var metricsRow: some View {
        HStack(spacing: 18) {
            metricItem(
                symbol: "clock",
                text: game.playtimeSeconds > 0
                    ? GameFormatting.playtime(Duration.seconds(Int64(game.playtimeSeconds)))
                    : L10n.string("ui.game.never_played")
            )

            metricItem(
                symbol: "calendar",
                text: GameFormatting.lastPlayed(game.lastPlayed)
            )

            metricItem(
                symbol: "cpu",
                text: game.engine == .rosetta ? "Rosetta" : "Wine"
            )

            metricItem(
                symbol: "display",
                text: game.graphics == .d3dMetal ? "D3DMetal" : "DXVK"
            )
        }
        .padding(.top, 4)
    }

    private func metricItem(symbol: String, text: String) -> some View {
        HStack(spacing: 6) {
            Image(systemName: symbol)
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(.secondary)
            Text(verbatim: text)
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(.secondary)
        }
    }

    private var details: some View {
        VStack(alignment: .leading, spacing: 20) {
            ObeliskCard {
                ObeliskRow(title: L10n.string("ui.custom.executable_path")) {
                    SymbolTile(name: "terminal.fill", colors: TileColors.graphite)
                } detail: {
                    Text(verbatim: game.executablePath)
                        .font(.system(size: 12, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                } accessory: {
                    GlassCapsuleButton(
                        title: L10n.string("ui.custom.show_in_finder"),
                        systemImage: "folder",
                        identifier: "custom-executable-finder"
                    ) {
                        NSWorkspace.shared.selectFile(game.executablePath, inFileViewerRootedAtPath: "")
                    }
                }

                ObeliskCardDivider()

                ObeliskRow(title: L10n.string("ui.custom.launch_arguments")) {
                    SymbolTile(name: "text.quote", colors: TileColors.grey)
                } detail: {
                    Text(verbatim: game.launchArguments.isEmpty ? "-" : game.launchArguments)
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                } accessory: {
                    EmptyView()
                }
            }

            ObeliskCard {
                ObeliskRow(title: L10n.string("ui.custom.wine_prefix")) {
                    SymbolTile(name: "folder.badge.gearshape", colors: TileColors.blue)
                } detail: {
                    Text(verbatim: game.winePrefixPath ?? L10n.string("ui.custom.wine_prefix_default"))
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                } accessory: {
                    if let prefix = game.winePrefixPath, FileManager.default.fileExists(atPath: prefix) {
                        GlassCapsuleButton(
                            title: L10n.string("ui.custom.show_in_finder"),
                            systemImage: "folder",
                            identifier: "custom-prefix-finder"
                        ) {
                            NSWorkspace.shared.selectFile(prefix, inFileViewerRootedAtPath: "")
                        }
                    }
                }

                ObeliskCardDivider()

                ObeliskRow(title: L10n.string("ui.custom.compatibility_section")) {
                    SymbolTile(name: "gearshape.2.fill", colors: TileColors.violet)
                } detail: {
                    let engineText = game.engine == .rosetta ? "Rosetta" : "Wine"
                    let graphicsText = game.graphics == .d3dMetal ? "D3DMetal" : "DXVK"
                    let rtText = game.rayTracing ? "RT: On" : "RT: Off"
                    let hudText = game.performanceHud ? "HUD: On" : "HUD: Off"
                    let summaryText = "\(engineText) · \(graphicsText) · \(rtText) · \(hudText) · \(game.renderPercent)%"
                    Text(verbatim: summaryText)
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                } accessory: {
                    GlassCapsuleButton(
                        title: L10n.string("ui.custom.launch_settings_title"),
                        systemImage: "slider.horizontal.3",
                        identifier: "custom-detail-open-settings"
                    ) {
                        model.settingsGame = game
                    }
                }
            }
        }
    }

    private var topBar: some View {
        LibraryGlassBar(
            searchText: .constant(""),
            searchLabel: "",
            showsBackdrop: showsBackdrop,
            game: LibraryGlassBar.GameBar(
                title: game.title,
                libraryTitle: L10n.string("model.section.custom.title"),
                showsTitle: showsTitle,
                back: {
                    withAnimation(ObeliskStyle.pageAnimation(reduceMotion: reduceMotion)) {
                        model.selectedGame = nil
                    }
                }
            )
        ) {
            EmptyView()
        }
    }
}
