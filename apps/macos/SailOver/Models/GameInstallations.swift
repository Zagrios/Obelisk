import Foundation
import Observation

/// What a storefront adds to its installation actions.
protocol InstallationStorefront: GameStorefront {
    associatedtype Receipt: InstallReceipt where Receipt.GameID == GameID
    /// What the storefront offers to install: one game's build.
    associatedtype Offer: Equatable & Sendable
    /// A failed answer, as the storefront's models describe errors.
    @MainActor static func describe(_ error: Error) -> String
    /// The game Rust names `id`.
    static func game(_ id: String) -> GameID?
    /// Shown on a game's page while another game's operation holds the storefront.
    static var otherGameOperation: String { get }
}

/// The game on screen, as the storefront model sees it.
struct InstallationSelection<Store: InstallationStorefront> {
    let game: Store.GameID
    let platform: String
    /// The build SailOver can install, once the storefront knows one.
    let offer: Store.Offer?
}

/// What may start now, of one kind of action, as Rust's Reservation shows it.
enum InstallationOpening<GameID: Hashable>: Equatable {
    case closed
    /// It may start, for any game but these.
    case open(except: [GameID])

    func allows(_ game: GameID) -> Bool {
        switch self {
        case .closed: false
        case let .open(except): !except.contains(game)
        }
    }
}

/// What Rust's Reservation lets start now.
struct InstallationsGate<GameID: Hashable>: Equatable {
    /// A Transfer or uninstall.
    var modify: InstallationOpening<GameID>
    /// A launch, of any game a Transfer may touch.
    var launch: Bool
    var maintain: InstallationOpening<GameID>

    static var closed: Self { Self(modify: .closed, launch: false, maintain: .closed) }

    init(modify: InstallationOpening<GameID>, launch: Bool, maintain: InstallationOpening<GameID>) {
        self.modify = modify
        self.launch = launch
        self.maintain = maintain
    }

    /// Rust's gate, whose games `game` names.
    init(_ gate: InstallationGate, game: (String) -> GameID?) {
        func opening(_ opening: GameOpening) -> InstallationOpening<GameID> {
            switch opening {
            case .closed: .closed
            case let .open(except): .open(except: except.compactMap(game))
            }
        }
        self.init(modify: opening(gate.modify), launch: gate.launch, maintain: opening(gate.maintain))
    }
}

/// A signed-in account's storefront operations. Every closure belongs to one
/// connection; signing out drops them.
struct InstallationPort<Store: InstallationStorefront> {
    var verifiesBeforeLaunch: @MainActor () -> Bool
    /// Presents a failure; nil clears the last one.
    var report: @MainActor (Error?) -> Void
    var warn: @MainActor (String) -> Void
    /// The game's last installation left the receipts.
    var uninstalled: @MainActor (Store.GameID) -> Void = { _ in }
}

/// Everything one account's installations present, as Rust keeps them.
struct InstallationsSnapshot<Store: InstallationStorefront>: Equatable {
    var revision: UInt64
    var receipts: [Store.Receipt]
    var transfer: GameTransfer?
    var transferGame: Store.GameID?
    var downloading: Bool
    var launching: Bool
    var verifyingBeforeLaunch: Bool
    var launchGame: Store.GameID?
    var uninstalling: Bool
    var uninstallGame: Store.GameID?
    /// The game whose Transfer stopped before its receipt.
    var resumable: Store.GameID?
    var report: InstallationReport?
    var warning: InstallationWarning?
    var maintenance: GameCloudMaintenance<Store.GameID>?
    var gate: InstallationsGate<Store.GameID>
}

