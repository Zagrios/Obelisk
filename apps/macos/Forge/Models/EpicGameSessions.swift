import Foundation
import Observation

extension EpicGameSnapshot {
    var finished: Bool { phase == .finished || phase == .failed }
    var libraryID: String { epicLibraryID(namespace: namespace, appName: appName) }
}

/// Keeps the installation reserved until the native supervisor confirms exit.
/// Account sign-out revokes Cloud access without releasing a running process.
@MainActor @Observable
final class EpicGameSessions {
    static let shared = EpicGameSessions(recoverExisting: true)
    static func sessionDirectory(in directory: URL) -> String {
        directory.appending(path: "EpicSessions").path
    }
    static var sessionDirectory: String { sessionDirectory(in: AppIdentity.libraryDirectory) }

    struct Session: Identifiable, Equatable {
        let id: String
        let libraryID: String
        let accountId: String
        var phase: EpicGamePhase = .preparing
        var cloudPhase: EpicCloudPhase = .checking
        var decisionId: UInt64 = 0
        var allowedChoices: [EpicCloudChoice] = []
        var error: String?
        var rosetta: RosettaRequest?
        var reconnecting = false
        var observed = false

        init(id: String = UUID().uuidString, libraryID: String, accountId: String) {
            self.id = id
            self.libraryID = libraryID
            self.accountId = accountId
        }

        init(_ snapshot: EpicGameSnapshot) {
            id = snapshot.id
            libraryID = snapshot.libraryID
            accountId = snapshot.accountId
            phase = snapshot.phase
            cloudPhase = snapshot.cloudPhase
            decisionId = snapshot.decisionId
            allowedChoices = snapshot.allowedChoices
            error = snapshot.error
            rosetta = snapshot.rosetta
            reconnecting = snapshot.reconnecting
            observed = true
        }
    }

    private(set) var active: Session?
    private(set) var last: Session?
    private(set) var isRecovering = false
    private(set) var recoveryError: String?
    var isBusy: Bool { active != nil || isRecovering }
    private var cancelledRosetta: RosettaPrompt?
    @ObservationIgnored private var operation: Task<Void, Never>?
    @ObservationIgnored private var client: (any EpicGameSessionProtocol)?
    @ObservationIgnored private var revokedAccounts: Set<String> = []
    @ObservationIgnored private var appliedRevocations: Set<String> = []
    @ObservationIgnored private let recoverSession: @MainActor () async throws -> (any EpicGameSessionProtocol)?

    init(recoverExisting: Bool = false,
         recoverSession: @escaping @MainActor () async throws -> (any EpicGameSessionProtocol)? = {
             try await recoverEpicGameSession(sessionDirectory: EpicGameSessions.sessionDirectory, helper: SteamGameLauncher.helper)
         }) {
        self.recoverSession = recoverSession
        if recoverExisting {
            isRecovering = true
            operation = Task {
                _ = try? await recoverAndObserve(onRunning: {})
                finish()
            }
        }
    }

    /// Admission happens before verification or authentication can suspend.
    @discardableResult
    func launch(libraryID: String, accountId: String,
                prepare: @escaping @MainActor () async throws -> any EpicGameSessionProtocol,
                onRunning: @escaping @MainActor () -> Void = {},
                onCompletion: @escaping @MainActor (Error?) -> Void = { _ in }) -> Task<Void, Never>? {
        guard !isBusy else { return nil }
        revokedAccounts.removeAll()
        appliedRevocations.removeAll()
        active = Session(libraryID: libraryID, accountId: accountId)
        recoveryError = nil
        let task = Task {
            var failure: Error?
            do {
                let started: any EpicGameSessionProtocol
                do {
                    try Task.checkCancellation()
                    started = try await prepare()
                } catch {
                    // A failed handoff can have accepted a process. Recovery is
                    // the only authority that may release that reservation.
                    isRecovering = true
                    active?.reconnecting = true
                    if try await recoverAndObserve(onRunning: onRunning) {
                        finish()
                        onCompletion(nil)
                        return
                    }
                    throw error
                }
                try await observe(started, begin: true, onRunning: onRunning)
            } catch { failure = error }
            finish()
            onCompletion(failure)
        }
        operation = task
        return task
    }

