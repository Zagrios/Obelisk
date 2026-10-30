import AppKit
import Observation

/// What the in-game overlay shows. The player opens it on `.home`; a Steam
/// game asks for the others through its own overlay calls, as it would ask
/// Steam's overlay.
enum OverlayRequest: Equatable {
    case home
    case friends
    /// The game's invite dialog: the friends list picks friends into its target.
    case invite(SteamInviteOffer)
    case achievements
    /// A Steam user: their conversation, or their profile when they are no friend.
    case user(steamId: UInt64)
    case webPage(URL)
    /// The in-game purchase a Steam game asks the player to authorize.
    case microTxn(orderId: UInt64)
}

/// The running game the overlay shows, whichever storefront launched it.
struct OverlaySession: Equatable {
    enum Game: Equatable {
        case steam(appId: UInt32)
        case epic(String)
        case gog(String)
    }

    let id: String
    let game: Game
    /// When this Obelisk saw the game start; a game recovered running has none.
    let startedAt: Date?

    var storeID: String {
        switch game {
        case .steam: "steam"
        case .epic: "epic"
        case .gog: "gog"
        }
    }
}

/// An application as the overlay tells the game's apart from the others.
struct OverlayApplication: Equatable, Sendable {
    let pid: pid_t
    let bundleIdentifier: String?
    let executable: URL?

    init(pid: pid_t, bundleIdentifier: String? = nil, executable: URL? = nil) {
        self.pid = pid
        self.bundleIdentifier = bundleIdentifier
        self.executable = executable
    }

    init(_ application: NSRunningApplication) {
        self.init(pid: application.processIdentifier, bundleIdentifier: application.bundleIdentifier,
                  executable: application.executableURL)
    }
}

/// The applications of the running game, as each engine runs it: the Linux
/// engine's windows belong to its `wsd` app, Rosetta Wine's to a Wine process
/// of Voile, and a Mac game is its own app in its installation.
struct GameApplicationMatch: Equatable {
    static let linuxEngineBundlePrefix = "com.zagrios.obelisk.game."
    /// Every folder whose executables are the game's, resolved, ending in "/".
    let folders: [String]

    /// A development runtime of `voile` may link elsewhere: each runtime
    /// counts as resolved.
    init(voile: String?, native: [String]) {
        let root = voile.map { URL(fileURLWithPath: $0) }
        let runtimes = root.flatMap {
            try? FileManager.default.contentsOfDirectory(at: $0, includingPropertiesForKeys: nil)
        } ?? []
        folders = ([root].compactMap { $0 } + runtimes + native.map(URL.init(fileURLWithPath:))).map {
            $0.resolvingSymlinksInPath().path + "/"
        }
    }

    func contains(_ application: OverlayApplication) -> Bool {
        if application.bundleIdentifier?.hasPrefix(Self.linuxEngineBundlePrefix) == true { return true }
        guard let executable = application.executable?.resolvingSymlinksInPath().path else { return false }
        return folders.contains { executable.hasPrefix($0) }
    }
}

/// How the overlay reaches the system; tests pass their own.
@MainActor
struct GameOverlayEnvironment {
    var hotKey: any OverlayHotKey
    var presenter: any OverlayPresenter
    /// Tells of an invitation over the game while the overlay is closed.
    var toaster: any OverlayToastPresenter
    /// Tells of an invitation while no game runs; the test host posts none.
    var notifier: (any SteamInviteNotifying)? = nil
    /// The application in front now.
    var frontmost: @MainActor () -> OverlayApplication?
    /// Where Voile's runtimes are installed.
    var voile: String
    /// Follows the applications macOS activates; tests tell them instead.
    var observesActivations: Bool

    static var live: GameOverlayEnvironment {
        GameOverlayEnvironment(
            hotKey: CarbonOverlayHotKey(), presenter: GameOverlayPanelPresenter(),
            toaster: OverlayToastPanelPresenter(), notifier: SteamInviteNotifier(),
            frontmost: { NSWorkspace.shared.frontmostApplication.map(OverlayApplication.init) },
            voile: ObeliskLocations.live.voile, observesActivations: true
        )
    }