/// One account's installations of one storefront, in Rust. Each action
/// returns at once; the snapshot tells how it goes.
@MainActor
protocol InstallationsBackend<Store>: AnyObject {
    associatedtype Store: InstallationStorefront
    func snapshot() -> InstallationsSnapshot<Store>
    /// Waits for a revision newer than `after`.
    func changed(after: UInt64) async -> InstallationsSnapshot<Store>
    func setDiagnostic(_ running: Bool)
    /// Installs `offer`'s build, or resumes the game's interrupted Transfer.
    func install(_ offer: Store.Offer)
    /// Updates `receipt`'s installation in place, or resumes the game's
    /// interrupted Transfer.
    func update(_ receipt: Store.Receipt)
    func cancel()
    /// Launches `receipt`'s installation, verifying it first when `verify`
    /// asks for it or the installation needs it.
    func launch(_ receipt: Store.Receipt, verify: Bool)
    func uninstall(_ receipt: Store.Receipt)
    func signedOut()
    /// Syncs the game's Cloud saves: those of `installed`, or an uninstalled
    /// game's when nil.
    func maintainCloud(_ game: Store.GameID, installed: Store.Receipt?)
    /// Answers the Cloud question the game shows: its maintenance's or its
    /// Game session's.
    func answerCloud(_ game: Store.GameID, decisionId: UInt64, choice: CloudChoice) async throws
    /// The games' Cloud indicators, and those of every other game of the
    /// account with pending work.
    func cloudIndicators(_ games: [Store.GameID]) async throws -> [(Store.GameID, CloudIndicator)]
}

/// One storefront's installed games and the actions on them, as Rust keeps
/// them: Transfers, verified launches and uninstalls. This presents them for
/// the game on screen and forwards the player's actions.
@MainActor @Observable
final class GameInstallations<Store: InstallationStorefront> {
    typealias Receipt = Store.Receipt

    private(set) var receipts: [Receipt] = [] {
        // Every path that removes a game's last installation reports it here,
        // so no caller has to remember to. Each change is one assignment, so
        // replacing a receipt never reads as a removal.
        didSet {
            for game in Set(oldValue.map(\.gameID)).subtracting(receipts.map(\.gameID)) { port?.uninstalled(game) }
        }
    }
    private(set) var transfer: GameTransfer?
    /// The game the last Transfer progress belongs to.
    private(set) var transferGame: Store.GameID?
    private(set) var isDownloading = false
    private(set) var isLaunching = false
    private(set) var isVerifyingBeforeLaunch = false
    private(set) var launchGame: Store.GameID?
    private(set) var isUninstalling = false
    private(set) var uninstallGame: Store.GameID?
    /// The game whose Transfer stopped before its receipt: the player can
    /// resume it.
    private(set) var resumableGame: Store.GameID?
    /// What Rust's Reservation lets start now.
    private(set) var gate = InstallationsGate<Store.GameID>.closed
    /// A DEBUG diagnostic fences the storefront like a Game session would.
    var diagnosticReservation = false {
        didSet { backend?.setDiagnostic(diagnosticReservation) }
    }
    @ObservationIgnored var selection: @MainActor () -> InstallationSelection<Store>? = { nil }

    let sessions: GameSessions<Store>
    let cloud: CloudModel<Store>
    private var account: Store.AccountID?
    @ObservationIgnored private var backend: (any InstallationsBackend<Store>)?
    @ObservationIgnored private var port: InstallationPort<Store>?
    @ObservationIgnored private var revision: UInt64 = 0
    @ObservationIgnored private var report: InstallationReport?
    @ObservationIgnored private var warning: InstallationWarning?
    @ObservationIgnored private var observation: Task<Void, Never>?

    init(sessions: GameSessions<Store>, cloud: CloudModel<Store>) {
        self.sessions = sessions
        self.cloud = cloud
    }

    func connect(account: Store.AccountID, backend: any InstallationsBackend<Store>, port: InstallationPort<Store>) {
        reset()
        self.account = account
        self.backend = backend
        self.port = port
        cloud.connect(account: account, backend: backend)
        backend.setDiagnostic(diagnosticReservation)
        apply(backend.snapshot())
        observation = Task { [weak self, backend] in
            var after = self?.revision ?? 0
            while !Task.isCancelled {
                let snapshot = await backend.changed(after: after)
                guard let self, self.backend === backend else { return }
                after = snapshot.revision
                apply(snapshot)
            }
        }
    }

    /// Stops the account's Transfer. A running game keeps its Game session.
    func signedOut() {
        backend?.signedOut()
        reset()
    }

    private func reset() {
        observation?.cancel()
        observation = nil
        port = nil
        backend = nil
        account = nil
        revision = 0
        report = nil
        warning = nil
        receipts = []
        transfer = nil
        transferGame = nil
        resumableGame = nil
        gate = .closed
        isDownloading = false
        isLaunching = false
        isVerifyingBeforeLaunch = false
        launchGame = nil
        isUninstalling = false
        uninstallGame = nil
    }

