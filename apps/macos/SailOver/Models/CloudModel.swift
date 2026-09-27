import Foundation
import Observation

/// The account's Cloud sync running now, as Rust shows it: a launch's, an
/// exit's or a maintenance's, with the one Cloud question the account asks.
struct GameCloudSync<Game: Hashable>: Equatable {
    let game: Game
    let kind: CloudSyncKind
    /// Binds a cancellation to the sync as it shows now.
    let decisionId: UInt64
    /// It compares or transfers saves now.
    let working: Bool
    /// Its Game session's control connection was lost: nothing reaches it.
    let reconnecting: Bool
    /// The player may cancel it: a launch check still in progress.
    let cancellable: Bool
    let question: CloudQuestion?
}

typealias SteamCloudModel = CloudModel<SteamGames>
typealias EpicCloudModel = CloudModel<EpicGames>

/// Presents one account's Cloud: indicators, and the Cloud sync and question
/// Rust shows for it. Rust runs every sync, asks every question and routes
/// each answer; this owns no save comparison or transfer.
@MainActor @Observable
final class CloudModel<Store: InstallationStorefront> {
    private let sessions: GameSessions<Store>
    private(set) var account: Store.AccountID?
    @ObservationIgnored private var backend: (any InstallationsBackend<Store>)?
    private var generation = 0
    private(set) var indicators: [Store.GameID: CloudIndicator] = [:]
    /// The account's Cloud sync, as its installations mirror it. Once a
    /// maintenance ends, its game's indicator is read again and the failure of
    /// an answer goes.
    var sync: GameCloudSync<Store.GameID>? {
        didSet {
            guard let ended = oldValue, ended.kind == .maintenance,
                  sync?.kind != .maintenance || sync?.game != ended.game else { return }
            if question == nil { promptError = nil }
            refreshIndicators(for: [ended.game])
        }
    }
    private(set) var promptBusy = false
    private var promptError: String?

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
        sync = nil
        indicators = [:]
        account = nil
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

    var isMaintaining: Bool { sync?.kind == .maintenance }

    /// The one Cloud question the account asks, until it has its answer.
    var question: CloudQuestion? { account == nil ? nil : sync?.question }

    var promptPresented: Bool { question?.kind == .conflict }
    var promptGame: Store.GameID? { question == nil ? nil : sync?.game }
    /// Only a launch check asks after a failure; a failed maintenance shows in
    /// the game's Cloud indicator instead.
    var failurePresented: Bool { question?.kind == .failed }
    var promptMessage: String { promptError ?? question?.error ?? L10n.string("ui.cloud.working") }
    var promptWorking: Bool { promptBusy || sync?.reconnecting == true || sync?.working == true }
    var canLaunchAnyway: Bool { question?.kind == .failed && question?.choices.contains(.launchAnyway) == true }

    func isSynchronizing(for game: Store.GameID) -> Bool {
        guard account != nil, let sync, sync.game == game else { return false }
        return sync.working || promptBusy
    }

    func canCancelSynchronization(for game: Store.GameID) -> Bool {
        guard account != nil, let sync, sync.game == game else { return false }
        return sync.cancellable && !promptBusy
    }

    /// Answers the question shown. Rust gives it to the sync that asked.
    func respond(_ choice: CloudChoice) {
        guard let game = sync?.game, let question else { return }
        answer(game, decision: question.decisionId, choice: choice)
    }

    /// Cancels `game`'s launch check still in progress.
    func cancelSynchronization(for game: Store.GameID) {
        guard canCancelSynchronization(for: game), let sync else { return }
        answer(game, decision: sync.decisionId, choice: .cancel)
    }

    private func answer(_ game: Store.GameID, decision: UInt64, choice: CloudChoice) {
        guard account != nil, let backend, !promptBusy else { return }
        let current = generation
        promptBusy = true
        promptError = nil
        Task {
            defer { if current == generation { promptBusy = false } }
            do {
                try await backend.answerCloud(game, decisionId: decision, choice: choice)
            } catch {
                guard current == generation else { return }
                promptError = Store.describe(error)
            }
        }
    }
}
