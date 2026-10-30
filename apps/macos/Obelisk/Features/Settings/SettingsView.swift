import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// Obelisk's settings: a hero drawn from Obelisk's icon, its prism and the
/// stores' beams, reaching under the title bar band; then the appearance, the
/// language, the games, the storage, the storefronts and help, each a card of rows
/// as a game page's. The
/// bar's backdrop and the page's title show once the hero scrolled under it.
struct SettingsView: View {
    @Bindable var model: AppModel
    @AppStorage("downloadSpeedInMbps") private var downloadSpeedInMbps = false
    @AppStorage("steamDownloadLimitMbps") private var steamDownloadParallelism = 0
    private let steamSpeedLimits = [5, 10, 25, 50, 100, 250, 500, 1000, 0]
    @State private var showsBackdrop = false
    @State private var showsTitle = false
    /// The player picks another folder for the games.
    @State private var picksGamesFolder = false

    /// The hero's height below the bar it reaches under.
    private static var heroHeight: CGFloat { 210 }
    /// How wide the cards grow: rows any wider read poorly.
    private static var contentWidth: CGFloat { 900 }

    var body: some View {
        StorefrontScrollView {
            VStack(alignment: .leading, spacing: 0) {
                SettingsHero(height: Self.heroHeight)
                VStack(alignment: .leading, spacing: 34) {
                    GamePageSection(title: L10n.string("ui.settings.appearance")) {
                        ObeliskCard {
                            AppearancePicker(selection: $model.appearance)
                                .padding(18)
                            ObeliskCardDivider()
                            HStack {
                                Text(L10n.string("ui.settings.speed_units"))
                                Spacer()
                                Picker(L10n.string("ui.settings.speed_units"), selection: $downloadSpeedInMbps) {
                                    Text(L10n.string("ui.settings.speed_bytes")).tag(false)
                                    Text(L10n.string("ui.settings.speed_bits")).tag(true)
                                }
                                .labelsHidden()
                                .pickerStyle(.segmented)
                                .frame(width: 190)
                            }
                            .padding(18)
                        }
                    }
                    GamePageSection(title: L10n.string("ui.settings.language")) {
                        ObeliskCard {
                            LanguagePicker(selection: $model.language)
                                .padding(18)
                            if model.languageNeedsRestart {
                                ObeliskCardDivider()
                                languageRestart
                                    .transition(.opacity)
                            }
                        }
                        .animation(.easeInOut(duration: 0.2), value: model.languageNeedsRestart)
                    }
                    GamePageSection(title: L10n.string("ui.settings.games")) {
                        ObeliskCard {
                            automaticUpdates
                            ObeliskCardDivider()
                            overlay
                            ObeliskCardDivider()
                            sharePerformance
                            ObeliskCardDivider()
                            steamDownloadParallelismRow
                        }
                    }
                    GamePageSection(title: L10n.string("ui.settings.storage")) {
                        storage
                    }
                    GamePageSection(title: L10n.string("ui.settings.stores")) {
                        ObeliskCard {
                            ForEach(LibrarySection.storefronts) { section in
                                if section != LibrarySection.storefronts.first { ObeliskCardDivider() }
                                SettingsStoreRow(section: section, status: status(of: section)) {
                                    model.selection = section
                                }
                            }
                        }
                    }
                    GamePageSection(title: L10n.string("ui.settings.updates")) {
                        ObeliskCard {
                            installUpdates
                            ObeliskCardDivider()
                            version
                        }
                    }
                    GamePageSection(title: L10n.string("ui.settings.help")) {
                        ObeliskCard {
                            reportBug
                            ObeliskCardDivider()
                            requestFeature
                        }
                    }
                    #if DEBUG
                    GamePageSection(title: "Developer") {
                        DeveloperModalsCard()
                    }
                    #endif
                }
                .frame(maxWidth: Self.contentWidth, alignment: .leading)
                .padding(.horizontal, ObeliskStyle.contentPadding)
                .padding(.top, 30)
                .padding(.bottom, 40)
            }
        }
        // The bar's backdrop shows once the hero scrolled under it, and the
        // page's title once the hero's own scrolled past the bar.
        .onScrollGeometryChange(for: Bool.self, of: { $0.contentOffset.y + $0.contentInsets.top > Self.heroHeight }) { _, under in
            showsBackdrop = under
        }
        .onScrollGeometryChange(for: Bool.self, of: { $0.contentOffset.y + $0.contentInsets.top > Self.heroHeight - 40 }) { _, under in
            showsTitle = under
        }
        .overlay(alignment: .top) {
            SettingsBar(showsBackdrop: showsBackdrop, showsTitle: showsTitle)
        }
        .ignoresSafeArea(.container, edges: .top)
        .fileImporter(isPresented: $picksGamesFolder, allowedContentTypes: [.folder]) { result in
            if case let .success(folder) = result { model.gamesFolder.propose(folder) }
        }
        .fileDialogMessage(L10n.string("ui.settings.games_folder_pick", AppIdentity.name))
        .fileDialogConfirmationLabel(L10n.string("ui.settings.games_folder_pick_confirm"))
        .fileDialogDefaultDirectory(URL(filePath: "/Volumes", directoryHint: .isDirectory))
        .onAppear { applySteamDownloadParallelism() }
        .onChange(of: steamDownloadParallelism) { _, _ in applySteamDownloadParallelism() }
        .sheet(item: Bindable(model.gamesFolder).proposal) { proposal in
            GamesFolderSheet(proposal: proposal, cancel: { model.gamesFolder.proposal = nil },
                             confirm: model.gamesFolder.confirm)
        }
        .errorAlert(L10n.string("ui.settings.games_folder_failed"), message: Bindable(model.gamesFolder).failure)
    }

