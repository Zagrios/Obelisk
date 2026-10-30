import Observation
import SwiftUI

/// A storefront whose account shows its friends: Steam and Epic.
@MainActor
protocol FriendsStorefront: AnyObject {
    var friends: FriendsPresentationState? { get }
    func closeFriends()
    func retryFriends()
    func signOut()
}

extension SteamModel: FriendsStorefront {}
extension WebAccountModel: FriendsStorefront where Store == EpicGames {}

@MainActor
@Observable
final class AppModel {
    var selection: LibrarySection = .home {
        didSet { searchText = "" }
    }
    var searchText = ""
    let libraryDirectory: String
    let steam: SteamModel
    let epic: EpicModel
    let gog: GogModel
    /// The in-game overlay, over whichever storefront's game runs.
    let overlay: GameOverlayController
    /// Every storefront's Transfers, in their one queue.
    let downloads: DownloadsModel
    /// Where Obelisk installs games, which the player may change.
    let gamesFolder: GamesFolderModel
    /// The storefront's store, open over the whole window.
    var presentedStore: StorePresentation?
    /// The bug report being written, open over the whole window.
    var bugReport: BugReportModel?
    private(set) var activeFriendsAccount: FriendsAccountIdentity?
    /// The game whose page Home opened, while its storefront still shows it.
    var homeGame: HomeGame.ID?
    /// The storefronts whose saved session is still restoring since launch,
    /// so Home tells a library on its way from a signed-out one.
    private(set) var restoringStorefronts = Set(LibrarySection.storefronts)
    var appearance: AppAppearance {
        didSet { defaults.set(appearance.rawValue, forKey: "appearance") }
    }
    /// The language Obelisk shows from its next start.
    var language: AppLanguage {
        didSet { language.save(in: defaults) }
    }
    /// Whether Obelisk shows another language than the chosen one until it
    /// restarts.
    var languageNeedsRestart: Bool {
        language.resolved(system: AppLanguage.systemLanguages) != L10n.languageCode
    }
    /// Quits Obelisk and opens it again; its lifecycle provides it.
    @ObservationIgnored var restart: () -> Void = {}
    /// Whether every storefront installs a game's newer build as soon as it
    /// is published, rather than at the game's next launch.
    var updatesGamesAutomatically: Bool {
        didSet {
            defaults.set(updatesGamesAutomatically, forKey: Self.automaticUpdatesKey)
            applyAutomaticUpdates()
        }
    }
    static let automaticUpdatesKey = "obelisk.games.updateAutomatically"
    /// Whether Obelisk reports each game's frame rate and how it ended to
    /// getobelisk.app, which shows how games run on each Mac.
    var sharesGamePerformance: Bool {
        didSet {
            defaults.set(sharesGamePerformance, forKey: Self.sharePerformanceKey)
            performance.isEnabled = sharesGamePerformance
        }
    }
    static let sharePerformanceKey = "obelisk.games.sharePerformance"
    @ObservationIgnored let performance: PerformanceReports
    /// How each game runs on this Mac's chip, as other players measured it.
    let estimates: PerformanceEstimates
    /// Obelisk's own new versions.
    let updates: AppUpdates

    /// The last library of every storefront account.
    @ObservationIgnored let libraryCache: LibraryCache
    @ObservationIgnored private let defaults: UserDefaults

