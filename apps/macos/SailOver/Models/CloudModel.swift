import Foundation
import Observation

/// What a storefront adds to its Cloud presentation.
protocol CloudStorefront: GameStorefront {
    /// A game's persisted Cloud indicator, as the bridge returns it.
    associatedtype Indicator: Sendable
    /// What an indicator read names for one game.
    associatedtype IndicatorRequest: Sendable
    /// What Cloud maintenance of one game starts from.
    associatedtype Maintenance
    static func game(of indicator: Indicator) -> GameID
    static func cloud(of indicator: Indicator) -> CloudIndicator
    static func game(of maintenance: Maintenance) -> GameID
    static func request(of maintenance: Maintenance) -> IndicatorRequest
    /// A failed answer, as the storefront's models describe errors.
    @MainActor static func describe(_ error: Error) -> String
    /// Where the storefront's Game sessions keep their journals.
    static func sessions(in locations: SailOverLocations) -> String
}

extension SteamGames: CloudStorefront {
    static func game(of indicator: SteamCloudIndicator) -> UInt32 { indicator.appId }
    static func cloud(of indicator: SteamCloudIndicator) -> CloudIndicator { indicator.indicator }
    static func game(of maintenance: SteamCloudMaintenanceRequest) -> UInt32 { maintenance.appId }
    static func request(of maintenance: SteamCloudMaintenanceRequest) -> UInt32 { maintenance.appId }
    @MainActor static func describe(_ error: Error) -> String { SteamModel.message(error) }
    static func sessions(in locations: SailOverLocations) -> String { locations.steamSessions }
}

extension EpicGames: CloudStorefront {
    static func game(of indicator: EpicCloudIndicator) -> String {
        epicLibraryID(namespace: indicator.namespace, appName: indicator.appName)
    }
    static func cloud(of indicator: EpicCloudIndicator) -> CloudIndicator { indicator.indicator }
    static func game(of maintenance: EpicCloudConfiguration) -> String { maintenance.libraryID }
    static func request(of maintenance: EpicCloudConfiguration) -> EpicCloudGame {
        EpicCloudGame(namespace: maintenance.namespace, appName: maintenance.appName)
    }
    @MainActor static func describe(_ error: Error) -> String { EpicModel.message(error) }
    static func sessions(in locations: SailOverLocations) -> String { locations.epicSessions }
}

extension EpicCloudConfiguration {
    var libraryID: String { epicLibraryID(namespace: namespace, appName: appName) }
}

/// The Rust entry points the Steam Cloud presentation uses. Tests replace them.
struct SteamCloudServices: Sendable {
    var indicators: @Sendable (_ root: String, _ accountId: UInt64, _ appIds: [UInt32]) throws -> [SteamCloudIndicator]
    var startMaintenance: @MainActor @Sendable (SteamCredentials, SteamCloudMaintenanceRequest) -> any CloudMaintenanceProtocol

    static let live = SteamCloudServices(
        indicators: { try steamCloudIndicators(root: $0, accountId: $1, appIds: $2) },
        startMaintenance: { startSteamCloudMaintenance(credentials: $0, request: $1) }
    )
}

/// The Rust entry point the Epic Cloud presentation reads. Tests replace it.
struct EpicCloudServices: Sendable {
    var indicators: @Sendable (_ root: String, _ accountId: String, _ games: [EpicCloudGame]) throws -> [EpicCloudIndicator]

    static let live = EpicCloudServices(indicators: { try epicCloudIndicators(root: $0, accountId: $1, games: $2) })
}

typealias SteamCloudModel = CloudModel<SteamGames>
typealias EpicCloudModel = CloudModel<EpicGames>

/// Presents one account's Cloud: indicators, Cloud questions of its Game
/// sessions, and Cloud maintenance. It owns no save comparison or transfer.
@MainActor @Observable
final class CloudModel<Store: CloudStorefront> {
    typealias Indicators = @Sendable (_ root: String, _ account: Store.AccountID,
                                      _ games: [Store.IndicatorRequest]) throws -> [Store.Indicator]
    typealias StartMaintenance = @MainActor (Store.Maintenance) async -> any CloudMaintenanceProtocol