    private var steamDownloadParallelismRow: some View {
        ObeliskRow(title: L10n.string("ui.settings.steam_speed_title")) {
            SymbolTile(name: "arrow.down.circle", colors: TileColors.blue)
        } detail: {
            Text(L10n.string("ui.settings.steam_speed_detail"))
        } accessory: {
            HStack(spacing: 8) {
                Slider(value: Binding(
                    get: { Double(steamSpeedLimits.firstIndex(of: steamDownloadParallelism) ?? 8) },
                    set: { steamDownloadParallelism = steamSpeedLimits[Int($0.rounded())] }
                ), in: 0...8, step: 1)
                .frame(width: 150)
                .accessibilityLabel(L10n.string("ui.settings.steam_speed_title"))
                .accessibilityValue(steamSpeedLabel)
                Text(steamSpeedLabel)
                    .monospacedDigit()
                    .frame(width: 100, alignment: .trailing)
            }
        }
    }

    private var steamSpeedLabel: String {
        steamDownloadParallelism == 0 ? L10n.string("ui.settings.steam_speed_unlimited")
            : L10n.string("ui.settings.steam_speed_value", Int64(steamDownloadParallelism))
    }

    private func applySteamDownloadParallelism() {
        setSteamDownloadLimitMbps(value: UInt64(max(0, min(1000, steamDownloadParallelism))))
    }

    /// Asks for the restart a newly chosen language waits for.
    private var languageRestart: some View {
        let language = model.language.resolved(system: AppLanguage.systemLanguages)
        return ObeliskRow(title: L10n.string("ui.settings.language_restart_title")) {
            SymbolTile(name: "arrow.clockwise", colors: TileColors.violet)
        } detail: {
            Text(L10n.string("ui.settings.language_restart", AppIdentity.name,
                             L10n.locale.localizedString(forLanguageCode: language) ?? AppLanguage.name(of: language)))
        } accessory: {
            AccentCapsuleButton(title: L10n.string("ui.settings.restart"), identifier: "settings-restart",
                                 action: { model.restart() })
        }
    }

    private var automaticUpdates: some View {
        ObeliskRow(title: L10n.string("ui.settings.update_automatically")) {
            SymbolTile(name: "arrow.down", colors: ObeliskStyle.installGradient)
        } detail: {
            Text(L10n.string("ui.settings.update_automatically_description"))
        } accessory: {
            Toggle(L10n.string("ui.settings.update_automatically"), isOn: $model.updatesGamesAutomatically)
                .toggleStyle(.switch)
                .labelsHidden()
                .pointerCursor()
                .accessibilityIdentifier("settings-update-automatically")
        }
    }

    private var sharePerformance: some View {
        ObeliskRow(title: L10n.string("ui.settings.share_performance")) {
            SymbolTile(name: "chart.bar.fill", colors: TileColors.violet)
        } detail: {
            Text(L10n.string("ui.settings.share_performance_description"))
        } accessory: {
            Toggle(L10n.string("ui.settings.share_performance"), isOn: $model.sharesGamePerformance)
                .toggleStyle(.switch)
                .labelsHidden()
                .pointerCursor()
                .accessibilityIdentifier("settings-share-performance")
        }
    }