    /// Registers no key and opens no window, for the test host's own models.
    static var inert: GameOverlayEnvironment {
        GameOverlayEnvironment(hotKey: InertOverlayHotKey(), presenter: InertOverlayPresenter(),
                               toaster: InertOverlayToastPresenter(), frontmost: { nil },
                               voile: "/nonexistent", observesActivations: false)
    }
}

/// Obelisk's in-game overlay, common to every storefront: Shift-Tab shows it
/// over the running game, with the friends of the game's storefront, its
/// achievements and web pages. It follows the running Game session, and
/// takes Shift-Tab only while the game, or the overlay itself, is in front.
@MainActor @Observable
final class GameOverlayController {
    private(set) var isPresented = false
    /// What shows; navigating inside the overlay replaces it.
    private(set) var request: OverlayRequest = .home
    private(set) var session: OverlaySession?
    /// The running game's achievements, once their page opened.
    private(set) var achievements: GameAchievementsModel?
    /// The friends column's keyboard state and conversation.
    let focus = FriendsPanelFocus()
    let avatars = FriendsAvatarCache()
    let steam: SteamModel
    let epic: EpicModel
    let gog: GogModel
    /// Whether the overlay shows. A Steam game learns it too, as it pauses
    /// or stops reading input while Steam's own overlay shows.
    @ObservationIgnored var onVisibilityChanged: (@MainActor (Bool) -> Void)?
    /// Brings the library window forward, such as to sign in again.
    @ObservationIgnored var showLibrary: @MainActor () -> Void = {}

    @ObservationIgnored private let environment: GameOverlayEnvironment
    @ObservationIgnored private var voileMatch: GameApplicationMatch?
    /// The game's application last in front, whose screen the overlay covers.
    @ObservationIgnored private var gameApplication: OverlayApplication?
    @ObservationIgnored private var gameIsFrontmost = false
    @ObservationIgnored private var suspendedForAppSwitch = false
    /// What was in front when the overlay opened, in front again once it closes.
    @ObservationIgnored private var returnTo: OverlayApplication?
    @ObservationIgnored private var activations: NSObjectProtocol?
    @ObservationIgnored private var capturedOverlayKeys: NSObjectProtocol?
    /// The player opened the overlay, rather than the game.
    @ObservationIgnored private var openedByPlayer = false
    /// What the Steam game was last told of the overlay, in order.
    @ObservationIgnored private var steamNotice: Task<Void, Never>?

    init(steam: SteamModel, epic: EpicModel, gog: GogModel, environment: GameOverlayEnvironment = .live) {
        self.steam = steam
        self.epic = epic
        self.gog = gog
        self.environment = environment
        if environment.observesActivations {
            activations = NSWorkspace.shared.notificationCenter.addObserver(
                forName: NSWorkspace.didActivateApplicationNotification, object: nil, queue: .main
            ) { [weak self] notification in
                let application = notification.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication
                let activated = application.map(OverlayApplication.init)
                MainActor.assumeIsolated {
                    if let activated { self?.applicationActivated(activated) }
                }
            }
            capturedOverlayKeys = DistributedNotificationCenter.default().addObserver(
                forName: Self.capturedOverlayKey, object: nil, queue: .main
            ) { [weak self] notification in
                let pid = (notification.userInfo?["pid"] as? NSNumber)?.int32Value
                let application = pid.flatMap(NSRunningApplication.init(processIdentifier:)).map(OverlayApplication.init)
                MainActor.assumeIsolated {
                    if let application { self?.overlayKeyPressed(in: application) }
                }
            }
        }
        observeSessions()
    }

    isolated deinit {
        if let activations { NSWorkspace.shared.notificationCenter.removeObserver(activations) }
        if let capturedOverlayKeys { DistributedNotificationCenter.default().removeObserver(capturedOverlayKeys) }
        environment.hotKey.unregister()
    }