    /// The test host's models register no Shift-Tab, open no overlay and post
    /// no notification.
    init(directory: URL = AppIdentity.libraryDirectory, defaults: UserDefaults = .standard,
         defaultsDomain: String = Bundle.main.bundleIdentifier ?? AppIdentity.storageIdentifier, steam: SteamModel? = nil,
         epic: EpicModel? = nil, gog: GogModel? = nil, downloads: DownloadsModel? = nil,
         gamesFolder: GamesFolderModel? = nil,
         performance: PerformanceReports? = nil, estimates: PerformanceEstimates? = nil, updates: AppUpdates? = nil,
         overlay: GameOverlayEnvironment = ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] == nil
            ? .live : .inert) {
        libraryDirectory = directory.path
        self.defaults = defaults
        let steamLimit = max(0, defaults.integer(forKey: "steamDownloadLimitMbps"))
        setSteamDownloadLimitMbps(value: UInt64(min(1000, steamLimit)))
        appearance = AppAppearance(rawValue: defaults.string(forKey: "appearance") ?? "system") ?? .system
        language = AppLanguage(defaults: defaults, domain: defaultsDomain)
        updatesGamesAutomatically = defaults.object(forKey: Self.automaticUpdatesKey) as? Bool ?? true
        let sharesGamePerformance = defaults.object(forKey: Self.sharePerformanceKey) as? Bool ?? true
        self.sharesGamePerformance = sharesGamePerformance
        // Every storefront's library shares one cache, as every cache of the
        // directory does, `LaunchHistory.live`'s included.
        libraryCache = LibraryCache(directory: directory.path)
        self.steam = steam ?? SteamModel(directory: directory, libraryCache: libraryCache)
        self.epic = epic ?? EpicModel(directory: directory, libraryCache: libraryCache)
        self.gog = gog ?? GogModel(directory: directory, libraryCache: libraryCache)
        self.overlay = GameOverlayController(steam: self.steam, epic: self.epic, gog: self.gog, environment: overlay)
        self.steam.overlay = self.overlay
        self.steam.notifier = overlay.notifier
        self.downloads = downloads ?? DownloadsModel()
        self.gamesFolder = gamesFolder ?? GamesFolderModel(library: directory)
        let performance = performance ?? PerformanceReports(isEnabled: sharesGamePerformance)
        performance.isEnabled = sharesGamePerformance
        self.performance = performance
        self.estimates = estimates ?? PerformanceEstimates()
        self.updates = updates ?? AppUpdates()
        self.downloads.progress = { [weak self] item in self?.downloadTransfer(item) }
        applyAutomaticUpdates()
        reportPerformance()
    }

    /// Each storefront's ended games go to the performance reports, with the
    /// title its library knows them by.
    private func reportPerformance() {
        report(steam.gameSessions) { [steam] in steam.gameName(appId: $0) }
        report(epic.gameSessions) { [epic] id in epic.library.game(id)?.title }
        report(gog.gameSessions) { [gog] id in gog.library.game(id)?.title }
    }

    private func report<Store: GameStorefront>(_ sessions: GameSessions<Store>,
                                               title: @escaping (Store.GameID) -> String?) {
        let performance = performance
        sessions.reportPlayed = { game, played in
            guard let id = Store.storeGameID(game) else { return }
            performance.report(played, of: id, titled: title(game), in: Store.storefront)
        }
    }

    /// Opens the bug report sheet on `subject`.
    func reportBug(_ subject: BugReportSubject) {
        bugReport = BugReportModel(subject: subject, sources: .live)
    }

    /// A queued Transfer's progress, from its storefront's installations.
    func downloadTransfer(_ item: DownloadItem) -> GameTransfer? {
        switch item.storefront {
        case .steam: steam.installations.transfer(named: item.game)
        case .epic: epic.installations.transfer(named: item.game)
        case .gog: gog.installations.transfer(named: item.game)
        }
    }

    /// What the download queue shows of a queued Transfer's game.
    func downloadGame(_ item: DownloadItem) -> DownloadGame {
        switch item.storefront {
        case .steam: steam.downloadGame(item, cover: SteamView.cover, artwork: SteamView.heroArtwork)
        case .epic: epic.downloadGame(item, cover: EpicView.cover, artwork: EpicView.heroArtwork)
        case .gog: gog.downloadGame(item, cover: GogView.cover, artwork: GogView.heroArtwork)
        }
    }

    /// Opens a queued Transfer's game page, in its storefront's library.
    func openDownload(_ item: DownloadItem) {
        downloads.isPresented = false
        switch item.storefront {
        case .steam: open(item, in: steam)
        case .epic: open(item, in: epic)
        case .gog: open(item, in: gog)
        }
    }

    private func open<Model: LibraryStorefront>(_ item: DownloadItem, in model: Model) {
        guard let id = Model.Store.game(item.game), let game = model.library.game(id) else { return }
        selection = item.storefront.section
        model.open(game)
    }

    /// Shows the section the sidebar picked at its first page: Home or the
    /// library, not the game page left open in it.
    /// The sidebar asks twice for one click, so asking again changes nothing.
    func show(_ section: LibrarySection) {
        if selection != section { selection = section }
        if section == .home { closeHomeGame() } else { closeGame(of: section) }
    }

    /// Returns Home from the game page it opened.
    func closeHomeGame() {
        if let section = homeGame?.section { closeGame(of: section) }
    }

    /// Returns a storefront from its game page, which Home shows too when it
    /// opened it.
    private func closeGame(of section: LibrarySection) {
        switch section {
        case .steam: closeGame(in: steam)
        case .epic: closeGame(in: epic)
        case .gog: closeGame(in: gog)
        case .home, .settings: return
        }
        if homeGame?.section == section { homeGame = nil }
    }

    private func closeGame<Model: LibraryStorefront>(in model: Model) {
        if model.selectedGame != nil { model.closeGame() }
    }

    /// The installed games outside the games folder: their move was stopped,
    /// or they were installed meanwhile. Two accounts' installations of one
    /// folder count once.
    var gamesOutsideFolder: Int {
        let directories = steam.installations.receipts.map(\.directory)
            + epic.installations.receipts.map(\.directory)
            + gog.installations.receipts.map(\.directory)
        return Set(directories.filter { !gamesFolder.holds($0) }).count
    }

    /// The games whose move into the games folder waits or runs.
    var gamesMoving: Int {
        downloads.waiting.count(where: { $0.kind == .move }) + (downloads.running?.kind == .move ? 1 : 0)
    }

    private func applyAutomaticUpdates() {
        steam.installations.updatesAutomatically = updatesGamesAutomatically
        epic.installations.updatesAutomatically = updatesGamesAutomatically
        gog.installations.updatesAutomatically = updatesGamesAutomatically
    }

    /// The newer Voile release, Cloud or Rosetta question shown over the whole
    /// app, whichever tab is open. One sheet at a time, in a fixed order.
    enum StorefrontPrompt: Equatable {
        case steamVoile, steamRosetta, steamCloud, epicVoile, epicRosetta, epicCloud, gogVoile, gogRosetta
    }

    var presentedPrompt: StorefrontPrompt? {
        if steam.gameSessions.voileUpdate != nil { return .steamVoile }
        if steam.rosetta.prompt != nil { return .steamRosetta }
        if steam.cloud.question != nil { return .steamCloud }
        if epic.gameSessions.voileUpdate != nil { return .epicVoile }
        if epic.rosetta.prompt != nil { return .epicRosetta }
        if epic.cloud.question != nil { return .epicCloud }
        if gog.gameSessions.voileUpdate != nil { return .gogVoile }
        if gog.rosetta.prompt != nil { return .gogRosetta }
        return nil
    }

    /// The storefront whose friends `storeID` names; GOG shows none.
    private func friendsModel(storeID: String?) -> (any FriendsStorefront)? {
        switch storeID {
        case "steam": steam
        case "epic": epic
        default: nil
        }
    }

    var presentedFriends: FriendsPresentationState? {
        guard let activeFriendsAccount else { return nil }
        let state = friendsModel(storeID: activeFriendsAccount.storeID)?.friends
        return state?.account == activeFriendsAccount ? state : nil
    }

    func presentFriends(storeID: String) {
        guard let model = friendsModel(storeID: storeID), let state = model.friends else { return }
        activeFriendsAccount = state.account
        for other in [steam, epic] as [any FriendsStorefront] where other !== model { other.closeFriends() }
    }

    func closeFriends(storeID: String? = nil) {
        if let storeID, activeFriendsAccount?.storeID != storeID { return }
        activeFriendsAccount = nil
        steam.closeFriends()
        epic.closeFriends()
    }

    func validateFriendsAccount() {
        if activeFriendsAccount != nil && presentedFriends == nil { closeFriends() }
    }

    func retryPresentedFriends() {
        friendsModel(storeID: activeFriendsAccount?.storeID)?.retryFriends()
    }

    func signInPresentedFriends() {
        friendsModel(storeID: activeFriendsAccount?.storeID)?.signOut()
        closeFriends()
    }

    /// Restores every storefront's saved session, so Home shows each library
    /// from launch. A signed-out store offers its sign-in on its own tab.
    func restoreStorefronts() async {
        let restorations = [
            Task { await steam.start(signIn: false); restoringStorefronts.remove(.steam) },
            Task { await epic.start(); restoringStorefronts.remove(.epic) },
            Task { await gog.start(); restoringStorefronts.remove(.gog) },
        ]
        for restoration in restorations { await restoration.value }
    }

    /// Reloads the open storefront's library; Home's, every storefront's.
    func refresh() {
        switch selection {
        case .home:
            steam.refresh()
            epic.refresh()
            gog.refresh()
        case .steam: steam.refresh()
        case .epic: epic.refresh()
        case .gog: gog.refresh()
        case .settings: break
        }
    }
}
