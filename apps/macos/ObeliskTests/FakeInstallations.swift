import Foundation
@testable import Obelisk

/// One account's installations as Rust would publish them, driven by the test.
/// Rust's own rules are tested in Rust; this only shows snapshots and records
/// the actions Swift forwards.
@MainActor
final class FakeInstallations<Store: InstallationStorefront>: InstallationsBackend {
    enum Call: Equatable {
        case diagnostic(Bool)
        case install(GameOffer)
        case update(InstallationReceipt<Store.GameID>)
        case cancel(Store.GameID)
        case checkUpdates
        case automaticUpdates(Bool)
        case launch(InstallationReceipt<Store.GameID>)
        case join(InstallationReceipt<Store.GameID>, SteamGameInvite)
        case uninstall(InstallationReceipt<Store.GameID>)
        case discard(InstallationReceipt<Store.GameID>)
        case signedOut
        case maintain(Store.GameID, installed: InstallationReceipt<Store.GameID>?)
        case answer(Store.GameID, decision: UInt64, CloudChoice)
    }

    private(set) var state: InstallationsSnapshot<Store>
    private(set) var calls: [Call] = []
    private var waiters: [CheckedContinuation<Void, Never>] = []
    /// How an answer to a Cloud question ends.
    var answering: (Store.GameID, UInt64, CloudChoice) async throws -> Void = { _, _, _ in }
    /// The indicators the account's journals hold.
    var indicators: [(Store.GameID, CloudIndicator)] = []
    /// How many times Swift asked for indicators.
    private(set) var indicatorRequests = 0
    /// Runs as each action reaches Rust, before the call returns.
    var onCall: (Call) -> Void = { _ in }
    /// The Steam client every Windows installation starts through; nil for
    /// no launch settings.
    var steamClient: LaunchSteamClient?
    /// The account Windows Steam signs in to by itself; nil while it would ask.
    var windowsSteamAccountName: String?

