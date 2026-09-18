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

    struct Session {
        let id: UUID
        let appId: UInt32
        var phase = SteamGamePhase.preparing
        var connection = SteamSupervisorConnection.connected
        var host = SteamHostState.online
        var running: Bool { phase == .running }
    }

    private(set) var active: Session?
    private(set) var isRecovering = false
    private(set) var recoveryError: String?
    var isBusy: Bool { isRecovering || active != nil }

    @ObservationIgnored private var operation: Task<Void, Never>?
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
                    recoveryError = SteamModel.message(error)
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
        if isRecovering { return true }
        guard let appId else { return false }
        return active?.appId == appId
    }

    private func run(_ request: Launch, onRunning: @escaping @MainActor () -> Void) async throws {
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
                if snapshot.finished { continue }
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
        try await withTaskCancellationHandler {
            var pending = initial
            var revision: UInt64?
            var reportedRunning = false
            while true {
                let snapshot: SteamGameSnapshot
                do {
                    if let first = pending { snapshot = first; pending = nil }
                    else {
                        let after = revision
                        snapshot = try await Task.detached { try await client.snapshot(after: after) }.value
                    }
                    guard let id = UUID(uuidString: snapshot.id) else {
                        throw SteamError.Failure(reason: "backend.steam.runtime.invalidSession")
                    }
                    if active?.id != id {
                        active = Session(id: id, appId: snapshot.appId)
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
                if isRecovering { isRecovering = false }
                if recoveryError != nil { recoveryError = nil }
                if snapshot.finished {
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
        active = nil
        isRecovering = false
        operation = nil
    }
}
