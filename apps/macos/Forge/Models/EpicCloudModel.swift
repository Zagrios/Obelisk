import Foundation
import Observation

extension EpicCloudConfiguration {
    var libraryID: String { epicLibraryID(namespace: namespace, appName: appName) }
}

/// The Rust entry points the Epic Cloud presentation reads. Tests replace them.
struct EpicCloudServices: Sendable {
    var indicators: @Sendable (_ root: String, _ accountId: String, _ games: [EpicCloudGame]) throws -> [EpicCloudIndicator]

    static let live = EpicCloudServices(indicators: { try epicCloudIndicators(root: $0, accountId: $1, games: $2) })
}

/// Account-scoped presentation for the native EGS Cloud coordinator.
@MainActor @Observable
final class EpicCloudModel {
    typealias StartMaintenance = @MainActor (EpicCloudConfiguration) async -> any CloudMaintenanceProtocol

    let root: String
    let sessionDirectory: String
    private let sessions: EpicGameSessions
    @ObservationIgnored private let services: EpicCloudServices
    private var accountId: String?
    private var generation = 0
    private(set) var indicators: [String: EpicCloudIndicator] = [:]
    private(set) var maintenance: CloudMaintenanceState<String>?
    @ObservationIgnored private var attempt: (any CloudMaintenanceProtocol)?
    @ObservationIgnored private var lastConfiguration: EpicCloudConfiguration?
    @ObservationIgnored private var startAttempt: StartMaintenance?
    private(set) var promptBusy = false
    private var promptError: String?
    private var dismissed: Decision?

    private struct Decision: Equatable {
        let sessionId: String
        let decisionId: UInt64
        init(_ session: EpicGameSessions.Session) {
            sessionId = session.id
            decisionId = session.decisionId
        }
    }

    init(root: String, sessionDirectory: String, sessions: EpicGameSessions, services: EpicCloudServices = .live) {
        self.root = root
        self.sessionDirectory = sessionDirectory
        self.sessions = sessions
        self.services = services
    }

    /// `startMaintenance` runs Cloud maintenance with the connected session.
    func connect(accountId: String, startMaintenance: StartMaintenance? = nil) {
        reset()
        self.accountId = accountId
        startAttempt = startMaintenance
    }

    func signedOut() {
        let old = accountId
        reset()
        if let old { sessions.signedOut(accountId: old) }
    }

    private func reset() {
        generation += 1
        // Cloud maintenance runs inside Forge for one account only.
        attempt?.cancel()
        attempt = nil
        maintenance = nil
        lastConfiguration = nil
        startAttempt = nil
        indicators = [:]
        accountId = nil
        dismissed = nil
        promptError = nil
        promptBusy = false
    }

    func indicator(for libraryID: String) -> EpicCloudIndicator? { indicators[libraryID] }

    /// Reads the journals off the main thread. Never polls: callers refresh
    /// when a game page opens, the account changes, or a sync ends.
    func refreshIndicators(for games: [EpicGame]) {
        refreshIndicators(games.map { EpicCloudGame(namespace: $0.namespace, appName: $0.appName) })
    }

    private func refreshIndicators(_ requested: [EpicCloudGame]) {
        guard let accountId else { return }
        let current = generation
        let (root, load) = (root, services.indicators)
        Task {
            guard let values = try? await Task.detached(operation: { try load(root, accountId, requested) }).value,
                  current == generation else { return }
            for value in values { indicators[epicLibraryID(namespace: value.namespace, appName: value.appName)] = value }
        }
    }

    var isMaintaining: Bool { maintenance != nil }

    func startMaintenance(_ configuration: EpicCloudConfiguration) {
        guard accountId != nil, let startAttempt, maintenance == nil else { return }
        promptError = nil
        lastConfiguration = configuration
        let game = configuration.libraryID
        // Swift holds the Reservation before the Rust attempt and its idle guard exist.
        maintenance = CloudMaintenanceState(game: game)
        let current = generation
        Task {
            let attempt = await startAttempt(configuration)
            guard current == generation else {
                attempt.cancel()
                return
            }
            self.attempt = attempt
            await followCloudMaintenance(attempt) { status in
                guard current == generation, self.attempt === attempt else { return false }
                maintenance?.status = status
                if status.finished { finish() }
                return true
            }
        }
    }

