import Foundation

/// Where a game's button shows. The library plays an installed game, and its
/// launch installs a newer build first; the game's page offers that update alone.
enum GameActionPlace {
    case page
    case library
}

/// What the player can do with one game of the account now, as its button
/// shows it, and what pressing or cancelling it does.
@MainActor
struct GameAction {
    enum Kind: Equatable {
        case launch
        case download(Download)
        /// The game's Transfer waits in the download queue.
        case queued
        case transferring(Transfer)
        case waiting(Wait)
        /// A step outside any Transfer holds the game.
        case step(Step)
        /// This account's game runs; nil while its session cannot be asked anything.
        case running(GameQuitStage?)
    }

    /// What a download starts.
    enum Download: Equatable {
        /// Installs the Offer.
        case install
        /// Resumes the game's interrupted Transfer.
        case resume
        /// Updates the installation, from the game's page.
        case update
    }

    /// Whose Transfer runs for the game.
    enum Transfer: Equatable {
        /// The game's own: an install, an update or a repair.
        case files
        /// The one its launch runs first, to verify or update its files.
        case launch
    }

    /// What the game waits for.
    enum Wait: Equatable {
        case uninstall
        /// Its launch, or this account's Game session before its game runs.
        case launch
        /// This account's Game session after its game exited, until it ends.
        case closing
        /// Its launch's Transfer, until that Transfer's turn comes.
        case verification
    }

    enum Step: Equatable {
        case cloudSync
        /// A runtime the launch waits for.
        case runtime(RuntimeComponent)
    }

    let kind: Kind
    let title: String
    let isAvailable: Bool
    /// Green to launch, blue to install or update.
    let launches: Bool
    /// Whether the button shows its cross.
    let isCancellable: Bool
    /// Where the button shows, which changes how it shows a wait.
    let place: GameActionPlace
    /// The build it acts on, of a game with both installed: a library card
    /// has no switch, and names the build its button launches.
    var build: GamePlatform?
    /// The game's Transfer, read only in the body of the view showing its
    /// progress, so a progress tick redraws that view alone.
    let transfer: @MainActor () -> GameTransfer?
    /// How far the runtime the launch waits for downloaded, read likewise.
    let stepFraction: @MainActor () -> Double?
    /// Runs at once, a Force Quit too: whoever shows the action asks first.
    let perform: @MainActor () -> Void
    let cancel: @MainActor () -> Void

    /// A Force Quit loses what the game did not save: its player confirms it.
    var needsConfirmation: Bool { kind == .running(.forceQuit) }

    init(kind: Kind, title: String, isAvailable: Bool, launches: Bool, isCancellable: Bool = false,
         place: GameActionPlace = .page,
         transfer: @escaping @MainActor () -> GameTransfer? = { nil },
         stepFraction: @escaping @MainActor () -> Double? = { nil },
         perform: @escaping @MainActor () -> Void = {}, cancel: @escaping @MainActor () -> Void = {}) {
        self.kind = kind
        self.title = title
        self.isAvailable = isAvailable
        self.launches = launches
        self.isCancellable = isCancellable
        self.place = place
        self.transfer = transfer
        self.stepFraction = stepFraction
        self.perform = perform
        self.cancel = cancel
    }
}

/// What every button of one place shows, read once for a whole set of games:
/// it changes with an operation, not with its progress.
@MainActor
struct GameActions<Store: InstallationStorefront> {
    private typealias Receipt = InstallationReceipt<Store.GameID>

    private let installations: GameInstallations<Store>
    private let place: GameActionPlace
    private let receipts: [Store.GameID: [Receipt]]
    private let transferring: Store.GameID?
    /// The game whose launch prepares, and whether it verifies its files first.
    private let launching: Store.GameID?
    private let verifying: Bool
    private let uninstalling: Store.GameID?
    private let queued: Set<Store.GameID>
    /// The build each of those Transfers installs, when it is known.
    private let queuedBuilds: [Store.GameID: GamePlatform]
    /// The games whose install resumes a Transfer that stopped.
    private let resumable: Set<Store.GameID>
    /// The game whose Transfer stopped in this run of Obelisk, and its build
    /// when it is known.
    private let stopped: (game: Store.GameID, build: GamePlatform?)?
    /// The builds of each game whose download stopped before its receipt.
    private let incomplete: [Store.GameID: Set<GamePlatform>]
    private let isConnected: Bool
    private let isRecovering: Bool
    private let session: GameSessions<Store>.ActiveGame?
    /// What ending this account's running game asks of its player now.
    private let quitStage: GameQuitStage?
    /// The game whose Cloud sync works, and the one whose sync the player may cancel.
    private let synchronizing: Store.GameID?
    private let cancellableSync: Store.GameID?