    private var overlay: some View {
        ObeliskRow(title: L10n.string("ui.settings.overlay")) {
            SymbolTile(name: "rectangle.on.rectangle", colors: TileColors.violet)
        } detail: {
            Text(L10n.string("ui.settings.overlay_description", AppIdentity.name))
        } accessory: {
            HStack(spacing: 5) {
                KeyCap(text: L10n.string("ui.settings.key_shift"))
                KeyCap(text: L10n.string("ui.settings.key_tab"))
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(L10n.string("ui.settings.overlay_shortcut"))
        }
    }

    private var installUpdates: some View {
        ObeliskRow(title: L10n.string("ui.settings.install_updates", AppIdentity.name)) {
            SymbolTile(name: "arrow.triangle.2.circlepath", colors: TileColors.violet)
        } detail: {
            Text(L10n.string("ui.settings.install_updates_description", AppIdentity.name))
        } accessory: {
            Toggle(L10n.string("ui.settings.install_updates", AppIdentity.name),
                   isOn: Bindable(model.updates).installsAutomatically)
                .toggleStyle(.switch)
                .labelsHidden()
                .pointerCursor()
                .accessibilityIdentifier("settings-install-updates")
        }
    }

    private var version: some View {
        ObeliskRow(title: L10n.string("ui.settings.version", AppIdentity.version)) {
            SymbolTile(name: "info", colors: TileColors.grey)
        } detail: {
            Text(L10n.string("ui.settings.version_description", AppIdentity.name))
        } accessory: {
            GlassCapsuleButton(title: L10n.string("ui.settings.check_updates"), systemImage: "arrow.clockwise",
                                identifier: "settings-check-updates", action: model.updates.check)
                .disabled(!model.updates.canCheck)
        }
    }

    private var reportBug: some View {
        ObeliskRow(title: L10n.string("ui.settings.report_bug")) {
            SymbolTile(name: "ladybug.fill", colors: TileColors.red)
        } detail: {
            Text(L10n.string("ui.settings.report_bug_description", AppIdentity.name))
        } accessory: {
            GlassCapsuleButton(title: L10n.string("ui.settings.report"), systemImage: "paperplane",
                                identifier: "settings-report-bug") {
                model.reportBug(.app)
            }
        }
    }

    private var requestFeature: some View {
        ObeliskRow(title: L10n.string("ui.settings.request_feature")) {
            SymbolTile(name: "lightbulb.fill", colors: TileColors.gold)
        } detail: {
            Text(L10n.string("ui.settings.request_feature_description", AppIdentity.name))
        } accessory: {
            GlassCapsuleButton(title: L10n.string("ui.settings.suggest"), systemImage: "arrow.up.right",
                                identifier: "settings-request-feature") {
                NSWorkspace.shared.open(GitHubIssues.feature(obeliskVersion: AppIdentity.version))
            }
        }
    }

    private var storage: some View {
        let moving = model.gamesMoving
        let outside = moving > 0 ? 0 : model.gamesOutsideFolder
        return ObeliskCard {
            gamesFolder(isMoving: moving > 0)
            if moving > 0 {
                ObeliskCardDivider()
                gamesMoving(moving)
            } else if outside > 0 {
                ObeliskCardDivider()
                gamesOutside(outside)
            }
            ObeliskCardDivider()
            libraryFolder
        }
    }

    private func gamesFolder(isMoving: Bool) -> some View {
        let folder = model.gamesFolder
        return ObeliskRow(title: L10n.string("ui.settings.games_folder")) {
            SymbolTile(name: folder.isDefault ? "folder.fill" : "externaldrive.fill", colors: TileColors.blue)
        } detail: {
            VStack(alignment: .leading, spacing: 4) {
                Text(L10n.string("ui.settings.games_folder_description", AppIdentity.name))
                if !folder.isConnected {
                    Label(L10n.string("ui.settings.games_folder_disconnected"), systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                }
                FolderPath(path: folder.path)
            }
        } accessory: {
            HStack(spacing: 8) {
                StorefrontGlassButton(systemName: "folder", size: 13, label: L10n.string("ui.settings.show_in_finder"),
                                      identifier: "settings-show-games-folder") {
                    NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: folder.path)
                }
                .help(L10n.string("ui.settings.show_in_finder"))
                if folder.isDefault {
                    GlassCapsuleButton(title: L10n.string("ui.settings.games_folder_change"),
                                       identifier: "settings-change-games-folder") { picksGamesFolder = true }
                } else {
                    GlassCapsuleMenu(title: L10n.string("ui.settings.games_folder_change"),
                                     identifier: "settings-change-games-folder") {
                        Button(L10n.string("ui.settings.games_folder_choose")) { picksGamesFolder = true }
                        Button(L10n.string("ui.settings.games_folder_back")) { folder.proposeDefault() }
                    }
                }
            }
            .disabled(isMoving)
        }
    }

    private func gamesMoving(_ count: Int) -> some View {
        ObeliskRow(title: L10n.string("ui.settings.games_moving", Int64(count))) {
            SymbolTile(name: "arrow.right.arrow.left", colors: TileColors.blue)
        } detail: {
            Text(L10n.string("ui.settings.games_moving_description"))
        } accessory: {
            GlassCapsuleButton(title: L10n.string("ui.settings.games_moving_show"),
                               identifier: "settings-show-moves") { model.downloads.isPresented = true }
        }
    }

    private func gamesOutside(_ count: Int) -> some View {
        ObeliskRow(title: L10n.string("ui.settings.games_outside", Int64(count))) {
            SymbolTile(name: "exclamationmark.triangle.fill", colors: TileColors.gold)
        } detail: {
            Text(L10n.string("ui.settings.games_outside_description"))
        } accessory: {
            GlassCapsuleButton(title: L10n.string("ui.settings.games_outside_move"),
                               identifier: "settings-move-outside-games", action: model.gamesFolder.gather)
        }
    }

    private var libraryFolder: some View {
        ObeliskRow(title: L10n.string("ui.settings.library_folder")) {
            SymbolTile(name: "folder.fill", colors: TileColors.blue)
        } detail: {
            VStack(alignment: .leading, spacing: 4) {
                Text(L10n.string("ui.settings.library_folder_description", AppIdentity.name))
                FolderPath(path: model.libraryDirectory)
            }
        } accessory: {
            GlassCapsuleButton(title: L10n.string("ui.settings.show_in_finder"), systemImage: "folder",
                                identifier: "settings-show-library-folder") {
                NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: model.libraryDirectory)
            }
        }
    }

