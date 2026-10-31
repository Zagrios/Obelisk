import Foundation
import XCTest
@testable import Obelisk

/// The three storefront accounts of one AppModel, as the tests that cross
/// storefronts drive them: each signed in over a session and installations the
/// test controls, or signed out. A restoration or a sign-in the test holds
/// waits on its gate. Nothing reaches a storefront, the Keychain or the
/// player's library.
@MainActor
final class StorefrontsFixture {
    nonisolated static let steamAccount: UInt64 = 42
    nonisolated static let epicAccount = "epic-account"
    nonisolated static let gogAccount = "gog-account"

    /// What the test sets up. A storefront without games is signed out: its
    /// Keychain holds nothing to restore. One with `[]` is signed in over an
    /// empty library.
    struct Setup {
        var steam: [SteamGame]?
        var epic: [EpicGame]?
        var gog: [GogGame]?
        var steamReceipts: [InstallationReceipt<UInt32>] = []
        var epicReceipts: [InstallationReceipt<String>] = []
        var gogReceipts: [InstallationReceipt<String>] = []
        var steamProfile = SteamProfile(displayName: nil, avatarUrl: nil)
        /// Replaces Epic's session, such as to count its friends' reconnections.
        var epicSession: (any EpicSessionProtocol)?
        /// The storefronts whose restoration waits for the test.
        var held: Set<LibrarySection> = []
        /// The storefronts whose restoration fails.
        var failing: Set<LibrarySection> = []
        var cache: CachedLibraries?
        var gamesFolder: String?
    }

    /// The Library cache the storefronts find at launch.
    struct CachedLibraries {
        var steam: [CachedSteamGame] = []
        var epic: [CachedEpicGame] = []
        var gog: [CachedGogGame] = []
    }

    let root: URL
    let defaults: UserDefaults
    private let suite: String
    let steamBackend: FakeInstallations<SteamGames>
    let epicBackend: FakeInstallations<EpicGames>
    let gogBackend: FakeInstallations<GogGames>
    let steamSessions = SteamGameSessions()
    let epicSessions = EpicGameSessions()
    let gogSessions = GogGameSessions()
    let steamSession: StubSteamSession
    /// Epic's session unless the setup replaced it.
    let epicWeb: WebTestSession<EpicGames>
    let gogSession: GogTestSession
    /// Each held storefront's restoration waits on its gate.
    let restoreGates: [LibrarySection: GameSessionGate<Void>]
    /// The storefronts' sign-in pages, which a sign-in waits on.
    let signInGate = GameSessionGate<Void>()
    let logins = LoginAttempts()
    let sent = SentReports()
    let steam: SteamModel
    let epic: EpicModel
    let gog: GogModel
    let app: AppModel

    static let mac = MacHardware(chip: "Apple M2 Pro", model: "Mac14,12", memoryGB: 32, gpuCores: 19,
                                 macOSVersion: "27.0")

