import Foundation
import Observation

/// The Rust entry points the Steam Cloud presentation uses. Tests replace them.
struct SteamCloudServices: Sendable {
    var indicators: @Sendable (_ root: String, _ accountId: UInt64, _ appIds: [UInt32]) throws -> [SteamCloudIndicator]
    var startMaintenance: @MainActor @Sendable (SteamCredentials, SteamCloudMaintenanceRequest) -> any CloudMaintenanceProtocol

    static let live = SteamCloudServices(
        indicators: { try steamCloudIndicators(root: $0, accountId: $1, appIds: $2) },
        startMaintenance: { startSteamCloudMaintenance(credentials: $0, request: $1) }
    )
}

/// Presents the Rust coordinator. It owns no save comparison or transfer loop.
@MainActor @Observable
final class SteamCloudModel {
    let root: String
    private let sessions: SteamGameSessions
    @ObservationIgnored private let services: SteamCloudServices
    private var account: SteamCredentials?
    private var generation = 0
    private(set) var indicators: [UInt32: CloudIndicator] = [:]
    private(set) var maintenance: CloudMaintenanceState<UInt32>?
    @ObservationIgnored private var attempt: (any CloudMaintenanceProtocol)?
    private(set) var promptBusy = false
    private var promptError: String?
    private struct Decision: Equatable {
        let sessionId: UUID
        let decisionId: UInt64

        init(_ session: SteamGameSessions.Session) {
            sessionId = session.id
            decisionId = session.cloud.decisionId
        }
    }
    private var dismissedDecision: Decision?

    init(root: String, sessions: SteamGameSessions, services: SteamCloudServices = .live) {
        self.root = root
        self.sessions = sessions
        self.services = services
    }

    func connect(_ credentials: SteamCredentials) {
        reset(to: credentials)
    }

    func signedOut() {
        let old = account?.steamId
        reset(to: nil)
        if let old {
            Task { try? await sessions.revokeCloud(accountId: old) }
        }
    }

    /// Everything presented belongs to one account; a change invalidates it all.
    private func reset(to credentials: SteamCredentials?) {
        generation += 1
        // Cloud maintenance runs inside Forge for one account only.
        attempt?.cancel()
        attempt = nil
        maintenance = nil
        indicators = [:]
        account = credentials
        promptError = nil
        promptBusy = false
        dismissedDecision = nil
    }

    func indicator(for appId: UInt32) -> CloudIndicator? { indicators[appId] }

    /// Reads the journals off the main thread. Never polls: callers refresh
    /// when a game page opens, the account changes, or a sync ends.
    func refreshIndicators(for appIds: [UInt32]) {
        guard let account else { return }
        let current = generation
        let (root, load) = (root, services.indicators)
        Task {
            guard let values = try? await Task.detached(operation: { try load(root, account.steamId, appIds) }).value,
                  current == generation else { return }
            for value in values { indicators[value.appId] = value.indicator }
        }
    }

    var isMaintaining: Bool { maintenance != nil }

    /// Starts Cloud maintenance. `install` is nil for an uninstalled game.
    func startMaintenance(appId: UInt32, install: String?) {
        let request = SteamCloudMaintenanceRequest(root: root, home: NSHomeDirectory(), install: install, appId: appId,
                                                   sessionDirectory: SteamGameLauncher.sessionDirectory,
                                                   helper: SteamGameLauncher.helper)
        guard let account, maintenance == nil else { return }
        promptError = nil
        let attempt = services.startMaintenance(account, request)
        self.attempt = attempt
        maintenance = CloudMaintenanceState(game: request.appId)
        let current = generation
        Task {
            await followCloudMaintenance(attempt) { status in
                guard current == generation, self.attempt === attempt else { return false }
                maintenance?.status = status
                if status.finished { finish(appId: request.appId) }
                return true
            }
        }
    }

    /// A failure shows only in the game's Cloud indicator; Sync now retries.
    private func finish(appId: UInt32) {
        attempt = nil
        maintenance = nil
        promptError = nil
        refreshIndicators(for: [appId])
    }

    private var maintenanceConflict: CloudMaintenanceStatus? { maintenance?.conflict }

    var prompt: SteamGameSessions.Session? {
        guard let account, let prompt = sessions.cloudPrompt,
              prompt.accountId == account.steamId, Decision(prompt) != dismissedDecision else { return nil }
        return prompt
    }
    var promptPresented: Bool { prompt?.cloud.phase == .conflict || maintenanceConflict != nil }
    var promptAppId: UInt32? { prompt?.appId ?? (maintenanceConflict == nil ? nil : maintenance?.game) }
    /// Only a launch check shows a banner. A failed maintenance shows in the
    /// game's Cloud indicator instead.
    var failurePresented: Bool { prompt?.cloud.phase == .failed }
    var promptMessage: String {
        promptError ?? prompt?.cloud.error ?? maintenanceConflict?.error ?? "ui.cloud.working"
    }
    var promptWorking: Bool { promptBusy || prompt?.cloud.phase == .checking }

    var canLaunchAnyway: Bool {
        guard let prompt, prompt.cloud.phase == .failed else { return false }
        return prompt.cloud.allowedChoices.contains(.launchAnyway)
    }

    /// The live session's Cloud status for this app, if it is this account's.
    private func activeStatus(for appId: UInt32) -> SteamGameSessions.CloudStatus? {
        guard let active = sessions.active, active.appId == appId, active.accountId == account?.steamId else { return nil }
        return active.cloud
    }

    func isSynchronizing(for appId: UInt32) -> Bool {
        if let phase = activeStatus(for: appId)?.phase, [.checking, .syncing].contains(phase) { return true }
        if let maintenance, maintenance.game == appId, maintenance.isSyncing { return true }
        return promptBusy && prompt?.appId == appId
    }

    func canCancelSynchronization(for appId: UInt32) -> Bool {
        prompt?.appId == appId && prompt?.phase == .preparing && prompt?.cloud.phase == .checking && !promptBusy
    }

    func respond(_ choice: SteamCloudChoice) {
        if prompt == nil, let conflict = maintenanceConflict, let attempt {
            guard let answer = CloudMaintenanceChoice(choice) else { return }
            promptError = attempt.answer(decisionId: conflict.decisionId, choice: answer)
                ? nil : "backend.steam.runtime.invalidSession"
            return
        }
        guard let account, let prompt, !promptBusy else { return }
        promptError = nil
        let current = generation
        promptBusy = true
        Task {
            defer { if current == generation { promptBusy = false } }
            guard current == generation, self.account?.steamId == account.steamId,
                  !Task.isCancelled, let active = sessions.cloudPrompt,
                  active.id == prompt.id, active.cloud.decisionId == prompt.cloud.decisionId else { return }
            do {
                try await sessions.chooseCloud(choice, accountId: account.steamId)
                guard current == generation else { return }
                if choice == .cancel { dismissedDecision = Decision(prompt) }
            } catch {
                guard current == generation else { return }
                promptError = SteamModel.message(error)
            }
        }
    }

    func archive(_ receipt: SteamInstallReceipt, session: any SteamSessionProtocol) async throws {
        guard account != nil else { throw SteamError.AuthenticationRequired }
        try await session.archiveCloud(config: configuration(receipt))
    }
    func removed(_ receipt: SteamInstallReceipt, accountId: UInt64) async throws {
        try await steamCloudRemoved(config: configuration(receipt), accountId: accountId)
    }
    private func configuration(_ receipt: SteamInstallReceipt) -> SteamCloudConfiguration {
        SteamCloudConfiguration(root: root, home: NSHomeDirectory(), install: receipt.directory, appId: receipt.appId)
    }
}
