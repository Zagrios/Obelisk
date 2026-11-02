import Foundation
import XCTest
@testable import Obelisk

/// A storefront's Game sessions as Rust would publish them, driven by the test.
/// Rust's own rules are tested in Rust; this only shows states and records the
/// calls Swift makes.
final class FakeSessions: StorefrontSessionsProtocol, @unchecked Sendable {
    struct RosettaCall: Equatable, Sendable {
        let session: String
        let account: GameAccount
        let decisionID: UInt64
    }

    struct OverlayStateCall: Equatable, Sendable {
        let session: String
        let active: Bool
        let userInitiated: Bool
    }

    struct AcceptedInvite: Equatable, Sendable {
        let session: String
        let friend: UInt64
        let invite: SteamGameInvite
    }

    struct MicroTxnAnswer: Equatable, Sendable {
        let session: String
        let orderID: UInt64
        let approve: Bool
    }

    private let lock = NSLock()
    private var current: GameSessionsState
    private var waiters: [(after: UInt64, continuation: CheckedContinuation<GameSessionsState, Never>)] = []
    private var _rosettaChecks: [RosettaCall] = []
    private var _rosettaResumes: [RosettaCall] = []
    private var _declines: [RosettaCall] = []
    private var _revocations: [GameAccount] = []
    private var _stops = 0
    private var _quits: [String] = []
    private var _forceQuits: [String] = []
    private var _overlayStates: [OverlayStateCall] = []
    private var _acceptedInvites: [AcceptedInvite] = []
    private var _microTxnAnswers: [MicroTxnAnswer] = []
    private var _voileAnswers: [VoileUpdateAnswer] = []
    private var _statuses: [OnlineStatus] = []
    /// Steam's in-game calls fail with this key until the test clears it.
    private var steamFailure: String?
    /// Steam's in-game answers wait on it, when the test holds them.
    private var steamGate: GameSessionGate<Void>?
    /// Resuming after Rosetta fails with this key until the test clears it.
    private var rosettaFailure: String?
    private var rosettaFailuresRemaining = 0

    var rosettaChecks: [RosettaCall] { lock.withLock { _rosettaChecks } }
    var rosettaResumes: [RosettaCall] { lock.withLock { _rosettaResumes } }
    var declines: [RosettaCall] { lock.withLock { _declines } }
    /// The player's answers to the newer Voile releases launches offered, in order.
    var voileAnswers: [VoileUpdateAnswer] { lock.withLock { _voileAnswers } }
    var revocations: [GameAccount] { lock.withLock { _revocations } }
    /// How the player chose to show to their friends, in order.
    var statuses: [OnlineStatus] { lock.withLock { _statuses } }
    var stops: Int { lock.withLock { _stops } }
    /// The sessions the player asked to quit, then to force quit, in order.
    var quits: [String] { lock.withLock { _quits } }
    var forceQuits: [String] { lock.withLock { _forceQuits } }
    /// What the running Steam game was told of the overlay, in order.
    var overlayStates: [OverlayStateCall] { lock.withLock { _overlayStates } }
    var acceptedInvites: [AcceptedInvite] { lock.withLock { _acceptedInvites } }
    var microTxnAnswers: [MicroTxnAnswer] { lock.withLock { _microTxnAnswers } }

    /// Steam's in-game calls fail with `key` until it is nil again.
    func failSteamCalls(_ key: String?) { lock.withLock { steamFailure = key } }

    /// Accepting an invite and answering a purchase wait on `gate` until it resolves.
    func holdSteamAnswers(_ gate: GameSessionGate<Void>?) { lock.withLock { steamGate = gate } }

    init(recovering: Bool = false) {
        current = GameSessionsState(revision: 1, active: nil, last: nil, recovering: recovering,
                                    recoveryFailed: false, revoked: [], rosetta: nil)
    }

    /// Publishes a change, as Rust does after every one.
    func publish(_ change: (inout GameSessionsState) -> Void) {
        let (state, woken) = lock.withLock {
            change(&current)
            current.revision += 1
            let woken = waiters.filter { $0.after < current.revision }
            waiters.removeAll { $0.after < current.revision }
            return (current, woken)
        }
        for waiter in woken { waiter.continuation.resume(returning: state) }
    }

