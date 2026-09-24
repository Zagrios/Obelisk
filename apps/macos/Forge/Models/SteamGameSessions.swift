import Foundation
import Observation

extension SteamGameSnapshot {
    var finished: Bool { phase == .finished || phase == .failed }
}

/// Owns Swift's launch reservation and observation, independently of the account.
/// Only the native supervisor owns the game and Steam host processes.
@MainActor @Observable
final class SteamGameSessions {
    static let shared = SteamGameSessions(recoverExisting: true)

    struct Launch {
        let plan: SteamLaunchPlan
        let credentials: SteamCredentials
    }

    struct Session: Identifiable {
        let id: UUID
        let appId: UInt32
        var accountId: UInt64 = 0
        var phase = SteamGamePhase.preparing
        var connection = SteamSupervisorConnection.connected
        var host = SteamHostState.online
        var cloud = CloudStatus()
        var rosetta: RosettaRequest?
        var running: Bool { phase == .running }
    }

    /// The supervisor's published Cloud decision state, as one value.
    struct CloudStatus: Equatable {
        var phase = SteamCloudPhase.idle
        var decisionId: UInt64 = 0
        var error: String?
        var allowedChoices: [SteamCloudChoice] = []

        init() {}

        init(_ snapshot: SteamGameSnapshot) {
            phase = snapshot.cloudPhase
            decisionId = snapshot.cloudDecisionId
            error = snapshot.cloudError
            allowedChoices = snapshot.cloudAllowedChoices
        }
    }

    /// The last Cloud status any supervisor published, retained after its
    /// session ends so a terminal indicator can be shown for that account.
    struct LastCloud: Equatable {
        let appId: UInt32
        let accountId: UInt64
        let status: CloudStatus
    }

    private(set) var active: Session?
    private(set) var lastCloud: LastCloud?
    private(set) var isRecovering = false
    private(set) var recoveryError: String?
    var isBusy: Bool { isRecovering || active != nil || cloudProbeReserved }
    private var cloudProbeReserved = false
    private var revokedAccounts: Set<UInt64> = []
    private var cancelledRosetta: RosettaPrompt?

    /// A Cloud question is asked before the game starts and after it exits.
    var cloudPrompt: Session? {
        guard let active, active.phase == .preparing || active.phase == .finalizing,
              !revokedAccounts.contains(active.accountId),
              [.checking, .failed, .conflict].contains(active.cloud.phase) else { return nil }
        return active
    }

    #if DEBUG
    func reserveCloudProbe() -> Bool {
        guard !isBusy, recoveryError == nil else { return false }
        cloudProbeReserved = true
        return true
    }

    func finishCloudProbe() { cloudProbeReserved = false }
    #endif

    @ObservationIgnored private var operation: Task<Void, Never>?
    @ObservationIgnored private var activeClient: (any SteamGameSessionProtocol)?
    @ObservationIgnored private var appliedRevocations: Set<UInt64> = []
    @ObservationIgnored private let startSession: @MainActor (SteamLaunchPlan, SteamCredentials) async throws -> any SteamGameSessionProtocol
    @ObservationIgnored private let recoverSession: @MainActor () async throws -> (any SteamGameSessionProtocol)?

    init(
        recoverExisting: Bool = false,
        startSession: @escaping @MainActor (SteamLaunchPlan, SteamCredentials) async throws -> any SteamGameSessionProtocol = {
            try await SteamGameLauncher.start($0, credentials: $1)
        },
        recoverSession: @escaping @MainActor () async throws -> (any SteamGameSessionProtocol)? = {
            try await recoverSteamGameSession(sessionDirectory: SteamGameLauncher.sessionDirectory, helper: SteamGameLauncher.helper)
        }
    ) {
        self.startSession = startSession
        self.recoverSession = recoverSession
        if recoverExisting {
            isRecovering = true
            operation = Task {
                do {
                    _ = try await recoverAndObserve(onRunning: {})
                } catch {
                    if !Task.isCancelled && !(error is CancellationError) { recoveryError = SteamModel.message(error) }
                }
                finish()
            }
        }
    }