    private func apply(_ snapshot: InstallationsSnapshot<Store>) {
        guard snapshot.revision > revision else { return }
        revision = snapshot.revision
        // Repeated snapshots do not invalidate unchanged presentation.
        if receipts != snapshot.receipts { receipts = snapshot.receipts }
        if transfer != snapshot.transfer { transfer = snapshot.transfer }
        if transferGame != snapshot.transferGame { transferGame = snapshot.transferGame }
        if isDownloading != snapshot.downloading { isDownloading = snapshot.downloading }
        if isLaunching != snapshot.launching { isLaunching = snapshot.launching }
        if isVerifyingBeforeLaunch != snapshot.verifyingBeforeLaunch {
            isVerifyingBeforeLaunch = snapshot.verifyingBeforeLaunch
        }
        if launchGame != snapshot.launchGame { launchGame = snapshot.launchGame }
        if isUninstalling != snapshot.uninstalling { isUninstalling = snapshot.uninstalling }
        if uninstallGame != snapshot.uninstallGame { uninstallGame = snapshot.uninstallGame }
        if resumableGame != snapshot.resumable { resumableGame = snapshot.resumable }
        if gate != snapshot.gate { gate = snapshot.gate }
        if cloud.maintenance != snapshot.maintenance { cloud.maintenance = snapshot.maintenance }
        if report != snapshot.report {
            report = snapshot.report
            if let report { port?.report(Self.failure(report)) }
        }
        if warning != snapshot.warning {
            warning = snapshot.warning
            if let warning { port?.warn(L10n.string(warning.key)) }
        }
    }

    private static func failure(_ report: InstallationReport) -> Error {
        switch report {
        case let .storefront(key): Store.failure(key)
        case let .downloadDirectory(message): GameInstallationFailure.downloadDirectory(message)
        case let .uninstall(reason): GameInstallationFailure.uninstall(reason)
        }
    }

    /// Runs an action on the account's installations, then shows its state.
    private func act(_ action: (any InstallationsBackend<Store>) -> Void) {
        guard let backend else { return }
        port?.report(nil)
        action(backend)
        apply(backend.snapshot())
        sessions.refresh()
    }

    // MARK: Reservation gate

    /// Whether a Transfer or uninstall of `game` may start, as Rust's
    /// Reservation shows it.
    func canModify(_ game: Store.GameID?) -> Bool {
        game.map(gate.modify.allows) ?? false
    }

    /// Whether `game` may launch: no Game session runs.
    func canLaunch(_ game: Store.GameID) -> Bool {
        gate.launch && gate.modify.allows(game)
    }

    /// Whether the player may sync `game`'s Cloud saves.
    func canMaintainCloud(_ game: Store.GameID) -> Bool {
        account != nil && gate.maintain.allows(game)
    }

    /// Identifies the operation before its first progress snapshot arrives.
    var activeTransferGame: Store.GameID? {
        if isDownloading { return transferGame }
        if isLaunching && isVerifyingBeforeLaunch { return launchGame }
        return nil
    }

    /// The last Transfer progress, when it belongs to `game`.
    func transfer(for game: Store.GameID) -> GameTransfer? {
        transferGame == game ? transfer : nil
    }

    func receipt(for game: Store.GameID, platform: String) -> Receipt? {
        receipts.first { $0.gameID == game && $0.platform == platform }
    }

    // MARK: The game on screen

    /// The installation of the selected platform, else the game's other one: an
    /// installed build stays on screen whatever the storefront would install now.
    var selectedReceipt: Receipt? {
        selection().flatMap { selection in
            receipt(for: selection.game, platform: selection.platform) ?? receipts.first { $0.gameID == selection.game }
        }
    }

    /// The active Game session when it is this account's, for the game on screen.
    var selectedSession: GameSessions<Store>.Session? {
        guard let active = sessions.active, active.account == account,
              active.game == selection()?.game else { return nil }
        return active
    }

    var canResumeDownload: Bool {
        guard let resumableGame else { return false }
        return canModify(resumableGame) && resumableGame == selection()?.game
    }

    var canUninstallSelectedGame: Bool {
        account != nil && selectedReceipt != nil && canModify(selection()?.game)
    }