    let locations: SailOverLocations
    /// The Cloud archive.
    var root: String { locations.cloudArchive }
    var sessionDirectory: String { Store.sessions(in: locations) }
    private let sessions: GameSessions<Store>
    @ObservationIgnored private let loadIndicators: Indicators
    private(set) var account: Store.AccountID?
    private var generation = 0
    private(set) var indicators: [Store.GameID: Store.Indicator] = [:]
    private(set) var maintenance: CloudMaintenanceState<Store.GameID>?
    @ObservationIgnored private var attempt: (any CloudMaintenanceProtocol)?
    @ObservationIgnored private var startAttempt: StartMaintenance?
    private(set) var promptBusy = false
    private var promptError: String?
    private var dismissed: Decision?

    private struct Decision: Equatable {
        let session: String
        let decision: UInt64
        init(_ session: GameSessions<Store>.Session) {
            self.session = session.id
            decision = session.cloud.decisionId
        }
    }

    init(locations: SailOverLocations, sessions: GameSessions<Store>, indicators: @escaping Indicators) {
        self.locations = locations
        self.sessions = sessions
        loadIndicators = indicators
    }

    /// `startMaintenance` runs Cloud maintenance with the account's session.
    func connect(account: Store.AccountID, startMaintenance: StartMaintenance? = nil) {
        reset()
        self.account = account
        startAttempt = startMaintenance
    }

    func signedOut() {
        let old = account
        reset()
        if let old { sessions.signedOut(account: old) }
    }

    /// Everything presented belongs to one account; a change invalidates it all.
    private func reset() {
        generation += 1
        // Cloud maintenance runs inside SailOver for one account only.
        attempt?.cancel()
        attempt = nil
        maintenance = nil
        startAttempt = nil
        indicators = [:]
        account = nil
        dismissed = nil
        promptError = nil
        promptBusy = false
    }

    func indicator(for game: Store.GameID) -> CloudIndicator? {
        indicators[game].map { Store.cloud(of: $0) }
    }

    /// Reads the journals off the main thread. Never polls: callers refresh
    /// when a game page opens, the account changes, or a sync ends.
    func refreshIndicators(for requests: [Store.IndicatorRequest]) {
        guard let account else { return }
        let current = generation
        let (root, load) = (root, loadIndicators)
        Task {
            guard let values = try? await Task.detached(operation: { try load(root, account, requests) }).value,
                  current == generation else { return }
            for value in values { indicators[Store.game(of: value)] = value }
        }
    }

    var isMaintaining: Bool { maintenance != nil }

    /// Starts Cloud maintenance of one game. Its Reservation holds from here,
    /// before the Rust attempt and its idle guard exist.
    func startMaintenance(_ request: Store.Maintenance) {
        guard account != nil, let startAttempt, maintenance == nil else { return }
        promptError = nil
        maintenance = CloudMaintenanceState(game: Store.game(of: request))
        let refresh = Store.request(of: request)
        let current = generation
        Task {
            let attempt = await startAttempt(request)
            guard current == generation else {
                attempt.cancel()
                return
            }
            self.attempt = attempt
            await followCloudMaintenance(attempt) { status in
                guard current == generation, self.attempt === attempt else { return false }
                maintenance?.status = status
                if status.finished { finish(refresh) }
                return true
            }
        }
    }

    /// A failure shows only in the game's Cloud indicator; Sync now retries.
    private func finish(_ refresh: Store.IndicatorRequest) {
        attempt = nil
        maintenance = nil
        promptError = nil
        refreshIndicators(for: [refresh])
    }

    private var maintenanceConflict: CloudMaintenanceStatus? { maintenance?.conflict }

    private var active: GameSessions<Store>.Session? {
        guard let account, let active = sessions.active, active.observed, active.account == account else { return nil }
        return active
    }

    /// The live session's Cloud question: asked before the game starts and
    /// after it exits, for this account, once its supervisor showed it.
    var prompt: GameSessions<Store>.Session? {
        guard let active, active.phase == .preparing || active.phase == .finalizing,
              !sessions.revoked(active.account), Decision(active) != dismissed else { return nil }
        return active
    }

