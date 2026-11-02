import Foundation
import XCTest
@testable import Obelisk

/// A web storefront as the shared account tests drive it: how its fixtures
/// are named, and what its session answers and throws.
protocol WebStoreFixture: WebStorefront where Credentials: Equatable {
    associatedtype Playtime: Sendable
    associatedtype DetailsRequest: Equatable & Sendable

    /// The account a test signs in unless it names another.
    static var fixtureAccount: String { get }
    static var fixtureName: String { get }
    /// The fixture account carries `fixtureName`; any other is named
    /// "Name of" its id, so a test tells two accounts, and a name from an id, apart.
    static func credentials(_ token: String, account: String) -> Credentials
    /// A Windows game. The `title:` label keeps it apart from `InstallationStorefront.game(_:)`.
    static func game(_ key: String, title: String) -> Game
    static func playtime(_ key: String, minutes: UInt64) -> Playtime
    static func details(bytes: UInt64?) -> Details
    /// The Library cache of `directory` once it holds `entries` for `account`.
    static func cache(_ entries: [(game: Game, minutes: UInt64?)], account: String, in directory: URL) throws -> LibraryCache
    static var unauthorized: Error { get }
    static var network: Error { get }
    static var expectedAvatar: URL? { get }
    /// What the store shows once it refuses the account, spelled out here
    /// rather than asked of the adapter, so a wrong mapping fails.
    static var unauthorizedMessage: String { get }
    /// The warning after a sign-out the store may not have revoked, spelled
    /// out for the same reason.
    static var revocationMessage: String { get }
    static func session(_ fake: WebTestSession<Self>) -> Session
}

extension WebStoreFixture {
    static func credentials(_ token: String) -> Credentials { credentials(token, account: fixtureAccount) }
}

/// The store's account model, with test Game sessions. A sign-in or
/// restoration the test does not inject fails rather than reach the storefront.
@MainActor
func webModel<S: WebStoreFixture>(
    _: S.Type,
    _ keychain: MemoryStore<S.Credentials>,
    authorize: @escaping @MainActor () async throws -> String = { "test-code" },
    signIn: @escaping @Sendable (String) async throws -> S.Session = { _ in throw S.network },
    restore: @escaping @Sendable (S.Credentials) async throws -> S.Session = { _ in throw S.network },
    language: String = "en-US",
    gameSessions: GameSessions<S>? = nil,
    libraryCache: LibraryCache? = nil
) -> WebAccountModel<S> {
    WebAccountModel(keychain: keychain, authorize: authorize, signIn: signIn, restore: restore, language: language,
                    gameSessions: gameSessions ?? GameSessions(), libraryCache: libraryCache)
}

/// A model whose saved session restores as `session`, once its library loaded.
@MainActor
func connectedModel<S: WebStoreFixture>(_ session: WebTestSession<S>, language: String = "en-US") async -> WebAccountModel<S> {
    let model = webModel(S.self, MemoryStore(S.credentials("current")), restore: { _ in S.session(session) },
                         language: language)
    await model.start()
    XCTAssertTrue(model.isConnected)
    return model
}

extension EpicGames: WebStoreFixture {
    typealias Playtime = EpicPlaytime
    typealias DetailsRequest = EpicDetailsRequest

