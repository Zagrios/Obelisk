import SwiftUI

/// The launch settings of one installed Windows game, as its sheet shows
/// them: read from Rust, and kept there at each change, one change at a time
/// and each over the ones before it.
@MainActor @Observable
final class LaunchSettingsModel: Identifiable {
    private(set) var settings: LaunchSettings?
    private(set) var isLoading = true
    private(set) var error: String?
    /// The choices as the player last made them, which Rust may not have kept yet.
    private var chosen = LaunchChoices()
    /// The choices still to keep. Only the latest wait: a change made while
    /// Rust keeps an earlier one replaces those waiting.
    private var waiting: LaunchChoices?
    private var isKeeping = false
    /// The arguments as the player types them, until the field is done.
    private var typedArguments: String?
    /// Where the player holds the render resolution's thumb, until they let go.
    private var heldRenderPercent: UInt32?
    private(set) var holdsRenderPercent = false
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
            if !isKeeping, let settings { chosen = settings.choices }
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
        isLoading = false
    }

    /// Changes one of the player's choices and keeps them: Rust drops those
    /// the game already has by default. A change made before Rust kept the
    /// last one adds to it.
    func choose(_ change: (inout LaunchChoices) -> Void) {
        guard settings != nil else { return }
        let before = chosen
        change(&chosen)
        if chosen != before { keep() }
    }

    /// Forgets every choice: the game runs as Obelisk knows it best.
    func reset() {
        chosen = LaunchChoices()
        keep()
    }

    /// The game's launch arguments as the field shows them: what the player
    /// types, else the ones last chosen.
    var arguments: String {
        get { typedArguments ?? chosen.arguments ?? "" }
        set { typedArguments = newValue }
    }

    /// Keeps what the player typed as the game's launch arguments, when it
    /// changed, once the field is done. Spaces alone are no arguments.
    func keepArguments() {
        guard let typed = typedArguments else { return }
        typedArguments = nil
        let arguments = typed.trimmingCharacters(in: .whitespacesAndNewlines)
        choose { $0.arguments = arguments.isEmpty ? nil : arguments }
    }

    /// The render resolution as the slider shows it: where the thumb is held,
    /// else the share last chosen, before Rust kept it too.
    var renderPercent: UInt32? {
        if let heldRenderPercent { return heldRenderPercent }
        if isKeeping, let percent = chosen.renderPercent { return percent }
        return settings?.renderPercent
    }

    /// The player took the render resolution's thumb.
    func holdRenderPercent() {
        holdsRenderPercent = true
    }

    /// Moves the render resolution to `percent`, snapped to the stops. A held
    /// thumb only shows it; a keyboard or VoiceOver step keeps it at once.
    func moveRenderPercent(to percent: UInt32) {
        let snapped = RenderPercent.snapped(percent)
        if holdsRenderPercent {
            heldRenderPercent = snapped
        } else {
            choose { $0.renderPercent = snapped }
        }
    }

    /// The player let the thumb go: the share it was held at is kept.
    func letGoOfRenderPercent() {
        holdsRenderPercent = false
        guard let held = heldRenderPercent else { return }
        heldRenderPercent = nil
        choose { $0.renderPercent = held }
    }

    /// Keeps the choices in Rust, one save at a time, so that an earlier save
    /// never comes back over a later one.
    private func keep() {
        waiting = chosen
        guard !isKeeping else { return }
        isKeeping = true
        Task {
            while let choices = waiting {
                waiting = nil
                do {
                    if let saved = try await save(choices) { settings = saved }
                    error = nil
                } catch {
                    self.error = error.localizedDescription
                    // The field keeps the arguments Rust could not keep, to try again.
                    if waiting == nil, typedArguments == nil, choices.arguments != settings?.choices.arguments {
                        typedArguments = choices.arguments ?? ""
                    }
                }
                // Nothing newer waits: the choices are the ones Rust has.
                if waiting == nil, let settings { chosen = settings.choices }
            }
            isKeeping = false
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
    @Bindable var model: LaunchSettingsModel
    let gameName: String
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
        .task { await model.appear() }
        .onChange(of: editsArguments) { _, editing in
            if !editing { model.keepArguments() }
        }
        .onDisappear { model.keepArguments() }
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
                    ObeliskCard {
                        engineRow(settings)
                        if model.engineOffer?.gameMode == true {
                            ObeliskCardDivider()
                            gameModeRow(settings)
                        }
                    }
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
            TextField(L10n.string("ui.launch_settings.arguments"), text: $model.arguments,
                      prompt: Text(L10n.string("ui.launch_settings.arguments_placeholder")))
                .textFieldStyle(.plain)
                .font(.system(size: 13, design: .monospaced))
                .labelsHidden()
                .focused($editsArguments)
                .onSubmit { model.keepArguments() }
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

    private func gameModeRow(_ settings: LaunchSettings) -> some View {
        ObeliskRow(title: L10n.string("ui.launch_settings.game_mode")) {
            SymbolTile(name: "gamecontroller.fill", colors: TileColors.green)
        } detail: {
            Text(L10n.string("ui.launch_settings.game_mode_detail"))
        } accessory: {
            Toggle(L10n.string("ui.launch_settings.game_mode"),
                   isOn: Binding(get: { settings.gameMode },
                                 set: { allowed in model.choose { $0.gameMode = allowed } }))
                .toggleStyle(.switch)
                .labelsHidden()
                .pointerCursor()
                .accessibilityIdentifier("launch-settings-game-mode")
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
            RenderPercentSlider(model: model, percent: model.renderPercent ?? settings.renderPercent, lowest: lowest)
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

/// A slider from the lowest share to Native, with the share beside it. Its
/// model snaps it to the stops and keeps the share when the thumb is let go,
/// or at once for a keyboard or VoiceOver step.
private struct RenderPercentSlider: View {
    let model: LaunchSettingsModel
    /// The share to show.
    let percent: UInt32
    let lowest: UInt32

    /// The stops, as the slider's own type has them.
    private static let ticks = RenderPercent.stops.map { Double($0) }

    var body: some View {
        let title = L10n.string("ui.launch_settings.render_percent")
        let value = RenderPercent.name(of: percent)
        let position = Binding<Double>(get: { Double(percent) }, set: { move(to: UInt32($0.rounded())) })
        let range = Double(lowest)...Double(RenderPercent.native)
        HStack(spacing: 12) {
            Slider(value: position,
                   in: range,
                   label: { Text(title) },
                   ticks: {
                       SliderTickContentForEach(Self.ticks, id: \.self) { stop in SliderTick(stop) }
                   },
                   onEditingChanged: { editing in
                       if editing { model.holdRenderPercent() } else { model.letGoOfRenderPercent() }
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
    }

    /// Moves the thumb, with a tap under the finger as a held thumb reaches a stop.
    private func move(to position: UInt32) {
        let before = model.renderPercent
        model.moveRenderPercent(to: position)
        if model.holdsRenderPercent, let now = model.renderPercent, now != before,
           RenderPercent.stops.contains(now) {
            NSHapticFeedbackManager.defaultPerformer.perform(.alignment, performanceTime: .now)
        }
    }
}
