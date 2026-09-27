import Foundation
@testable import SailOver

/// One account's installations as Rust would publish them, driven by the test.
/// Rust's own rules are tested in Rust; this only shows snapshots and records
/// the actions Swift forwards.
@MainActor
final class FakeInstallations<Store: InstallationStorefront>: InstallationsBackend {
    enum Call: Equatable {
        case diagnostic(Bool)
        case install(Store.Offer)
        case update(Store.Receipt)
        case cancel
        case launch(Store.Receipt, verify: Bool)
        case uninstall(Store.Receipt)
        case signedOut
        case maintain(Store.GameID, installed: Store.Receipt?)
        case answer(Store.GameID, decision: UInt64, CloudChoice)
    }

    private(set) var state: InstallationsSnapshot<Store>
    private(set) var calls: [Call] = []
    private var waiters: [CheckedContinuation<Void, Never>] = []
    /// How an answer to a Cloud question ends.
    var answering: (Store.GameID, UInt64, CloudChoice) async throws -> Void = { _, _, _ in }
    /// The indicators the account's journals hold.
    var indicators: [(Store.GameID, CloudIndicator)] = []
    /// Runs as each action reaches Rust, before the call returns.
    var onCall: (Call) -> Void = { _ in }

    init(receipts: [Store.Receipt] = []) {
        state = InstallationsSnapshot(
            revision: 1, receipts: receipts, transfer: nil, transferGame: nil, downloading: false, launching: false,
            verifyingBeforeLaunch: false, launchGame: nil, uninstalling: false, uninstallGame: nil,
            resumable: nil, report: nil, warning: nil, maintenance: nil, gate: .open
        )
    }

    /// Publishes a change, as Rust does after every one.
    func publish(_ change: (inout InstallationsSnapshot<Store>) -> Void) {
        change(&state)
        state.revision += 1
        let woken = waiters
        waiters.removeAll()
        for waiter in woken { waiter.resume() }
    }

    private func record(_ call: Call) {
        calls.append(call)
        onCall(call)
    }

    func snapshot() -> InstallationsSnapshot<Store> { state }

    func changed(after: UInt64) async -> InstallationsSnapshot<Store> {
        while state.revision <= after {
            await withCheckedContinuation { waiters.append($0) }
        }
        return state
    }

    func setDiagnostic(_ running: Bool) { record(.diagnostic(running)) }
    func install(_ offer: Store.Offer) { record(.install(offer)) }
    func update(_ receipt: Store.Receipt) { record(.update(receipt)) }
    func cancel() { record(.cancel) }
    func launch(_ receipt: Store.Receipt, verify: Bool) { record(.launch(receipt, verify: verify)) }
    func uninstall(_ receipt: Store.Receipt) { record(.uninstall(receipt)) }
    func signedOut() { record(.signedOut) }
    func maintainCloud(_ game: Store.GameID, installed: Store.Receipt?) {
        record(.maintain(game, installed: installed))
    }
    func answerCloud(_ game: Store.GameID, decisionId: UInt64, choice: CloudChoice) async throws {
        record(.answer(game, decision: decisionId, choice))
        try await answering(game, decisionId, choice)
    }
    func cloudIndicators(_ games: [Store.GameID]) async throws -> [(Store.GameID, CloudIndicator)] { indicators }

    /// The actions Swift forwarded, without diagnostics.
    var actions: [Call] {
        calls.filter { if case .diagnostic = $0 { false } else { true } }
    }
}

/// Gates Rust publishes, as a test shows them.
extension InstallationsGate {
    /// Nothing holds the storefront.
    static var open: Self { Self(modify: .open(except: []), launch: true, maintain: .open(except: [])) }

    /// A Game session runs `game`: its files, every launch and every Cloud
    /// operation wait.
    static func session(_ game: GameID) -> Self {
        Self(modify: .open(except: [game]), launch: false, maintain: .closed)
    }

    /// A Transfer, launch or uninstall of `game` runs: every installation
    /// waits, and so does the game's own Cloud maintenance.
    static func action(_ game: GameID) -> Self {
        Self(modify: .closed, launch: false, maintain: .open(except: [game]))
    }
}

extension SteamModel {
    /// Installations the test drives, whatever session signs in.
    static func installations(_ fake: FakeInstallations<SteamGames>) -> MakeInstallations {
        { _, _, _, _ in fake }
    }
}

extension EpicModel {
    /// Installations the test drives, whatever session signs in.
    static func installations(_ fake: FakeInstallations<EpicGames>) -> MakeInstallations {
        { _, _, _, _, _ in fake }
    }
}

extension CloudIndicator {
    static let neverSynced = CloudIndicator(state: .neverSynced, gameSession: false, syncedAt: nil,
                                            pendingUpload: false, installationRemoved: false)
}