    /// The session holding the Reservation, as its supervisor last showed it.
    func show(_ session: GameSessionState?) {
        publish { state in
            state.active = session
            if let session { state.last = session }
            state.recovering = false
        }
    }

    /// The active session's supervisor asks for Rosetta with `decisionID`, or
    /// no longer does.
    func requireRosetta(decisionID: UInt64?) {
        publish { state in
            state.rosetta = decisionID.flatMap { decision in
                state.active.map { RosettaPromptState(sessionId: $0.id, account: $0.game.account, decisionId: decision) }
            }
        }
    }

    /// Every resumption fails with `key` until it is nil again.
    func failRosetta(_ key: String?) { lock.withLock { rosettaFailure = key } }

    /// The next `count` resumptions find Rosetta still unavailable.
    func failNextRosettaResumes(_ count: Int) { lock.withLock { rosettaFailuresRemaining = count } }

    func state() -> GameSessionsState { lock.withLock { current } }

    func changed(after: UInt64) async -> GameSessionsState {
        await withCheckedContinuation { continuation in
            let ready: GameSessionsState? = lock.withLock {
                if current.revision > after { return current }
                waiters.append((after, continuation))
                return nil
            }
            if let ready { continuation.resume(returning: ready) }
        }
    }

    func answerVoileUpdate(sessionId: String, answer: VoileUpdateAnswer) throws {
        lock.withLock { _voileAnswers.append(answer) }
        publish { state in
            guard var active = state.active, active.id == sessionId else { return }
            active.voileUpdate = nil
            state.active = active
        }
    }

    func declineRosetta(sessionId: String, account: GameAccount, decisionId: UInt64) {
        lock.withLock { _declines.append(.init(session: sessionId, account: account, decisionID: decisionId)) }
        publish { $0.rosetta = nil }
    }

    func resumeAfterRosetta(sessionId: String, account: GameAccount, decisionId: UInt64) async throws {
        let call = RosettaCall(session: sessionId, account: account, decisionID: decisionId)
        let failure: String? = lock.withLock {
            _rosettaChecks.append(call)
            if let rosettaFailure { return rosettaFailure }
            if rosettaFailuresRemaining > 0 {
                rosettaFailuresRemaining -= 1
                return SteamGameSessions.rosettaUnavailableKey
            }
            _rosettaResumes.append(call)
            return nil
        }
        if let failure { throw GameSessionError.Failure(reason: failure) }
    }

    func revoke(account: GameAccount) {
        lock.withLock { _revocations.append(account) }
        publish { if !$0.revoked.contains(account) { $0.revoked.append(account) } }
    }

    func stop() async throws { lock.withLock { _stops += 1 } }

    func quit(sessionId: String) async throws { lock.withLock { _quits.append(sessionId) } }

    func forceQuit(sessionId: String) async throws { lock.withLock { _forceQuits.append(sessionId) } }

    func setStatus(status: OnlineStatus) async { lock.withLock { _statuses.append(status) } }

    func overlayState(sessionId: String, active: Bool, userInitiated: Bool) async throws {
        let failure = lock.withLock {
            _overlayStates.append(.init(session: sessionId, active: active, userInitiated: userInitiated))
            return steamFailure
        }
        if let failure { throw GameSessionError.Failure(reason: failure) }
    }

    /// As Rust does, the accepted invitation leaves the game's list.
    func acceptInvite(sessionId: String, friendSteamId: UInt64, invite: SteamGameInvite) async throws {
        try await answerSteam()
        lock.withLock { _acceptedInvites.append(.init(session: sessionId, friend: friendSteamId, invite: invite)) }
        publish { state in
            guard state.active?.id == sessionId else { return }
            state.active?.receivedInvites.removeAll { $0.friendSteamId == friendSteamId && $0.invite == invite }
            state.last = state.active
        }
    }

    /// As Rust does, the answered purchase no longer waits.
    func authorizeMicroTxn(sessionId: String, orderId: UInt64, approve: Bool) async throws {
        try await answerSteam()
        lock.withLock { _microTxnAnswers.append(.init(session: sessionId, orderID: orderId, approve: approve)) }
        publish { state in
            guard state.active?.id == sessionId, state.active?.microTxn?.orderId == orderId else { return }
            state.active?.microTxn = nil
            state.last = state.active
        }
    }

