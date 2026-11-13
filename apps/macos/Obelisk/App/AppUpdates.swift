import Foundation
import Observation
import os
import Sparkle

/// Obelisk's own updates, through Sparkle. Once a day Obelisk reads the latest
/// release's appcast.xml and, by default, downloads a newer version and
/// installs it when Obelisk really quits; otherwise Sparkle offers it in its
/// own window. A player who takes beta releases gets the pre-releases the
/// feed lists too, whichever version is newest; one who stops taking them
/// goes back to the latest release, as `ReleaseReturn` tells.
@MainActor
@Observable
final class AppUpdates {
    /// Whether a check may start now: not while one runs, nor in a build that
    /// never updates.
    private(set) var canCheck = false
    /// Where the return to the latest release stands, as Settings shows it.
    enum ReturnStanding: Equatable {
        /// Checked and laid beside Obelisk: it takes the beta's place as
        /// Obelisk quits.
        case ready(FeedRelease)
        /// Obelisk cannot put it back by itself where it is, a folder it may
        /// not write: the player installs it by hand.
        case unreachable(FeedRelease)
    }
    private(set) var returnStanding: ReturnStanding?
    /// Whether Obelisk and Voile take their pre-releases too. Voile's choice
    /// waits beside its runtimes, where the next Windows game launch reads it.
    var takesBetaReleases: Bool {
        didSet {
            guard takesBetaReleases != oldValue else { return }
            defaults.set(takesBetaReleases, forKey: Self.betaReleasesKey)
            keepVoileChoice(takesBetaReleases)
            if takesBetaReleases { discardReturn() }
            // An update Sparkle already got installs as Obelisk quits whatever
            // the feed says now, and a check would only show it again: after
            // a beta, the return follows at the next start.
            if delegate.updateWaiting { return }
            checkChannel()
        }
    }
    static let betaReleasesKey = "obelisk.updates.betaReleases"
    /// The newest release a feed ever listed, below which Obelisk never goes
    /// back.
    static let newestReleaseKey = "obelisk.updates.newestRelease"

    @ObservationIgnored private let delegate: UpdaterDelegate
    @ObservationIgnored private let controller: SPUStandardUpdaterController
    @ObservationIgnored private let starts: Bool
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private let keepVoileChoice: @MainActor (Bool) -> Void
    @ObservationIgnored private var observation: NSKeyValueObservation?
    /// The return under way or ready: the release it goes to, the folder
    /// `ReleaseReturn` lays it in, and the work that does.
    @ObservationIgnored private var pending: (release: FeedRelease, folder: URL, task: Task<Void, Never>)?
    private static let logger = Logger(subsystem: AppIdentity.storageIdentifier, category: "Updates")

    /// Debug builds and the test host never start: the latest release would
    /// replace them. `keepVoileChoice` tells Voile's launches whether they
    /// take pre-releases.
    init(starts: Bool = AppUpdates.startsByDefault, defaults: UserDefaults = .standard,
         keepVoileChoice: @escaping @MainActor (Bool) -> Void = AppUpdates.keepLiveVoileChoice) {
        let delegate = UpdaterDelegate()
        self.delegate = delegate
        self.starts = starts
        self.defaults = defaults
        self.keepVoileChoice = keepVoileChoice
        // A beta the player installed without ever choosing is their choice
        // of beta versions: Obelisk keeps it rather than going back.
        let running = Self.runningVersion
        if defaults.object(forKey: Self.betaReleasesKey) == nil && ReleaseReturn.isBeta(running) {
            defaults.set(true, forKey: Self.betaReleasesKey)
        }
        takesBetaReleases = defaults.bool(forKey: Self.betaReleasesKey)
        controller = SPUStandardUpdaterController(startingUpdater: starts, updaterDelegate: delegate,
                                                  userDriverDelegate: nil)
        delegate.takesBetaReleases = { [weak self] in self?.takesBetaReleases ?? false }
        delegate.loaded = { [weak self] feed in self?.read(feed) }
        delegate.foundUpdate = { [weak self] in self?.discardReturn() }
        // Sparkle changes it on the main thread, as it does everything.
        observation = controller.updater.observe(\.canCheckForUpdates, options: [.initial, .new]) { [weak self] _, change in
            let canCheck = change.newValue ?? false
            MainActor.assumeIsolated { self?.canCheck = canCheck }
        }
        self.keepVoileChoice(takesBetaReleases)
        // A beta left with beta versions off goes back without waiting for the
        // next daily check.
        if starts, !takesBetaReleases, ReleaseReturn.isBeta(running) {
            controller.updater.checkForUpdatesInBackground()
        }
    }