    /// What a storefront's row tells of its account, as Home's tile does.
    private func status(of section: LibrarySection) -> LibraryTile.Status {
        switch section {
        case .steam:
            status(of: model.steam, in: section, showsLibrary: model.steam.showsLibrary,
                   isRestoring: model.steam.isRestoring, account: model.steam.displayName,
                   avatarURL: model.steam.avatarURL)
        case .epic:
            status(of: model.epic, in: section, showsLibrary: model.epic.showsLibrary,
                   isRestoring: model.epic.isRestoring, account: model.epic.accountName,
                   avatarURL: model.epic.avatarURL)
        default:
            status(of: model.gog, in: section, showsLibrary: model.gog.showsLibrary,
                   isRestoring: model.gog.isRestoring, account: model.gog.accountName,
                   avatarURL: model.gog.avatarURL)
        }
    }

    private func status<Store: LibraryStorefront>(
        of store: Store, in section: LibrarySection, showsLibrary: Bool, isRestoring: Bool, account: String?,
        avatarURL: URL?
    ) -> LibraryTile.Status {
        if showsLibrary {
            .signedIn(account: account, avatarURL: avatarURL, games: store.library.games.count,
                      installed: Set(store.installations.receipts.map(\.gameID)).count)
        } else if isRestoring || model.restoringStorefronts.contains(section) {
            .restoring
        } else {
            .signedOut
        }
    }
}

/// The settings' bar: the window's drag region, whose backdrop and title show
/// once the hero scrolled under it, as a game page's.
private struct SettingsBar: View {
    let showsBackdrop: Bool
    let showsTitle: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        StorefrontWindowDragRegion()
            .frame(maxWidth: .infinity)
            .frame(height: ObeliskStyle.titleBarHeight)
            .overlay {
                ZStack {
                    if showsTitle {
                        Text(L10n.string("ui.settings.title"))
                            .font(.system(size: 13, weight: .semibold))
                            .transition(.opacity.combined(with: .offset(y: 6)))
                    }
                }
                .animation(reduceMotion ? nil : .easeOut(duration: 0.2), value: showsTitle)
                .allowsHitTesting(false)
                .accessibilityHidden(true)
            }
            .background {
                Rectangle()
                    .fill(.bar)
                    .opacity(showsBackdrop ? 1 : 0)
                    .animation(reduceMotion ? nil : .easeInOut(duration: 0.2), value: showsBackdrop)
                    .allowsHitTesting(false)
            }
    }
}

/// The settings' hero: Obelisk's icon as a wide scene, then the page's title
/// and Obelisk's version, light in both appearances as a library hero's.
private struct SettingsHero: View {
    /// The hero's height below the bar it reaches under.
    let height: CGFloat

    var body: some View {
        let reach = ObeliskStyle.titleBarHeight
        return HeroBackdrop { SettingsHeroArtwork() }
            .overlay(alignment: .bottomLeading) {
                VStack(alignment: .leading, spacing: 10) {
                    Text(L10n.string("ui.settings.title"))
                        .font(.system(size: 40, weight: .bold))
                        .tracking(-1)
                    HStack(spacing: 16) {
                        Label {
                            Text(AppIdentity.name)
                        } icon: {
                            Image("MenuBarIcon")
                                .renderingMode(.template)
                                .resizable()
                                .scaledToFit()
                                .frame(width: 13, height: 13)
                        }
                        Label(L10n.string("ui.settings.version", AppIdentity.version), systemImage: "tag")
                            .textSelection(.enabled)
                    }
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(.white.opacity(0.85))
                }
                .foregroundStyle(.white)
                .shadow(color: .black.opacity(0.35), radius: 10, y: 2)
                .padding(.horizontal, ObeliskStyle.contentPadding)
                .padding(.bottom, 28)
                .accessibilityElement(children: .combine)
                .accessibilityAddTraits(.isHeader)
                // The text fades out as it slides under the bar, as a library
                // hero's.
                .visualEffect { [reach] content, geometry in
                    let top = geometry.frame(in: .scrollView).minY
                    return content.opacity(min(max((top - reach + 40) / 80, 0), 1))
                }
            }
            .frame(height: height + reach)
            .padding(.top, -reach)
            .environment(\.colorScheme, .dark)
    }
}

/// Obelisk's icon, wide: the stores' colored beams meet in a glass prism on
/// the trailing side and leave it as one white beam, on the icon's night sky.
private struct SettingsHeroArtwork: View {
    var body: some View {
        Canvas { context, size in
            SettingsHeroPrism(size: size).draw(in: &context)
        }
        .accessibilityHidden(true)
    }
}

/// The icon's prism scene, in the icon's 1024-point units, placed on the
/// hero: the prism on the trailing side, its beams reaching toward the leading
/// side but fading out before the title.
private struct SettingsHeroPrism {
    let size: CGSize

