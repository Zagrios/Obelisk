import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// Sheet to add a new custom Windows game or edit an existing one.
struct AddCustomGameSheet: View {
    @Bindable var model: CustomGamesModel
    let editingGame: CustomGame?

    @State private var title: String
    @State private var executablePath: String
    @State private var launchArguments: String
    @State private var customCoverPath: String?
    @State private var customBannerPath: String?
    @State private var winePrefixPath: String
    @State private var runnerPath: String
    @State private var engine: LaunchEngine
    @State private var graphics: LaunchGraphics
    @State private var rayTracing: Bool
    @State private var performanceHud: Bool
    @State private var renderPercent: UInt32
    @State private var showsAdvanced: Bool = false
    @State private var cropTarget: ArtworkCropTarget?

    @Environment(\.dismiss) private var dismiss

    init(model: CustomGamesModel, editingGame: CustomGame? = nil) {
        self.model = model
        self.editingGame = editingGame

        _title = State(initialValue: editingGame?.title ?? "")
        _executablePath = State(initialValue: editingGame?.executablePath ?? "")
        _launchArguments = State(initialValue: editingGame?.launchArguments ?? "")
        _customCoverPath = State(initialValue: editingGame?.customCoverPath)
        _customBannerPath = State(initialValue: editingGame?.customBannerPath)
        _winePrefixPath = State(initialValue: editingGame?.winePrefixPath ?? "")
        _runnerPath = State(initialValue: editingGame?.runnerPath ?? "")
        _engine = State(initialValue: editingGame?.engine ?? .rosetta)
        _graphics = State(initialValue: editingGame?.graphics ?? .d3dMetal)
        _rayTracing = State(initialValue: editingGame?.rayTracing ?? true)
        _performanceHud = State(initialValue: editingGame?.performanceHud ?? false)
        _renderPercent = State(initialValue: editingGame?.renderPercent ?? 100)
    }