    var isSynchronizingSaves: Bool {
        selection().map { cloud.isSynchronizing(for: $0.game) } ?? false
    }

    var canSyncCloud: Bool { selection().map { canMaintainCloud($0.game) } ?? false }

    /// Syncs the Cloud saves of the game on screen: of its installation, or
    /// of an uninstalled game.
    func syncCloud() {
        guard canSyncCloud, let selection = selection() else { return }
        let installed = selectedReceipt
        act { $0.maintainCloud(selection.game, installed: installed) }
    }

    /// The game on screen's Cloud indicator, or nil when it is hidden.
    var cloudIndicator: CloudIndicatorPresentation? {
        guard let game = selection()?.game, account != nil else { return nil }
        let playing = sessions.active.map { $0.game == game && $0.account == account } ?? false
        return CloudIndicatorPresentation.make(saved: cloud.indicator(for: game), isSyncing: isSynchronizingSaves,
                                               isPlaying: playing, isInstalled: selectedReceipt != nil)
    }

    var primaryActionTitle: String {
        let game = selection()?.game
        if isSynchronizingSaves { return L10n.string("ui.cloud.sync_short") }
        if sessions.isRecovering { return L10n.string(Store.recoveringSession) }
        if selectedSession?.running == true { return L10n.string("model.game.running") }
        if isUninstalling && uninstallGame == game { return L10n.string("model.game.uninstalling") }
        if isLaunching && launchGame == game {
            if runtimeInstallFraction != nil { return L10n.string("model.game.installingVoile") }
            return L10n.string(isVerifyingBeforeLaunch ? "model.game.verifying" : "model.game.launching")
        }
        if selectedSession != nil { return L10n.string("model.game.launching") }
        if selectedReceipt != nil { return L10n.string("model.game.launch") }
        return L10n.string(canResumeDownload ? "model.game.resume" : "model.game.download")
    }

    /// How much of Voile has downloaded while the game on screen waits for it.
    var runtimeInstallFraction: Double? {
        guard isLaunching, let game = launchGame, game == selection()?.game,
              let active = sessions.active, active.game == game else { return nil }
        return active.runtimeInstall
    }

    /// Whether the primary action launches the game rather than installing it.
    var primaryActionLaunches: Bool { selectedSession != nil || selectedReceipt != nil }

    /// An installation launches from its receipt; only a new download waits for
    /// the storefront to know what it can install.
    var primaryActionAvailable: Bool {
        guard let selection = selection(), account != nil else { return false }
        if selectedReceipt != nil { return canLaunch(selection.game) }
        return selection.offer != nil && canModify(selection.game)
    }

    /// Launches the installed game, otherwise installs it.
    func primaryAction() {
        guard primaryActionAvailable else { return }
        if let receipt = selectedReceipt {
            guard let port else { return }
            let verify = port.verifiesBeforeLaunch()
            act { $0.launch(receipt, verify: verify) }
        } else {
            downloadSelected()
        }
    }

    /// Installs the game on screen, or updates its installation in place with
    /// the installed build. Rust resumes the game's interrupted Transfer
    /// instead, and picks every folder.
    func downloadSelected() {
        guard let selection = selection(), canModify(selection.game) else { return }
        if let receipt = selectedReceipt {
            act { $0.update(receipt) }
        } else if let offer = selection.offer {
            act { $0.install(offer) }
        }
    }

    // MARK: Transfers

    /// Stops the Transfer, and a launch still preparing. A running game keeps
    /// its Game session.
    func cancelDownload() {
        guard isDownloading || isLaunching, let backend else { return }
        backend.cancel()
        apply(backend.snapshot())
        sessions.refresh()
    }

    // MARK: Uninstall

    func uninstall(_ receipt: Receipt) {
        guard account != nil, canModify(receipt.gameID), receipts.contains(receipt) else { return }
        act { $0.uninstall(receipt) }
    }
}

extension TransferState {
    var isActive: Bool { self == .preparing || self == .verifying || self == .downloading }
}

extension SteamGames: InstallationStorefront {
    typealias Receipt = SteamInstallReceipt
    typealias Offer = SteamOffer
    static func game(_ id: String) -> UInt32? { UInt32(id) }
    static let otherGameOperation = "ui.steam.other_game_operation"
    @MainActor static func describe(_ error: Error) -> String { SteamModel.message(error) }
}