    // MARK: Showing

    /// Shows `request`, at the player's demand (`userInitiated`) or the
    /// game's. False when no game runs to show it over.
    @discardableResult
    func present(_ request: OverlayRequest, userInitiated: Bool = false) -> Bool {
        // A request may come before the session's change reached the overlay.
        follow(runningSession)
        guard session != nil else { return false }
        show(request)
        guard !isPresented else { return true }
        isPresented = true
        steam.chat.isOverlayPresented = true
        openedByPlayer = userInitiated
        environment.toaster.hide()
        let frontmost = environment.frontmost()
        returnTo = frontmost?.pid == ProcessInfo.processInfo.processIdentifier ? nil : frontmost
        if let frontmost, isGame(frontmost) { gameApplication = frontmost }
        avatars.activate(account: friends?.account)
        environment.presenter.show(self, over: gameApplication)
        updateHotKey()
        onVisibilityChanged?(true)
        tellSteam(active: true)
        return true
    }

    /// Shows `request` in the open overlay. Leaving the game's invite dialog
    /// ends it, as closing Steam's does. A purchase the Steam game waits on
    /// shows instead of the home page, until the player answers it.
    func show(_ request: OverlayRequest) {
        let wasInviting = isInviting
        switch request {
        case .home:
            self.request = pendingPurchase.map { .microTxn(orderId: $0.orderId) } ?? .home
        case .user(let steamId):
            if let friend = steamFriend(steamId) {
                focus.target = .composer
                focus.conversation = friend.id
                self.request = .friends
            } else {
                self.request = .webPage(Self.profileURL(steamId: steamId))
            }
        case .achievements:
            self.request = .achievements
            loadAchievements()
        case .invite:
            // Each of the game's requests is another picker, on the list.
            if focus.conversation != nil {
                focus.target = .search
                focus.conversation = nil
            }
            self.request = request
        case .friends, .webPage, .microTxn:
            self.request = request
        }
        if wasInviting && !isInviting { steam.chat.closeInvitePicker() }
    }

    /// Back from a page to the overlay's home; false on the home already.
    @discardableResult
    func back() -> Bool {
        switch request {
        case .achievements, .webPage:
            request = .home
            return true
        case .microTxn(let orderId):
            // An unanswered purchase stays; closing the overlay leaves it waiting.
            guard pendingPurchase?.orderId != orderId else { return false }
            show(.home)
            return true
        case .home, .friends, .invite, .user:
            return false
        }
    }

    /// Closes the overlay, and the game comes back in front.
    func hide() {
        hide(returningFocus: true)
    }

    /// The player's Shift-Tab.
    func toggle() {
        if isPresented { hide() } else { present(.home, userInitiated: true) }
    }

    /// A game that ended is told nothing more.
    private func hide(returningFocus: Bool, gameEnded: Bool = false) {
        steam.chat.isOverlayPresented = false
        guard isPresented else { return }
        let wasSuspended = suspendedForAppSwitch
        suspendedForAppSwitch = false
        if isInviting { steam.chat.closeInvitePicker() }
        isPresented = false
        request = .home
        focus.conversation = nil
        environment.presenter.hide(returningTo: returningFocus && !wasSuspended ? returnTo : nil)
        returnTo = nil
        updateHotKey()
        onVisibilityChanged?(false)
        if !gameEnded { tellSteam(active: false) }
    }

    /// The running Steam game learns that the overlay opened or closed, as
    /// Steam's overlay tells it, each in turn. A session that ended in the
    /// meantime refuses it, which changes nothing.
    private func tellSteam(active: Bool) {
        guard let session, case .steam = session.game else { return }
        let sessions = steam.gameSessions
        let userInitiated = openedByPlayer
        let previous = steamNotice
        steamNotice = Task {
            await previous?.value
            try? await sessions.overlayState(sessionId: session.id, active: active, userInitiated: userInitiated)
        }
    }

    // MARK: The Steam game's requests

