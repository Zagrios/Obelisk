import Foundation
import Observation

/// One Transfer the player asked for: a game's platform, installed under a
/// parent folder.
protocol GameTransferRequest: Sendable {
    associatedtype GameID: Hashable & Sendable
    var gameID: GameID { get }
    var platform: String { get }
    /// The parent folder the storefront's installation directory goes in.
    var directory: URL { get }
}

/// What a storefront adds to its installation actions.
protocol InstallationStorefront: CloudStorefront {
    associatedtype Receipt: InstallReceipt where Receipt.GameID == GameID
    associatedtype Request: GameTransferRequest where Request.GameID == GameID
    /// The name a Transfer's progress gives the game.
    static func transferName(of request: Request) -> String
    /// Where new installations go, inside Forge's library directory.
    static var gamesDirectory: String { get }
    static var receiptsUnavailable: String { get }
    static var receiptsNotSaved: String { get }
    /// Shown on a game's page while another game's operation holds the storefront.
    static var otherGameOperation: String { get }
}

/// The game on screen, as the storefront model sees it.
struct InstallationSelection<Store: InstallationStorefront> {
    let game: Store.GameID
    let platform: String
    /// The storefront knows a build of the game Forge can install.
    let installable: Bool
    /// The Transfer that installs the game in `directory`, or verifies `receipt` there.
    let request: @MainActor (_ receipt: Store.Receipt?, _ directory: URL) -> Store.Request
}

/// A signed-in account's storefront operations. Every closure belongs to one
/// connection; signing out drops them.
struct InstallationPort<Store: InstallationStorefront> {
    /// The connected storefront session, or nil while it reconnects.
    var session: @MainActor () -> InstallationSession<Store>?
    var verifiesBeforeLaunch: @MainActor () -> Bool
    /// How the storefront removes an installation, or nil when it cannot.
    var uninstall: @MainActor (Store.Receipt) -> GameUninstall?
    /// Presents a failure; nil clears the last one.
    var report: @MainActor (Error?) -> Void
    var warn: @MainActor (String) -> Void
    /// The game's last installation left the receipts.
    var uninstalled: @MainActor (Store.GameID) -> Void = { _ in }
}

/// One storefront session's Transfer and launch operations. A Transfer keeps
/// the session it started on, even when the storefront reconnects meanwhile.
struct InstallationSession<Store: InstallationStorefront> {
    /// Runs one Transfer, reporting it through `events`.
    var download: @MainActor (Store.Request, TransferEvents<Store.Receipt>) async throws -> Void
    var progress: @Sendable () -> GameTransfer?
    var cancel: @Sendable () -> Void
    /// Hands a verified installation to its supervisor. `proceed` throws when
    /// the launch was cancelled; call it right before the game is handed off.
    var start: @MainActor (Store.Receipt, _ proceed: @MainActor () throws -> Void) async throws -> any GameSessionProtocol
}

/// What a storefront reports while it runs one Transfer.
struct TransferEvents<Receipt> {
    /// Call right before the storefront's Transfer starts. Throws when it was
    /// cancelled meanwhile; from here on, its progress is this Transfer's.
    let started: @MainActor () throws -> Void
    /// Call with the new receipt as soon as the storefront returns it, before
    /// any account bookkeeping. Throws when the Transfer was cancelled first.
    let installed: @MainActor (Receipt) throws -> Void
}

/// A storefront's part of an uninstall. Forge removes the directory between
/// `archive` and `removed`.
struct GameUninstall {
    /// The dedicated directory name the storefront installed into.
    let directoryName: String
    /// Saves the game's Cloud archive before its files go.
    let archive: @MainActor () async throws -> Void
    /// Records the removal for Cloud sync. Runs even after sign-out.
    let removed: @Sendable () async throws -> Void
    /// Deletes a Windows installation's Wine prefix once its folder is gone.
    var removePrefix: (@Sendable () throws -> Void)? = nil
}