    var promptPresented: Bool { prompt?.cloud.phase == .conflict || maintenanceConflict != nil }
    var promptGame: Store.GameID? { prompt?.game ?? (maintenanceConflict == nil ? nil : maintenance?.game) }
    /// Only a launch check shows a banner. A failed maintenance shows in the
    /// game's Cloud indicator instead.
    var failurePresented: Bool { prompt?.cloud.phase == .failed && prompt?.cloud.allowedChoices.isEmpty == false }
    var promptMessage: String {
        promptError ?? prompt?.cloud.error ?? maintenanceConflict?.error ?? L10n.string("ui.cloud.working")
    }
    var promptWorking: Bool {
        promptBusy || prompt?.reconnecting == true || prompt?.cloud.phase == .checking
    }
    var canLaunchAnyway: Bool {
        prompt?.cloud.phase == .failed && prompt?.cloud.allowedChoices.contains(.launchAnyway) == true
    }

    func isSynchronizing(for game: Store.GameID) -> Bool {
        if let maintenance, maintenance.game == game, maintenance.isSyncing { return true }
        guard let active, active.game == game else { return false }
        return [.checking, .syncing].contains(active.cloud.phase) || (promptBusy && prompt?.game == game)
    }

    func canCancelSynchronization(for game: Store.GameID) -> Bool {
        guard let active, active.game == game, active.phase == .preparing,
              !active.reconnecting, !promptBusy else { return false }
        return [.checking, .syncing].contains(active.cloud.phase)
    }

    func respond(_ choice: CloudChoice) {
        if prompt == nil, let conflict = maintenanceConflict, let attempt {
            promptError = attempt.answer(decisionId: conflict.decisionId, choice: choice)
                ? nil : L10n.message(Store.invalidSession)
            return
        }
        guard account != nil, let prompt, !promptBusy else { return }
        let current = generation
        promptBusy = true
        promptError = nil
        Task {
            defer { if current == generation { promptBusy = false } }
            guard current == generation else { return }
            do {
                try await sessions.choose(choice, expected: prompt)
                guard current == generation else { return }
                if choice == .cancel { dismissed = Decision(prompt) }
            } catch {
                guard current == generation else { return }
                promptError = Store.describe(error)
            }
        }
    }
}

extension CloudModel where Store == SteamGames {
    /// Starts Cloud maintenance. `receipt` is nil for an uninstalled game.
    func startMaintenance(appId: UInt32, receipt: SteamInstallReceipt?) {
        startMaintenance(SteamCloudMaintenanceRequest(root: root, home: locations.home, install: receipt?.directory,
                                                      prefix: receipt.flatMap(prefix), appId: appId,
                                                      sessionDirectory: sessionDirectory, helper: locations.helper))
    }

    func archive(_ receipt: SteamInstallReceipt, session: any SteamSessionProtocol) async throws {
        guard account != nil else { throw SteamError.AuthenticationRequired }
        try await session.archiveCloud(config: configuration(receipt))
    }

    func removed(_ receipt: SteamInstallReceipt, accountId: UInt64) async throws {
        try await steamCloudRemoved(config: configuration(receipt), accountId: accountId)
    }

    private func configuration(_ receipt: SteamInstallReceipt) -> SteamCloudConfiguration {
        SteamCloudConfiguration(root: root, home: locations.home, install: receipt.directory, appId: receipt.appId,
                                prefix: prefix(receipt))
    }

    /// A Windows installation keeps its saves in its Wine prefix.
    private func prefix(_ receipt: SteamInstallReceipt) -> String? {
        receipt.platform == "windows" ? winePrefix(locations: locations, installDirectory: receipt.directory) : nil
    }
}

extension CloudModel where Store == EpicGames {
    func refreshIndicators(for games: [EpicGame]) {
        refreshIndicators(for: games.map { EpicCloudGame(namespace: $0.namespace, appName: $0.appName) })
    }

    /// An uninstalled game's saved configuration, with this SailOver's paths.
    func savedConfiguration(for libraryID: String) -> EpicCloudConfiguration? {
        guard var configuration = indicators[libraryID]?.configuration else { return nil }
        configuration.root = root
        configuration.sessionDirectory = sessionDirectory
        configuration.helper = locations.helper
        return configuration
    }

    func configuration(game: EpicGame, receipt: EpicInstallReceipt, language: String) -> EpicCloudConfiguration {
        EpicCloudConfiguration(root: root, home: locations.home, install: receipt.directory,
                               sessionDirectory: sessionDirectory, helper: locations.helper,
                               namespace: game.namespace, catalogItemId: game.catalogItemId,
                               appName: game.appName, platform: receipt.platform, language: language)
    }
}
