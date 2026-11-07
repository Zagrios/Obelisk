import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// Sheet to configure launch settings and compatibility layer for a custom Windows game.
struct CustomLaunchSettingsSheet: View {
    @Bindable var model: CustomGamesModel
    let game: CustomGame

    @State private var launchArguments: String
    @State private var winePrefixPath: String
    @State private var runnerPath: String
    @State private var engine: LaunchEngine
    @State private var graphics: LaunchGraphics
    @State private var rayTracing: Bool
    @State private var performanceHud: Bool
    @State private var renderPercent: UInt32

    @Environment(\.dismiss) private var dismiss

    init(model: CustomGamesModel, game: CustomGame) {
        self.model = model
        self.game = game

        _launchArguments = State(initialValue: game.launchArguments)
        _winePrefixPath = State(initialValue: game.winePrefixPath ?? "")
        _runnerPath = State(initialValue: game.runnerPath ?? "")
        _engine = State(initialValue: game.engine)
        _graphics = State(initialValue: game.graphics)
        _rayTracing = State(initialValue: game.rayTracing)
        _performanceHud = State(initialValue: game.performanceHud)
        _renderPercent = State(initialValue: game.renderPercent)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            SheetHeader(
                title: L10n.string("ui.custom.launch_settings_title"),
                subtitle: game.title,
                symbol: "slider.horizontal.3",
                colors: TileColors.violet,
                close: { dismiss() },
                closeIdentifier: "custom-launch-settings-close"
            )
            .padding(.horizontal, 24)
            .padding(.top, 24)
            .padding(.bottom, 16)

            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    argumentsCard
                    engineCard
                    graphicsCard
                    prefixCard
                }
                .padding(.horizontal, 24)
                .padding(.bottom, 24)
            }

            Divider()

            bottomBar
                .padding(.horizontal, 24)
                .padding(.vertical, 16)
        }
        .frame(width: 560, height: 640)
        .tint(ObeliskStyle.accent)
    }

    private var argumentsCard: some View {
        SheetSection(title: L10n.string("ui.custom.launch_arguments")) {
            ObeliskCard {
                VStack(alignment: .leading, spacing: 6) {
                    TextField(L10n.string("ui.custom.launch_arguments_placeholder"), text: $launchArguments)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 12))
                    Text(L10n.string("ui.custom.launch_arguments_description"))
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                }
                .padding(14)
            }
        }
    }

    private var engineCard: some View {
        SheetSection(title: L10n.string("ui.custom.compatibility_section")) {
            ObeliskCard {
                ObeliskRow(title: L10n.string("ui.custom.engine")) {
                    SymbolTile(name: "cpu", colors: TileColors.violet)
                } detail: {
                    Text(LaunchSettingsSheet.name(of: engine))
                } accessory: {
                    Picker(selection: $engine) {
                        Text(LaunchSettingsSheet.name(of: LaunchEngine.rosetta)).tag(LaunchEngine.rosetta)
                        Text(LaunchSettingsSheet.name(of: LaunchEngine.linux)).tag(LaunchEngine.linux)
                    } label: {
                        EmptyView()
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                }

                ObeliskCardDivider()

                ObeliskRow(title: L10n.string("ui.custom.graphics")) {
                    SymbolTile(name: "display", colors: TileColors.blue)
                } detail: {
                    Text(LaunchSettingsSheet.name(of: graphics))
                } accessory: {
                    Picker(selection: $graphics) {
                        Text(LaunchSettingsSheet.name(of: LaunchGraphics.d3dMetal)).tag(LaunchGraphics.d3dMetal)
                        Text(LaunchSettingsSheet.name(of: LaunchGraphics.dxmt)).tag(LaunchGraphics.dxmt)
                    } label: {
                        EmptyView()
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                }

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
            }
        }
    }

    private var graphicsCard: some View {
        ObeliskCard {
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

    private var prefixCard: some View {
        ObeliskCard {
            VStack(alignment: .leading, spacing: 6) {
                SheetFieldLabel(text: L10n.string("ui.custom.wine_prefix"))
                HStack(spacing: 10) {
                    TextField(L10n.string("ui.custom.wine_prefix_default"), text: $winePrefixPath)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 11, design: .monospaced))

                    GlassCapsuleButton(
                        title: L10n.string("ui.custom.wine_prefix_browse"),
                        identifier: "custom-settings-browse-prefix",
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
                        identifier: "custom-settings-browse-runner",
                        action: pickRunner
                    )
                }
            }
            .padding(14)
        }
    }

    private var bottomBar: some View {
        HStack(spacing: 12) {
            Spacer(minLength: 0)

            GlassCapsuleButton(
                title: L10n.string("ui.common.cancel"),
                size: .large,
                identifier: "custom-settings-cancel",
                action: { dismiss() }
            )
            .keyboardShortcut(.cancelAction)

            AccentCapsuleButton(
                title: L10n.string("ui.custom.save"),
                size: .large,
                identifier: "custom-settings-save",
                action: saveSettings
            )
            .keyboardShortcut(.defaultAction)
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

    private func saveSettings() {
        var updated = game
        updated.launchArguments = launchArguments.trimmingCharacters(in: .whitespacesAndNewlines)
        let prefix = winePrefixPath.trimmingCharacters(in: .whitespacesAndNewlines)
        updated.winePrefixPath = prefix.isEmpty ? nil : prefix
        let runner = runnerPath.trimmingCharacters(in: .whitespacesAndNewlines)
        updated.runnerPath = runner.isEmpty ? nil : runner
        updated.engine = engine
        updated.graphics = graphics
        updated.rayTracing = rayTracing
        updated.performanceHud = performanceHud
        updated.renderPercent = renderPercent

        model.updateGame(updated)
        dismiss()
    }
}