    /// A failure shows only in the game's Cloud indicator; Sync now retries.
    private func finish() {
        attempt = nil
        maintenance = nil
        promptError = nil
        if let configuration = lastConfiguration {
            refreshIndicators([EpicCloudGame(namespace: configuration.namespace, appName: configuration.appName)])
        }
    }

    private var maintenanceConflict: CloudMaintenanceStatus? { maintenance?.conflict }

    private var active: EpicGameSessions.Session? {
        guard let value = sessions.active, value.observed, value.accountId == accountId else { return nil }
        return value
    }

    var prompt: EpicGameSessions.Session? {
        guard let active, active.phase == .preparing || active.phase == .finalizing,
              Decision(active) != dismissed else { return nil }
        return active
    }

    var promptPresented: Bool { prompt?.cloudPhase == .conflict || maintenanceConflict != nil }
    private var promptFailed: Bool { prompt?.cloudPhase == .failed && prompt?.allowedChoices.isEmpty == false }
    /// Only a launch check shows a banner. A failed maintenance shows in the
    /// game's Cloud indicator instead.
    var failurePresented: Bool { promptFailed }
    var promptLibraryID: String? { prompt?.libraryID ?? (maintenanceConflict == nil ? nil : maintenance?.game) }
    var promptMessage: String {
        promptError ?? prompt?.error ?? maintenanceConflict?.error ?? L10n.string("ui.cloud.working")
    }
    var promptWorking: Bool { promptBusy || prompt?.reconnecting == true }
    var canLaunchAnyway: Bool { prompt?.allowedChoices.contains(.launchAnyway) == true }

    func isSynchronizing(for libraryID: String) -> Bool {
        if let maintenance, maintenance.game == libraryID, maintenance.isSyncing { return true }
        guard let active, active.libraryID == libraryID else { return false }
        return [.checking, .syncing].contains(active.cloudPhase) || (promptBusy && prompt?.libraryID == libraryID)
    }

    func canCancelSynchronization(for libraryID: String) -> Bool {
        guard let active, active.libraryID == libraryID, active.phase == .preparing,
              !active.reconnecting, !promptBusy else { return false }
        return [.checking, .syncing].contains(active.cloudPhase)
    }

    func respond(_ choice: EpicCloudChoice) {
        if prompt == nil, let conflict = maintenanceConflict, let attempt {
            guard let answer = CloudMaintenanceChoice(choice) else { return }
            promptError = attempt.answer(decisionId: conflict.decisionId, choice: answer)
                ? nil : "backend.epic.cloud.session"
            return
        }
        guard let accountId, let prompt, !promptBusy else { return }
        let current = generation
        promptBusy = true
        promptError = nil
        Task {
            defer { if current == generation { promptBusy = false } }
            guard current == generation, self.accountId == accountId else { return }
            do {
                try await sessions.choose(choice, expected: prompt, accountId: accountId)
                guard current == generation else { return }
                if choice == .cancel { dismissed = Decision(prompt) }
            } catch {
                guard current == generation else { return }
                promptError = EpicModel.message(error)
            }
        }
    }

    /// An uninstalled game's saved configuration, with this Forge's paths.
    func savedConfiguration(for libraryID: String) -> EpicCloudConfiguration? {
        guard var configuration = indicators[libraryID]?.configuration else { return nil }
        configuration.root = root
        configuration.sessionDirectory = sessionDirectory
        configuration.helper = SteamGameLauncher.helper
        return configuration
    }

    func configuration(game: EpicGame, receipt: EpicInstallReceipt, language: String) -> EpicCloudConfiguration {
        EpicCloudConfiguration(root: root, home: NSHomeDirectory(), install: receipt.directory,
                               sessionDirectory: sessionDirectory, helper: SteamGameLauncher.helper,
                               namespace: game.namespace, catalogItemId: game.catalogItemId,
                               appName: game.appName, platform: receipt.platform, language: language)
    }
}