    private var isValid: Bool {
        !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty &&
        !executablePath.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            SheetHeader(
                title: L10n.string(editingGame == nil ? "ui.custom.add_title" : "ui.custom.edit_title"),
                subtitle: editingGame?.title,
                symbol: "gamecontroller.fill",
                colors: TileColors.violet,
                close: { dismiss() },
                closeIdentifier: "add-custom-game-close"
            )
            .padding(.horizontal, 24)
            .padding(.top, 24)
            .padding(.bottom, 16)

            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    gameInfoSection
                    artworkSection
                    launchOptionsSection
                    compatibilitySection
                }
                .padding(.horizontal, 24)
                .padding(.bottom, 24)
            }

            Divider()

            bottomBar
                .padding(.horizontal, 24)
                .padding(.vertical, 16)
        }
        .frame(width: 580, height: 680)
        .tint(ObeliskStyle.accent)
        .sheet(item: $cropTarget) { target in
            ArtworkCropSheet(target: target) { croppedPath in
                if target.isCover {
                    customCoverPath = croppedPath
                } else {
                    customBannerPath = croppedPath
                }
            }
        }
    }

    // MARK: - Sections

    private var gameInfoSection: some View {
        SheetSection(title: L10n.string("ui.custom.game_information")) {
            ObeliskCard {
                VStack(alignment: .leading, spacing: 6) {
                    SheetFieldLabel(text: L10n.string("ui.custom.title_label"))
                    TextField(L10n.string("ui.custom.title_placeholder"), text: $title)
                        .textFieldStyle(.roundedBorder)
                        .accessibilityIdentifier("custom-game-title-input")
                }
                .padding(14)

                ObeliskCardDivider()

                VStack(alignment: .leading, spacing: 6) {
                    SheetFieldLabel(text: L10n.string("ui.custom.executable_path"))
                    HStack(spacing: 10) {
                        TextField(L10n.string("ui.custom.executable_path"), text: $executablePath)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 11, design: .monospaced))
                            .accessibilityIdentifier("custom-game-executable-input")

                        GlassCapsuleButton(
                            title: L10n.string("ui.custom.executable_browse"),
                            identifier: "custom-game-browse-exe",
                            action: pickExecutable
                        )
                    }

                    if !executablePath.isEmpty && !FileManager.default.fileExists(atPath: executablePath) {
                        Label(L10n.string("ui.custom.executable_missing"), systemImage: "exclamationmark.triangle.fill")
                            .font(.system(size: 11))
                            .foregroundStyle(.orange)
                            .padding(.top, 2)
                    }
                }
                .padding(14)
            }
        }
    }

    private var artworkSection: some View {
        SheetSection(title: L10n.string("ui.custom.artwork_section")) {
            ObeliskCard {
                HStack(spacing: 14) {
                    coverPreview
                    VStack(alignment: .leading, spacing: 6) {
                        Text(L10n.string("ui.custom.cover_image"))
                            .font(.system(size: 13, weight: .semibold))
                        HStack(spacing: 8) {
                            GlassCapsuleButton(
                                title: L10n.string("ui.custom.cover_choose"),
                                identifier: "custom-game-browse-cover",
                                action: pickCover
                            )
                            if let path = customCoverPath {
                                GlassCapsuleButton(
                                    title: L10n.string("ui.custom.crop_artwork"),
                                    identifier: "custom-game-crop-cover",
                                    action: { cropTarget = .cover(path: path) }
                                )
                                GlassCapsuleButton(
                                    title: L10n.string("ui.custom.remove_artwork"),
                                    identifier: "custom-game-remove-cover",
                                    action: { customCoverPath = nil }
                                )
                            }
                        }
                    }
                    Spacer(minLength: 0)
                }
                .padding(14)

                ObeliskCardDivider()

                HStack(spacing: 14) {
                    bannerPreview
                    VStack(alignment: .leading, spacing: 6) {
                        Text(L10n.string("ui.custom.banner_image"))
                            .font(.system(size: 13, weight: .semibold))
                        HStack(spacing: 8) {
                            GlassCapsuleButton(
                                title: L10n.string("ui.custom.banner_choose"),
                                identifier: "custom-game-browse-banner",
                                action: pickBanner
                            )
                            if let path = customBannerPath {
                                GlassCapsuleButton(
                                    title: L10n.string("ui.custom.crop_artwork"),
                                    identifier: "custom-game-crop-banner",
                                    action: { cropTarget = .banner(path: path) }
                                )
                                GlassCapsuleButton(
                                    title: L10n.string("ui.custom.remove_artwork"),
                                    identifier: "custom-game-remove-banner",
                                    action: { customBannerPath = nil }
                                )
                            }
                        }
                    }
                    Spacer(minLength: 0)
                }
                .padding(14)
            }
        }
    }

    private var coverPreview: some View {
        let shape = RoundedRectangle(cornerRadius: 8, style: .continuous)
        return Group {
            if let path = customCoverPath, let image = NSImage(contentsOfFile: path) {
                Image(nsImage: image)
                    .resizable()
                    .scaledToFill()
            } else {
                ZStack {
                    Color.primary.opacity(0.08)
                    Image(systemName: "photo")
                        .font(.system(size: 16))
                        .foregroundStyle(.secondary)
                }
            }
        }
        .frame(width: 44, height: 66)
        .clipShape(shape)
        .overlay { shape.strokeBorder(Color.primary.opacity(0.12), lineWidth: 1) }
    }

    private var bannerPreview: some View {
        let shape = RoundedRectangle(cornerRadius: 8, style: .continuous)
        return Group {
            if let path = customBannerPath, let image = NSImage(contentsOfFile: path) {
                Image(nsImage: image)
                    .resizable()
                    .scaledToFill()
            } else {
                ZStack {
                    Color.primary.opacity(0.08)
                    Image(systemName: "photo")
                        .font(.system(size: 16))
                        .foregroundStyle(.secondary)
                }
            }
        }
        .frame(width: 80, height: 44)
        .clipShape(shape)
        .overlay { shape.strokeBorder(Color.primary.opacity(0.12), lineWidth: 1) }
    }

    private var launchOptionsSection: some View {
        SheetSection(title: L10n.string("ui.custom.launch_arguments")) {
            ObeliskCard {
                VStack(alignment: .leading, spacing: 6) {
                    TextField(L10n.string("ui.custom.launch_arguments_placeholder"), text: $launchArguments)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 12))
                        .accessibilityIdentifier("custom-game-args-input")
                    Text(L10n.string("ui.custom.launch_arguments_description"))
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                }
                .padding(14)
            }
        }
    }

    private var compatibilitySection: some View {
        SheetSection(title: L10n.string("ui.custom.compatibility_section")) {
            ObeliskCard {
                ObeliskRow(title: L10n.string("ui.custom.engine")) {
                    SymbolTile(name: "cpu", colors: TileColors.violet)
                } detail: {
                    Text(engine == .rosetta ? L10n.string("ui.custom.engine_rosetta") : L10n.string("ui.custom.engine_linux"))
                } accessory: {
                    Picker(selection: $engine) {
                        Text(L10n.string("ui.custom.engine_rosetta")).tag(LaunchEngine.rosetta)
                        Text(L10n.string("ui.custom.engine_linux")).tag(LaunchEngine.linux)
                    } label: {
                        EmptyView()
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                }

                ObeliskCardDivider()

                HStack(spacing: 12) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(model.isEngineInstalled
                            ? L10n.string("ui.custom.engine_installed", model.installedEngineVersion ?? "Voile")
                            : L10n.string("ui.custom.engine_not_installed"))
                            .font(.system(size: 12, weight: .medium))
                            .foregroundStyle(model.isEngineInstalled ? Color.secondary : Color.orange)
                    }

                    Spacer(minLength: 0)

                    if model.isDownloadingEngine {
                        HStack(spacing: 8) {
                            ProgressView()
                                .controlSize(.small)
                            Text(L10n.string("ui.custom.downloading_engine"))
                                .font(.system(size: 11))
                                .foregroundStyle(.secondary)
                        }
                    } else {
                        GlassCapsuleButton(
                            title: L10n.string("ui.custom.download_engine"),
                            identifier: "custom-game-download-engine"
                        ) {
                            Task { await model.downloadEngine() }
                        }
                    }
                }
                .padding(14)

                ObeliskCardDivider()

                ObeliskRow(title: L10n.string("ui.custom.graphics")) {
                    SymbolTile(name: "display", colors: TileColors.blue)
                } detail: {
                    Text(graphics == .d3dMetal ? L10n.string("ui.custom.graphics_d3dmetal") : L10n.string("ui.custom.graphics_dxmt"))
                } accessory: {
                    Picker(selection: $graphics) {
                        Text(L10n.string("ui.custom.graphics_d3dmetal")).tag(LaunchGraphics.d3dMetal)
                        Text(L10n.string("ui.custom.graphics_dxmt")).tag(LaunchGraphics.dxmt)
                    } label: {
                        EmptyView()
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                }

                ObeliskCardDivider()

                VStack(alignment: .leading, spacing: 6) {
                    SheetFieldLabel(text: L10n.string("ui.custom.wine_prefix"))
                    HStack(spacing: 10) {
                        TextField(L10n.string("ui.custom.wine_prefix_default"), text: $winePrefixPath)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 11, design: .monospaced))

                        GlassCapsuleButton(
                            title: L10n.string("ui.custom.wine_prefix_browse"),
                            identifier: "custom-game-browse-prefix",
                            action: pickWinePrefix
                        )
                    }
                }
                .padding(14)

                ObeliskCardDivider()

                VStack(alignment: .leading, spacing: 6) {
                    SheetFieldLabel(text: L10n.string("ui.custom.custom_runner"))
                    HStack(spacing: 10) {
                        TextField(L10n.string("ui.custom.custom_runner_placeholder"), text: $runnerPath)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 11, design: .monospaced))

                        GlassCapsuleButton(
                            title: L10n.string("ui.custom.custom_runner_browse"),
                            identifier: "custom-game-browse-runner",
                            action: pickRunner
                        )
                    }
                }
                .padding(14)

                ObeliskCardDivider()

                Toggle(isOn: $rayTracing) {
                    Text(L10n.string("ui.custom.ray_tracing"))
                        .font(.system(size: 13, weight: .semibold))
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 10)

                ObeliskCardDivider()

                Toggle(isOn: $performanceHud) {
                    Text(L10n.string("ui.custom.performance_hud"))
                        .font(.system(size: 13, weight: .semibold))
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 10)

                ObeliskCardDivider()

                VStack(alignment: .leading, spacing: 8) {
                    Text(L10n.string("ui.custom.render_resolution", Int64(renderPercent)))
                        .font(.system(size: 13, weight: .semibold))
                    Slider(
                        value: Binding(
                            get: { Double(renderPercent) },
                            set: { renderPercent = UInt32($0) }
                        ),
                        in: 50...200,
                        step: 5
                    )
                }
                .padding(14)
            }
        }
    }

    private var bottomBar: some View {
        HStack(spacing: 12) {
            Spacer(minLength: 0)

            GlassCapsuleButton(
                title: L10n.string("ui.common.cancel"),
                size: .large,
                identifier: "custom-game-cancel",
                action: { dismiss() }
            )
            .keyboardShortcut(.cancelAction)

            AccentCapsuleButton(
                title: L10n.string(editingGame == nil ? "ui.custom.add_game" : "ui.custom.save"),
                size: .large,
                identifier: "custom-game-save",
                action: saveGame
            )
            .keyboardShortcut(.defaultAction)
            .disabled(!isValid)
        }
    }

    // MARK: - Actions

    private func pickExecutable() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.prompt = L10n.string("ui.custom.executable_select_prompt")
        if let exeType = UTType(filenameExtension: "exe") {
            panel.allowedContentTypes = [exeType]
        }

        if panel.runModal() == .OK, let url = panel.url {
            executablePath = url.path
            if title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                // Infer game name from executable without extension
                title = url.deletingPathExtension().lastPathComponent
            }
            // Auto-detect optimal graphics backend (DXMT for DX11, D3DMetal for DX12)
            graphics = recommendedGraphicsBackend(executablePath: url.path)
        }
    }

    private func pickCover() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = [UTType.png, UTType.jpeg, UTType.webP].compactMap { $0 }

        if panel.runModal() == .OK, let url = panel.url {
            let gameId = editingGame?.id ?? UUID().uuidString
            let path = model.copyArtwork(from: url, for: gameId, kind: "cover") ?? url.path
            customCoverPath = path
            cropTarget = .cover(path: path)
        }
    }

    private func pickBanner() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = [UTType.png, UTType.jpeg, UTType.webP].compactMap { $0 }

        if panel.runModal() == .OK, let url = panel.url {
            let gameId = editingGame?.id ?? UUID().uuidString
            let path = model.copyArtwork(from: url, for: gameId, kind: "banner") ?? url.path
            customBannerPath = path
            cropTarget = .banner(path: path)
        }
    }

    private func pickWinePrefix() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false

        if panel.runModal() == .OK, let url = panel.url {
            winePrefixPath = url.path
        }
    }

    private func pickRunner() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false

        if panel.runModal() == .OK, let url = panel.url {
            runnerPath = url.path
        }
    }

    private func saveGame() {
        let trimmedTitle = title.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedExe = executablePath.trimmingCharacters(in: .whitespacesAndNewlines)
        let prefix = winePrefixPath.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : winePrefixPath
        let runner = runnerPath.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : runnerPath

        if var game = editingGame {
            game.title = trimmedTitle
            game.executablePath = trimmedExe
            game.launchArguments = launchArguments.trimmingCharacters(in: .whitespacesAndNewlines)
            game.customCoverPath = customCoverPath
            game.customBannerPath = customBannerPath
            game.winePrefixPath = prefix
            game.runnerPath = runner
            game.engine = engine
            game.graphics = graphics
            game.rayTracing = rayTracing
            game.performanceHud = performanceHud
            game.renderPercent = renderPercent
            model.updateGame(game)
        } else {
            _ = model.addGame(
                title: trimmedTitle,
                executablePath: trimmedExe,
                launchArguments: launchArguments.trimmingCharacters(in: .whitespacesAndNewlines),
                customCoverPath: customCoverPath,
                customBannerPath: customBannerPath,
                winePrefixPath: prefix,
                runnerPath: runner,
                engine: engine,
                graphics: graphics,
                rayTracing: rayTracing,
                performanceHud: performanceHud,
                renderPercent: renderPercent
            )
        }

        dismiss()
    }
}
