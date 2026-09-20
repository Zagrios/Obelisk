import Foundation
import Observation

/// Presents the Rust coordinator. It owns no save comparison or transfer loop.
@MainActor @Observable
final class SteamCloudModel {
    let root: String
    private let sessions: SteamGameSessions
    private var account: SteamCredentials?
    private var generation = 0
    @ObservationIgnored private var maintenance: (any SteamCloudMaintenanceProtocol)?
    @ObservationIgnored private let makeMaintenance: (SteamCredentials) -> any SteamCloudMaintenanceProtocol
    @ObservationIgnored private var work: Task<Void, Never>?
    private(set) var isBusy = false
    private(set) var pending: [SteamCloudPending] = []
    private(set) var deferredPrompt: SteamCloudPending?
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
    private var presentedFinalizationToken: String?

    init(root: String, sessions: SteamGameSessions,
         makeMaintenance: @escaping (SteamCredentials) -> any SteamCloudMaintenanceProtocol = { SteamCloudMaintenance(credentials: $0) }) {
        self.root = root
        self.sessions = sessions
        self.makeMaintenance = makeMaintenance
    }

    func connect(_ credentials: SteamCredentials) {
        reset(to: credentials)
        resume()
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
        maintenance?.cancel()
        work?.cancel()
        account = credentials
        maintenance = credentials.map(makeMaintenance)
        pending = []
        promptError = nil
        deferredPrompt = nil
        promptBusy = false
        dismissedDecision = nil
        presentedFinalizationToken = nil
        isBusy = false
    }

    func resume() {
        guard let maintenance, let account, !sessions.isBusy, !isBusy else { return }
        presentDetectedConflict()
        // Unresolved choices belong to their game. Other games may still drain
        // their durable work without reopening a dismissed conflict.
        let excludedAppIds = pending.filter { $0.phase == .conflict }.map(\.appId)
        guard FileManager.default.fileExists(atPath: root + "/" + String(account.steamId)) else { return }
        let current = generation
        isBusy = true
        work = Task {
            defer { if current == generation { isBusy = false; work = nil } }
            do {
                let sessionDirectory = URL(fileURLWithPath: root).deletingLastPathComponent().appending(path: "SteamSessions").path
                let results = try await maintenance.resume(root: root, sessionDirectory: sessionDirectory,
                                                           helper: SteamGameLauncher.helper, excludedAppIds: excludedAppIds)
                guard current == generation, !Task.isCancelled else { return }
                let retained = pending.filter { excludedAppIds.contains($0.appId) }
                let updated = results.filter { $0.phase != .synced && $0.phase != .disabled }
                pending = retained + updated
                if !excludedAppIds.contains(where: { $0 == deferredPrompt?.appId }) {
                    deferredPrompt = updated.first { $0.phase == .conflict }
                }
            } catch {
                guard current == generation, !Task.isCancelled else { return }
                // Durable pending data remains the source of truth. A failed
                // wake is retried by a new network event or an explicit action.
            }
        }
    }

    var prompt: SteamGameSessions.Session? {
        guard let account, let prompt = sessions.cloudPrompt,
              prompt.accountId == account.steamId, Decision(prompt) != dismissedDecision else { return nil }
        return prompt
    }
    var promptPresented: Bool {
        prompt?.cloud.phase == .conflict || deferredPrompt?.phase == .conflict || detectedConflict != nil
    }
    var promptAppId: UInt32? { prompt?.appId ?? deferredPrompt?.appId ?? detectedConflict?.appId }
    var failurePresented: Bool {
        prompt?.cloud.phase == .failed || (deferredPrompt != nil && !promptPresented)
    }
    var promptMessage: String {
        promptError ?? prompt?.cloud.error ?? deferredPrompt?.error ?? detectedConflict?.error ?? "ui.steam.cloud.working"
    }
    var promptWorking: Bool {
        promptBusy || prompt?.cloud.phase == .checking || isBusy
            || ((detectedConflict != nil || deferredPrompt != nil) && sessions.isBusy)
    }

