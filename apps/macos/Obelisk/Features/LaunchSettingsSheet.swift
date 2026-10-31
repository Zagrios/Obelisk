import SwiftUI

/// The launch settings of one installed Windows game, as its sheet shows
/// them: read from Rust, and kept there at each change.
@MainActor @Observable
final class LaunchSettingsModel: Identifiable {
    private(set) var settings: LaunchSettings?
    private(set) var isLoading = true
    private(set) var error: String?
    @ObservationIgnored private let load: () async throws -> LaunchSettings?
    @ObservationIgnored private let save: (LaunchChoices) async throws -> LaunchSettings?

    init(load: @escaping () async throws -> LaunchSettings?,
         save: @escaping (LaunchChoices) async throws -> LaunchSettings?) {
        self.load = load
        self.save = save
    }

    func appear() async {
        isLoading = true
        do {
            settings = try await load()
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
        isLoading = false
    }

    /// Changes one of the player's choices and keeps them: Rust drops those
    /// the game already has by default.
    func choose(_ change: (inout LaunchChoices) -> Void) {
        guard var choices = settings?.choices else { return }
        change(&choices)
        Task { await keep(choices) }
    }

    /// Keeps what the player typed as the game's launch arguments, when it
    /// changed. Spaces alone are no arguments.
    func setArguments(_ text: String) {
        let arguments = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let settings, (settings.choices.arguments ?? "") != arguments else { return }
        choose { $0.arguments = arguments.isEmpty ? nil : arguments }
    }

    /// Forgets every choice: the game runs as Obelisk knows it best.
    func reset() {
        Task { await keep(LaunchChoices()) }
    }

    private func keep(_ choices: LaunchChoices) async {
        do {
            if let saved = try await save(choices) { settings = saved }
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    var isCustomized: Bool { settings.map { $0.choices != LaunchChoices() } ?? false }

    /// What the chosen engine offers.
    var engineOffer: LaunchEngineOffer? {
        settings.flatMap { settings in settings.engines.first { $0.engine == settings.engine } }
    }

    /// What the chosen backend offers on the chosen engine.
    var graphicsOffer: LaunchGraphicsOffer? {
        guard let settings else { return nil }
        return engineOffer?.graphics.first { $0.graphics == settings.graphics }
    }
}

/// The sheet behind a Windows game's Launch Settings: the arguments added to
/// its command line, its engine, then the CPU translator that engine has,
/// then the graphics backend and its options, each as the chosen engine
/// allows, the render resolution where the engine offers one, and Metal's
/// Performance HUD, every one said in a sentence.
struct LaunchSettingsSheet: View {
    let model: LaunchSettingsModel
    let gameName: String
    /// The arguments as the player types them, kept once the field is done.
    @State private var arguments = ""
    @FocusState private var editsArguments: Bool
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            SheetHeader(title: L10n.string("ui.launch_settings.title"), subtitle: gameName,
                        symbol: "slider.horizontal.3", colors: TileColors.violet, close: { dismiss() },
                        closeIdentifier: "launch-settings-close")
                .padding(.horizontal, 24)
                .padding(.top, 24)
                .padding(.bottom, 18)
            content
        }
        .frame(width: 560, height: 680)
        .tint(ObeliskStyle.accent)
        .task {
            await model.appear()
            arguments = model.settings?.choices.arguments ?? ""
        }
        .onChange(of: model.settings?.choices.arguments) { _, kept in
            if !editsArguments { arguments = kept ?? "" }
        }
        .onChange(of: editsArguments) { _, editing in
            if !editing { model.setArguments(arguments) }
        }
        .onDisappear { model.setArguments(arguments) }
        .accessibilityIdentifier("launch-settings-sheet")
    }

    @ViewBuilder
    private var content: some View {
        if model.isLoading {
            ProgressView(L10n.string("ui.launch_settings.loading"))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if let settings = model.settings {
            form(settings)
        } else if let error = model.error {
            SheetFailure(message: error)
        }
    }

    private func form(_ settings: LaunchSettings) -> some View {
        VStack(spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    ObeliskCard { argumentsRow }
                    ObeliskCard { engineRow(settings) }
                    SheetSection(title: L10n.string("ui.launch_settings.cpu")) {
                        ObeliskCard { translatorRow }
                    }
                    SheetSection(title: L10n.string("ui.launch_settings.graphics")) {
                        ObeliskCard {
                            graphicsRow(settings)
                            if model.graphicsOffer?.options.contains(.rayTracing) == true {
                                ObeliskCardDivider()
                                rayTracingRow(settings)
                            }
                            if let lowest = model.engineOffer?.lowestRenderPercent {
                                ObeliskCardDivider()
                                renderPercentRow(settings, lowest: lowest)
                            }
                            ObeliskCardDivider()
                            performanceHudRow(settings)
                        }
                    }
                }
                .padding(.horizontal, 24)
                .padding(.bottom, 22)
            }
            .scrollBounceBehavior(.basedOnSize)
            Divider()
            footer
        }
    }