    /// Admission is synchronous, including the preparation supplied by the model.
    /// Cancelling the returned task cancels preparation, never releases a live game.
    @discardableResult
    func launch(
        appId: UInt32,
        prepare: @escaping @MainActor () async throws -> Launch,
        onRunning: @escaping @MainActor () -> Void = {},
        onCompletion: @escaping @MainActor (Error?) -> Void = { _ in }
    ) -> Task<Void, Never>? {
        guard !isBusy else { return nil }
        // A newly admitted launch belongs to the currently authenticated request.
        // Revocations from a finished session must not disable a later sign-in.
        revokedAccounts.removeAll()
        appliedRevocations.removeAll()
        active = Session(id: UUID(), appId: appId)
        recoveryError = nil
        let task = Task {
            var failure: Error?
            do {
                try Task.checkCancellation()
                let request = try await prepare()
                try Task.checkCancellation()
                try await run(request, onRunning: onRunning)
            } catch {
                failure = error
            }
            finish()
            onCompletion(failure)
        }
        operation = task
        return task
    }

    func protects(_ appId: UInt32?) -> Bool {
        if isRecovering || cloudProbeReserved { return true }
        guard let appId else { return false }
        return active?.appId == appId
    }

    func chooseCloud(_ choice: SteamCloudChoice, accountId: UInt64) async throws {
        guard let active, let client = activeClient, accountId != 0,
              active.accountId == accountId, !revokedAccounts.contains(accountId),
              active.phase == .preparing || active.phase == .finalizing else {
            throw SteamError.Failure(reason: "backend.steam.runtime.invalidSession")
        }
        if choice == .cancel && active.cloud.phase == .checking && active.phase == .preparing {
            client.cancelPreparation()
            return
        }
        guard [.failed, .conflict].contains(active.cloud.phase) else {
            throw SteamError.Failure(reason: "backend.steam.runtime.invalidSession")
        }
        try await client.chooseCloud(accountId: accountId, decisionId: active.cloud.decisionId, choice: choice)
    }

    typealias RosettaPrompt = RosettaLaunchPrompt<UUID, UInt64>

    func rosettaPrompt(accountId: UInt64) -> RosettaPrompt? {
        guard let active, active.phase == .preparing, active.accountId == accountId,
              !revokedAccounts.contains(accountId), let request = active.rosetta else { return nil }
        let prompt = RosettaPrompt(id: active.id, accountId: accountId, decisionId: request.decisionId)
        return prompt == cancelledRosetta ? nil : prompt
    }

    func resumeAfterRosetta(_ prompt: RosettaPrompt) async throws {
        guard rosettaPrompt(accountId: prompt.accountId) == prompt, let client = activeClient else {
            throw SteamError.Failure(reason: "backend.steam.runtime.invalidSession")
        }
        try Task.checkCancellation()
        try await client.resumeAfterRosetta(accountId: prompt.accountId, decisionId: prompt.decisionId)
    }

    func cancelRosetta(_ prompt: RosettaPrompt) {
        guard rosettaPrompt(accountId: prompt.accountId) == prompt else { return }
        cancelledRosetta = prompt
        // Observation survives cancellation until the supervisor confirms its
        // terminal state. Mark the operation cancelled to suppress a user error.
        operation?.cancel()
    }

    func revokeCloud(accountId: UInt64) async throws {
        guard accountId != 0 else {
            throw SteamError.Failure(reason: "backend.steam.runtime.invalidSession")
        }
        // Record this before awaiting a client or reply. A private handoff may
        // still be in flight, or startup recovery may not know its account yet.
        revokedAccounts.insert(accountId)
        guard active?.accountId == accountId, let client = activeClient else { return }
        try await applyRevocation(accountId: accountId, client: client)
    }

    private func applyRevocation(accountId: UInt64, client: any SteamGameSessionProtocol) async throws {
        guard revokedAccounts.contains(accountId), !appliedRevocations.contains(accountId) else { return }
        // Sign-out cancels the launch observer, but must still reach its detached
        // supervisor. Successful revocation remains tied to this exact client.
        try await Task.detached { try await client.revokeCloud(accountId: accountId) }.value
        if activeClient === client { appliedRevocations.insert(accountId) }
    }

    /// A recovered snapshot may not carry its account yet; the reservation does.
    private func accountId(of snapshot: SteamGameSnapshot) -> UInt64 {
        snapshot.accountId == 0 ? active?.accountId ?? 0 : snapshot.accountId
    }

    private func rememberCloud(_ snapshot: SteamGameSnapshot) {
        let last = LastCloud(appId: snapshot.appId, accountId: accountId(of: snapshot),
                             status: CloudStatus(snapshot))
        if lastCloud != last { lastCloud = last }
    }