/// One storefront's installed games and the actions on them: Transfers,
/// verified launches and uninstalls. Every action asks one Reservation gate,
/// so a new one cannot touch a running game's files.
@MainActor @Observable
final class GameInstallations<Store: InstallationStorefront> {
    typealias Receipt = Store.Receipt
    typealias Request = Store.Request

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
    /// A Transfer that stopped before its receipt, which the player can resume.
    private(set) var lastDownload: Request?
    /// A DEBUG diagnostic fences the storefront like a Game session would.
    var diagnosticReservation = false
    @ObservationIgnored var selection: @MainActor () -> InstallationSelection<Store>? = { nil }

    let sessions: GameSessions<Store>
    let cloud: CloudModel<Store>
    @ObservationIgnored private let directory: URL
    private var account: Store.AccountID?
    @ObservationIgnored private var receiptsFile: URL?
    @ObservationIgnored private var port: InstallationPort<Store>?
    /// The session running the current Transfer, which cancellation reaches.
    @ObservationIgnored private var transferSession: InstallationSession<Store>?
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var cancellationRequested = false
    @ObservationIgnored private var downloadTask: Task<Void, Never>?
    @ObservationIgnored private var launchTask: Task<Void, Never>?
    @ObservationIgnored private var uninstallTask: Task<Void, Never>?
    @ObservationIgnored private var progressTask: Task<Void, Never>?

    /// `directory` is Forge's library directory, which an uninstall never removes.
    init(directory: URL, sessions: GameSessions<Store>, cloud: CloudModel<Store>) {
        self.directory = directory
        self.sessions = sessions
        self.cloud = cloud
    }

    func connect(account: Store.AccountID, receipts file: URL, port: InstallationPort<Store>) {
        reset()
        self.account = account
        receiptsFile = file
        self.port = port
        loadReceipts()
    }

    /// Stops the account's Transfer. A running game keeps its Game session.
    func signedOut() {
        (transferSession ?? port?.session())?.cancel()
        reset()
    }

    private func reset() {
        generation += 1
        cancellationRequested = true
        downloadTask?.cancel()
        launchTask?.cancel()
        uninstallTask?.cancel()
        progressTask?.cancel()
        downloadTask = nil
        launchTask = nil
        progressTask = nil
        port = nil
        transferSession = nil
        account = nil
        receiptsFile = nil
        receipts = []
        transfer = nil
        transferGame = nil
        lastDownload = nil
        isDownloading = false
        isLaunching = false
        isVerifyingBeforeLaunch = false
        launchGame = nil
    }

    // MARK: Reservation gate

    /// Game sessions or a diagnostic hold the storefront.
    var isBusy: Bool { sessions.isBusy || diagnosticReservation }

    /// Every installation-mutating action asks this. Cloud maintenance holds
    /// the game's Reservation; its idle guard also excludes every other launch,
    /// so the whole storefront waits.
    func canModify(_ game: Store.GameID?) -> Bool {
        !isDownloading && !isLaunching && !isUninstalling && !cloud.isMaintaining && !diagnosticReservation
            && !sessions.protects(game)
    }

    /// One Cloud operation at a time: a Game session or maintenance of any
    /// game, or this game's Transfer or uninstall, keeps it closed.
    func canMaintainCloud(_ game: Store.GameID) -> Bool {
        account != nil && !isBusy && !cloud.isMaintaining && !isLaunching
            && activeTransferGame != game && !(isUninstalling && uninstallGame == game)
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
        guard let lastDownload else { return false }
        return canModify(lastDownload.gameID) && lastDownload.gameID == selection()?.game
    }

    var canUninstallSelectedGame: Bool {
        account != nil && selectedReceipt != nil && canModify(selection()?.game)
    }

    var isSynchronizingSaves: Bool {
        selection().map { cloud.isSynchronizing(for: $0.game) } ?? false
    }