    private func answerSteam() async throws {
        let (gate, failure) = lock.withLock { (steamGate, steamFailure) }
        if let gate { try await gate.wait() }
        if let failure { throw GameSessionError.Failure(reason: failure) }
    }
}

extension GameIdentity {
    var account: GameAccount {
        switch self {
        case let .steam(_, accountId): .steam(steamId: accountId)
        case let .epic(accountId, _, _): .epic(accountId: accountId)
        case let .gog(accountId, _): .gog(userId: accountId)
        }
    }
}

extension GameSessionState {
    /// A Steam game's session.
    static func steam(appID: UInt32 = 42, accountID: UInt64 = 42, id: String = UUID().uuidString,
                      phase: GamePhase = .running, cloud: CloudStatus = .idle, observed: Bool = true) -> Self {
        GameSessionState(id: id, game: .steam(appId: appID, accountId: accountID), phase: phase, reconnecting: false,
                         cloud: cloud, host: .online, observed: observed, runtimeInstall: nil)
    }

    /// An Epic game's session.
    static func epic(account: String, namespace: String = "namespace", appName: String = "game",
                     id: String = UUID().uuidString, phase: GamePhase = .running, cloud: CloudStatus = .idle,
                     observed: Bool = true) -> Self {
        GameSessionState(id: id, game: .epic(accountId: account, namespace: namespace, appName: appName),
                         phase: phase, reconnecting: false, cloud: cloud, host: nil, observed: observed,
                         runtimeInstall: nil)
    }
}

extension CloudStatus {
    static let idle = CloudStatus(phase: .idle, decisionId: 0, error: nil, allowedChoices: [])

    /// A question the sync waits on: a conflict, or a failed launch check.
    static func asking(_ phase: CloudPhase, decision: UInt64, error: String? = nil) -> CloudStatus {
        let choices: [CloudChoice] = switch phase {
        case .conflict: [.local, .cloud, .cancel]
        case .failed: [.retry, .cancel]
        default: []
        }
        return CloudStatus(phase: phase, decisionId: decision, error: error, allowedChoices: choices)
    }

    static func showing(_ phase: CloudPhase, error: String? = nil) -> CloudStatus {
        CloudStatus(phase: phase, decisionId: 0, error: error, allowedChoices: [])
    }
}

extension GameSessions {
    /// Game sessions whose state the test publishes through `fake`. No Mac
    /// app of the test's machine is ever asked to quit.
    convenience init(quitting: GameQuitting = GameQuitting(quitMacApps: { _ in false })) {
        self.init(FakeSessions(), history: .temporary(), quitting: quitting)
    }

    var fake: FakeSessions { core as! FakeSessions }
}

/// A Quit's waits, which end only when the test wakes them, in order, and
/// the Mac apps it asked to quit.
@MainActor
final class FakeQuitting {
    private var sleeping: [CheckedContinuation<Void, Never>] = []
    /// What each wait was for, in order.
    private(set) var delays: [Duration] = []
    /// The folders of each Quit asked of the Mac apps running from them.
    private(set) var macApps: [[String]] = []
    /// Whether a Mac app ran from them and was asked.
    var macAppRuns = false

    var quitting: GameQuitting {
        GameQuitting(
            sleep: { [self] delay in await self.sleep(delay) },
            quitMacApps: { [self] folders in
                self.macApps.append(folders)
                return self.macAppRuns
            }
        )
    }

    var waiting: Int { sleeping.count }

    func wake() { sleeping.removeFirst().resume() }

    private func sleep(_ delay: Duration) async {
        delays.append(delay)
        await withCheckedContinuation { sleeping.append($0) }
    }
}

actor GameSessionGate<Value: Sendable> {
    private var result: Result<Value, Error>?
    private var waiters: [CheckedContinuation<Value, Error>] = []
    private(set) var requests = 0

    func wait() async throws -> Value {
        requests += 1
        if let result { return try result.get() }
        return try await withCheckedThrowingContinuation { waiters.append($0) }
    }

    func resolve(_ value: Value) { complete(.success(value)) }
    func fail(_ error: Error) { complete(.failure(error)) }

    private func complete(_ value: Result<Value, Error>) {
        result = value
        let pending = waiters
        waiters.removeAll()
        for waiter in pending { waiter.resume(with: value) }
    }
}