extension EpicGames: InstallationStorefront {
    typealias Receipt = EpicInstallReceipt
    typealias Offer = EpicOffer
    /// Rust names an Epic game as its library ID does.
    static func game(_ id: String) -> String? { epicGameID(id) == nil ? nil : id }
    static let otherGameOperation = "ui.epic.other_game_operation"
    @MainActor static func describe(_ error: Error) -> String { EpicModel.message(error) }
}

typealias SteamInstallations = GameInstallations<SteamGames>
typealias EpicInstallations = GameInstallations<EpicGames>

/// The installations of a session that is not Rust's: it has none, and every
/// action does nothing.
final class NoInstallations<Store: InstallationStorefront>: InstallationsBackend {
    private let empty = InstallationsSnapshot<Store>(
        revision: 1, receipts: [], transfer: nil, transferGame: nil, downloading: false, launching: false,
        verifyingBeforeLaunch: false, launchGame: nil, uninstalling: false, uninstallGame: nil, resumable: nil,
        report: nil, warning: nil, maintenance: nil, gate: .closed
    )

    func snapshot() -> InstallationsSnapshot<Store> { empty }
    func changed(after: UInt64) async -> InstallationsSnapshot<Store> {
        // Nothing ever changes: wait until the observer goes.
        while !Task.isCancelled { try? await Task.sleep(for: .seconds(3600)) }
        return empty
    }
    func setDiagnostic(_ running: Bool) {}
    func install(_ offer: Store.Offer) {}
    func update(_ receipt: Store.Receipt) {}
    func cancel() {}
    func launch(_ receipt: Store.Receipt, verify: Bool) {}
    func uninstall(_ receipt: Store.Receipt) {}
    func signedOut() {}
    func maintainCloud(_ game: Store.GameID, installed: Store.Receipt?) {}
    func answerCloud(_ game: Store.GameID, decisionId: UInt64, choice: CloudChoice) async throws {
        throw Store.failure(Store.invalidSession)
    }
    func cloudIndicators(_ games: [Store.GameID]) async throws -> [(Store.GameID, CloudIndicator)] { [] }
}

/// A Steam account's installations in Rust.
final class SteamInstallationsBackend: InstallationsBackend {
    typealias Store = SteamGames
    private let core: SteamAccountInstallations

    init(_ core: SteamAccountInstallations) { self.core = core }

    func snapshot() -> InstallationsSnapshot<SteamGames> { Self.snapshot(core.state()) }
    func changed(after: UInt64) async -> InstallationsSnapshot<SteamGames> {
        Self.snapshot(await core.changed(after: after))
    }
    func setDiagnostic(_ running: Bool) { core.setDiagnostic(running: running) }
    func install(_ offer: SteamOffer) { core.install(offer: offer) }
    func update(_ receipt: SteamInstallReceipt) { core.update(receipt: receipt.id) }
    func cancel() { core.cancel() }
    func launch(_ receipt: SteamInstallReceipt, verify: Bool) { core.launch(receipt: receipt.id, verify: verify) }
    func uninstall(_ receipt: SteamInstallReceipt) { core.uninstall(receipt: receipt.id) }
    func signedOut() { core.signedOut() }
    func maintainCloud(_ game: UInt32, installed: SteamInstallReceipt?) {
        core.maintainCloud(appId: game, receipt: installed?.id)
    }
    func answerCloud(_ game: UInt32, decisionId: UInt64, choice: CloudChoice) async throws {
        do { try await core.answerCloud(appId: game, decisionId: decisionId, choice: choice) }
        catch { throw SteamGameSessions.translated(error) }
    }
    func cloudIndicators(_ games: [UInt32]) async throws -> [(UInt32, CloudIndicator)] {
        try await core.cloudIndicators(appIds: games).map { ($0.appId, $0.indicator) }
    }

