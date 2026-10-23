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
    /// A Steam game's Windows Steam, which may start it instead of Obelisk's
    /// Steam client; nil for another storefront's game.
    let windowsSteam: WindowsSteamAccess?
    /// The account Windows Steam signs in to by itself; nil while it would ask.
    private(set) var windowsSteamAccount: String?
    private(set) var isCheckingWindowsSteam = false

    init(windowsSteam: WindowsSteamAccess? = nil,
         load: @escaping () async throws -> LaunchSettings?,
         save: @escaping (LaunchChoices) async throws -> LaunchSettings?) {
        self.windowsSteam = windowsSteam
        self.load = load
        self.save = save
    }

    /// A Steam game, which may start through Valve's Windows Steam.
    var offersSteamClient: Bool { windowsSteam != nil }

    func appear() async {
        isLoading = true
        do {
            settings = try await load()
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
        isLoading = false
        await checkWindowsSteam()
    }

    /// Reads again which account Windows Steam signs in to, when it starts
    /// the game.
    func checkWindowsSteam() async {
        guard let windowsSteam, settings?.steamClient == .windows else { return }
        isCheckingWindowsSteam = true
        windowsSteamAccount = await windowsSteam.account()
        isCheckingWindowsSteam = false
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
        await checkWindowsSteam()
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

/// Valve's Windows Steam of one installed Steam game: the account it signs in
/// to by itself, and how to sign it in to one.
struct WindowsSteamAccess {
    let account: () async -> String?
    let signIn: (SteamCredentials) async throws -> Void
}

/// Signs Valve's Windows Steam in for one game: a Steam sign-in of its own,
/// by QR code, apart from Obelisk's session, whose login Obelisk hands to
/// Windows Steam.
@MainActor @Observable
final class WindowsSteamSignInModel: Identifiable {
    enum Phase: Equatable {
        case starting
        case scanning(url: String, confirming: Bool)
        case signingIn
        case failed(String)
        case done
    }

    private(set) var phase = Phase.starting
    @ObservationIgnored private let signIn: (SteamCredentials) async throws -> Void
    @ObservationIgnored private let begin: () async throws -> any SteamLoginProtocol
    @ObservationIgnored private var task: Task<Void, Never>?

    init(signIn: @escaping (SteamCredentials) async throws -> Void,
         begin: @escaping () async throws -> any SteamLoginProtocol = { try await SteamLogin.beginQr() }) {
        self.signIn = signIn
        self.begin = begin
    }

    /// Shows a new code, then waits for the Steam Mobile App to approve it.
    func start() {
        task?.cancel()
        phase = .starting
        task = Task { [weak self] in await self?.run() }
    }

    func cancel() {
        task?.cancel()
    }

    private func run() async {
        do {
            let login = try await begin()
            while !Task.isCancelled {
                let challenge = await login.challenge()
                guard let url = challenge.qrUrl else { throw CancellationError() }
                phase = .scanning(url: url, confirming: challenge.remoteInteraction)
                if let credentials = try await login.poll() {
                    phase = .signingIn
                    try await signIn(credentials)
                    phase = .done
                    return
                }
                try await Task.sleep(for: .milliseconds(Int(max(challenge.pollIntervalMs, 1000))))
            }
        } catch is CancellationError {
        } catch {
            phase = .failed(error.localizedDescription)
        }
    }
}

/// The sheet that signs Valve's Windows Steam in for one game.
struct WindowsSteamSignInSheet: View {
    let model: WindowsSteamSignInModel
    let gameName: String
    let finished: () -> Void
    /// The player closed it before signing in.
    var closed: () -> Void = {}
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            SheetHeader(title: L10n.string("ui.launch_settings.windows_steam_sign_in_title"), subtitle: gameName,
                        symbol: "shield.lefthalf.filled", colors: TileColors.blue, close: { closed(); dismiss() },
                        closeIdentifier: "windows-steam-sign-in-close")
            content
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .padding(24)
        .frame(width: 440, height: 520)
        .tint(ObeliskStyle.accent)
        .task { model.start() }
        .onDisappear { model.cancel() }
        .onChange(of: model.phase) { _, phase in
            if phase == .done {
                finished()
                dismiss()
            }
        }
        .accessibilityIdentifier("windows-steam-sign-in-sheet")
    }

    @ViewBuilder
    private var content: some View {
        switch model.phase {
        case .starting, .done:
            ProgressView()
        case let .scanning(url, confirming):
            VStack(spacing: 16) {
                SteamQRCode(value: url)
                    .frame(width: 220, height: 220)
                Text(L10n.string(confirming ? "ui.steam.qr_confirm" : "ui.steam.qr_instructions"))
                    .font(.system(size: 13, weight: .medium))
                Text(L10n.string("ui.launch_settings.windows_steam_sign_in_detail"))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
            }
        case .signingIn:
            VStack(spacing: 12) {
                ProgressView()
                Text(L10n.string("ui.launch_settings.windows_steam_signing_in"))
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
            }
        case let .failed(message):
            SheetFailure(message: message) {
                GlassCapsuleButton(title: L10n.string("ui.steam.qr_refresh"), systemImage: "arrow.clockwise",
                                   identifier: "windows-steam-sign-in-retry", action: model.start)
            }
        }
    }
}

/// The sheet behind a Windows game's Launch Settings: the arguments added to
/// its command line, its engine, then the CPU translator that engine has,
/// then the graphics backend and its options, each as the chosen engine
/// allows, and Metal's Performance HUD, every one said in a sentence.
struct LaunchSettingsSheet: View {
    let model: LaunchSettingsModel
    let gameName: String
    @State private var windowsSteamSignIn: WindowsSteamSignInModel?
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
        .sheet(item: $windowsSteamSignIn) { signIn in
            WindowsSteamSignInSheet(model: signIn, gameName: gameName) {
                Task { await model.checkWindowsSteam() }
            }
        }
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
                        if model.offersSteamClient && settings.engine == .rosetta {
                            ObeliskCardDivider()
                            steamClientRow(settings)
                            if settings.steamClient == .windows {
                                ObeliskCardDivider()
                                windowsSteamAccountRow
                            }
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

    /// A whole command line: a field across the card, under its row.
    private var argumentsRow: some View {
        VStack(alignment: .leading, spacing: 0) {
            ObeliskRow(title: L10n.string("ui.launch_settings.arguments")) {
                SymbolTile(name: "terminal.fill", colors: TileColors.graphite)
            } detail: {
                Text(L10n.string("ui.launch_settings.arguments_detail"))
            } accessory: {
                EmptyView()
            }
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
                .padding(.leading, 64)
                .padding(.trailing, 18)
                .padding(.bottom, 16)
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

    /// Valve's Windows Steam starts a Windows Steam game in Wine, where
    /// Rosetta's Wine runs it: VAC-secured servers accept it then.
    private func steamClientRow(_ settings: LaunchSettings) -> some View {
        ObeliskRow(title: L10n.string("ui.launch_settings.steam_client")) {
            SymbolTile(name: "shield.lefthalf.filled", colors: TileColors.blue)
        } detail: {
            Text(Self.detail(of: settings.steamClient))
        } accessory: {
            GlassMenuPicker(title: L10n.string("ui.launch_settings.steam_client"),
                            value: marked(Self.name(of: settings.steamClient), isDefault: settings.steamClient == settings.defaultSteamClient),
                            selection: Binding(get: { settings.steamClient },
                                               set: { client in model.choose { $0.steamClient = client } }),
                            identifier: "launch-settings-steam-client") {
                ForEach([LaunchSteamClient.obelisk, .windows], id: \.self) { client in
                    Text(marked(Self.name(of: client), isDefault: client == settings.defaultSteamClient)).tag(client)
                }
            }
        }
    }

    /// Who Windows Steam signs in to by itself, and the way to sign it in:
    /// its own window cannot show in Wine.
    private var windowsSteamAccountRow: some View {
        ObeliskRow(title: L10n.string("ui.launch_settings.windows_steam_account")) {
            SymbolTile(name: "person.crop.circle.fill", colors: TileColors.blue)
        } detail: {
            if model.isCheckingWindowsSteam {
                Text(L10n.string("ui.launch_settings.windows_steam_checking"))
            } else if let account = model.windowsSteamAccount {
                Text(L10n.string("ui.launch_settings.windows_steam_signed_in", account))
            } else {
                Text(L10n.string("ui.launch_settings.windows_steam_signed_out"))
            }
        } accessory: {
            GlassCapsuleButton(title: L10n.string(model.windowsSteamAccount == nil
                                                    ? "ui.launch_settings.windows_steam_sign_in"
                                                    : "ui.launch_settings.windows_steam_sign_in_again"),
                               identifier: "launch-settings-windows-steam-sign-in") {
                guard let windowsSteam = model.windowsSteam else { return }
                windowsSteamSignIn = WindowsSteamSignInModel(signIn: windowsSteam.signIn)
            }
            .disabled(model.isCheckingWindowsSteam)
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

    static func name(of client: LaunchSteamClient) -> String {
        switch client {
        case .obelisk: L10n.string("ui.launch_settings.steam_client.obelisk")
        case .windows: L10n.string("ui.launch_settings.steam_client.windows")
        }
    }

    static func detail(of client: LaunchSteamClient) -> String {
        switch client {
        case .obelisk: L10n.string("ui.launch_settings.steam_client.obelisk_detail")
        case .windows: L10n.string("ui.launch_settings.steam_client.windows_detail")
        }
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