    /// Whether Obelisk downloads a new version by itself and installs it when
    /// it quits. Sparkle keeps the choice; its update window can change it too.
    var installsAutomatically: Bool {
        get {
            access(keyPath: \.installsAutomatically)
            return controller.updater.automaticallyDownloadsUpdates
        }
        set {
            withMutation(keyPath: \.installsAutomatically) {
                controller.updater.automaticallyDownloadsUpdates = newValue
            }
        }
    }

    /// How Obelisk quits when the player installs a new version from Sparkle's
    /// window, before Sparkle opens the new one: quitting otherwise only hides
    /// Obelisk. It calls `install` once Obelisk may end.
    var quitToInstall: (_ install: @escaping () -> Void) -> Void {
        get { delegate.quit }
        set { delegate.quit = newValue }
    }

    /// Looks for a new version now, in Sparkle's window.
    func check() {
        controller.checkForUpdates(nil)
    }

    /// Puts the latest release in place of the pre-release Obelisk runs, once
    /// it is ready: Obelisk is quitting, and opens as that release next.
    func finishReturn() {
        guard let pending else { return }
        self.pending = nil
        pending.task.cancel()
        guard returnStanding == .ready(pending.release) else {
            ReleaseReturn.abandon(pending.folder)
            return
        }
        do {
            try ReleaseReturn.install(from: pending.folder, over: Bundle.main.bundleURL)
            Self.logger.notice("Obelisk \(pending.release.version, privacy: .public) replaced the beta.")
        } catch {
            ReleaseReturn.abandon(pending.folder)
            Self.logger.error("""
                Obelisk could not go back to \(pending.release.version, privacy: .public): \
                \(String(describing: error), privacy: .public)
                """)
        }
    }

    /// A new choice of channel reads the feed again at once, unless a check
    /// already runs: the next one follows it then.
    private func checkChannel() {
        guard starts else { return }
        if controller.updater.canCheckForUpdates {
            controller.updater.checkForUpdatesInBackground()
        } else {
            controller.updater.resetUpdateCycle()
        }
    }

    /// What the feed Sparkle just read lists: the newest release, and the
    /// release a beta running goes back to.
    private func read(_ feed: [FeedRelease]) {
        var floor = defaults.string(forKey: Self.newestReleaseKey)
        if let latest = ReleaseReturn.latestRelease(in: feed),
           floor.map({ ReleaseReturn.newer(latest.version, than: $0) }) ?? true {
            floor = latest.version
            defaults.set(floor, forKey: Self.newestReleaseKey)
        }
        guard let release = ReleaseReturn.target(running: Self.runningVersion, in: feed,
                                                 takesBetaReleases: takesBetaReleases, floor: floor) else {
            discardReturn()
            return
        }
        if pending?.release == release { return }
        discardReturn()
        guard let key = Bundle.main.object(forInfoDictionaryKey: "SUPublicEDKey") as? String else { return }
        let app = Bundle.main.bundleURL
        let folder: URL
        do {
            folder = try ReleaseReturn.folder(replacing: app)
        } catch {
            failed(release, error)
            return
        }
        let task = Task { [weak self] in
            do {
                try await ReleaseReturn.stage(release, in: folder, replacing: app, publicKey: key)
            } catch {
                ReleaseReturn.abandon(folder)
                // The next feed read tries again. A return cancelled since
                // leaves alone the one that replaced it.
                if let self, pending?.folder == folder {
                    pending = nil
                    failed(release, error)
                }
                return
            }
            guard let self, pending?.folder == folder else {
                ReleaseReturn.abandon(folder)
                return
            }
            returnStanding = .ready(release)
            Self.logger.notice("Obelisk \(release.version, privacy: .public) waits to replace the beta.")
        }
        pending = (release, folder, task)
    }