    /// A row whose control spans the card, under it.
    private func wideRow<Icon: View, Detail: View, Control: View>(
        title: String,
        @ViewBuilder icon: () -> Icon,
        @ViewBuilder detail: () -> Detail,
        @ViewBuilder control: () -> Control
    ) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            ObeliskRow(title: title, icon: icon, detail: detail) { EmptyView() }
            control()
                .padding(.leading, 64)
                .padding(.trailing, 18)
                .padding(.bottom, 16)
        }
    }

    /// A whole command line.
    private var argumentsRow: some View {
        wideRow(title: L10n.string("ui.launch_settings.arguments")) {
            SymbolTile(name: "terminal.fill", colors: TileColors.graphite)
        } detail: {
            Text(L10n.string("ui.launch_settings.arguments_detail"))
        } control: {
            TextField(L10n.string("ui.launch_settings.arguments"), text: $arguments,
                      prompt: Text(L10n.string("ui.launch_settings.arguments_placeholder")))
                .textFieldStyle(.plain)
                .font(.system(size: 13, design: .monospaced))
                .labelsHidden()
                .focused($editsArguments)
                .onSubmit { model.setArguments(arguments) }
                .padding(.horizontal, 12)
                .frame(height: 36)
                .modifier(SignInWell(isFocused: editsArguments))
                .accessibilityIdentifier("launch-settings-arguments")
        }
    }

    private func engineRow(_ settings: LaunchSettings) -> some View {
        ObeliskRow(title: L10n.string("ui.launch_settings.engine")) {
            SymbolTile(name: "gearshape.2.fill", colors: TileColors.violet)
        } detail: {
            Text(Self.detail(of: settings.engine))
        } accessory: {
            GlassMenuPicker(title: L10n.string("ui.launch_settings.engine"),
                            value: marked(Self.name(of: settings.engine),
                                          isDefault: settings.engine == settings.defaultEngine),
                            selection: Binding(get: { settings.engine },
                                               set: { engine in model.choose { $0.engine = engine } }),
                            identifier: "launch-settings-engine") {
                ForEach(settings.engines, id: \.engine) { offer in
                    Text(marked(Self.name(of: offer.engine), isDefault: offer.engine == settings.defaultEngine))
                        .tag(offer.engine)
                }
            }
        }
    }

    @ViewBuilder
    private var translatorRow: some View {
        if let offer = model.engineOffer {
            ObeliskRow(title: L10n.string("ui.launch_settings.translator")) {
                SymbolTile(name: "cpu", colors: TileColors.blue)
            } accessory: {
                Text(Self.name(of: offer.translator))
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder
    private func graphicsRow(_ settings: LaunchSettings) -> some View {
        if let engine = model.engineOffer {
            ObeliskRow(title: L10n.string("ui.launch_settings.backend")) {
                SymbolTile(name: "square.3.layers.3d", colors: TileColors.green)
            } detail: {
                Text(Self.detail(of: settings.graphics))
            } accessory: {
                GlassMenuPicker(title: L10n.string("ui.launch_settings.backend"),
                                value: marked(Self.name(of: settings.graphics),
                                              isDefault: settings.graphics == engine.defaultGraphics),
                                selection: Binding(get: { settings.graphics },
                                                   set: { graphics in model.choose { $0.graphics = graphics } }),
                                identifier: "launch-settings-graphics") {
                    ForEach(engine.graphics, id: \.graphics) { offer in
                        Text(marked(Self.name(of: offer.graphics), isDefault: offer.graphics == engine.defaultGraphics))
                            .tag(offer.graphics)
                    }
                }
            }
        }
    }

    private func rayTracingRow(_ settings: LaunchSettings) -> some View {
        ObeliskRow(title: L10n.string("ui.launch_settings.ray_tracing")) {
            SymbolTile(name: "sparkles", colors: TileColors.gold)
        } detail: {
            Text(L10n.string("ui.launch_settings.ray_tracing_detail"))
        } accessory: {
            Toggle(L10n.string("ui.launch_settings.ray_tracing"),
                   isOn: Binding(get: { settings.rayTracing },
                                 set: { rayTracing in model.choose { $0.rayTracing = rayTracing } }))
                .toggleStyle(.switch)
                .labelsHidden()
                .pointerCursor()
                .accessibilityIdentifier("launch-settings-ray-tracing")
        }
    }

    private func renderPercentRow(_ settings: LaunchSettings, lowest: UInt32) -> some View {
        wideRow(title: L10n.string("ui.launch_settings.render_percent")) {
            SymbolTile(name: "display", colors: TileColors.blue)
        } detail: {
            Text(L10n.string("ui.launch_settings.render_percent_detail"))
        } control: {
            RenderPercentSlider(percent: settings.renderPercent, lowest: lowest) { percent in
                model.choose { $0.renderPercent = percent }
            }
        }
    }

    private func performanceHudRow(_ settings: LaunchSettings) -> some View {
        ObeliskRow(title: L10n.string("ui.launch_settings.performance_hud")) {
            SymbolTile(name: "gauge.with.needle.fill", colors: TileColors.violet)
        } detail: {
            Text(L10n.string("ui.launch_settings.performance_hud_detail"))
        } accessory: {
            Toggle(L10n.string("ui.launch_settings.performance_hud"),
                   isOn: Binding(get: { settings.performanceHud },
                                 set: { shown in model.choose { $0.performanceHud = shown } }))
                .toggleStyle(.switch)
                .labelsHidden()
                .pointerCursor()
                .accessibilityIdentifier("launch-settings-performance-hud")
        }
    }

    /// When the choices apply, how to forget them, and why the last could not
    /// be kept.
    private var footer: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let error = model.error {
                SheetNotice(kind: .warning, text: error)
            }
            HStack(spacing: 12) {
                Text(L10n.string("ui.launch_settings.next_launch"))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                GlassCapsuleButton(title: L10n.string("ui.launch_settings.reset"), systemImage: "arrow.counterclockwise",
                                   identifier: "launch-settings-reset", action: model.reset)
                    .disabled(!model.isCustomized)
            }
        }
        .padding(.horizontal, 24)
        .padding(.vertical, 16)
    }

    private func marked(_ name: String, isDefault: Bool) -> String {
        isDefault ? L10n.string("ui.launch_settings.default", name) : name
    }

    static func name(of engine: LaunchEngine) -> String {
        switch engine {
        case .rosetta: L10n.string("ui.launch_settings.engine.rosetta")
        case .linux: L10n.string("ui.launch_settings.engine.linux")
        }
    }

    static func detail(of engine: LaunchEngine) -> String {
        switch engine {
        case .rosetta: L10n.string("ui.launch_settings.engine.rosetta_detail")
        case .linux: L10n.string("ui.launch_settings.engine.linux_detail")
        }
    }

    static func name(of translator: LaunchTranslator) -> String {
        switch translator {
        case .rosetta: L10n.string("ui.launch_settings.translator.rosetta")
        case .fex: L10n.string("ui.launch_settings.translator.fex")
        }
    }

    static func name(of graphics: LaunchGraphics) -> String {
        switch graphics {
        case .d3dMetal: L10n.string("ui.launch_settings.graphics.d3dmetal")
        case .dxmt: L10n.string("ui.launch_settings.graphics.dxmt")
        }
    }

    static func detail(of graphics: LaunchGraphics) -> String {
        switch graphics {
        case .d3dMetal: L10n.string("ui.launch_settings.graphics.d3dmetal_detail")
        case .dxmt: L10n.string("ui.launch_settings.graphics.dxmt_detail")
        }
    }
}