    private static let apex = CGPoint(x: 512, y: 300)
    private static let baseLeading = CGPoint(x: 292, y: 681)
    private static let baseTrailing = CGPoint(x: 732, y: 681)
    private static let center = CGPoint(x: 512, y: 490)
    /// The stores' beams, magenta to cyan: where the icon starts each one and
    /// how far down the prism's leading side it enters.
    private static let beams: [(color: Color, start: CGPoint, entry: CGFloat)] = [
        (Color(red: 1, green: 0.31, blue: 0.847), CGPoint(x: 20, y: 600), 0.46),
        (Color(red: 0.753, green: 0.294, blue: 1), CGPoint(x: 20, y: 750), 0.505),
        (Color(red: 0.482, green: 0.361, blue: 1), CGPoint(x: 70, y: 890), 0.55),
        (Color(red: 0.298, green: 0.553, blue: 1), CGPoint(x: 170, y: 1000), 0.595),
        (Color(red: 0.247, green: 0.878, blue: 1), CGPoint(x: 310, y: 1080), 0.64),
    ]
    /// Where the white beam leaves the prism's trailing side.
    private static let exit = (on(apex, baseTrailing, 0.40), on(apex, baseTrailing, 0.47))

    /// Points per icon unit: the prism stands 58% of the hero's height.
    private var scale: CGFloat { size.height * 0.58 / 381 }

    /// How far the beams run past where the icon starts them: further on a wide
    /// hero, their faded start kept clear of the title on a narrow one.
    private var reach: CGFloat {
        let first = Self.beams[0]
        let entry = place(Self.on(Self.apex, Self.baseLeading, first.entry))
        let run = (Self.on(Self.apex, Self.baseLeading, first.entry).x - first.start.x) * scale
        return min(max((entry.x - 300) / run, 0.7), 1.8)
    }

    func draw(in context: inout GraphicsContext) {
        context.fill(Path(CGRect(origin: .zero, size: size)), with: .radialGradient(
            Gradient(stops: [
                .init(color: Color(red: 0.165, green: 0.102, blue: 0.416), location: 0),
                .init(color: Color(red: 0.067, green: 0.043, blue: 0.2), location: 0.55),
                .init(color: Color(red: 0.02, green: 0.012, blue: 0.071), location: 1),
            ]),
            center: place(CGPoint(x: 560, y: 420)), startRadius: 0, endRadius: max(size.width * 0.8, 680 * scale)))

        let beams = Self.beams.map { beam in
            let entry = Self.on(Self.apex, Self.baseLeading, beam.entry)
            let start = Self.on(entry, beam.start, reach)
            let length = hypot(start.x - entry.x, start.y - entry.y)
            let normal = CGPoint(x: -(entry.y - start.y) / length, y: (entry.x - start.x) / length)
            let startWidth = 6 + 20 * reach
            let path = polygon([
                CGPoint(x: start.x + normal.x * startWidth, y: start.y + normal.y * startWidth),
                CGPoint(x: entry.x + normal.x * 6, y: entry.y + normal.y * 6),
                CGPoint(x: entry.x - normal.x * 6, y: entry.y - normal.y * 6),
                CGPoint(x: start.x - normal.x * startWidth, y: start.y - normal.y * startWidth),
            ])
            let shading = GraphicsContext.Shading.linearGradient(
                Gradient(stops: [
                    .init(color: beam.color.opacity(0), location: 0),
                    .init(color: beam.color.opacity(0.85), location: 0.5),
                    .init(color: beam.color, location: 1),
                ]),
                startPoint: place(start), endPoint: place(entry))
            return (path, shading)
        }
        context.drawLayer { glow in
            glow.addFilter(.blur(radius: 24 * scale))
            for (path, shading) in beams { glow.fill(path, with: shading) }
        }
        for (path, shading) in beams { context.fill(path, with: shading) }

        // The light crossing the prism, from the beams to the white beam.
        let (exitTop, exitBottom) = Self.exit
        let entries = Self.beams.map { Self.on(Self.apex, Self.baseLeading, $0.entry) }
        context.drawLayer { inner in
            inner.addFilter(.blur(radius: 8 * scale))
            inner.fill(polygon([entries[0], entries[4], exitBottom, exitTop]), with: .linearGradient(
                Gradient(colors: [.white.opacity(0.45), .white.opacity(0.95)]),
                startPoint: place(entries[2]), endPoint: place(exitTop)))
        }

        let beamOut = polygon([
            exitTop, Self.on(exitTop, CGPoint(x: 1060, y: 170), 3),
            Self.on(exitBottom, CGPoint(x: 1060, y: 250), 3), exitBottom,
        ])
        context.drawLayer { glow in
            glow.addFilter(.blur(radius: 24 * scale))
            glow.fill(beamOut, with: .color(Color(red: 1, green: 0.839, blue: 0.965).opacity(0.9)))
        }
        context.fill(beamOut, with: .linearGradient(
            Gradient(colors: [.white, .white.opacity(0.75)]),
            startPoint: place(exitTop), endPoint: place(CGPoint(x: 1024, y: 200))))

        let prism = polygon([Self.apex, Self.baseTrailing, Self.baseLeading])
        let top = place(Self.apex)
        let bottom = CGPoint(x: top.x, y: place(Self.baseLeading).y)
        context.fill(prism, with: .linearGradient(
            Gradient(colors: [.white.opacity(0.3), .white.opacity(0.06)]), startPoint: top, endPoint: bottom))
        context.stroke(prism, with: .linearGradient(
            Gradient(colors: [.white.opacity(0.95), .white.opacity(0.35)]), startPoint: top, endPoint: bottom),
            style: StrokeStyle(lineWidth: 10 * scale, lineJoin: .round))
    }