    static let fixtureAccount = "fixture-id"
    static let fixtureName = "Fixture"
    static func credentials(_ token: String, account: String) -> EpicCredentials {
        EpicCredentials(accountId: account, displayName: account == fixtureAccount ? fixtureName : "Name of \(account)",
                        refreshToken: token)
    }
    /// The app name and catalog item are the key, in the "fixture" namespace.
    static func game(_ key: String, title: String) -> EpicGame {
        EpicGame(appName: key, namespace: "fixture", catalogItemId: key, title: title, description: "", imageUrl: nil,
                 headerImageUrl: nil, developer: "", windows: true, macos: false)
    }
    static func playtime(_ key: String, minutes: UInt64) -> EpicPlaytime { EpicPlaytime(appName: key, seconds: minutes * 60) }
    static func details(bytes: UInt64?) -> EpicGameDetails { EpicGameDetails(platform: nil, installationBytes: bytes) }
    static func cache(_ entries: [(game: EpicGame, minutes: UInt64?)], account: String, in directory: URL) throws -> LibraryCache {
        let games = entries.map { CachedEpicGame(game: $0.game, playtimeSeconds: $0.minutes.map { $0 * 60 }) }
        return try LibraryCache.holding(epic: [account: games], in: directory)
    }
    static var unauthorized: Error { EpicError.Unauthorized }
    static var network: Error { EpicError.Network }
    static let expectedAvatar: URL? = nil
    static var unauthorizedMessage: String { L10n.string("model.epic.unauthorized") }
    static var revocationMessage: String { L10n.string("model.epic.revocationUnconfirmed") }
    static func session(_ fake: WebTestSession<EpicGames>) -> any EpicSessionProtocol { fake }
}

extension GogGames: WebStoreFixture {
    typealias Playtime = GogPlaytime
    typealias DetailsRequest = GogDetailsRequest

    static let fixtureAccount = "48628349957132247"
    static let fixtureName = "Geralt"
    static func credentials(_ token: String, account: String) -> GogCredentials {
        GogCredentials(userId: account, username: account == fixtureAccount ? fixtureName : "Name of \(account)",
                       refreshToken: token)
    }
    /// The product id is the key.
    static func game(_ key: String, title: String) -> GogGame {
        GogGame(productId: key, title: title, imageUrl: nil, headerImageUrl: nil, windows: true, macos: false,
                summary: "", developers: [], publishers: [], releaseDate: nil, genres: [], rating: nil, languages: [])
    }
    static func playtime(_ key: String, minutes: UInt64) -> GogPlaytime { GogPlaytime(productId: key, minutes: minutes) }
    static func details(bytes: UInt64?) -> GogGameDetails { GogGameDetails(platform: nil, installationBytes: bytes) }
    static func cache(_ entries: [(game: GogGame, minutes: UInt64?)], account: String, in directory: URL) throws -> LibraryCache {
        let games = entries.map { CachedGogGame(game: $0.game, playtimeMinutes: $0.minutes) }
        return try LibraryCache.holding(gog: [account: games], in: directory)
    }
    static var unauthorized: Error { GogError.Unauthorized }
    static var network: Error { GogError.Network }
    static let expectedAvatar = URL(string: "https://images.gog.com/avatar_avm.jpg")
    static var unauthorizedMessage: String { L10n.string("model.gog.unauthorized") }
    static var revocationMessage: String { L10n.string("model.gog.revocationUnconfirmed") }
    static func session(_ fake: WebTestSession<GogGames>) -> any GogSessionProtocol { fake }
}

struct EpicDetailsRequest: Equatable, Sendable {
    let namespace: String
    let catalogItemId: String
    let appName: String
}

struct GogDetailsRequest: Equatable, Sendable {
    let productId: String
    let language: String
}

typealias EpicTestSession = WebTestSession<EpicGames>
typealias GogTestSession = WebTestSession<GogGames>

