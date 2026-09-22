import Foundation
import Observation

extension EpicCloudConfiguration {
    var libraryID: String { epicLibraryID(namespace: namespace, appName: appName) }
}

/// Account-scoped presentation for the native EGS Cloud coordinator.
@MainActor @Observable
final class EpicCloudModel {
    let root: String
    let sessionDirectory: String
    private let sessions: EpicGameSessions
    private var accountId: String?
    private var generation = 0
    private var attempted: Set<String> = []
    private(set) var pending: [EpicCloudConfiguration] = []
    private(set) var isLoadingPending = false
    private(set) var promptBusy = false
    private var promptError: String?
    private var dismissed: Decision?
    @ObservationIgnored private var work: Task<Void, Never>?
    @ObservationIgnored var canResume: @MainActor () -> Bool = { true }
    @ObservationIgnored private var resumeSession: (@MainActor (EpicCloudConfiguration) async throws -> any EpicGameSessionProtocol)?
    @ObservationIgnored private let loadPending: @Sendable (String, String) throws -> [EpicCloudConfiguration]

    private struct Decision: Equatable {
        let sessionId: String
        let decisionId: UInt64
        init(_ session: EpicGameSessions.Session) {
            sessionId = session.id
            decisionId = session.decisionId
        }
    }

    init(root: String, sessionDirectory: String, sessions: EpicGameSessions,
         loadPending: @escaping @Sendable (String, String) throws -> [EpicCloudConfiguration] = {
             try epicCloudPending(root: $0, accountId: $1)
         }) {
        self.root = root
        self.sessionDirectory = sessionDirectory
        self.sessions = sessions
        self.loadPending = loadPending
        observeSessions()
    }

    func connect(accountId: String,
                 resume: @escaping @MainActor (EpicCloudConfiguration) async throws -> any EpicGameSessionProtocol) {
        reset()
        self.accountId = accountId
        resumeSession = resume
        self.resume()
    }

    func signedOut() {
        let old = accountId
        reset()
        if let old { sessions.signedOut(accountId: old) }
    }

    private func observeSessions() {
        withObservationTracking {
            _ = sessions.isBusy
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.sessionFinished()
                self.observeSessions()
            }
        }
    }

    private func reset() {
        generation += 1
        work?.cancel()
        work = nil
        accountId = nil
        resumeSession = nil
        pending = []
        attempted = []
        dismissed = nil
        promptError = nil
        promptBusy = false
        isLoadingPending = false
    }

    /// A connect or wake retries each pending game once. Completion advances the
    /// queue without spinning forever on an unavailable network or a conflict.
    func resume() {
        attempted = []
        refreshPending()
    }

    func sessionFinished() {
        if let last = sessions.last, last.accountId == accountId,
           [.finished, .failed].contains(last.phase), [.conflict, .pending, .failed].contains(last.cloudPhase) {
            attempted.insert(last.libraryID)
        }
        refreshPending()
    }

    private func refreshPending() {
        guard let accountId, !sessions.isBusy, !isLoadingPending, canResume() else { return }
        let current = generation
        let root = root
        let load = loadPending
        isLoadingPending = true
        work = Task {
            defer { if current == generation { isLoadingPending = false; work = nil } }
            guard let values = try? await Task.detached(operation: { try load(root, accountId) }).value,
                  current == generation, !Task.isCancelled else { return }
            pending = values
            startNext()
        }
    }

    private func startNext(preferred: String? = nil) {
        guard let accountId, let resumeSession, !sessions.isBusy, canResume(),
              var configuration = pending.first(where: {
                  if let preferred { preferred == $0.libraryID } else { !attempted.contains($0.libraryID) }
              }) else { return }
        let id = configuration.libraryID
        configuration.root = root
        configuration.sessionDirectory = sessionDirectory
        configuration.helper = SteamGameLauncher.helper
        attempted.insert(id)
        let request = configuration
        sessions.launch(libraryID: id, accountId: accountId, prepare: {
            try await resumeSession(request)
        }, onCompletion: { [weak self] _ in
            // A former account may have kept its game alive through sign-out.
            // Once it releases the session, the current account may resume.
            self?.sessionFinished()
        })
    }

    private var active: EpicGameSessions.Session? {
        guard let value = sessions.active, value.observed, value.accountId == accountId else { return nil }
        return value
    }

    var prompt: EpicGameSessions.Session? {
        guard let active, active.phase == .preparing || active.phase == .finalizing,
              Decision(active) != dismissed else { return nil }
        return active
    }

    var promptPresented: Bool { prompt?.cloudPhase == .conflict }
    var failurePresented: Bool { prompt?.cloudPhase == .failed && prompt?.allowedChoices.isEmpty == false }
    var promptLibraryID: String? { prompt?.libraryID }
    var promptMessage: String { promptError ?? prompt?.error ?? L10n.string("ui.cloud.working") }
    var promptWorking: Bool { promptBusy || prompt?.reconnecting == true }
    var canLaunchAnyway: Bool { prompt?.allowedChoices.contains(.launchAnyway) == true }

    func isSynchronizing(for libraryID: String) -> Bool {
        guard let active, active.libraryID == libraryID else { return false }
        return [.checking, .syncing].contains(active.cloudPhase) || (promptBusy && prompt?.libraryID == libraryID)
    }

    func canCancelSynchronization(for libraryID: String) -> Bool {
        guard let active, active.libraryID == libraryID, active.phase == .preparing,
              !active.reconnecting, !promptBusy else { return false }
        return [.checking, .syncing].contains(active.cloudPhase)
    }

    func respond(_ choice: EpicCloudChoice) {
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

    func hasPending(for libraryID: String) -> Bool {
        pending.contains { $0.libraryID == libraryID }
            || status(for: libraryID).map { [.pending, .failed, .conflict].contains($0.cloudPhase) } == true
    }

    func retryPending(libraryID: String) {
        guard !sessions.isBusy else { return }
        dismissed = nil
        if pending.contains(where: { $0.libraryID == libraryID }) {
            startNext(preferred: libraryID)
        } else {
            attempted.remove(libraryID)
            refreshPending()
        }
    }

    private func status(for libraryID: String) -> EpicGameSessions.Session? {
        if let active, active.libraryID == libraryID { return active }
        guard let last = sessions.last, last.accountId == accountId, last.libraryID == libraryID else { return nil }
        return last
    }

    func message(for libraryID: String) -> String? {
        switch status(for: libraryID)?.cloudPhase {
        case .checking, .syncing: L10n.string("ui.cloud.working")
        case .pending, .failed: L10n.string("ui.cloud.pending")
        case .synced: L10n.string("ui.cloud.synced")
        case .disabled: L10n.string("ui.cloud.disabled")
        case .unsupported: L10n.string("ui.epic.cloud.unsupported")
        default: nil
        }
    }

    func configuration(game: EpicGame, receipt: EpicInstallReceipt, language: String) -> EpicCloudConfiguration {
        EpicCloudConfiguration(root: root, home: NSHomeDirectory(), install: receipt.directory,
                               sessionDirectory: sessionDirectory, helper: SteamGameLauncher.helper,
                               namespace: game.namespace, catalogItemId: game.catalogItemId,
                               appName: game.appName, platform: receipt.platform, language: language)
    }
}
