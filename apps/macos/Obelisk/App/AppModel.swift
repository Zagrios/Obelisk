import Observation
import SwiftUI

@MainActor
@Observable
final class AppModel {
    var selection: LibraryTab = .home {
        didSet {
            searchText = ""
            // A slot the player left without signing in goes.
            if selection != oldValue { removeVacantAccounts() }
        }
    }
    var searchText = ""
    let libraryDirectory: String
    /// Each storefront's accounts, in the order the player added them.
    let steam: AccountList<SteamModel>
    let epic: AccountList<EpicModel>
    let gog: AccountList<GogModel>
    /// The in-game overlay, over whichever storefront's game runs.
    let overlay: GameOverlayController
    /// Tells every Steam account's invitations while no game shows the overlay.
    @ObservationIgnored private let inviteNotifier: (any SteamInviteNotifying)?
    /// Every storefront's Transfers, in their one queue.
    let downloads: DownloadsModel
    /// Where Obelisk installs games, which the player may change.
    let gamesFolder: GamesFolderModel
    /// The storefront's store, open over the whole window.
    var presentedStore: StorePresentation?
    /// The bug report being written, open over the whole window.
    var bugReport: BugReportModel?
    private(set) var activeFriendsAccount: FriendsAccountIdentity?
    /// The accounts whose friends list was open when `followFriends` last looked.
    @ObservationIgnored private var askedFriends: Set<AccountSlot> = []
    /// The game whose page Home opened, while its account still shows it.
    var homeGame: HomeGame.ID?
    /// The accounts whose saved session is still restoring since launch, so
    /// Home tells a library on its way from a signed-out one.
    private var restoringAccounts: Set<AccountSlot>
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
    /// Where the Mac's heat is noted while a game runs.
    @ObservationIgnored private let thermal: ThermalLog
    /// How each game runs on this Mac's chip, as other players measured it.
    let estimates: PerformanceEstimates
    /// Obelisk's own new versions.
    let updates: AppUpdates
    /// The ways to support Obelisk, which it shows by itself once.
    let support: SupportModel

    /// The last library of every storefront account.
    @ObservationIgnored let libraryCache: LibraryCache
    @ObservationIgnored private let defaults: UserDefaults

    /// The test host's models register no overlay shortcut, open no overlay
    /// and post no notification. `steam`, `epic` and `gog` stand for each
    /// storefront's first account, and `makeEpic` makes Epic's account of
    /// every slot `epic` does not stand for: tests pass the ones they drive.
    init(directory: URL = AppIdentity.libraryDirectory, defaults: UserDefaults = .standard,
         defaultsDomain: String = Bundle.main.bundleIdentifier ?? AppIdentity.storageIdentifier, steam: SteamModel? = nil,
         epic: EpicModel? = nil, gog: GogModel? = nil,
         makeEpic: (@MainActor (AccountSlot) -> EpicModel)? = nil,
         downloads: DownloadsModel? = nil,
         gamesFolder: GamesFolderModel? = nil,
         performance: PerformanceReports? = nil, estimates: PerformanceEstimates? = nil, updates: AppUpdates? = nil,
         support: SupportModel? = nil, thermal: ThermalLog = .shared,
         overlay: GameOverlayEnvironment = AppIdentity.isTestHost ? .inert : .live) {
        libraryDirectory = directory.path
        self.defaults = defaults
        appearance = AppAppearance(rawValue: defaults.string(forKey: "appearance") ?? "system") ?? .system
        language = AppLanguage(defaults: defaults, domain: defaultsDomain)
        updatesGamesAutomatically = defaults.object(forKey: Self.automaticUpdatesKey) as? Bool ?? true
        let sharesGamePerformance = defaults.object(forKey: Self.sharePerformanceKey) as? Bool ?? true
        self.sharesGamePerformance = sharesGamePerformance
        // Every storefront's library shares one cache, as every cache of the
        // directory does, `LaunchHistory.live`'s included.
        let libraryCache = LibraryCache(directory: directory.path)
        self.libraryCache = libraryCache
        // A storefront's accounts share its Game sessions, as one of its
        // games runs at a time, and how the player shows to their friends.
        let steamSessions = steam?.gameSessions ?? .shared
        let steamStatus = steam?.onlineStatus
            ?? OnlineStatusChoice(storeID: LibrarySection.steam.rawValue, defaults: defaults)
        self.steam = AccountList(.steam, first: steam, defaults: defaults) { slot in
            SteamModel(directory: directory, slot: slot, gameSessions: steamSessions, libraryCache: libraryCache,
                       onlineStatus: steamStatus)
        }
        let epicSessions = epic?.gameSessions ?? EpicGames.sessions
        let epicStatus = epic?.onlineStatus ?? OnlineStatusChoice(storeID: EpicGames.storeID, defaults: defaults)
        self.epic = AccountList(.epic, first: epic, defaults: defaults, make: makeEpic ?? { slot in
            EpicModel(directory: directory, slot: slot, gameSessions: epicSessions, libraryCache: libraryCache,
                      onlineStatus: epicStatus)
        })
        let gogSessions = gog?.gameSessions ?? GogGames.sessions
        self.gog = AccountList(.gog, first: gog, defaults: defaults) { slot in
            GogModel(directory: directory, slot: slot, gameSessions: gogSessions, libraryCache: libraryCache)
        }
        restoringAccounts = Set(self.steam.accounts.map(\.slot) + self.epic.accounts.map(\.slot)
            + self.gog.accounts.map(\.slot))
        self.overlay = GameOverlayController(steam: self.steam, epic: self.epic, gog: self.gog, defaults: defaults,
                                             environment: overlay)
        inviteNotifier = overlay.notifier
        self.downloads = downloads ?? DownloadsModel(defaults: defaults)
        self.gamesFolder = gamesFolder ?? GamesFolderModel(library: directory)
        let performance = performance ?? PerformanceReports(isEnabled: sharesGamePerformance)
        performance.isEnabled = sharesGamePerformance
        self.performance = performance
        self.thermal = thermal
        self.estimates = estimates ?? PerformanceEstimates()
        self.updates = updates ?? AppUpdates(defaults: defaults)
        self.support = support ?? SupportModel(defaults: defaults)
        self.downloads.progress = { [weak self] item in self?.downloadTransfer(item) }
        for section in LibrarySection.storefronts {
            accounts(of: section)?.onRemoved = { [weak self] in self?.slotWent($0) }
        }
        for account in accounts { adopt(account) }
        inviteNotifier?.answer = { [weak self] in self?.answerInviteNotice($0, $1) }
        reportPerformance()
        observeGameRunning()
        followFriends()
    }