    var canSyncCloud: Bool { selection().map { canMaintainCloud($0.game) } ?? false }

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
            return L10n.string(isVerifyingBeforeLaunch ? "model.game.verifying" : "model.game.launching")
        }
        if selectedSession != nil { return L10n.string("model.game.launching") }
        if selectedReceipt != nil { return L10n.string("model.game.launch") }
        return L10n.string(canResumeDownload ? "model.game.resume" : "model.game.download")
    }

    /// Whether the primary action launches the game rather than installing it.
    var primaryActionLaunches: Bool { selectedSession != nil || selectedReceipt != nil }

    /// An installation launches from its receipt; only a new download waits for
    /// the storefront to know what it can install.
    var primaryActionAvailable: Bool {
        guard let selection = selection(), account != nil else { return false }
        let installed = selectedReceipt != nil
        return (installed || selection.installable) && canModify(selection.game) && !(installed && isBusy)
    }

    /// Launches the installed game, otherwise installs it.
    func primaryAction() {
        guard primaryActionAvailable, let selection = selection() else { return }
        if let receipt = selectedReceipt {
            launch(receipt, verification: selection.request(receipt, parent(of: receipt)))
        } else {
            downloadSelected()
        }
    }

    /// Installs the game on screen, or updates its installation in place with
    /// the installed build.
    func downloadSelected() {
        guard let selection = selection(), canModify(selection.game) else { return }
        let receipt = selectedReceipt
        guard receipt != nil || selection.installable else { return }
        if canResumeDownload { return resumeDownload() }
        let destination = receipt.map(parent(of:))
            ?? directory.appending(path: Store.gamesDirectory, directoryHint: .isDirectory)
        download(selection.request(receipt, destination))
    }

    private func parent(of receipt: Receipt) -> URL {
        URL(fileURLWithPath: receipt.directory).deletingLastPathComponent()
    }

    // MARK: Transfers

    func download(_ request: Request) {
        guard let port, let session = port.session(), canModify(request.gameID) else { return }
        do {
            try FileManager.default.createDirectory(at: request.directory, withIntermediateDirectories: true)
        } catch {
            port.report(GameInstallationFailure.downloadDirectory(error))
            return
        }
        let current = generation
        cancellationRequested = false
        port.report(nil)
        begin(request, on: session)
        downloadTask = Task {
            defer { if current == generation { downloadTask = nil } }
            do {
                _ = try await perform(request, on: session, generation: current)
            } catch {
                guard current == generation, !(error is CancellationError), !cancellationRequested else { return }
                port.report(error)
            }
        }
    }

    func resumeDownload() {
        guard canResumeDownload, let lastDownload else { return }
        download(lastDownload)
    }

    /// Stops the Transfer, and a launch still preparing. A running game keeps
    /// its Game session.
    func cancelDownload() {
        guard isDownloading || isLaunching else { return }
        cancellationRequested = true
        (transferSession ?? port?.session())?.cancel()
        if isLaunching && !isDownloading { launchTask?.cancel() }
    }

    private func requireCurrent(_ current: Int) throws {
        guard current == generation && !Task.isCancelled && !cancellationRequested else { throw CancellationError() }
    }

    /// Marks the Transfer's game before its first progress snapshot arrives.
    private func begin(_ request: Request, on session: InstallationSession<Store>) {
        transferSession = session
        lastDownload = request
        transferGame = request.gameID
        transfer = GameTransfer(game: Store.transferName(of: request), state: .preparing, completedBytes: 0,
                                totalBytes: 0, networkBytes: 0, contentBytes: 0, currentFile: "", message: "")
        isDownloading = true
        if let index = receipts.firstIndex(where: { $0.gameID == request.gameID && $0.platform == request.platform }) {
            receipts[index].needsVerification = true
            saveReceipts()
        }
    }

    /// Download, resume and verification before launch share this path, after `begin`.
    private func perform(_ request: Request, on session: InstallationSession<Store>,
                         generation current: Int) async throws -> Receipt {
        let name = Store.transferName(of: request)
        var started = false
        var installed: Receipt?
        var settled = false
        func settle(_ state: TransferState) {
            guard current == generation, !settled else { return }
            settled = true
            progressTask?.cancel()
            progressTask = nil
            if started, let snapshot = session.progress(), snapshot.game == name, snapshot.state == state {
                transfer = snapshot
            } else {
                transfer?.message = ""
            }
            transfer?.state = state
        }
        var terminal = TransferState.failed
        defer {
            settle(terminal)
            if current == generation {
                transferSession = nil
                isDownloading = false
            }
        }
        let events = TransferEvents<Receipt>(started: { [self] in
            try requireCurrent(current)
            started = true
            progressTask?.cancel()
            progressTask = Task {
                while current == generation && !Task.isCancelled {
                    if let snapshot = session.progress(), snapshot.game == name, snapshot.state.isActive,
                       snapshot != transfer {
                        transfer = snapshot
                    }
                    try? await Task.sleep(for: .milliseconds(250))
                }
            }
        }, installed: { [self] receipt in
            try requireCurrent(current)
            guard session.progress()?.state != .cancelled else { throw CancellationError() }
            receipts = receipts.filter { $0.id != receipt.id } + [receipt]
            lastDownload = nil
            saveReceipts()
            installed = receipt
            settle(.complete)
        })
        do {
            try requireCurrent(current)
            try await session.download(request, events)
            guard let installed else { throw CancellationError() }
            return installed
        } catch {
            // The installation stays complete whatever happens after its receipt.
            if installed != nil {
                terminal = .complete
                throw error
            }
            let cancelled = error is CancellationError || cancellationRequested
                || (started && session.progress()?.state == .cancelled)
            terminal = cancelled ? .cancelled : .failed
            throw cancelled ? CancellationError() : error
        }
    }

    // MARK: Launch

    /// Verifies the installation first when the storefront asks for it or the
    /// receipt needs it, then hands it to a Game session.
    private func launch(_ receipt: Receipt, verification: Request) {
        guard let port, let session = port.session(), let account, canModify(receipt.gameID), !isBusy else { return }
        let current = generation
        let game = receipt.gameID
        let mustVerify = port.verifiesBeforeLaunch() || receipt.needsVerification == true
        isLaunching = true
        isVerifyingBeforeLaunch = mustVerify
        launchGame = game
        cancellationRequested = false
        port.report(nil)
        var gameWasRunning = false
        // The shared session store outlives this model: a strong capture here
        // would pin the whole library graph for the game's lifetime.
        launchTask = sessions.launch(game: game, account: account, prepare: { [weak self] in
            guard let self else { throw CancellationError() }
            try self.requireCurrent(current)
            var ready = receipt
            if mustVerify {
                self.begin(verification, on: session)
                ready = try await self.perform(verification, on: session, generation: current)
            }
            try self.requireCurrent(current)
            self.isVerifyingBeforeLaunch = false
            return try await session.start(ready) { [weak self] in
                guard let self else { throw CancellationError() }
                try self.requireCurrent(current)
            }
        }, onRunning: { [weak self] in
            gameWasRunning = true
            guard let self, self.generation == current else { return }
            self.isLaunching = false
        }, onCompletion: { [weak self] error in
            guard let self, self.generation == current else { return }
            self.isLaunching = false
            self.isVerifyingBeforeLaunch = false
            self.launchGame = nil
            self.launchTask = nil
            guard let error, !(error is CancellationError),
                  gameWasRunning || (!Task.isCancelled && !self.cancellationRequested) else { return }
            port.report(error)
        })
    }

    // MARK: Uninstall

    func uninstall(_ receipt: Receipt) {
        guard let port, account != nil, canModify(receipt.gameID), receipts.contains(receipt),
              let file = receiptsFile, let removal = port.uninstall(receipt) else { return }
        let current = generation
        let remaining = receipts.filter { $0 != receipt }
        let game = receipt.gameID
        isUninstalling = true
        uninstallGame = game
        port.report(nil)
        uninstallTask = Task {
            defer {
                isUninstalling = false
                uninstallGame = nil
                uninstallTask = nil
            }
            do {
                guard current == generation && !Task.isCancelled else { return }
                try await removal.archive()
                guard current == generation && !Task.isCancelled else { return }
                do {
                    try await GameInstallationManager.uninstall(
                        directory: receipt.directory, expectedDirectoryName: removal.directoryName,
                        protecting: directory, receiptFile: file, remaining: remaining
                    )
                } catch {
                    // Receipt persistence can fail after the directory was
                    // removed. The Cloud fence must still record that removal.
                    if !FileManager.default.fileExists(atPath: receipt.directory) { try await removal.removed() }
                    throw error
                }
                // Record the original account even if it signed out while the
                // file operation was completing. Removal is never a Cloud delete.
                try await removal.removed()
                // As Steam deletes a Proton prefix, the prefix goes with its game,
                // after the Cloud archive above took its saves.
                if let removePrefix = removal.removePrefix {
                    do { try removePrefix() } catch { port.warn(L10n.string("model.installations.prefixNotRemoved")) }
                }
                guard current == generation else {
                    // A reconnected account may have loaded its receipts before
                    // the removal completed. Refresh only its own receipt file.
                    if receiptsFile != nil { loadReceipts() }
                    return
                }
                receipts = remaining
                if lastDownload?.gameID == game && lastDownload?.platform == receipt.platform { lastDownload = nil }
                if transferGame == game {
                    transfer = nil
                    transferGame = nil
                }
            } catch {
                guard current == generation && !(error is CancellationError) else { return }
                receipts = InstallationReceipts.retainingExistingDirectories(receipts)
                port.report(GameInstallationFailure.uninstall(error))
            }
        }
    }

    // MARK: Receipts

    private func loadReceipts() {
        guard let receiptsFile else {
            receipts = []
            return
        }
        do { receipts = try InstallationReceipts.load(Receipt.self, from: receiptsFile) }
        catch {
            receipts = []
            port?.warn(L10n.string(Store.receiptsUnavailable))
        }
    }

    private func saveReceipts() {
        guard let receiptsFile else { return }
        do { try InstallationReceipts.save(receipts, to: receiptsFile) }
        catch { port?.warn(L10n.string(Store.receiptsNotSaved)) }
    }
}