    init(_ setup: Setup = Setup()) {
        let root = FileManager.default.temporaryDirectory.appending(path: "ObeliskStorefronts-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        self.root = root
        suite = "ObeliskTests.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)!

        steamBackend = FakeInstallations(receipts: setup.steamReceipts)
        epicBackend = FakeInstallations(receipts: setup.epicReceipts)
        gogBackend = FakeInstallations(receipts: setup.gogReceipts)
        let steamSession = StubSteamSession(games: setup.steam ?? [], profile: setup.steamProfile)
        self.steamSession = steamSession
        let epicWeb = WebTestSession<EpicGames>(credentials: EpicGames.credentials("current", account: Self.epicAccount),
                                                games: setup.epic ?? [])
        self.epicWeb = epicWeb
        let gogSession = GogTestSession(credentials: GogGames.credentials("current", account: Self.gogAccount),
                                        games: setup.gog ?? [])
        self.gogSession = gogSession
        let epicSession: any EpicSessionProtocol = setup.epicSession ?? EpicGames.session(epicWeb)
        let gogProtocolSession = GogGames.session(gogSession)

        var gates: [LibrarySection: GameSessionGate<Void>] = [:]
        for section in setup.held { gates[section] = GameSessionGate<Void>() }
        restoreGates = gates
        let (steamGate, epicGate, gogGate) = (gates[.steam], gates[.epic], gates[.gog])
        let failing = setup.failing
        let signInGate = signInGate
        let logins = logins

        let cache = setup.cache.flatMap { cached in
            try? LibraryCache.holding(steam: [Self.steamAccount: cached.steam], epic: [Self.epicAccount: cached.epic],
                                      gog: [Self.gogAccount: cached.gog], in: root)
        }

        steam = SteamModel(
            directory: root,
            keychain: MemoryStore<SteamCredentials>(setup.steam == nil ? nil : SteamCredentials(
                accountName: "fixture-steam", steamId: Self.steamAccount, refreshToken: "fixture-only-never-sent")),
            connect: { _ in
                try await steamGate?.wait()
                if failing.contains(.steam) { throw SteamError.Failure(reason: "Fixture: the connection fails") }
                return steamSession
            },
            news: { _ in [] },
            webCookies: { _ in [] },
            beginLogin: { _ in
                await logins.record()
                throw SteamError.Failure(reason: "Fixture: sign-in is offline in tests")
            },
            gameSessions: steamSessions,
            libraryCache: cache,
            installations: SteamModel.installations(steamBackend),
            chat: SteamChatModel(isApplicationActive: false)
        )
        epic = EpicModel(
            directory: root,
            keychain: MemoryStore<EpicCredentials>(setup.epic == nil ? nil
                : EpicGames.credentials("saved", account: Self.epicAccount)),
            authorize: {
                try await signInGate.wait()
                return "test-code"
            },
            signIn: { _ in epicSession },
            restore: { _ in
                try await epicGate?.wait()
                if failing.contains(.epic) { throw EpicGames.network }
                return epicSession
            },
            gameSessions: epicSessions,
            libraryCache: cache,
            installations: EpicModel.installations(epicBackend)
        )
        gog = GogModel(
            directory: root,
            keychain: MemoryStore<GogCredentials>(setup.gog == nil ? nil
                : GogGames.credentials("saved", account: Self.gogAccount)),
            authorize: {
                try await signInGate.wait()
                return "test-code"
            },
            signIn: { _ in gogProtocolSession },
            restore: { _ in
                try await gogGate?.wait()
                if failing.contains(.gog) { throw GogGames.network }
                return gogProtocolSession
            },
            gameSessions: gogSessions,
            libraryCache: cache,
            installations: GogModel.installations(gogBackend)
        )
        let gamesFolder = setup.gamesFolder ?? root.appending(path: "Games").path
        let sent = sent
        app = AppModel(
            directory: root, defaults: defaults, steam: steam, epic: epic, gog: gog,
            downloads: DownloadsModel(backend: FakeDownloads(), defaults: defaults),
            gamesFolder: GamesFolderModel(library: root, backend: FakeGamesFolder(path: gamesFolder),
                                          workspace: NotificationCenter()),
            performance: PerformanceReports(isEnabled: true, mac: Self.mac) { await sent.add($0) }
        )
    }

    /// Restores every saved session, as Obelisk does at launch.
    func signIn() async {
        await app.restoreStorefronts()
    }

    /// Ends the accounts and the files the test made.
    func finish() {
        for gate in restoreGates.values { Task { await gate.resolve(()) } }
        Task { [signInGate] in await signInGate.resolve(()) }
        if steam.isConnected { steam.signOut() }
        if epic.isConnected { epic.signOut() }
        if gog.isConnected { gog.signOut() }
        defaults.removePersistentDomain(forName: suite)
        try? FileManager.default.removeItem(at: root)
    }
}

/// The QR sign-ins Steam began; a signed-out Steam restored at launch begins none.
actor LoginAttempts {
    private(set) var count = 0
    func record() { count += 1 }
}

/// The performance reports Obelisk sent.
actor SentReports {
    private(set) var reports: [SessionReport] = []
    func add(_ report: SessionReport) { reports.append(report) }
}

/// A Steam account's session: its library and profile, and how many times it
/// was asked for its games and to connect again.
final class StubSteamSession: SteamSessionProtocol, @unchecked Sendable {
    private let lock = NSLock()
    private let library: [SteamGame]
    private let account: SteamProfile
    private var _gameCalls = 0
    private var _reconnects = 0

    init(games: [SteamGame], profile: SteamProfile) {
        library = games
        account = profile
    }

    var gameCalls: Int { lock.withLock { _gameCalls } }
    var reconnects: Int { lock.withLock { _reconnects } }

    func games() async throws -> [SteamGame] {
        lock.withLock { _gameCalls += 1 }
        return library
    }
    func profile() async throws -> SteamProfile { account }
    func details(appId: UInt32, language: String) async throws -> SteamGameDetails {
        throw SteamError.Failure(reason: "Fixture: no details")
    }
    func reconnect() { lock.withLock { _reconnects += 1 } }
}

// MARK: Games and receipts

extension StorefrontsFixture {
    static let portal = SteamGame(appId: 620, name: "Portal 2", iconHash: "", lastPlayed: 0, playtimeMinutes: 0)
    static let counterStrike = SteamGame(appId: 730, name: "Counter-Strike 2", iconHash: "", lastPlayed: 0,
                                         playtimeMinutes: 0)
    /// Epic's game `key`, which its library names "fixture:key".
    static func epicGame(_ key: String = "one", title: String = "Epic One") -> EpicGame {
        EpicGames.game(key, title: title)
    }
    static func gogGame(_ id: String = "1207658924", title: String = "GOG One") -> GogGame {
        GogGames.game(id, title: title)
    }

    static func gogReceipt(_ id: String, platform: GamePlatform = .windows, directory: String) -> InstallationReceipt<String> {
        InstallationReceipt(id: "\(id)-\(platform == .mac ? "macos" : "windows")", gameID: id, name: "Game \(id)",
                            directory: directory, platform: platform, totalBytes: 100)
    }
}

extension DownloadItem {
    /// A Transfer of `game` of `storefront` waiting in the download queue.
    static func queued(_ storefront: SessionStorefront, game: String, name: String = "Queued name") -> DownloadItem {
        DownloadItem(id: 1, storefront: storefront, account: "account", game: game, name: name, kind: .install,
                     automatic: false, launching: false, paused: false)
    }
}

extension GameSessionState {
    /// A GOG game's session.
    static func gog(account: String = StorefrontsFixture.gogAccount, productId: String = "1207658924",
                    id: String = UUID().uuidString, phase: GamePhase = .running, observed: Bool = true) -> Self {
        GameSessionState(id: id, game: .gog(accountId: account, productId: productId), phase: phase,
                         reconnecting: false, cloud: .idle, host: nil, observed: observed, runtimeInstall: nil)
    }

    /// This session once the launch waits for the player to take or skip a newer Voile.
    func askingForVoile(_ version: String = "v26.3.0-17") -> Self {
        var asking = self
        asking.voileUpdate = VoileUpdateOffer(version: version, required: false, releases: [
            VoileReleaseNotes(version: version, notes: "### Fixes\n\n- Fewer crashes"),
        ])
        return asking
    }
}