    /// The last status a supervisor published, only while it belongs to the
    /// signed-in account.
    private var lastCloud: SteamGameSessions.LastCloud? {
        guard let account, let last = sessions.lastCloud, last.accountId == account.steamId else { return nil }
        return last
    }
    private func lastStatus(for appId: UInt32) -> SteamGameSessions.CloudStatus? {
        guard let last = lastCloud, last.appId == appId else { return nil }
        return last.status
    }

    private var detectedConflict: SteamCloudPending? {
        guard let last = lastCloud, last.status.phase == .conflict,
              let token = last.status.decisionToken,
              !token.isEmpty, token != presentedFinalizationToken else { return nil }
        return SteamCloudPending(appId: last.appId, phase: .conflict, error: last.status.error, decisionToken: token)
    }

    @discardableResult
    private func presentDetectedConflict() -> Bool {
        guard let conflict = detectedConflict else { return false }
        presentedFinalizationToken = conflict.decisionToken
        pending.removeAll { $0.appId == conflict.appId }
        pending.append(conflict)
        deferredPrompt = conflict
        return true
    }
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
        if promptBusy, prompt?.appId == appId { return true }
        return isBusy && (deferredPrompt?.appId == appId || pending.contains { $0.appId == appId })
    }

    func canCancelSynchronization(for appId: UInt32) -> Bool {
        prompt?.appId == appId && prompt?.cloud.phase == .checking && !promptBusy
    }

    func respond(_ choice: SteamCloudChoice) {
        guard let account, !promptBusy else { return }
        promptError = nil
        if let prompt {
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
            return
        }
        // A finalizing supervisor may have published its conflict just before
        // releasing the archive. Show the sheet now; resolve after that handoff.
        if detectedConflict != nil {
            if choice == .cancel {
                presentDetectedConflict()
                deferredPrompt = nil
                resume()
                return
            }
            guard !sessions.isBusy else { return }
            presentDetectedConflict()
        }
        guard let deferred = deferredPrompt, !isBusy else { return }
        if choice == .cancel || choice == .launchAnyway {
            deferredPrompt = nil
            if deferred.phase == .conflict { resume() }
            return
        }
        guard !sessions.isBusy else { return }
        if choice == .retry {
            pending.removeAll { $0.appId == deferred.appId }
            deferredPrompt = nil
            resume()
            return
        }
        guard let maintenance else { return }
        let current = generation
        isBusy = true
        work = Task {
            defer { if current == generation { isBusy = false; work = nil } }
            guard current == generation, self.account?.steamId == account.steamId, !Task.isCancelled else { return }
            do {
                let result = try await maintenance.resolve(root: root, appId: deferred.appId,
                                                           choice: choice, decisionToken: deferred.decisionToken)
                guard current == generation, !Task.isCancelled else { return }
                pending.removeAll { $0.appId == result.appId }
                let deferred = result.phase != .synced && result.phase != .disabled
                if deferred { pending.append(result) }
                deferredPrompt = deferred ? result : nil
            } catch {
                guard current == generation, !Task.isCancelled else { return }
                deferredPrompt = SteamCloudPending(appId: deferred.appId, phase: .pending, error: SteamModel.message(error), decisionToken: deferred.decisionToken)
            }
        }
    }

    func hasPending(for appId: UInt32) -> Bool {
        pending.contains { $0.appId == appId } || lastStatus(for: appId)?.phase == .pending
    }
    func retryPending(appId: UInt32) {
        if let entry = pending.first(where: { $0.appId == appId && $0.phase == .conflict }) { deferredPrompt = entry }
        else { resume() }
    }
    func message(for appId: UInt32) -> String? {
        let phase = activeStatus(for: appId)?.phase
            ?? pending.first { $0.appId == appId }?.phase
            ?? lastStatus(for: appId)?.phase
        switch phase {
        case .checking, .syncing: return L10n.string("ui.steam.cloud.working")
        case .pending, .failed: return L10n.string("ui.steam.cloud.pending")
        case .conflict: return nil
        case .synced: return L10n.string("ui.steam.cloud.synced")
        case .disabled: return L10n.string("ui.steam.cloud.disabled")
        default: return nil
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