    /// Notes why Obelisk could not get `release` back; a folder it may not
    /// write is the player's to fix, so Settings tells them.
    private func failed(_ release: FeedRelease, _ error: any Error) {
        if (error as? ReleaseReturn.Failure) == .unwritable { returnStanding = .unreachable(release) }
        Self.logger.error("""
            Obelisk could not get \(release.version, privacy: .public) back: \
            \(String(describing: error), privacy: .public)
            """)
    }

    /// Forgets the return under way or ready, its files away from the main
    /// thread.
    private func discardReturn() {
        if let pending {
            pending.task.cancel()
            let folder = pending.folder
            Task.detached(priority: .utility) { ReleaseReturn.abandon(folder) }
        }
        pending = nil
        returnStanding = nil
    }

    /// The version Sparkle compares, of the Obelisk running.
    private static var runningVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? ""
    }

    /// Keeps Voile's choice in the library's runtimes, one write after the
    /// other, so the last choice is the one kept; the test host keeps none, so
    /// it never touches the player's.
    private static func keepLiveVoileChoice(_ take: Bool) {
        guard ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] == nil else { return }
        let locations = ObeliskLocations.live
        voileChoices.async { _ = takeVoilePrereleases(locations: locations, take: take) }
    }
    private static let voileChoices = DispatchQueue(label: "com.zagrios.obelisk.voile-choice", qos: .utility)

    private static var startsByDefault: Bool {
        #if DEBUG
        false
        #else
        true
        #endif
    }
}

/// What Obelisk tells Sparkle: the channels it reads, the feed it read, and
/// when its relaunch may come, which waits until the lifecycle lets Obelisk
/// quit.
@MainActor
private final class UpdaterDelegate: NSObject, SPUUpdaterDelegate {
    var quit: (_ install: @escaping () -> Void) -> Void = { $0() }
    var takesBetaReleases: () -> Bool = { false }
    /// Whether Sparkle got an update it installs as Obelisk quits: it reads no
    /// feed again until then.
    private(set) var updateWaiting = false
    var loaded: ([FeedRelease]) -> Void = { _ in }
    var foundUpdate: () -> Void = {}

    func allowedChannels(for updater: SPUUpdater) -> Set<String> {
        takesBetaReleases() ? [ReleaseReturn.betaChannel] : []
    }

    func updater(_ updater: SPUUpdater, didFinishLoading appcast: SUAppcast) {
        loaded(appcast.items.compactMap(FeedRelease.init))
    }

    func updater(_ updater: SPUUpdater, didFindValidUpdate item: SUAppcastItem) {
        foundUpdate()
    }

    func updater(_ updater: SPUUpdater, didExtractUpdate item: SUAppcastItem) {
        updateWaiting = true
    }

    func updater(_ updater: SPUUpdater, willInstallUpdateOnQuit item: SUAppcastItem,
                 immediateInstallationBlock immediateInstallHandler: @escaping () -> Void) -> Bool {
        updateWaiting = true
        return false
    }

    func updater(_ updater: SPUUpdater, userDidMake choice: SPUUserUpdateChoice, forUpdate updateItem: SUAppcastItem,
                 state: SPUUserUpdateState) {
        if choice == .skip { updateWaiting = false }
    }

    func updater(_ updater: SPUUpdater, didAbortWithError error: any Error) {
        updateWaiting = false
    }

    func updater(_ updater: SPUUpdater, shouldPostponeRelaunchForUpdate item: SUAppcastItem,
                 untilInvokingBlock installHandler: @escaping () -> Void) -> Bool {
        quit(installHandler)
        return true
    }
}