/// A storefront's Rust session as the model tests drive it: the test holds,
/// fails or rotates each answer, and the session records every call.
actor WebTestSession<Store: WebStoreFixture> {
    nonisolated let credentialRevisions = EpicCredentialRevisionFeed()

    private var currentCredentials: Store.Credentials {
        didSet { credentialRevisions.publish() }
    }
    private let ownedGames: [Store.Game]
    private let ownedPlaytimes: [Store.Playtime]
    private var rotated: Store.Credentials?
    private var prepared: Store.Credentials?
    private var failNext = false
    private var expireNext = false
    private var holdNext = false
    private var gameContinuation: CheckedContinuation<[Store.Game], Never>?
    private(set) var waitingForGames = false
    private(set) var gameCalls = 0
    private var failNextPlaytimeRequest = false
    private(set) var playtimeCalls = 0
    /// The product ids of each of GOG's playtime requests.
    private(set) var playtimeRequests: [[String]] = []
    private var shouldHoldDetails = false
    private var detailsFailureCredentials: Store.Credentials?
    private var detailsContinuations: [Int: CheckedContinuation<Store.Details, Never>] = [:]
    private(set) var detailsCalls: [Store.DetailsRequest] = []
    private(set) var returnedDetails = 0
    private var shouldHoldCredentials = false
    private var credentialsContinuation: CheckedContinuation<Bool, Never>?
    private(set) var waitingForCredentials = false
    private var shouldHoldDisconnect = false
    private var shouldFailDisconnect = false
    private var disconnectContinuation: CheckedContinuation<Bool, Never>?
    private(set) var waitingForDisconnect = false
    private(set) var disconnections = 0
    /// Each achievements request, as the storefront received it.
    private(set) var achievementReads: [AchievementRead] = []
    /// Each game's achievements, unlocked or not, by the name requests give
    /// it; a game not named has one unlocked of two.
    private var achievementStates: [String: [Bool]] = [:]
    /// The games whose achievements request fails.
    private var failingAchievements: Set<String> = []

    init(credentials: Store.Credentials, games: [Store.Game], rotated: Store.Credentials? = nil,
         playtimes: [Store.Playtime] = []) {
        currentCredentials = credentials
        ownedGames = games
        self.rotated = rotated
        ownedPlaytimes = playtimes
    }

    /// Ignores cancellation, as the generated bindings do: a cancelled sign-in
    /// still gets its credentials back, and the model must drop them itself.
    func credentials() async throws -> Store.Credentials {
        if shouldHoldCredentials {
            shouldHoldCredentials = false
            waitingForCredentials = true
            let failing = await withCheckedContinuation { credentialsContinuation = $0 }
            waitingForCredentials = false
            if failing { throw Store.network }
        }
        return currentCredentials
    }
    func holdNextCredentials() { shouldHoldCredentials = true }
    func completeCredentials(failing: Bool) {
        credentialsContinuation?.resume(returning: failing)
        credentialsContinuation = nil
    }
    func rotate(_ credentials: Store.Credentials) { currentCredentials = credentials }
    func rotateOnPrepare(_ credentials: Store.Credentials) { prepared = credentials }

    func games() async throws -> [Store.Game] {
        gameCalls += 1
        if let rotated { currentCredentials = rotated; self.rotated = nil }
        if expireNext { expireNext = false; throw Store.unauthorized }
        if failNext { failNext = false; throw Store.network }
        if holdNext {
            holdNext = false
            waitingForGames = true
            let result = await withCheckedContinuation { gameContinuation = $0 }
            waitingForGames = false
            return result
        }
        return ownedGames
    }
    func holdNextGames() { holdNext = true }
    func completeGames(_ games: [Store.Game]) { gameContinuation?.resume(returning: games); gameContinuation = nil }
    func expireNextGames() { expireNext = true }
    func failNextGames(rotated: Store.Credentials) { failNext = true; self.rotated = rotated }

    func failNextPlaytimes() { failNextPlaytimeRequest = true }

    func holdNextDetails() { shouldHoldDetails = true }
    func completeDetails(_ invocation: Int, with details: Store.Details, rotated: Store.Credentials? = nil) {
        if let rotated { currentCredentials = rotated }
        detailsContinuations.removeValue(forKey: invocation)?.resume(returning: details)
    }
    func failNextDetails(rotated: Store.Credentials) { detailsFailureCredentials = rotated }

    func disconnect() async throws {
        disconnections += 1
        if shouldHoldDisconnect {
            shouldHoldDisconnect = false
            waitingForDisconnect = true
            let failing = await withCheckedContinuation { disconnectContinuation = $0 }
            waitingForDisconnect = false
            if failing { throw Store.network }
        }
        if shouldFailDisconnect { shouldFailDisconnect = false; throw Store.network }
    }
    func holdNextDisconnect() { shouldHoldDisconnect = true }
    func completeDisconnect(failing: Bool) { disconnectContinuation?.resume(returning: failing); disconnectContinuation = nil }
    func failNextDisconnect() { shouldFailDisconnect = true }

    func nextCredentialsRevision(afterRevision: UInt64) async -> UInt64? {
        await credentialRevisions.next(after: afterRevision)
    }
    nonisolated func shutdownObservers() { credentialRevisions.stop() }

    /// Rotates as Rust's client does when it renews its token before a call.
    private func preparing() {
        if let prepared { currentCredentials = prepared; self.prepared = nil }
    }

    private func answerPlaytimes() async throws -> [Store.Playtime] {
        playtimeCalls += 1
        if failNextPlaytimeRequest { failNextPlaytimeRequest = false; throw Store.network }
        return ownedPlaytimes
    }

    private func answerDetails(_ request: Store.DetailsRequest) async throws -> Store.Details {
        preparing()
        detailsCalls.append(request)
        defer { returnedDetails += 1 }
        if let rotated = detailsFailureCredentials {
            currentCredentials = rotated
            detailsFailureCredentials = nil
            throw Store.network
        }
        if shouldHoldDetails {
            shouldHoldDetails = false
            let invocation = detailsCalls.count
            return await withCheckedContinuation { detailsContinuations[invocation] = $0 }
        }
        return Store.details(bytes: nil)
    }

    func setAchievements(_ states: [String: [Bool]], failing: Set<String> = []) {
        achievementStates = states
        failingAchievements = failing
    }

    /// Records the request, then tells which of each game's achievements
    /// are unlocked, but the `failing` games'; it throws when it could tell
    /// none.
    private func readAchievements(_ ids: [String], locale: String, fresh: Bool) throws -> [String: [Bool]] {
        achievementReads.append(AchievementRead(ids: ids, locale: locale, fresh: fresh))
        let told = ids.filter { !failingAchievements.contains($0) }.map { ($0, achievementStates[$0] ?? [true, false]) }
        if told.isEmpty && !ids.isEmpty { throw Store.network }
        return Dictionary(told, uniquingKeysWith: { first, _ in first })
    }

    fileprivate func readAchievements(of id: String, locale: String, fresh: Bool) throws -> [LibraryAchievement] {
        let states = try readAchievements([id], locale: locale, fresh: fresh)[id] ?? []
        return states.enumerated().map { index, unlocked in
            LibraryAchievement(id: "\(index)", name: "Achievement \(index)", description: "", iconUrl: "",
                               hidden: false, unlocked: unlocked, unlockedAt: unlocked ? 1_790_000_000 : nil)
        }
    }

    fileprivate func readAchievementShares(_ ids: [String], locale: String) throws -> [String: Double] {
        try readAchievements(ids, locale: locale, fresh: false).compactMapValues { states in
            states.isEmpty ? nil : Double(states.count { $0 }) / Double(states.count)
        }
    }
}