    /// An icon point on the hero: the prism's center 240 points from the
    /// trailing edge, a little below the middle.
    private func place(_ point: CGPoint) -> CGPoint {
        CGPoint(x: size.width - 240 + (point.x - Self.center.x) * scale,
                y: size.height * 0.56 + (point.y - Self.center.y) * scale)
    }

    private func polygon(_ points: [CGPoint]) -> Path {
        Path { path in
            path.addLines(points.map(place))
            path.closeSubpath()
        }
    }

    private static func on(_ from: CGPoint, _ to: CGPoint, _ fraction: CGFloat) -> CGPoint {
        CGPoint(x: from.x + (to.x - from.x) * fraction, y: from.y + (to.y - from.y) * fraction)
    }
}

/// A key of a keyboard shortcut.
private struct KeyCap: View {
    let text: String

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: 7, style: .continuous)
        Text(text)
            .font(.system(size: 12, weight: .semibold))
            .padding(.horizontal, 9)
            .frame(minWidth: 30, minHeight: 26)
            .background(.primary.opacity(0.06), in: shape)
            .overlay { shape.strokeBorder(.primary.opacity(0.14), lineWidth: 0.75) }
            // The key's lower edge, as on a keyboard.
            .background(.primary.opacity(0.12), in: shape.offset(y: 1.5))
            .fixedSize()
    }
}

/// The three appearances as small windows of Obelisk, the chosen one ringed in
/// the accent color; the ring slides to a new choice.
private struct AppearancePicker: View {
    @Binding var selection: AppAppearance
    @State private var hovered: AppAppearance?
    @Namespace private var namespace
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        HStack(spacing: 20) {
            ForEach(AppAppearance.allCases) { option($0) }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(L10n.string("ui.settings.theme"))
    }

    private func option(_ appearance: AppAppearance) -> some View {
        let isSelected = appearance == selection
        let window = RoundedRectangle(cornerRadius: 10, style: .continuous)
        return Button {
            withAnimation(reduceMotion ? nil : ObeliskStyle.selectionAnimation) { selection = appearance }
        } label: {
            VStack(spacing: 9) {
                AppearancePreview(appearance: appearance)
                    .frame(width: 150, height: 96)
                    .clipShape(window)
                    .overlay { window.strokeBorder(.primary.opacity(0.14), lineWidth: 1) }
                    .shadow(color: .black.opacity(0.18), radius: 6, y: 3)
                    .padding(4)
                    .overlay {
                        if isSelected {
                            RoundedRectangle(cornerRadius: 14, style: .continuous)
                                .strokeBorder(ObeliskStyle.accent, lineWidth: 2.5)
                                .matchedGeometryEffect(id: "selection", in: namespace)
                        }
                    }
                    .scaleEffect(hovered == appearance && !isSelected && !reduceMotion ? 1.03 : 1)
                Text(appearance.title)
                    .font(.system(size: 12, weight: isSelected ? .semibold : .medium))
                    .foregroundStyle(isSelected ? .primary : .secondary)
            }
            // The choices share the card's width.
            .frame(maxWidth: .infinity)
            .contentShape(.rect)
        }
        .obeliskButtonStyle(.plain)
        .onHover { hovered = $0 ? appearance : (hovered == appearance ? nil : hovered) }
        .animation(reduceMotion ? nil : .spring(response: 0.3, dampingFraction: 0.75), value: hovered)
        .accessibilityLabel(appearance.title)
        .accessibilityAddTraits(isSelected ? .isSelected : [])
        .accessibilityIdentifier("settings-appearance-\(appearance.id)")
    }
}

/// The Mac's language, then every language Obelisk is translated into, by its
/// flag and its own name, the chosen one ringed in the accent color as an
/// appearance is; the ring slides to a new choice.
private struct LanguagePicker: View {
    @Binding var selection: AppLanguage
    @State private var hovered: AppLanguage?
    @Namespace private var namespace
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        HStack(spacing: 20) {
            ForEach(AppLanguage.choices) { option($0) }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(L10n.string("ui.settings.language"))
    }

    private func option(_ language: AppLanguage) -> some View {
        let isSelected = language == selection
        let title = language.code.map(AppLanguage.name(of:)) ?? L10n.string("ui.settings.language_system")
        return Button {
            withAnimation(reduceMotion ? nil : ObeliskStyle.selectionAnimation) { selection = language }
        } label: {
            VStack(spacing: 9) {
                LanguageFlag(language: language)
                    .frame(width: 81, height: 54)
                    .shadow(color: .black.opacity(0.18), radius: 6, y: 3)
                    .padding(5)
                    .overlay {
                        if isSelected {
                            RoundedRectangle(cornerRadius: 12, style: .continuous)
                                .strokeBorder(ObeliskStyle.accent, lineWidth: 2.5)
                                .matchedGeometryEffect(id: "selection", in: namespace)
                        }
                    }
                    .scaleEffect(hovered == language && !isSelected && !reduceMotion ? 1.04 : 1)
                Text(title)
                    .font(.system(size: 12, weight: isSelected ? .semibold : .medium))
                    .foregroundStyle(isSelected ? .primary : .secondary)
            }
            // The choices share the card's width, as the appearances do.
            .frame(maxWidth: .infinity)
            .contentShape(.rect)
        }
        .obeliskButtonStyle(.plain)
        .onHover { hovered = $0 ? language : (hovered == language ? nil : hovered) }
        .animation(reduceMotion ? nil : .spring(response: 0.3, dampingFraction: 0.75), value: hovered)
        .accessibilityLabel(title)
        .accessibilityAddTraits(isSelected ? .isSelected : [])
        .accessibilityIdentifier("settings-language-\(language.id)")
    }
}