/// Waits until `predicate` holds, or fails the test after three seconds.
@MainActor
func eventually(_ predicate: @MainActor () async -> Bool, file: StaticString = #filePath,
                line: UInt = #line) async throws {
    let deadline = ContinuousClock.now + .seconds(3)
    while !(await predicate()) && ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(10)) }
    let satisfied = await predicate()
    if !satisfied {
        XCTFail("Fixture timed out", file: file, line: line)
        throw SteamError.Failure(reason: "Fixture timed out")
    }
}

/// Lets the main queue run `times` turns, such as a continuation the code
/// under test scheduled there.
@MainActor
func drainMainQueue(times: Int = 1) async {
    for _ in 0..<times { await withCheckedContinuation { c in DispatchQueue.main.async { c.resume() } } }
}

extension LaunchHistory {
    /// A history no other test reads, never the live library's.
    static func temporary() -> LaunchHistory {
        LaunchHistory(cache: LibraryCache(
            directory: FileManager.default.temporaryDirectory.appending(path: "ObeliskLibraryCache-\(UUID().uuidString)").path
        ))
    }
}

/// Achievements as the Library cache keeps them, read in French: a hundred,
/// `share` of them unlocked.
private func keptAchievements(share: Double) -> [String: Any] {
    let unlocked = Int((share * 100).rounded())
    let list = (0..<100).map { index -> [String: Any] in
        ["id": "\(index)", "name": "", "description": "", "iconUrl": "", "hidden": false,
         "unlocked": index < unlocked, "unlockedAt": NSNull()]
    }
    return ["list": list, "locale": "french", "at": 0, "playtime": NSNull()]
}

extension LibraryCache {
    /// The cache of `directory` once it holds these libraries, in the file as
    /// Obelisk writes it: Steam's by account ID, Epic's by account.
    static func holding(steam: [UInt64: [CachedSteamGame]] = [:], epic: [String: [CachedEpicGame]] = [:],
                        gog: [String: [CachedGogGame]] = [:], in directory: URL) throws -> LibraryCache {
        let steam = steam.map { account, games in
            (String(account), games.map { cached -> [String: Any] in
                var entry: [String: Any] = [
                    "id": String(cached.game.appId), "appId": cached.game.appId, "name": cached.game.name,
                    "iconHash": cached.game.iconHash, "lastPlayed": cached.game.lastPlayed,
                    "playtimeMinutes": cached.game.playtimeMinutes,
                ]
                entry["platforms"] = cached.platforms
                entry["addedAt"] = cached.addedAt
                entry["achievements"] = cached.achievementShare.map(keptAchievements(share:))
                return entry
            })
        }
        let epic = epic.mapValues { games in
            games.map { cached -> [String: Any] in
                let game = cached.game
                var entry: [String: Any] = [
                    "id": game.libraryID, "appName": game.appName, "namespace": game.namespace,
                    "catalogItemId": game.catalogItemId, "title": game.title, "description": game.description,
                    "developer": game.developer, "windows": game.windows, "macos": game.macos,
                ]
                entry["imageUrl"] = game.imageUrl
                entry["headerImageUrl"] = game.headerImageUrl
                entry["playtimeSeconds"] = cached.playtimeSeconds
                entry["achievements"] = cached.achievementShare.map(keptAchievements(share:))
                return entry
            }
        }
        let gog = gog.mapValues { games in
            games.map { cached -> [String: Any] in
                let game = cached.game
                var entry: [String: Any] = [
                    "id": game.productId, "productId": game.productId, "title": game.title,
                    "windows": game.windows, "macos": game.macos,
                ]
                entry["imageUrl"] = game.imageUrl
                entry["headerImageUrl"] = game.headerImageUrl
                entry["playtimeMinutes"] = cached.playtimeMinutes
                entry["achievements"] = cached.achievementShare.map(keptAchievements(share:))
                return entry
            }
        }
        let document: [String: Any] = ["version": 1, "steam": Dictionary(uniqueKeysWithValues: steam), "epic": epic,
                                       "gog": gog]
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try JSONSerialization.data(withJSONObject: document).write(to: directory.appending(path: "library-cache.json"))
        return LibraryCache(directory: directory.path)
    }
}