    init(_ installations: GameInstallations<Store>, in place: GameActionPlace) {
        self.installations = installations
        self.place = place
        receipts = Dictionary(grouping: installations.receipts, by: \.gameID)
        transferring = installations.activeTransferGame
        launching = installations.isLaunching ? installations.launchGame : nil
        verifying = installations.isVerifyingBeforeLaunch
        uninstalling = installations.uninstallGame
        queued = Set(installations.queuedGames)
        queuedBuilds = installations.queuedBuilds
        incomplete = Dictionary(grouping: installations.incompleteReceipts, by: \.gameID)
            .mapValues { Set($0.map(\.platform)) }
        resumable = Set(installations.incompleteReceipts.map(\.gameID))
            .union([installations.resumableGame].compactMap { $0 })
        stopped = installations.resumableGame.map { ($0, installations.resumableBuild) }
        isConnected = installations.isConnected
        isRecovering = installations.sessions.isRecovering
        session = installations.accountSession
        // The quit stage reads the active session, which changes with its
        // progress: only this account's running game needs it.
        quitStage = session?.running == true ? installations.sessions.quitStage : nil
        let cloud = installations.cloud
        let syncGame = cloud.sync?.game
        synchronizing = syncGame.flatMap { cloud.isSynchronizing(for: $0) ? $0 : nil }
        cancellableSync = syncGame.flatMap { cloud.canCancelSynchronization(for: $0) ? $0 : nil }
    }

    func callAsFunction(_ selection: InstallationSelection<Store>) -> GameAction {
        let game = selection.game
        let receipt = Receipt.shown(among: receipts[game] ?? [], for: selection,
                                    lastPlayed: installations.lastPlayedBuild(of: game))
        let update = place == .page && receipt?.updateAvailable == true
        let ownSession = session?.game == game ? session : nil
        // Of a game whose player chooses its build, only the build whose
        // download stopped resumes; one that stopped before its build was
        // known resumes as either.
        let begun = incomplete[game] ?? []
        let resumes = selection.choosesBuild
            ? begun.contains(selection.platform) || (stopped?.game == game
                && (stopped?.build == selection.platform || (stopped?.build == nil && begun.isEmpty)))
            : resumable.contains(game)
        // A queued Transfer of the game's other build leaves this one free
        // to launch; one whose build is not known holds both.
        let isQueued = queued.contains(game)
            && (!selection.choosesBuild || queuedBuilds[game].map { $0 == selection.platform } ?? true)
        let kind = self.kind(of: game, session: ownSession, receipt: receipt, update: update, resumes: resumes,
                             isQueued: isQueued)
        let installations = installations
        var action = GameAction(
            kind: kind,
            title: title(of: kind),
            isAvailable: isAvailable(kind, of: selection),
            launches: ownSession != nil || (receipt != nil && !update),
            isCancellable: cancellableSync == game || transferring == game || launching == game
                || isQueued,
            place: place,
            transfer: { installations.transfer(for: game) },
            stepFraction: {
                guard installations.accountSession?.game == game else { return nil }
                return installations.sessions.active?.runtimeInstall?.fraction
            },
            perform: perform(kind, of: selection, receipt: receipt),
            cancel: {
                if installations.cloud.canCancelSynchronization(for: game) {
                    installations.cloud.cancelSynchronization(for: game)
                } else {
                    installations.cancel(game)
                }
            }
        )
        action.build = (receipts[game]?.count ?? 0) > 1 ? receipt?.platform : nil
        return action
    }

    /// The one order every place reads a game's state in. `session` is this
    /// account's Game session of `game`, if any; `update` tells that the
    /// page offers the installation's newer build.
    private func kind(of game: Store.GameID, session: GameSessions<Store>.ActiveGame?, receipt: Receipt?,
                      update: Bool, resumes: Bool, isQueued: Bool) -> GameAction.Kind {
        let isLaunching = launching == game
        if synchronizing == game { return .step(.cloudSync) }
        if isLaunching, let runtime = session?.runtime { return .step(.runtime(runtime)) }
        if session?.running == true { return .running(quitStage) }
        if transferring == game { return .transferring(isLaunching ? .launch : .files) }
        if uninstalling == game { return .waiting(.uninstall) }
        if isLaunching { return .waiting(verifying ? .verification : .launch) }
        if isQueued { return .queued }
        if let session { return .waiting(session.ended ? .closing : .launch) }
        if receipt != nil { return update ? .download(.update) : .launch }
        return .download(resumes && installations.canModify(game) ? .resume : .install)
    }

    private func title(of kind: GameAction.Kind) -> String {
        // Only a Cloud sync outranks the sessions' recovery.
        if isRecovering && kind != .step(.cloudSync) { return L10n.string(Store.recoveringSession) }
        return switch kind {
        case .launch: L10n.string(place == .page ? "model.game.launch" : "ui.game.play")
        case .download(.install): L10n.string("model.game.download")
        case .download(.resume): L10n.string("model.game.resume")
        case .download(.update): L10n.string("model.game.update")
        case .queued: L10n.string("model.game.queued")
        // The Transfer names itself once its progress shows.
        case .transferring(.files): L10n.string("ui.transfer.preparing_short")
        case .transferring(.launch), .waiting(.verification): L10n.string("model.game.verifying")
        case .waiting(.uninstall): L10n.string("model.game.uninstalling")
        case .waiting(.launch): L10n.string("model.game.launching")
        case .waiting(.closing): L10n.string("model.game.closing")
        case .step(.cloudSync): L10n.string("ui.cloud.sync_short")
        case .step(.runtime(.voile)): L10n.string("model.game.installingVoile")
        case .step(.runtime(.steamComponents)): L10n.string("model.game.installingSteamComponents")
        case let .running(stage): stage?.title ?? L10n.string("model.game.running")
        }
    }