    func protects(_ libraryID: String?) -> Bool {
        isRecovering || (libraryID != nil && active?.libraryID == libraryID)
    }

    func choose(_ choice: EpicCloudChoice, expected: Session, accountId: String) async throws {
        guard let active, let client, active.id == expected.id,
              active.decisionId == expected.decisionId, active.accountId == accountId,
              !revokedAccounts.contains(accountId), !active.reconnecting else {
            throw EpicError.Service(message: "backend.epic.cloud.invalidSession")
        }
        if choice == .cancel, active.phase == .preparing,
           [.checking, .syncing].contains(active.cloudPhase) {
            await Task.detached { client.cancelPreparation() }.value
            return
        }
        guard active.allowedChoices.contains(choice) else {
            throw EpicError.Service(message: "backend.epic.cloud.invalidDecision")
        }
        try await client.choose(accountId: accountId, decisionId: expected.decisionId, choice: choice)
    }

    typealias RosettaPrompt = RosettaLaunchPrompt<String, String>

    func rosettaPrompt(accountId: String) -> RosettaPrompt? {
        guard let active, active.phase == .preparing, active.accountId == accountId,
              !active.reconnecting, !revokedAccounts.contains(accountId), let request = active.rosetta else { return nil }
        let prompt = RosettaPrompt(id: active.id, accountId: accountId, decisionId: request.decisionId)
        return prompt == cancelledRosetta ? nil : prompt
    }

    func resumeAfterRosetta(_ prompt: RosettaPrompt) async throws {
        guard rosettaPrompt(accountId: prompt.accountId) == prompt, let client else {
            throw EpicError.Service(message: "backend.epic.cloud.invalidSession")
        }
        try Task.checkCancellation()
        try await client.resumeAfterRosetta(accountId: prompt.accountId, decisionId: prompt.decisionId)
    }

    func cancelRosetta(_ prompt: RosettaPrompt) {
        guard rosettaPrompt(accountId: prompt.accountId) == prompt else { return }
        cancelledRosetta = prompt
        operation?.cancel()
    }

    func revokeCloud(accountId: String) async throws {
        revokedAccounts.insert(accountId)
        guard let client, active?.accountId == accountId else { return }
        try await applyRevocation(accountId: accountId, client: client)
    }

    func signedOut(accountId: String) {
        // Admission checks this synchronously before sending Begin. The network
        // revocation may need to wait for an in-flight handoff to return a client.
        revokedAccounts.insert(accountId)
        guard let client, active?.accountId == accountId else { return }
        Task { try? await applyRevocation(accountId: accountId, client: client) }
    }

    private func applyRevocation(accountId: String, client: any EpicGameSessionProtocol) async throws {
        guard revokedAccounts.contains(accountId), !appliedRevocations.contains(accountId) else { return }
        try await Task.detached { try await client.revokeCloud(accountId: accountId) }.value
        if self.client === client { appliedRevocations.insert(accountId) }
    }

    private func recoverAndObserve(onRunning: @escaping @MainActor () -> Void) async throws -> Bool {
        while true {
            let recovered: (any EpicGameSessionProtocol)?
            do {
                // Cancellation must not turn unknown process ownership into exit.
                recovered = try await Task { try await self.recoverSession() }.value
            } catch {
                recoveryError = L10n.string("model.epic.recoveringSession")
                await retryDelay()
                continue
            }
            guard let recovered else { return false }
            try await observe(recovered, onRunning: onRunning)
            return true
        }
    }