    /// The purchase the running Steam game waits on.
    var pendingPurchase: SteamMicroTxn? {
        guard let session, case .steam = session.game, steam.purchase.session == session.id else { return nil }
        return steam.purchase.pending
    }

    /// Friends' invitations, while a Steam game runs: those into it join it
    /// at once.
    var receivedInvites: [SteamReceivedInvite] {
        guard let session, case .steam = session.game else { return [] }
        return steam.invitations.invites
    }

    /// The friend who sent `invite`, when the friends list knows them.
    func sender(of invite: SteamReceivedInvite) -> FriendPresentation? {
        steamFriend(invite.friendSteamId)
    }

    /// A friend's invitation arrived while a Steam game runs: the player
    /// hears of it without leaving the game, or sees it in the open overlay.
    /// False when no Steam game runs, so another notice tells of it.
    func announce(_ invite: SteamReceivedInvite) -> Bool {
        follow(runningSession)
        guard let session, case .steam = session.game else { return false }
        guard !isPresented else { return true }
        let friend = sender(of: invite)
        let name = friend?.displayName ?? L10n.string("ui.overlay.invites.someone")
        let toast = OverlayToast(
            friend: friend,
            message: steam.gameName(appId: invite.appId).map { L10n.string("ui.overlay.invites.toast", name, $0) }
                ?? L10n.string("ui.chat.invite.receivedUnknown", name),
            hint: L10n.string("ui.overlay.invites.toastHint")
        )
        avatars.activate(account: friends?.account)
        environment.toaster.show(toast, avatars: avatars, over: gameApplication) { [weak self] in
            self?.present(.home, userInitiated: true)
        }
        return true
    }

    func announceMessage(_ steamId: UInt64) -> Bool {
        follow(runningSession)
        guard let session, case .steam = session.game else { return false }
        let friend = steamFriend(steamId)
        let toast = OverlayToast(friend: friend,
            message: L10n.string("ui.chat.newMessage"),
            hint: friend?.displayName ?? L10n.string("ui.overlay.invites.someone"))
        avatars.activate(account: friends?.account)
        environment.toaster.show(toast, avatars: avatars, over: gameApplication) { [weak self] in
            self?.present(.user(steamId: steamId), userInitiated: true)
        }
        return true
    }

    /// Joins `invite`: once the game is told, the player is back in it.
    func accept(_ invite: SteamReceivedInvite) {
        let invitations = steam.invitations
        Task { [weak self] in
            guard await invitations.accept(invite) else { return }
            self?.hide()
        }
    }

    func dismiss(_ invite: SteamReceivedInvite) {
        steam.invitations.dismiss(invite)
    }

    /// The player's answer to the purchase: once the game is told, the
    /// player is back in it.
    func answerPurchase(approve: Bool) {
        let purchase = steam.purchase
        Task { [weak self] in
            guard await purchase.answer(approve: approve) else { return }
            self?.hide()
        }
    }

    private var isInviting: Bool {
        if case .invite = request { true } else { false }
    }

    /// The game's invite dialog the friends list picks for, while it shows.
    var invitePicker: SteamInviteOffer? { isInviting ? steam.chat.invitePicker : nil }

    // MARK: The running game

    /// The friends of the game's storefront; GOG shows none.
    var friends: FriendsPresentationState? {
        switch session?.game {
        case .steam?: steam.friends
        case .epic?: epic.friends
        case .gog?, nil: nil
        }
    }

    /// Steam's chat, while a Steam game runs.
    var chat: SteamChatModel? {
        if case .steam? = session?.game { steam.chat } else { nil }
    }

    var gameTitle: String {
        let title: String? = switch session?.game {
        case .steam(let appId)?: steam.gameName(appId: appId)
        case .epic(let id)?: epic.library.game(id)?.title
        case .gog(let id)?: gog.library.game(id)?.title
        case nil: nil
        }
        return title ?? L10n.string("ui.overlay.unknownGame")
    }