/// One achievements request: the games it names (Epic's namespaces, GOG's
/// products), its locale, and whether it skips the Library cache.
struct AchievementRead: Equatable, Sendable {
    let ids: [String]
    let locale: String
    let fresh: Bool
}

extension WebTestSession: EpicSessionProtocol where Store == EpicGames {
    func gameDetails(namespace: String, catalogItemId: String, appName: String) async throws -> EpicGameDetails {
        try await answerDetails(EpicDetailsRequest(namespace: namespace, catalogItemId: catalogItemId, appName: appName))
    }
    func playtimes() async throws -> [EpicPlaytime] { try await answerPlaytimes() }
    func achievements(namespace: String, locale: String, fresh: Bool) async throws -> [LibraryAchievement] {
        try readAchievements(of: namespace, locale: locale, fresh: fresh)
    }
    func achievementShares(namespaces: [String], locale: String) async throws -> [String: Double] {
        try readAchievementShares(namespaces, locale: locale)
    }

    nonisolated func setStatus(status: OnlineStatus) {}
    func startFriends() async {}
    func reconnectFriends() async {}
    nonisolated func stopFriends() {}
    nonisolated func friends() -> EpicFriendsSnapshot {
        EpicFriendsSnapshot(accountId: "unused", revision: 1, connection: .stopped, friends: [], message: nil)
    }
    func nextFriends(afterRevision: UInt64) async -> EpicFriendsSnapshot { friends() }
}