/// A language's flag, two split across the diagonal as the system's
/// appearance is for a language several countries speak, and for the Mac's
/// language a globe; on a flag's rounded shape, edged so a white stripe stays
/// visible.
private struct LanguageFlag: View {
    let language: AppLanguage

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: 7, style: .continuous)
        let flags = language.flags.filter { NSImage(named: $0) != nil }
        ZStack {
            if let first = flags.first {
                Image(first)
                    .resizable()
                if let second = flags.dropFirst().first {
                    // The second flag's middle, where its emblem sits, moves
                    // to the middle of its half.
                    GeometryReader { geometry in
                        Image(second)
                            .resizable()
                            .offset(x: geometry.size.width * (1 - (TrailingDiagonalHalf.top + TrailingDiagonalHalf.bottom) / 2) / 2)
                    }
                    .mask { TrailingDiagonalHalf() }
                    // A white edge parts two flags of the same colors.
                    DiagonalEdge()
                        .stroke(.white, lineWidth: 2.5)
                        .shadow(color: .black.opacity(0.35), radius: 1.5)
                }
            } else {
                LinearGradient(colors: [Color(red: 0.38, green: 0.68, blue: 0.98), Color(red: 0.2, green: 0.45, blue: 0.9)],
                               startPoint: .top, endPoint: .bottom)
                Image(systemName: language.code == nil ? "globe" : "character.bubble")
                    .font(.system(size: 22, weight: .medium))
                    .foregroundStyle(.white)
            }
        }
        .clipShape(shape)
        .overlay { shape.strokeBorder(.primary.opacity(0.14), lineWidth: 1) }
        .accessibilityHidden(true)
    }
}

/// A small window of Obelisk in one appearance; the system's is light, then
/// dark past a diagonal.
private struct AppearancePreview: View {
    let appearance: AppAppearance

    var body: some View {
        switch appearance {
        case .light: MiniatureWindow(colorScheme: .light)
        case .dark: MiniatureWindow(colorScheme: .dark)
        case .system:
            MiniatureWindow(colorScheme: .light)
                .overlay { MiniatureWindow(colorScheme: .dark).mask { TrailingDiagonalHalf() } }
        }
    }
}

/// Obelisk's window in miniature: its sidebar, a hero and a shelf of covers.
private struct MiniatureWindow: View {
    let colorScheme: ColorScheme

    private static let covers = [
        Color(red: 0.95, green: 0.58, blue: 0.25), Color(red: 0.3, green: 0.56, blue: 0.95),
        Color(red: 0.4, green: 0.76, blue: 0.45), Color(red: 0.92, green: 0.4, blue: 0.6),
    ]

    var body: some View {
        let isDark = colorScheme == .dark
        let line = isDark ? Color.white.opacity(0.22) : Color.black.opacity(0.13)
        HStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 5) {
                HStack(spacing: 2.5) {
                    ForEach([Color.red, .yellow, .green], id: \.self) { color in
                        Circle().fill(color.opacity(0.85)).frame(width: 4.5, height: 4.5)
                    }
                }
                .padding(.bottom, 3)
                Capsule().fill(ObeliskStyle.accent.opacity(0.85)).frame(width: 24, height: 5)
                ForEach([18, 21, 15] as [CGFloat], id: \.self) { width in
                    Capsule().fill(line).frame(width: width, height: 4)
                }
            }
            .padding(6)
            .frame(width: 42, alignment: .topLeading)
            .frame(maxHeight: .infinity, alignment: .top)
            .background(isDark ? Color(white: 0.19) : Color(white: 0.91))
            VStack(alignment: .leading, spacing: 6) {
                LinearGradient(colors: [Color(red: 0.62, green: 0.2, blue: 0.92), Color(red: 0.25, green: 0.12, blue: 0.6)],
                               startPoint: .topTrailing, endPoint: .bottomLeading)
                    .frame(height: 38)
                    .overlay(alignment: .bottomLeading) {
                        Capsule().fill(.white.opacity(0.8)).frame(width: 30, height: 4).padding(6)
                    }
                HStack(spacing: 4) {
                    ForEach(Self.covers, id: \.self) { color in
                        RoundedRectangle(cornerRadius: 2.5, style: .continuous)
                            .fill(color.opacity(isDark ? 0.8 : 0.9))
                            .frame(width: 22, height: 33)
                    }
                }
                .padding(.leading, 6)
                .frame(maxWidth: .infinity, alignment: .leading)
                .clipped()
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .background(ObeliskPalette(colorScheme: colorScheme).background)
        }
    }
}