    /// The game's wide artwork, which Steam publishes by app.
    var gameArtwork: String? {
        switch session?.game {
        case .epic(let id)?: epic.library.game(id).flatMap { $0.headerImageUrl ?? $0.imageUrl }
        case .gog(let id)?: gog.library.game(id).flatMap { $0.headerImageUrl ?? $0.imageUrl }
        case .steam?, nil: nil
        }
    }

    func retryFriends() {
        if case .epic? = session?.game { epic.retryFriends() } else { steam.retryFriends() }
    }

    /// Closes the overlay for the library window, where signing in again
    /// happens too.
    func openLibrary() {
        hide(returningFocus: false)
        showLibrary()
    }

    func retryAchievements() {
        achievements = nil
        loadAchievements()
    }

    private func loadAchievements() {
        guard let session, achievements == nil else { return }
        switch session.game {
        case .steam(let appId):
            let model = GameAchievementsModel(storeID: "steam", describe: SteamModel.message)
            achievements = model
            steam.loadAchievements(appId: appId, into: model)
        case .epic(let id):
            let model = GameAchievementsModel(storeID: "epic", describe: EpicGames.describe)
            achievements = model
            epic.loadAchievements(of: id, into: model)
        case .gog(let id):
            let model = GameAchievementsModel(storeID: "gog", describe: GogGames.describe)
            achievements = model
            gog.loadAchievements(of: id, into: model)
        }
    }

    private func steamFriend(_ steamId: UInt64) -> FriendPresentation? {
        guard case .steam? = session?.game else { return nil }
        return steam.friends?.friend(steamId)
    }

    /// Cookies that sign a Steam page in as the Steam account; a page of
    /// another site gets none.
    func webCookies(for url: URL) async -> [HTTPCookie] {
        guard let host = url.host(), Self.steamSites.contains(where: { host == $0 || host.hasSuffix("." + $0) })
        else { return [] }
        return await steam.webCookies()
    }

    private static let steamSites = ["steampowered.com", "steamcommunity.com"]

    /// A Steam game's store page; app 0 is the store's front page.
    nonisolated static func storeURL(appId: UInt32) -> URL {
        URL(string: appId == 0 ? "https://store.steampowered.com/" : "https://store.steampowered.com/app/\(appId)")!
    }

    /// Steam's store search for `query`.
    nonisolated static func storeSearchURL(_ query: String) -> URL {
        var url = URLComponents(string: "https://store.steampowered.com/search/")!
        url.queryItems = [URLQueryItem(name: "term", value: query)]
        // A `+` left as is reads as a space.
        url.percentEncodedQuery = url.percentEncodedQuery?.replacingOccurrences(of: "+", with: "%2B")
        return url.url!
    }

    nonisolated static func profileURL(steamId: UInt64) -> URL {
        URL(string: "https://steamcommunity.com/profiles/\(steamId)")!
    }

    /// The game's community hub.
    nonisolated static func communityURL(appId: UInt32) -> URL {
        URL(string: "https://steamcommunity.com/app/\(appId)")!
    }

    /// Someone's achievements in the game, on their profile.
    nonisolated static func achievementsURL(steamId: UInt64, appId: UInt32) -> URL {
        URL(string: "https://steamcommunity.com/profiles/\(steamId)/stats/\(appId)/achievements/")!
    }

    // MARK: Following the session

    /// The running session of any storefront: one game runs at a time.
    private var runningSession: OverlaySession? {
        if let active = steam.gameSessions.active, active.running {
            return OverlaySession(id: active.id, game: .steam(appId: active.game), startedAt: active.startedAt)
        }
        if let active = epic.gameSessions.active, active.running {
            return OverlaySession(id: active.id, game: .epic(active.game), startedAt: active.startedAt)
        }
        if let active = gog.gameSessions.active, active.running {
            return OverlaySession(id: active.id, game: .gog(active.game), startedAt: active.startedAt)
        }
        return nil
    }