extension TransferState {
    var isActive: Bool { self == .preparing || self == .verifying || self == .downloading }
}

/// A Steam Transfer: `language` picks the depots.
struct SteamDownload: GameTransferRequest {
    let game: SteamGame
    let platform: String
    let language: String
    let directory: URL
    var gameID: UInt32 { game.appId }
}

extension SteamGames: InstallationStorefront {
    typealias Receipt = SteamInstallReceipt
    typealias Request = SteamDownload
    static func transferName(of request: SteamDownload) -> String { String(request.game.appId) }
    static let gamesDirectory = "Games/Steam"
    static let receiptsUnavailable = "model.steam.receiptsUnavailable"
    static let receiptsNotSaved = "model.steam.receiptsNotSaved"
    static let otherGameOperation = "ui.steam.other_game_operation"
}

struct EpicDownload: GameTransferRequest {
    let game: EpicGame
    let platform: String
    let directory: URL
    var gameID: String { game.libraryID }
}

extension EpicGames: InstallationStorefront {
    typealias Receipt = EpicInstallReceipt
    typealias Request = EpicDownload
    static func transferName(of request: EpicDownload) -> String { request.game.appName }
    static let gamesDirectory = "Games/Epic"
    static let receiptsUnavailable = "model.epic.receiptsUnavailable"
    static let receiptsNotSaved = "model.epic.receiptsNotSaved"
    static let otherGameOperation = "ui.epic.other_game_operation"
}

typealias SteamInstallations = GameInstallations<SteamGames>
typealias EpicInstallations = GameInstallations<EpicGames>
