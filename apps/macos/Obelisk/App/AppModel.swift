import Observation
import SwiftUI

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
    /// The storefronts whose friends list was open when `followFriends` last looked.
    @ObservationIgnored private var askedFriends: Set<LibrarySection> = []
    /// The game whose page Home opened, while its storefront still shows it.
    var homeGame: HomeGame.ID?
    /// The storefronts whose saved session is still restoring since launch,
    /// so Home tells a library on its way from a signed-out one.
    private var restoringStorefronts = Set(LibrarySection.storefronts)
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
        self.downloads = downloads ?? DownloadsModel(defaults: defaults)
        self.gamesFolder = gamesFolder ?? GamesFolderModel(library: directory)
        let performance = performance ?? PerformanceReports(isEnabled: sharesGamePerformance)
        performance.isEnabled = sharesGamePerformance
        self.performance = performance
        self.estimates = estimates ?? PerformanceEstimates()
        self.updates = updates ?? AppUpdates()
        self.downloads.progress = { [weak self] item in self?.downloadTransfer(item) }
        applyAutomaticUpdates()
        reportPerformance()
        followFriends()
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
        account(item.storefront.section)?.transfer(named: item.game)
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
        let section = item.storefront.section
        guard account(section)?.openGame(named: item.game) == true else { return }
        selection = section
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
        account(section)?.closeGamePage()
        if homeGame?.section == section { homeGame = nil }
    }

    /// The installed games outside the games folder: their move was stopped,
    /// or they were installed meanwhile. Two accounts' installations of one
    /// folder count once.
    var gamesOutsideFolder: Int {
        let directories = accounts.flatMap(\.installedDirectories)
        return Set(directories.filter { !gamesFolder.holds($0) }).count
    }

    /// The games whose move into the games folder waits or runs.
    var gamesMoving: Int {
        downloads.waiting.count(where: { $0.kind == .move }) + (downloads.running?.kind == .move ? 1 : 0)
    }

    private func applyAutomaticUpdates() {
        for account in accounts { account.setUpdatesAutomatically(updatesGamesAutomatically) }
    }

    // MARK: Accounts

    /// The account of a storefront's section, which Home and Settings do not
    /// have. The one exhaustive switch over the storefronts: adding a section
    /// fails to compile here.
    func account(_ section: LibrarySection) -> (any StorefrontAccount)? {
        switch section {
        case .steam: steam
        case .epic: epic
        case .gog: gog
        case .home, .settings: nil
        }
    }

    /// Every storefront's account in sidebar order, which orders prompts,
    /// friends, refreshes, restorations and the lifecycle's stops.
    var accounts: [any StorefrontAccount] {
        LibrarySection.storefronts.compactMap { account($0) }
    }

    /// How a storefront's tile, row and sign-in card tell its account: its
    /// own standing, but restoring as long as the launch restoration lists it.
    func standing(of section: LibrarySection) -> AccountStanding {
        guard let standing = account(section)?.standing else { return .signedOut(connecting: false) }
        if case .signedIn = standing { return standing }
        return restoringStorefronts.contains(section) ? .restoring : standing
    }

    /// What Home shows: the libraries, a blank while saved sessions restore,
    /// or its sign-in page.
    enum HomeContent: Equatable { case libraries, restoring, signIn }

    var homeContent: HomeContent {
        if accounts.contains(where: { $0.showsLibrary }) { return .libraries }
        return restoringStorefronts.isEmpty ? .signIn : .restoring
    }

    // MARK: Prompts

    /// The newer Voile release, Cloud or Rosetta question shown over the whole
    /// app, whichever tab is open. One sheet at a time, in a fixed order: the
    /// storefronts' in the sidebar's, each one's Voile, Rosetta, Cloud.
    var presentedPrompt: AccountPrompt? {
        for account in accounts {
            if let kind = account.pendingPrompt { return AccountPrompt(section: account.section, kind: kind) }
        }
        return nil
    }

    /// The sheet of `prompt`, asked by its storefront's account.
    func promptSheet(for prompt: AccountPrompt) -> AnyView {
        account(prompt.section)?.promptSheet(prompt.kind) ?? AnyView(EmptyView())
    }

    // MARK: Friends

    /// The account whose friends `storeID` names, the one place a friends
    /// identity meets a section.
    private func account(storeID: String?) -> (any StorefrontAccount)? {
        accounts.first { $0.section.rawValue == storeID }
    }

    var presentedFriends: FriendsPresentationState? {
        guard let activeFriendsAccount else { return nil }
        let state = account(storeID: activeFriendsAccount.storeID)?.friends
        return state?.account == activeFriendsAccount ? state : nil
    }

    /// Shows `account`'s friends, which closes the other storefronts' lists.
    private func presentFriends(of account: any StorefrontAccount) {
        guard let state = account.friends else { return }
        activeFriendsAccount = state.account
        for other in accounts where other !== account { other.closeFriends() }
    }

    /// Closes the friends when they are `account`'s.
    private func closeFriends(ifActive account: any StorefrontAccount) {
        if activeFriendsAccount?.storeID == account.section.rawValue { closeFriends() }
    }

    func closeFriends() {
        activeFriendsAccount = nil
        for account in accounts { account.closeFriends() }
    }

    func retryPresentedFriends() {
        account(storeID: activeFriendsAccount?.storeID)?.retryFriends()
    }

    func signInPresentedFriends() {
        account(storeID: activeFriendsAccount?.storeID)?.signOut()
        closeFriends()
    }

    /// Follows the accounts' friends lists as their menus open and close them:
    /// the list just opened shows and the others close, and the friends of an
    /// account that went away close. It runs whether or not a window shows.
    private func followFriends() {
        let followed: [any StorefrontAccount] = withObservationTracking {
            // The active account's state: it changes, or goes, with the account.
            _ = presentedFriends
            // Every flag is read, so any of them wakes it.
            let all = accounts
            for account in all { _ = account.isFriendsPresented }
            return all
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in self?.followFriends() }
        }
        for account in followed {
            // Read again: presenting one account closed the others' lists.
            let presented = account.isFriendsPresented
            guard presented != askedFriends.contains(account.section) else { continue }
            if presented {
                askedFriends.insert(account.section)
                presentFriends(of: account)
            } else {
                askedFriends.remove(account.section)
                closeFriends(ifActive: account)
            }
        }
        if activeFriendsAccount != nil && presentedFriends == nil { closeFriends() }
    }

    // MARK: Restoring and refreshing

    /// Restores every storefront's saved session, so Home shows each library
    /// from launch. A signed-out store offers its sign-in on its own tab.
    func restoreStorefronts() async {
        let restorations = accounts.map { account in
            Task {
                await account.restoreSavedSession()
                restoringStorefronts.remove(account.section)
            }
        }
        for restoration in restorations { await restoration.value }
    }

    /// Reloads the open storefront's library; Home's, every storefront's.
    func refresh() {
        switch selection {
        case .home: for account in accounts { account.refresh() }
        case .settings: break
        default: account(selection)?.refresh()
        }
    }

    // MARK: Game sessions

    /// Whether any storefront has a Game session, one still preparing included.
    var runsGame: Bool { accounts.contains { $0.hasGameSession } }

    /// Ends the running game of every storefront, one storefront after the other.
    func stopGames() async {
        for account in accounts { await account.stopGames() }
    }
}