    private func run(_ request: Launch, onRunning: @escaping @MainActor () -> Void) async throws {
        active?.accountId = request.credentials.steamId
        let client: any SteamGameSessionProtocol
        do {
            client = try await startSession(request.plan, request.credentials)
        } catch let failure as SteamGameHandoffFailure {
            // Do not clear the reservation between handoff and recovery. Unknown
            // recovery state protects all installations until the game is known.
            isRecovering = true
            active?.connection = .reconnecting
            if try await recoverAndObserve(onRunning: onRunning) { return }
            throw failure.cause
        }
        try await observe(client, onRunning: onRunning)
    }

    /// Native recovery validates process identities and paces unresolved attempts.
    /// An empty result is its confirmation that no live session remains.
    private func recoverAndObserve(onRunning: @escaping @MainActor () -> Void) async throws -> Bool {
        while true {
            let client: any SteamGameSessionProtocol
            let snapshot: SteamGameSnapshot
            do {
                // Recovery survives cancellation of the original launch task.
                let recovery = Task { try await self.recoverSession() }
                guard let recovered = try await recovery.value else {
                    recoveryError = nil
                    return false
                }
                client = recovered
                snapshot = try await Task.detached { try await client.snapshot(after: nil) }.value
                if snapshot.finished {
                    rememberCloud(snapshot)
                    continue
                }
                guard UUID(uuidString: snapshot.id) != nil else {
                    throw SteamError.Failure(reason: "backend.steam.runtime.invalidSession")
                }
            } catch {
                recoveryError = L10n.string("model.steam.recoveringSession")
                await Task.yield()
                continue
            }
            try await observe(client, initial: snapshot, onRunning: onRunning)
            return true
        }
    }

    private func observe(
        _ client: any SteamGameSessionProtocol,
        initial: SteamGameSnapshot? = nil,
        onRunning: @escaping @MainActor () -> Void
    ) async throws {
        activeClient = client
        appliedRevocations.removeAll()
        try await withTaskCancellationHandler {
            var pending = initial
            var revision: UInt64?
            var reportedRunning = false
            while true {
                let snapshot: SteamGameSnapshot
                do {
                    if let accountId = active?.accountId {
                        do { try await applyRevocation(accountId: accountId, client: client) }
                        catch { active?.connection = .reconnecting }
                    }
                    if let first = pending { snapshot = first; pending = nil }
                    else {
                        let after = revision
                        snapshot = try await Task.detached { try await client.snapshot(after: after) }.value
                    }
                    guard let id = UUID(uuidString: snapshot.id) else {
                        throw SteamError.Failure(reason: "backend.steam.runtime.invalidSession")
                    }
                    if active?.id != id {
                        active = Session(id: id, appId: snapshot.appId, accountId: accountId(of: snapshot))
                    } else if snapshot.accountId != 0 && active?.accountId != snapshot.accountId {
                        active?.accountId = snapshot.accountId
                    }
                    if !snapshot.finished, let accountId = active?.accountId {
                        try await applyRevocation(accountId: accountId, client: client)
                    }
                } catch {
                    // Control loss is uncertainty, not evidence of game exit.
                    active?.connection = .reconnecting
                    await Task.yield()
                    continue
                }
                revision = snapshot.revision
                // Repeated snapshots should not invalidate unchanged presentation.
                if active?.phase != snapshot.phase { active?.phase = snapshot.phase }
                if active?.connection != snapshot.connection { active?.connection = snapshot.connection }
                if active?.host != snapshot.host { active?.host = snapshot.host }
                let cloud = CloudStatus(snapshot)
                if active?.cloud != cloud { active?.cloud = cloud }
                if active?.rosetta != snapshot.rosetta { active?.rosetta = snapshot.rosetta }
                rememberCloud(snapshot)
                if isRecovering { isRecovering = false }
                if recoveryError != nil { recoveryError = nil }
                if snapshot.finished {
                    if snapshot.phase == .failed && snapshot.failureReason == .cancelled { throw CancellationError() }
                    if let error = snapshot.error { throw SteamError.Failure(reason: error) }
                    if let code = snapshot.exitCode, code != 0 { throw SteamError.Failure(reason: "model.steam.gameExited") }
                    return
                }
                if snapshot.phase == .running && !reportedRunning {
                    reportedRunning = true
                    onRunning()
                }
            }
        } onCancel: {
            Task.detached { client.cancelPreparation() }
        }
    }

    private func finish() {
        cancelledRosetta = nil
        active = nil
        activeClient = nil
        appliedRevocations.removeAll()
        isRecovering = false
        operation = nil
    }
}

extension SteamGameSessions: RosettaSessions {
    static func rosettaUnavailable(_ error: Error) -> Bool {
        if case let SteamError.Failure(reason) = error {
            return reason == rosettaUnavailableKey
        }
        return false
    }
}