extension WebTestSession: GogSessionProtocol where Store == GogGames {
    func gameDetails(productId: String, language: String) async throws -> GogGameDetails {
        try await answerDetails(GogDetailsRequest(productId: productId, language: language))
    }
    func playtimes(productIds: [String]) async throws -> [GogPlaytime] {
        playtimeRequests.append(productIds)
        return try await answerPlaytimes()
    }
    func achievements(productId: String, locale: String, fresh: Bool) async throws -> [LibraryAchievement] {
        try readAchievements(of: productId, locale: locale, fresh: fresh)
    }
    func achievementShares(productIds: [String], locale: String) async throws -> [String: Double] {
        try readAchievementShares(productIds, locale: locale)
    }
    nonisolated func avatarUrl() -> String? { GogGames.expectedAvatar?.absoluteString }
}

typealias EpicMemoryStore = MemoryStore<EpicCredentials>

/// The Keychain as the model tests drive it: it records every write, and the
/// test holds or rejects saves.
actor MemoryStore<Credentials: Sendable>: CredentialStore {
    private(set) var value: Credentials?
    private(set) var loads = 0
    private(set) var writes: [Credentials?] = []
    private(set) var waitingForSave = false
    private var shouldHoldSave = false
    private var shouldRejectSaves = false
    private var saveContinuation: CheckedContinuation<Void, Never>?

    init(_ value: Credentials?) { self.value = value }
    func load() async throws -> Credentials? { loads += 1; return value }
    func save(_ credentials: Credentials) async throws {
        if shouldRejectSaves { throw CocoaError(.fileWriteUnknown) }
        if shouldHoldSave {
            shouldHoldSave = false
            waitingForSave = true
            await withCheckedContinuation { saveContinuation = $0 }
            waitingForSave = false
        }
        value = credentials
        writes.append(credentials)
    }
    func remove() async throws { value = nil; writes.append(nil) }
    func holdNextSave() { shouldHoldSave = true }
    func completeSave() { saveContinuation?.resume(); saveContinuation = nil }
    func rejectSaves(_ reject: Bool = true) { shouldRejectSaves = reject }
}

/// A sign-in that records each code it exchanges.
actor SignInCounter<Session: Sendable> {
    let session: Session
    private(set) var calls = 0
    private(set) var codes: [String] = []
    init(session: Session) { self.session = session }
    func signIn(code: String? = nil) -> Session {
        calls += 1
        if let code { codes.append(code) }
        return session
    }
}

/// A restoration whose first attempt fails with `failure`.
actor RestoreAttempt<Session: Sendable> {
    let session: Session
    private let failure: Error
    private var attempts = 0
    init(session: Session, failure: Error) {
        self.session = session
        self.failure = failure
    }
    func restore() throws -> Session {
        attempts += 1
        if attempts == 1 { throw failure }
        return session
    }
}

/// The storefront's page, answering each authorization when the test completes it.
@MainActor
final class AuthorizationGate {
    private var continuations: [Int: CheckedContinuation<String, Error>] = [:]
    private(set) var calls = 0
    private(set) var returnedCalls = 0

    func wait() async throws -> String {
        calls += 1
        let invocation = calls
        let code = try await withCheckedThrowingContinuation { continuations[invocation] = $0 }
        returnedCalls += 1
        return code
    }

    func complete(_ invocation: Int, code: String) {
        continuations.removeValue(forKey: invocation)?.resume(returning: code)
    }
}