/// The render resolution's shares, in whole percent of the screen's pixels.
enum RenderPercent {
    /// All of the pixels.
    static let native: UInt32 = 100
    /// The shares the slider snaps to: three round ones and Native.
    static let stops: [UInt32] = [25, 50, 75, native]

    /// `percent`, or the stop within two points of it.
    static func snapped(_ percent: UInt32) -> UInt32 {
        stops.first { abs($0.distance(to: percent)) <= 2 } ?? percent
    }

    static func name(of percent: UInt32) -> String {
        percent >= native ? L10n.string("ui.launch_settings.render_percent.native")
                          : DownloadFormatting.percent(Double(percent) / 100)
    }
}

/// A slider from the lowest share to Native, with the share beside it. It
/// snaps to the stops and saves when the thumb is let go, or at once for a
/// keyboard or VoiceOver step.
private struct RenderPercentSlider: View {
    /// The share kept for the game.
    let percent: UInt32
    let lowest: UInt32
    let keep: (UInt32) -> Void
    /// The snapped share the thumb was last moved to, until the kept share follows it.
    @State private var draft: UInt32?
    @State private var isDragging = false

    /// The stops, as the slider's own type has them.
    private static let ticks = RenderPercent.stops.map { Double($0) }

    private var shown: UInt32 { draft ?? percent }