    init(receipts: [InstallationReceipt<Store.GameID>] = []) {
        state = InstallationsSnapshot(
            revision: 1, receipts: receipts, transfer: nil, transferGame: nil, downloading: false, launching: false,
            verifyingBeforeLaunch: false, launchGame: nil, uninstallGame: nil,
            resumable: nil, report: nil, warning: nil, gate: .open, cloudSync: nil
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
    func install(_ offer: GameOffer) { record(.install(offer)) }
    func update(_ receipt: InstallationReceipt<Store.GameID>) { record(.update(receipt)) }
    func cancel(_ game: Store.GameID) { record(.cancel(game)) }
    func checkUpdates() { record(.checkUpdates) }
    func setAutomaticUpdates(_ enabled: Bool) { record(.automaticUpdates(enabled)) }
    func launch(_ receipt: InstallationReceipt<Store.GameID>) { record(.launch(receipt)) }
    func join(_ receipt: InstallationReceipt<Store.GameID>, invite: SteamGameInvite) {
        record(.join(receipt, invite))
    }
    func uninstall(_ receipt: InstallationReceipt<Store.GameID>) { record(.uninstall(receipt)) }
    func discard(_ receipt: InstallationReceipt<Store.GameID>) { record(.discard(receipt)) }
    func signedOut() { record(.signedOut) }
    func maintainCloud(_ game: Store.GameID, installed: InstallationReceipt<Store.GameID>?) {
        record(.maintain(game, installed: installed))
    }
    func answerCloud(_ game: Store.GameID, decisionId: UInt64, choice: CloudChoice) async throws {
        record(.answer(game, decision: decisionId, choice))
        try await answering(game, decisionId, choice)
    }
    func cloudIndicators(_ games: [Store.GameID]) async throws -> [(Store.GameID, CloudIndicator)] {
        indicatorRequests += 1
        return indicators
    }
    func launchSettings(_ receipt: InstallationReceipt<Store.GameID>) async throws -> LaunchSettings? {
        steamClient.map {
            LaunchSettings(engine: .rosetta, graphics: .d3dMetal, rayTracing: false, performanceHud: false,
                           steamClient: $0, choices: LaunchChoices(), defaultEngine: .rosetta,
                           defaultSteamClient: $0, engines: [])
        }
    }
    func windowsSteamAccount(_ receipt: InstallationReceipt<Store.GameID>) async -> String? {
        windowsSteamAccountName
    }
    func setLaunchSettings(_ receipt: InstallationReceipt<Store.GameID>, choices: LaunchChoices) async throws
        -> LaunchSettings? { nil }

    /// The actions Swift forwarded, without diagnostics or update settings
    /// and checks.
    var actions: [Call] {
        calls.filter {
            switch $0 {
            case .diagnostic, .checkUpdates, .automaticUpdates: false
            default: true
            }
        }
    }
}

/// Installations as Rust names them.
extension InstallationReceipt where GameID == UInt32 {
    static func steam(appId: UInt32, name: String, directory: String, platform: GamePlatform,
                      totalBytes: UInt64 = 100) -> Self {
        Self(id: "\(appId)-\(platform == .mac ? "macos" : "windows")", gameID: appId, name: name,
             directory: directory, platform: platform, totalBytes: totalBytes)
    }
}

extension InstallationReceipt where GameID == String {
    static func epic(namespace: String, appName: String, name: String, directory: String, platform: GamePlatform,
                     totalBytes: UInt64 = 100) -> Self {
        Self(id: "\(namespace):\(appName):\(platform == .mac ? "Mac" : "Windows")",
             gameID: epicLibraryID(namespace: namespace, appName: appName),
             name: name, directory: directory, platform: platform, totalBytes: totalBytes)
    }
}

extension GameOffer {
    var steamApp: UInt32? {
        guard case let .steam(appId, _, _) = self else { return nil }
        return appId
    }
}

/// Cloud syncs Rust publishes, as a test shows them.
extension GameCloudSync {
    static func launch(_ game: Game, decision: UInt64 = 1, working: Bool = false, cancellable: Bool = false,
                       question: CloudQuestion? = nil) -> Self {
        Self(game: game, kind: .launch, decisionId: question?.decisionId ?? decision, working: working,
             reconnecting: false, cancellable: cancellable, question: question)
    }

    static func exit(_ game: Game, working: Bool = false, question: CloudQuestion? = nil) -> Self {
        Self(game: game, kind: .exit, decisionId: question?.decisionId ?? 1, working: working, reconnecting: false,
             cancellable: false, question: question)
    }

    /// Maintenance works until it asks.
    static func maintenance(_ game: Game, question: CloudQuestion? = nil) -> Self {
        Self(game: game, kind: .maintenance, decisionId: question?.decisionId ?? 1, working: question == nil,
             reconnecting: false, cancellable: false, question: question)
    }
}

extension CloudQuestion {
    static func conflict(_ decision: UInt64, error: String? = nil) -> Self {
        Self(decisionId: decision, kind: .conflict, error: error, choices: [.local, .cloud, .cancel])
    }

    static func failed(_ decision: UInt64, error: String? = nil, launchAnyway: Bool = false) -> Self {
        Self(decisionId: decision, kind: .failed, error: error,
             choices: launchAnyway ? [.retry, .cancel, .launchAnyway] : [.retry, .cancel])
    }
}

/// Gates Rust publishes, as a test shows them.
extension InstallationsGate {
    /// Nothing holds the storefront.
    static var open: Self {
        Self(modify: .open(except: []), uninstall: .open(except: []), launch: .open(except: []),
             maintain: .open(except: []))
    }

    /// A Game session runs `game`: its files, every launch and every Cloud
    /// operation wait.
    static func session(_ game: GameID) -> Self {
        Self(modify: .open(except: [game]), uninstall: .open(except: [game]), launch: .closed, maintain: .closed)
    }

    /// A launch or uninstall of `game` runs: every installation and every
    /// launch wait, and so does the game's own Cloud maintenance.
    static func action(_ game: GameID) -> Self {
        Self(modify: .closed, uninstall: .closed, launch: .closed, maintain: .open(except: [game]))
    }

    /// A Transfer of `game` runs: other games may queue theirs and launch,
    /// none may be uninstalled.
    static func transfer(_ game: GameID) -> Self {
        Self(modify: .open(except: [game]), uninstall: .closed, launch: .open(except: [game]),
             maintain: .open(except: [game]))
    }
}

extension SteamModel {
    /// Installations the test drives, whatever session signs in.
    static func installations(_ fake: FakeInstallations<SteamGames>) -> MakeInstallations {
        { _, _, _, _ in fake }
    }
}

extension WebAccountModel {
    /// Installations the test drives, whatever session signs in.
    static func installations(_ fake: FakeInstallations<Store>) -> MakeInstallations {
        { _, _, _, _, _ in fake }
    }
}

extension CloudIndicator {
    static let neverSynced = CloudIndicator(state: .neverSynced, gameSession: false, syncedAt: nil,
                                            pendingUpload: false, installationRemoved: false)
}

/// One signed-in Steam account with Portal 2 on screen, installed for macOS.
@MainActor
final class InstallationsFixture {
    let sessions: SteamGameSessions
    let receipt: InstallationReceipt<UInt32>
    let installations: SteamInstallations
    private(set) var backend = FakeInstallations<SteamGames>()
    private(set) var reported: [String] = []
    private let installed: Bool

    init(installed: Bool = true, quitting: FakeQuitting = FakeQuitting()) {
        self.installed = installed
        sessions = SteamGameSessions(quitting: quitting.quitting)
        receipt = .steam(appId: 620, name: "Portal 2", directory: "/fixture/Games/Steam/steam-620-macos",
                         platform: .mac, totalBytes: 100)
        installations = SteamInstallations(sessions: sessions, cloud: SteamCloudModel(sessions: sessions))
    }

    /// What Steam offers to install of Portal 2; Steam picks its build as
    /// the Transfer starts.
    func offer() -> GameOffer {
        GameOffer.steam(appId: 620, name: receipt.name, language: "english")
    }

    /// `game` as its page or the library reads it. Steam offers Portal 2
    /// alone, once the storefront knows what it can install.
    func selection(_ game: UInt32, platform: GamePlatform = .mac,
                   installable: Bool = true) -> InstallationSelection<SteamGames> {
        InstallationSelection(game: game, platform: platform, offer: installable && game == 620 ? offer() : nil)
    }

    func connect(platform: GamePlatform = .mac, installable: Bool = true) -> SteamInstallations {
        let selection = selection(620, platform: platform, installable: installable)
        installations.selection = { selection }
        backend = FakeInstallations(receipts: installed ? [receipt] : [])
        installations.connect(account: 42, backend: backend, port: InstallationPort(
            report: { [unowned self] error in if let error { reported.append(SteamModel.message(error)) } },
            warn: { _ in }
        ))
        return installations
    }
}