    private static func snapshot(_ state: SteamInstallationsState) -> InstallationsSnapshot<SteamGames> {
        InstallationsSnapshot(
            revision: state.revision, receipts: state.receipts.map(receipt), transfer: state.transfer,
            transferGame: state.transferGame, downloading: state.downloading, launching: state.launching,
            verifyingBeforeLaunch: state.verifyingBeforeLaunch, launchGame: state.launchGame,
            uninstalling: state.uninstalling, uninstallGame: state.uninstallGame,
            resumable: state.resumable.flatMap(SteamGames.game), report: state.report, warning: state.warning,
            maintenance: state.maintenance.map { GameCloudMaintenance(game: $0.appId, cloud: $0.cloud) },
            gate: InstallationsGate(state.gate, game: SteamGames.game)
        )
    }
    private static func receipt(_ receipt: SteamReceipt) -> SteamInstallReceipt {
        SteamInstallReceipt(appId: receipt.appId, name: receipt.name, directory: receipt.directory,
                            platform: receipt.platform, language: receipt.language, totalBytes: receipt.totalBytes,
                            needsVerification: receipt.needsVerification)
    }
}

/// An Epic account's installations in Rust.
final class EpicInstallationsBackend: InstallationsBackend {
    typealias Store = EpicGames
    private let core: EpicAccountInstallations

    init(_ core: EpicAccountInstallations) { self.core = core }

    func snapshot() -> InstallationsSnapshot<EpicGames> { Self.snapshot(core.state()) }
    func changed(after: UInt64) async -> InstallationsSnapshot<EpicGames> {
        Self.snapshot(await core.changed(after: after))
    }
    func setDiagnostic(_ running: Bool) { core.setDiagnostic(running: running) }
    func install(_ offer: EpicOffer) { core.install(offer: offer) }
    func update(_ receipt: EpicInstallReceipt) { core.update(receipt: receipt.id) }
    func cancel() { core.cancel() }
    func launch(_ receipt: EpicInstallReceipt, verify: Bool) { core.launch(receipt: receipt.id, verify: verify) }
    func uninstall(_ receipt: EpicInstallReceipt) { core.uninstall(receipt: receipt.id) }
    func signedOut() { core.signedOut() }
    func maintainCloud(_ game: String, installed: EpicInstallReceipt?) {
        guard let id = Self.id(game) else { return }
        core.maintainCloud(game: id, receipt: installed?.id)
    }
    func answerCloud(_ game: String, decisionId: UInt64, choice: CloudChoice) async throws {
        guard let id = Self.id(game) else { throw EpicGames.failure(EpicGames.invalidSession) }
        do { try await core.answerCloud(game: id, decisionId: decisionId, choice: choice) }
        catch { throw EpicGameSessions.translated(error) }
    }
    func cloudIndicators(_ games: [String]) async throws -> [(String, CloudIndicator)] {
        try await core.cloudIndicators(games: games.compactMap(Self.id)).map {
            (epicLibraryID(namespace: $0.game.namespace, appName: $0.game.appName), $0.indicator)
        }
    }

    private static func game(_ game: EpicGameId?) -> String? {
        game.map { epicLibraryID(namespace: $0.namespace, appName: $0.appName) }
    }
    /// The game a library ID names.
    private static func id(_ libraryID: String) -> EpicGameId? {
        epicGameID(libraryID).map { EpicGameId(namespace: $0.namespace, appName: $0.appName) }
    }
    private static func snapshot(_ state: EpicInstallationsState) -> InstallationsSnapshot<EpicGames> {
        InstallationsSnapshot(
            revision: state.revision, receipts: state.receipts.map(receipt), transfer: state.transfer,
            transferGame: game(state.transferGame), downloading: state.downloading, launching: state.launching,
            verifyingBeforeLaunch: state.verifyingBeforeLaunch, launchGame: game(state.launchGame),
            uninstalling: state.uninstalling, uninstallGame: game(state.uninstallGame),
            resumable: state.resumable.flatMap(EpicGames.game), report: state.report, warning: state.warning,
            maintenance: state.maintenance.map {
                GameCloudMaintenance(game: epicLibraryID(namespace: $0.game.namespace, appName: $0.game.appName),
                                     cloud: $0.cloud)
            },
            gate: InstallationsGate(state.gate, game: EpicGames.game)
        )
    }
    private static func receipt(_ receipt: EpicReceipt) -> EpicInstallReceipt {
        EpicInstallReceipt(namespace: receipt.namespace, appName: receipt.appName, name: receipt.name,
                           directory: receipt.directory, platform: receipt.platform, totalBytes: receipt.totalBytes,
                           needsVerification: receipt.needsVerification)
    }
}