    var body: some View {
        let title = L10n.string("ui.launch_settings.render_percent")
        let value = RenderPercent.name(of: shown)
        let position = Binding<Double>(get: { Double(shown) }, set: { move(to: UInt32($0.rounded())) })
        let range = Double(lowest)...Double(RenderPercent.native)
        HStack(spacing: 12) {
            Slider(value: position,
                   in: range,
                   label: { Text(title) },
                   ticks: {
                       SliderTickContentForEach(Self.ticks, id: \.self) { stop in SliderTick(stop) }
                   },
                   onEditingChanged: { editing in
                       if editing { isDragging = true } else { letGo() }
                   })
                .labelsHidden()
                .accessibilityLabel(title)
                .accessibilityValue(value)
                .accessibilityIdentifier("launch-settings-render-percent")
            Text(value)
                .font(.system(size: 13, weight: .medium))
                .monospacedDigit()
                .foregroundStyle(.secondary)
                .frame(width: 56, alignment: .trailing)
                .accessibilityHidden(true)
        }
        // The draft goes once the kept share has caught up with it, or when the
        // share changes by itself (Reset) from where the draft was. An earlier save
        // that comes back while a newer step is pending leaves it, or the thumb
        // would jump back and the next step start from the old share.
        .onChange(of: percent) { old, new in
            if !isDragging, draft == new || draft == old { draft = nil }
        }
    }

    private func move(to position: UInt32) {
        let snapped = RenderPercent.snapped(position)
        let before = shown
        draft = snapped
        if !isDragging {
            keep(snapped)
        } else if snapped != before, RenderPercent.stops.contains(snapped) {
            NSHapticFeedbackManager.defaultPerformer.perform(.alignment, performanceTime: .now)
        }
    }

    private func letGo() {
        if let draft { keep(draft) }
        isDragging = false
    }
}