    private func observeSessions() {
        let running = withObservationTracking { runningSession } onChange: { [weak self] in
            Task { @MainActor [weak self] in self?.observeSessions() }
        }
        follow(running)
    }

    /// A game that stops running takes its overlay and its Shift-Tab along.
    private func follow(_ running: OverlaySession?) {
        guard running?.id != session?.id else {
            if running != session { session = running }
            return
        }
        hide(returningFocus: false, gameEnded: true)
        environment.toaster.hide()
        session = running
        achievements = nil
        gameApplication = nil
        voileMatch = running.map { _ in GameApplicationMatch(voile: environment.voile, native: []) }
        gameIsFrontmost = false
        if let frontmost = environment.frontmost(), isGame(frontmost) {
            gameIsFrontmost = true
            gameApplication = frontmost
        }
        updateHotKey()
    }

    /// macOS brought `application` to the front.
    func applicationActivated(_ application: OverlayApplication) {
        guard session != nil else { return }
        gameIsFrontmost = isGame(application)
        if gameIsFrontmost { gameApplication = application }
        if isPresented {
            if gameIsFrontmost && suspendedForAppSwitch {
                suspendedForAppSwitch = false
                environment.presenter.show(self, over: gameApplication)
                steam.chat.isOverlayPresented = true
                onVisibilityChanged?(true)
                tellSteam(active: true)
            } else if !gameIsFrontmost && application.pid != ProcessInfo.processInfo.processIdentifier
                        && !suspendedForAppSwitch {
                suspendedForAppSwitch = true
                steam.chat.isOverlayPresented = false
                environment.presenter.hide(returningTo: nil)
                environment.toaster.hide()
                onVisibilityChanged?(false)
                tellSteam(active: false)
            }
        }
        updateHotKey()
    }

    /// While a game in exclusive fullscreen captures the displays, every key
    /// goes to it, Shift-Tab too: Voile's Wine then hands the displays back
    /// and tells of the press, as it does for Command-Tab.
    static let capturedOverlayKey = Notification.Name("com.zagrios.obelisk.overlayKey")

    func overlayKeyPressed(in application: OverlayApplication) {
        guard session != nil, !isPresented, isGame(application) else { return }
        gameApplication = application
        present(.home, userInitiated: true)
    }

    /// Whether `application` is the running game's. Its Mac build's folder
    /// is read now: its installation may have been told after it started.
    func isGame(_ application: OverlayApplication) -> Bool {
        guard let session, application.pid != ProcessInfo.processInfo.processIdentifier else { return false }
        if voileMatch?.contains(application) == true { return true }
        return GameApplicationMatch(voile: nil, native: nativeFolders(of: session)).contains(application)
    }

    private func nativeFolders(of session: OverlaySession) -> [String] {
        switch session.game {
        case .steam(let appId): Self.nativeFolders(steam.installations, game: appId)
        case .epic(let id): Self.nativeFolders(epic.installations, game: id)
        case .gog(let id): Self.nativeFolders(gog.installations, game: id)
        }
    }

    private static func nativeFolders<Store: InstallationStorefront>(_ installations: GameInstallations<Store>, game: Store.GameID) -> [String] {
        installations.receipts.filter { $0.gameID == game && $0.platform == .mac }.map(\.directory)
    }

    /// Shift-Tab is the overlay's while the game or the overlay is in front.
    private func updateHotKey() {
        if session != nil && (gameIsFrontmost || isPresented && !suspendedForAppSwitch) {
            environment.hotKey.register { [weak self] in self?.toggle() }
        } else {
            environment.hotKey.unregister()
        }
    }
}

@MainActor
final class InertOverlayHotKey: OverlayHotKey {
    private(set) var isRegistered = false
    func register(_ action: @escaping @MainActor () -> Void) { isRegistered = true }
    func unregister() { isRegistered = false }
}

@MainActor
final class InertOverlayPresenter: OverlayPresenter {
    func show(_ overlay: GameOverlayController, over game: OverlayApplication?) {}
    func hide(returningTo application: OverlayApplication?) {}
}
