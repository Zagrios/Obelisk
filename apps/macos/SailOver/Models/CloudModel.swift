import Foundation
import Observation

/// Cloud maintenance of one game as the account's installations run it in
/// Rust. While it exists, it holds the Reservation of every game.
struct GameCloudMaintenance<Game: Equatable>: Equatable {
    let game: Game
    var cloud: CloudStatus

    /// The conflict question the sync waits on, if any.
    var conflict: CloudStatus? {
        cloud.phase == .conflict && !cloud.allowedChoices.isEmpty ? cloud : nil
    }

    var isSyncing: Bool { conflict == nil }
}

typealias SteamCloudModel = CloudModel<SteamGames>
typealias EpicCloudModel = CloudModel<EpicGames>

/// Presents one account's Cloud: indicators, and the one Cloud question its
/// Game session or Cloud maintenance asks. Rust runs every sync and routes
/// each answer; this owns no save comparison or transfer.
@MainActor @Observable
final class CloudModel<Store: InstallationStorefront> {
    private let sessions: GameSessions<Store>
    private(set) var account: Store.AccountID?
    @ObservationIgnored private var backend: (any InstallationsBackend<Store>)?
    private var generation = 0
    private(set) var indicators: [Store.GameID: CloudIndicator] = [:]
    /// The account's Cloud maintenance, as its installations mirror it. Once
    /// it ends, the game's indicator is read again and its question's error
    /// goes.
    var maintenance: GameCloudMaintenance<Store.GameID>? {
        didSet {
            guard let ended = oldValue, maintenance?.game != ended.game else { return }
            if prompt == nil { promptError = nil }
            refreshIndicators(for: [ended.game])
        }
    }
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

    init(sessions: GameSessions<Store>) {
        self.sessions = sessions
    }

    /// `backend` holds the account's installations, which run its Cloud.
    func connect(account: Store.AccountID, backend: any InstallationsBackend<Store>) {
        reset()
        self.account = account
        self.backend = backend
    }

    func signedOut() {
        let old = account
        reset()
        if let old { sessions.signedOut(account: old) }
    }

    /// Everything presented belongs to one account; a change invalidates it all.
    private func reset() {
        generation += 1
        backend = nil
        maintenance = nil
        indicators = [:]
        account = nil
        dismissed = nil
        promptError = nil
        promptBusy = false
    }

    func indicator(for game: Store.GameID) -> CloudIndicator? { indicators[game] }

    /// Reads the journals off the main thread. Never polls: callers refresh
    /// when a game page opens, the account changes, or a sync ends.
    func refreshIndicators(for games: [Store.GameID]) {
        guard account != nil, let backend else { return }
        let current = generation
        Task {
            guard let values = try? await backend.cloudIndicators(games), current == generation else { return }
            for (game, indicator) in values { indicators[game] = indicator }
        }
    }

    var isMaintaining: Bool { maintenance != nil }

    private var maintenanceConflict: CloudStatus? { maintenance?.conflict }

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

    /// Answers the question shown. Rust gives it to the sync that asked.
    func respond(_ choice: CloudChoice) {
        guard account != nil, let backend, !promptBusy else { return }
        let prompt = prompt
        let question: (game: Store.GameID, decision: UInt64)
        if let prompt {
            question = (prompt.game, prompt.cloud.decisionId)
        } else if let maintenance, let conflict = maintenanceConflict {
            question = (maintenance.game, conflict.decisionId)
        } else {
            return
        }
        let current = generation
        promptBusy = true
        promptError = nil
        Task {
            defer { if current == generation { promptBusy = false } }
            do {
                try await backend.answerCloud(question.game, decisionId: question.decision, choice: choice)
                guard current == generation else { return }
                if choice == .cancel, let prompt { dismissed = Decision(prompt) }
            } catch {
                guard current == generation else { return }
                promptError = Store.describe(error)
            }
        }
    }
}