    /// The Reservation gate is read here, for each game: it changes with an
    /// operation too.
    private func isAvailable(_ kind: GameAction.Kind, of selection: InstallationSelection<Store>) -> Bool {
        let game = selection.game
        return switch kind {
        case let .running(stage): stage == .quit || stage == .forceQuit
        case .launch: isConnected && installations.canLaunch(game)
        // An installation updates from its receipt; only a new download waits
        // for the storefront to know what it can install.
        case .download(.update): isConnected && installations.canModify(game)
        case .download: isConnected && selection.offer != nil && installations.canModify(game)
        case .queued, .transferring, .waiting, .step: false
        }
    }

    /// Each target checks again that it may still run.
    private func perform(_ kind: GameAction.Kind, of selection: InstallationSelection<Store>,
                         receipt: Receipt?) -> @MainActor () -> Void {
        let installations = installations
        switch kind {
        case .running(.quit):
            return { installations.quitRunningGame() }
        case .running(.forceQuit):
            return { installations.forceQuitRunningGame() }
        case .launch:
            guard let receipt else { return {} }
            return { installations.launch(receipt) }
        case .download(.update):
            guard let receipt else { return {} }
            return { installations.verify(receipt) }
        case .download:
            guard let offer = selection.offer else { return {} }
            let game = selection.game
            let build = selection.choosesBuild ? selection.platform : nil
            return { installations.install(offer, of: game, build: build) }
        case .running, .queued, .transferring, .waiting, .step:
            return {}
        }
    }
}

extension GameInstallations {
    /// The Game action of the game on screen.
    var selectedAction: GameAction? { shownSelection().map { GameActions(self, in: .page)($0) } }
}

/// Both stores use the same progress text, accessibility and transfer controls.
extension GameTransfer {
    /// The runtimes a Transfer gets before its game's files, by the message
    /// that names them while they download.
    private static let runtimeMessages: Set<String> = ["model.game.installingVoile", "model.game.installingSteamComponents"]

    /// The runtime the Transfer downloads now, before its game's files.
    var runtimeTitle: String? {
        guard isInProgress, Self.runtimeMessages.contains(message) else { return nil }
        return L10n.string(message)
    }

    var activityTitle: String {
        if let runtimeTitle { return runtimeTitle }
        return switch state {
        case .preparing: L10n.string("ui.transfer.checking_updates")
        case .verifying: L10n.string("ui.transfer.verifying_files")
        case .downloading: L10n.string("ui.transfer.downloading")
        case .moving: L10n.string("ui.transfer.moving")
        case .complete: L10n.string("ui.transfer.up_to_date")
        case .cancelled: L10n.string("ui.transfer.interrupted")
        case .failed: L10n.string("ui.transfer.failed")
        }
    }

    var verifiedProgressText: String {
        let key = state == .moving ? "ui.transfer.moved_files" : "ui.transfer.verified_files"
        return L10n.string(key, GameFormatting.bytes(completedBytes), GameFormatting.bytes(totalBytes))
    }

    var downloadedGameDataText: String? {
        if contentBytes > 0 {
            L10n.string("ui.transfer.downloaded_data", GameFormatting.bytes(contentBytes))
        } else if state == .verifying {
            L10n.string("ui.transfer.no_downloaded_data")
        } else {
            nil
        }
    }

    var buttonTitle: String {
        if let runtimeTitle { return runtimeTitle }
        return switch state {
        case .preparing: L10n.string("ui.transfer.preparing_short")
        case .verifying: L10n.string("ui.transfer.verifying_short")
        case .downloading: L10n.string("ui.transfer.downloading_short")
        case .moving: L10n.string("ui.transfer.moving_short")
        case .complete: L10n.string("ui.transfer.finishing_short")
        case .cancelled: L10n.string("ui.transfer.cancelling_short")
        case .failed: L10n.string("ui.transfer.failed_short")
        }
    }

    var isInProgress: Bool {
        state == .preparing || state == .verifying || state == .downloading || state == .moving
    }

    var progressFraction: Double? {
        guard totalBytes > 0 else { return nil }
        return Double(min(completedBytes, totalBytes)) / Double(totalBytes)
    }

    /// The files verified so far; a runtime's download counts no game files.
    var countedFilesText: String? {
        totalBytes > 0 && runtimeTitle == nil ? verifiedProgressText : nil
    }

    var progressSummary: String {
        [
            activityTitle,
            countedFilesText,
            downloadedGameDataText,
            currentFile.isEmpty ? nil : L10n.message(currentFile),
            message.isEmpty || runtimeTitle != nil ? nil : L10n.message(message),
        ]
        .compactMap { $0 }
        .joined(separator: "\n")
    }
}