/// The part of a rectangle past a diagonal from its top to its bottom edge.
private struct TrailingDiagonalHalf: Shape {
    /// Where the diagonal meets the top and the bottom edge, from the leading
    /// one.
    static let top = 0.62
    static let bottom = 0.38

    func path(in rect: CGRect) -> Path {
        Path { path in
            path.move(to: CGPoint(x: rect.minX + rect.width * Self.top, y: rect.minY))
            path.addLine(to: CGPoint(x: rect.maxX, y: rect.minY))
            path.addLine(to: CGPoint(x: rect.maxX, y: rect.maxY))
            path.addLine(to: CGPoint(x: rect.minX + rect.width * Self.bottom, y: rect.maxY))
            path.closeSubpath()
        }
    }
}

/// The diagonal that bounds `TrailingDiagonalHalf`, past both edges so a
/// stroke reaches the corners.
private struct DiagonalEdge: Shape {
    func path(in rect: CGRect) -> Path {
        let slope = rect.width * (TrailingDiagonalHalf.bottom - TrailingDiagonalHalf.top) / rect.height
        return Path { path in
            path.move(to: CGPoint(x: rect.minX + rect.width * TrailingDiagonalHalf.top - slope * 4, y: rect.minY - 4))
            path.addLine(to: CGPoint(x: rect.minX + rect.width * TrailingDiagonalHalf.bottom + slope * 4,
                                     y: rect.maxY + 4))
        }
    }
}

/// A storefront and its account, as Home's tile tells it, opening its library
/// or its sign-in.
private struct SettingsStoreRow: View {
    let section: LibrarySection
    let status: LibraryTile.Status
    let open: () -> Void

    private var name: String { section.storeName ?? section.title }

    var body: some View {
        ObeliskRow(title: name) {
            StoreTile(section: section)
        } detail: {
            detail
        } accessory: {
            accessory
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("settings-store-\(section.rawValue)")
    }

    @ViewBuilder private var detail: some View {
        switch status {
        case let .signedIn(account, avatarURL, games, installed):
            HStack(spacing: 6) {
                if let account, !account.isEmpty {
                    AccountAvatar(name: account, url: avatarURL)
                        .frame(width: 16, height: 16)
                    Text(account)
                        .foregroundStyle(.primary)
                    Text(verbatim: "·")
                }
                Text([L10n.string("ui.library.game_count", Int64(games)),
                      installed > 0 ? L10n.string("ui.home.installed_count", Int64(installed)) : nil]
                    .compactMap { $0 }
                    .joined(separator: " · "))
                    .monospacedDigit()
            }
            .lineLimit(1)
        case .restoring:
            HStack(spacing: 6) {
                ProgressView().controlSize(.mini)
                Text(L10n.string("ui.account.signing_in"))
            }
        case .signedOut:
            Text(L10n.string("ui.home.signed_out"))
        }
    }

    @ViewBuilder private var accessory: some View {
        switch status {
        case .signedIn:
            GlassCapsuleButton(title: L10n.string("ui.settings.open"),
                                accessibilityLabel: L10n.string("ui.settings.open_store", name),
                                identifier: "settings-open-\(section.rawValue)", action: open)
        case .restoring:
            EmptyView()
        case .signedOut:
            AccentCapsuleButton(title: L10n.string("ui.account.sign_in"),
                                 accessibilityLabel: L10n.string("ui.home.sign_in_store", name),
                                 identifier: "settings-sign-in-\(section.rawValue)", action: open)
        }
    }
}

/// A folder's path under a row's description, whole in its help tag.
private struct FolderPath: View {
    let path: String

    var body: some View {
        Text(path)
            .font(.system(size: 11, design: .monospaced))
            .foregroundStyle(.tertiary)
            .lineLimit(1)
            .truncationMode(.middle)
            .textSelection(.enabled)
            .help(path)
    }
}

#if DEBUG
/// Debug builds only: each modal Obelisk presents, opened over made-up data
/// to check how it looks.
private struct DeveloperModalsCard: View {
    @State private var modals = DeveloperModals()

    var body: some View {
        let all = DeveloperModal.all
        ObeliskCard {
            ForEach(all) { modal in
                if modal.id != all.first?.id { ObeliskCardDivider() }
                ObeliskRow(title: modal.title) {
                    SymbolTile(name: modal.symbol, colors: TileColors.grey)
                } detail: {
                    Text(modal.detail)
                } accessory: {
                    open(modal)
                }
            }
        }
        .developerModals(modals)
    }

    /// Opens the modal, or a menu of its states when it has several.
    @ViewBuilder private func open(_ modal: DeveloperModal) -> some View {
        if modal.variants.count == 1, let variant = modal.variants.first {
            GlassCapsuleButton(title: L10n.string("ui.settings.open"), identifier: "developer-modal-\(modal.id)") {
                variant.open(modals)
            }
        } else {
            GlassCapsuleMenu(title: L10n.string("ui.settings.open"), identifier: "developer-modal-\(modal.id)") {
                ForEach(modal.variants) { variant in
                    Button(variant.title) { variant.open(modals) }
                }
            }
        }
    }
}
#endif