    /// Each storefront's ended games go to the performance reports, with the
    /// title its library knows them by.
    private func reportPerformance() {
        report(steam.gameSessions) { [steam] id in steam.accounts.lazy.compactMap { $0.gameName(appId: id) }.first }
        report(epic.gameSessions) { [epic] id in epic.accounts.lazy.compactMap { $0.library.game(id)?.title }.first }
        report(gog.gameSessions) { [gog] id in gog.accounts.lazy.compactMap { $0.library.game(id)?.title }.first }
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

    /// The account a queued Transfer is of: the one signed in as the account
    /// the queue names.
    private func account(of item: DownloadItem) -> (any StorefrontAccount)? {
        accounts(of: item.storefront.section)?.account(signedInAs: item.account)
    }

    /// A queued Transfer's progress, from its account's installations.
    func downloadTransfer(_ item: DownloadItem) -> GameTransfer? {
        account(of: item)?.transfer(named: item.game)
    }

    /// What the download queue shows of a queued Transfer's game: as its
    /// account's library knows it, else by the name it was queued under. Its
    /// storefront is named with its account once it holds several.
    func downloadGame(_ item: DownloadItem) -> DownloadGame {
        let known: DownloadGame? = switch item.storefront {
        case .steam:
            steam.account(signedInAs: item.account).map { $0.downloadGame(item, cover: $0.cover, artwork: $0.heroArtwork) }
        case .epic:
            epic.account(signedInAs: item.account)?.downloadGame(item, cover: EpicView.cover, artwork: EpicView.heroArtwork)
        case .gog:
            gog.account(signedInAs: item.account)?.downloadGame(item, cover: GogView.cover, artwork: GogView.heroArtwork)
        }
        var game = known ?? DownloadGame(title: item.name, cover: nil, artwork: nil)
        game.source = account(of: item).map(title(of:)) ?? item.storefront.section.title
        return game
    }

    /// Opens a queued Transfer's game page, in its account's library.
    func openDownload(_ item: DownloadItem) {
        downloads.isPresented = false
        guard let account = account(of: item), account.openGame(named: item.game) else { return }
        selection = account.tab
    }

    /// Shows the tab the sidebar picked at its first page: Home or the
    /// library, not the game page left open in it.
    /// The sidebar asks twice for one click, so asking again changes nothing.
    func show(_ tab: LibraryTab) {
        if selection != tab { selection = tab }
        switch tab {
        case .home: closeHomeGame()
        case .account(let slot): closeGame(of: slot)
        case .settings: break
        }
    }

    /// Returns Home from the game page it opened.
    func closeHomeGame() {
        if let slot = homeGame?.slot { closeGame(of: slot) }
    }

    /// Returns an account from its game page, which Home shows too when it
    /// opened it.
    private func closeGame(of slot: AccountSlot) {
        account(slot)?.closeGamePage()
        if homeGame?.slot == slot { homeGame = nil }
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

    /// The accounts of a storefront's section, which Home and Settings do
    /// not have. The one exhaustive switch over the storefronts: adding a
    /// section fails to compile here.
    func accounts(of section: LibrarySection) -> (any AccountListing)? {
        switch section {
        case .steam: steam
        case .epic: epic
        case .gog: gog
        case .home, .settings: nil
        }
    }

    /// The account of `slot`; nil once its slot went.
    func account(_ slot: AccountSlot) -> (any StorefrontAccount)? {
        accounts(of: slot.section)?[slot]
    }

    /// Every storefront's accounts in sidebar order, which orders prompts,
    /// friends, refreshes, restorations and the lifecycle's stops.
    var accounts: [any StorefrontAccount] {
        LibrarySection.storefronts.flatMap { accounts(of: $0)?.accounts ?? [] }
    }

    /// How the sidebar names an account's library: its storefront, and its
    /// account too once the storefront holds several. An account shows the
    /// name it last had while its session restores.
    func title(of account: any StorefrontAccount) -> String {
        title(of: account, store: account.section.title)
    }

    /// The storefront of `slot` as Home names it beside a game, by its full
    /// name: with its account too, once the storefront holds several.
    func storeTitle(of slot: AccountSlot) -> String {
        let store = slot.section.storeTitle
        return account(slot).map { title(of: $0, store: store) } ?? store
    }

    /// `store`, a name of `account`'s storefront, with the account's once the
    /// storefront holds several.
    private func title(of account: any StorefrontAccount, store: String) -> String {
        guard let listing = accounts(of: account.section), listing.accounts.count > 1,
              let name = account.accountName ?? listing.rememberedName(of: account.slot), !name.isEmpty
        else { return store }
        return L10n.string("ui.account.store_named", store, name)
    }

    /// Shows the sign-in of a slot of `section` for another account: a new
    /// one, or the one that already holds nothing.
    func addAccount(to section: LibrarySection) {
        guard let account = accounts(of: section)?.add() else { return }
        adopt(account)
        selection = account.tab
        // Nothing is saved for a new slot: its restoration ends at once, and
        // tells that it holds nothing should the player leave it.
        Task { await account.restoreSavedSession() }
    }

    /// The player signs `account` out. Its slot goes with it, unless it is
    /// its storefront's last, which then offers its sign-in.
    func signOut(_ account: any StorefrontAccount) {
        accounts(of: account.section)?.signOut(account.slot)
    }

    /// What every account gets from the app as it joins it.
    private func adopt(_ account: any StorefrontAccount) {
        account.setUpdatesAutomatically(updatesGamesAutomatically)
        guard let steam = account as? SteamModel else { return }
        steam.overlay = overlay
        steam.notifier = inviteNotifier
    }

    /// A slot went: nothing of the app names it any more, and its
    /// storefront's first account shows instead when its tab was open.
    private func slotWent(_ slot: AccountSlot) {
        if homeGame?.slot == slot { homeGame = nil }
        restoringAccounts.remove(slot)
        askedFriends.remove(slot)
        if selection == .account(slot), let next = accounts(of: slot.section)?.accounts.first { selection = next.tab }
    }

    /// Takes away the slots that hold nothing, but the one whose tab shows.
    private func removeVacantAccounts() {
        for section in LibrarySection.storefronts {
            accounts(of: section)?.removeVacant(keeping: selection.slot)
        }
    }

    /// The player answered an invitation's notice: the Steam account that
    /// lists the invitation answers, else Steam's first, as a notice
    /// outlives its invitation.
    private func answerInviteNotice(_ answer: SteamInviteAnswer, _ invite: SteamReceivedInvite) {
        let account = steam.accounts.first { $0.invitations.listed(invite) != nil } ?? steam.first
        account.answerNotice(answer, invite)
    }

    /// How an account's tile, row and sign-in card tell it: its own
    /// standing, but restoring as long as the launch restoration lists it.
    func standing(of slot: AccountSlot) -> AccountStanding {
        guard let standing = account(slot)?.standing else { return .signedOut(connecting: false) }
        if case .signedIn = standing { return standing }
        return restoringAccounts.contains(slot) ? .restoring : standing
    }

    /// What Home shows: the libraries, a blank while saved sessions restore,
    /// or its sign-in page.
    enum HomeContent: Equatable { case libraries, restoring, signIn }

    var homeContent: HomeContent {
        if accounts.contains(where: { $0.showsLibrary }) { return .libraries }
        return restoringAccounts.isEmpty ? .signIn : .restoring
    }

    // MARK: Prompts

    /// The newer Voile release, Cloud or Rosetta question shown over the whole
    /// app, whichever tab is open. One sheet at a time, in a fixed order: the
    /// accounts' in the sidebar's, each one's Voile, Rosetta, Cloud.
    var presentedPrompt: AccountPrompt? {
        for account in accounts {
            if let kind = account.pendingPrompt { return AccountPrompt(slot: account.slot, kind: kind) }
        }
        return nil
    }

    /// The sheet of `prompt`, asked by its account.
    func promptSheet(for prompt: AccountPrompt) -> AnyView {
        account(prompt.slot)?.promptSheet(prompt.kind) ?? AnyView(EmptyView())
    }

    // MARK: Support

    /// Shows the ways to support Obelisk once it started for the third time,
    /// a moment after its window opened, to a player whose library shows. A
    /// start where something else is over the window by then, or a game runs,
    /// leaves it to the next one.
    func offerSupport(after delay: Duration = .seconds(2)) async {
        guard support.isDue else { return }
        try? await Task.sleep(for: delay)
        guard !Task.isCancelled, homeContent == .libraries, !isWindowTaken, !runsGame else { return }
        support.offer()
    }

    /// Whether a sheet or an alert is over a window of Obelisk, or the
    /// download queue is open. The windows tell: each view presents its own.
    private var isWindowTaken: Bool {
        downloads.isPresented || NSApp.windows.contains { $0.attachedSheet != nil }
    }

    // MARK: Friends

    /// The account whose friends `identity` names, the one place a friends
    /// identity meets an account.
    private func account(_ identity: FriendsAccountIdentity?) -> (any StorefrontAccount)? {
        guard let identity, let section = LibrarySection(rawValue: identity.storeID) else { return nil }
        return accounts(of: section)?.account(signedInAs: identity.accountID)
    }

    var presentedFriends: FriendsPresentationState? {
        guard let activeFriendsAccount else { return nil }
        let state = account(activeFriendsAccount)?.friends
        return state?.account == activeFriendsAccount ? state : nil
    }

    /// Steam's chat with the friends of `identity`, when it names a Steam account.
    func chat(of identity: FriendsAccountIdentity) -> SteamChatModel? {
        (account(identity) as? SteamModel)?.chat
    }

    /// Shows `account`'s friends, which closes the other accounts' lists.
    private func presentFriends(of account: any StorefrontAccount) {
        guard let state = account.friends else { return }
        activeFriendsAccount = state.account
        for other in accounts where other !== account { other.closeFriends() }
    }

    /// Closes the friends when they are `account`'s.
    private func closeFriends(ifActive account: any StorefrontAccount) {
        if self.account(activeFriendsAccount) === account { closeFriends() }
    }

    func closeFriends() {
        activeFriendsAccount = nil
        for account in accounts { account.closeFriends() }
    }

    func retryPresentedFriends() {
        account(activeFriendsAccount)?.retryFriends()
    }

    /// Signs the account of the friends shown out, to sign in again: its
    /// slot stays, and shows its sign-in.
    func signInPresentedFriends() {
        account(activeFriendsAccount)?.signOut()
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
            guard presented != askedFriends.contains(account.slot) else { continue }
            if presented {
                askedFriends.insert(account.slot)
                presentFriends(of: account)
            } else {
                askedFriends.remove(account.slot)
                closeFriends(ifActive: account)
            }
        }
        if activeFriendsAccount != nil && presentedFriends == nil { closeFriends() }
    }

    // MARK: Restoring and refreshing

    /// Restores every account's saved session, so Home shows each library
    /// from launch. A signed-out store offers its sign-in on its own tab,
    /// and a slot left with nothing to restore goes.
    func restoreStorefronts() async {
        let restorations = accounts.map { account in
            Task {
                await account.restoreSavedSession()
                restoringAccounts.remove(account.slot)
            }
        }
        for restoration in restorations { await restoration.value }
        removeVacantAccounts()
    }

    /// Reloads the open account's library; Home's, every account's.
    func refresh() {
        switch selection {
        case .home: for account in accounts { account.refresh() }
        case .settings: break
        case .account(let slot): account(slot)?.refresh()
        }
    }

    // MARK: Game sessions

    /// Whether any storefront has a Game session, one still preparing included.
    var runsGame: Bool { accounts.contains { $0.hasGameSession } }

    /// Tells the thermal log whether a game runs, as it starts and as it exits.
    private func observeGameRunning() {
        let running = withObservationTracking {
            steam.gameSessions.active?.running == true || epic.gameSessions.active?.running == true
                || gog.gameSessions.active?.running == true
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in self?.observeGameRunning() }
        }
        thermal.follow(running: running)
    }

    /// Ends the running game of every storefront, one storefront after the other.
    func stopGames() async {
        await steam.gameSessions.stop()
        await epic.gameSessions.stop()
        await gog.gameSessions.stop()
    }
}