    private func observe(_ observed: any EpicGameSessionProtocol, begin: Bool = false,
                         onRunning: @escaping @MainActor () -> Void) async throws {
        client = observed
        appliedRevocations.removeAll()
        try await withTaskCancellationHandler {
            // The cancellation handler already forwards a cancelled task.
            if begin && !Task.isCancelled {
                if active.map({ revokedAccounts.contains($0.accountId) }) == true {
                    await Task.detached { observed.cancelPreparation() }.value
                } else {
                    do { try await observed.begin() }
                    catch {
                        // A lost Begin reply cannot establish whether admission
                        // happened. Keep ownership until a terminal snapshot.
                        active?.reconnecting = true
                    }
                }
            }
            var revision: UInt64?
            var reportedRunning = false
            while true {
                let snapshot: EpicGameSnapshot
                var revocationFailed = false
                do {
                    if let account = active?.accountId {
                        do { try await applyRevocation(accountId: account, client: observed) }
                        catch { revocationFailed = true }
                    }
                    let after = revision
                    snapshot = try await Task.detached { try await observed.snapshot(afterRevision: after) }.value
                    guard !snapshot.id.isEmpty, !snapshot.accountId.isEmpty,
                          !snapshot.namespace.isEmpty, !snapshot.appName.isEmpty else {
                        throw EpicError.InvalidResponse
                    }
                    if !snapshot.finished {
                        do { try await applyRevocation(accountId: snapshot.accountId, client: observed) }
                        catch { revocationFailed = true }
                    }
                } catch {
                    active?.reconnecting = true
                    recoveryError = L10n.string("model.epic.recoveringSession")
                    await retryDelay()
                    continue
                }
                revision = snapshot.revision
                var updated = Session(snapshot)
                // A failed revoke cannot suppress the snapshot that establishes
                // real exit after a helper disappears or loses its control socket.
                if revocationFailed && !snapshot.finished { updated.reconnecting = true }
                if active != updated { active = updated }
                if last != updated { last = updated }
                isRecovering = false
                recoveryError = nil
                if snapshot.finished {
                    if snapshot.phase == .failed {
                        if snapshot.failureReason == .cancelled { throw CancellationError() }
                        throw EpicError.Service(message: snapshot.error ?? "backend.epic.cloud.failed")
                    }
                    if let code = snapshot.exitCode, code != 0 {
                        throw EpicError.Service(message: "model.epic.gameExited")
                    }
                    return
                }
                if snapshot.phase == .running && !reportedRunning {
                    reportedRunning = true
                    onRunning()
                }
            }
        } onCancel: {
            Task.detached { observed.cancelPreparation() }
        }
    }

    private func retryDelay() async {
        try? await Task.sleep(for: .milliseconds(250))
    }

    private func finish() {
        cancelledRosetta = nil
        active = nil
        client = nil
        appliedRevocations.removeAll()
        isRecovering = false
        recoveryError = nil
        operation = nil
    }
}

/// Forwards cancellation as soon as UniFFI returns a client, even while the
/// remembered account is still waiting for its ordered Keychain write.
final class EpicGameHandoff: @unchecked Sendable {
    private let lock = NSLock()
    private var client: (any EpicGameSessionProtocol)?
    private var cancelled = false
    private var cancellationSent = false

    func receive(_ client: any EpicGameSessionProtocol) {
        let shouldCancel = lock.withLock {
            self.client = client
            guard cancelled, !cancellationSent else { return false }
            cancellationSent = true
            return true
        }
        if shouldCancel { Task.detached { client.cancelPreparation() } }
    }

    func cancel() {
        let target = lock.withLock { () -> (any EpicGameSessionProtocol)? in
            cancelled = true
            guard let client, !cancellationSent else { return nil }
            cancellationSent = true
            return client
        }
        if let target { Task.detached { target.cancelPreparation() } }
    }

    func revoke(accountId: String) async throws {
        guard let target = lock.withLock({ client }) else { return }
        try await Task.detached { try await target.revokeCloud(accountId: accountId) }.value
    }
}

extension EpicGameSessions: RosettaSessions {
    static func rosettaUnavailable(_ error: Error) -> Bool {
        if case let EpicError.Service(message) = error {
            return message == rosettaUnavailableKey
        }
        return false
    }
}
