import Foundation
import Observation

/// A game of an account's library, from what its own library entry tells.
/// What the entry does not tell loads into the library.
protocol LibraryGame: Identifiable, Equatable where ID: Comparable & Sendable {
    var title: String { get }
    /// How long the account played the game, as its entry tells; nil when it does not.
    var playtime: Duration? { get }
    /// When the account last played the game on its storefront; nil when the entry does not tell.
    var lastPlayedOnStorefront: Date? { get }
    /// What the entry tells that `sort` orders the game by, such as its date
    /// added; nil when it tells nothing.
    func libraryValue(for sort: StorefrontLibrarySort) -> Double?
}

/// A game of the Library cache with what the cache knew apart from its entry.
struct CachedLibraryGame<Game> {
    let game: Game
    var playtime: Duration? = nil
    /// Each order's value, such as Steam's license date or the achievements' share.
    var values: [StorefrontLibrarySort: Double] = [:]
}

/// One storefront account's library (see CONTEXT.md, Account library). Its
/// session pushes what it loads; the library page, Home, the game page, the
/// downloads and the overlay read it. Ordering every game is the costly part:
/// each order is kept until what it reads changes, and a search or a filter
/// only narrows a kept order.
@MainActor
@Observable
final class AccountLibrary<Store: InstallationStorefront, Game: LibraryGame> where Game.ID == Store.GameID {
    /// The games as the storefront lists them.
    private(set) var games: [Game] = [] {
        didSet { changes &+= 1 }
    }
    var searchText = ""
    var filter: StorefrontLibraryFilter = .all
    /// Every storefront opens on its most recently played games.
    var sort: StorefrontLibrarySort = .lastPlayed
    /// The Library cache of an account whose session has not connected yet.
    private(set) var isCached = false

    /// The cached or connected account, whose launches date the games.
    private var account: Store.AccountID? {
        didSet { if account != oldValue { changes &+= 1 } }
    }
    /// Each played game's playtime, once the storefront or the cache told it.
    private var playtimes: [Game.ID: Duration] = [:] {
        didSet { changes &+= 1 }
    }
    /// Each order's values by game, such as Steam's license dates or each
    /// game's achievements: what the Library cache or an earlier load knew
    /// shows until a load replaces it.
    private var values: [StorefrontLibrarySort: [Game.ID: Double]] = [:]
    /// Each order's revision, which changes whenever its values may have.
    private var revisions: [StorefrontLibrarySort: Int] = [:]
    /// Changes with the games, their playtimes and the account whose launches they read.
    private var changes = 0
    private var playtimeLoad = PlaytimeLoad.idle

    private enum PlaytimeLoad { case idle, loading, failed }

    private struct Criteria: Hashable {
        var query: String
        var sort: StorefrontLibrarySort
        var filter: StorefrontLibraryFilter
        /// Each installed game's size, read only by the installed filter and
        /// the order by size.
        var installed: [Game.ID: UInt64]
        /// What the order read apart from the games.
        var inputs: Inputs
    }

    /// What an order reads apart from the games: its values, and for the
    /// order by size, the installed games' sizes.
    private struct Inputs: Hashable {
        var values: Int
        var sizes: [Game.ID: UInt64]
    }

    @ObservationIgnored private let history: LaunchHistory
    @ObservationIgnored private let cache: LibraryCache?
    @ObservationIgnored private let installations: GameInstallations<Store>
    @ObservationIgnored private var keptRevision: Int?
    @ObservationIgnored private var orders: [StorefrontLibrarySort: (inputs: Inputs, games: [Game])] = [:]
    /// A few narrowed results, so Home's and the library page's do not
    /// replace each other on every pass.
    @ObservationIgnored private var narrowed: [Criteria: [Game]] = [:]
    /// The library's featured game, once read: it may have none.
    @ObservationIgnored private var featuredGame: Game??
    @ObservationIgnored private var tasks: [StorefrontLibrarySort: Task<Void, Never>] = [:]
    @ObservationIgnored private var playtimeTask: Task<Void, Never>?

    init(history: LaunchHistory, cache: LibraryCache?, installations: GameInstallations<Store>) {
        self.history = history
        self.cache = cache
        self.installations = installations
    }

    // MARK: Reading

    /// The games the search and the filter keep, in `sort` order.
    var shown: [Game] { games(by: sort, filter: filter, matching: searchText) }

    /// The whole library's featured game: the last played, else the most
    /// played, equal ones by name.
    var featured: Game? {
        keep(revision)
        if let featuredGame { return featuredGame }
        let game: Game?
        if let first = orders[.lastPlayed]?.games.first, lastPlayed(of: first) != nil {
            // The kept last played order already has it first.
            game = first
        } else {
            game = LibraryOrder.featured(games, title: \.title, id: \.id, lastPlayed: lastPlayed(of:),
                                         playtime: playtime(of:))
        }
        featuredGame = .some(game)
        return game
    }

    /// Whether the library shows: once its account connected, or from the
    /// cache while the saved session restores.
    func shows(connected: Bool, restoring: Bool) -> Bool { connected || (isCached && restoring) }

    /// The featured game, unless a search or a filter narrows the library.
    var hero: Game? {
        guard searchText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, filter == .all else { return nil }
        return featured
    }

    /// The whole library, or what `filter` and `query` keep, in `sort` order,
    /// kept between reads.
    func games(by sort: StorefrontLibrarySort, filter: StorefrontLibraryFilter = .all,
               matching query: String = "") -> [Game] {
        // Read every input even when a result is kept, so the library keeps
        // updating after a search, a filter, an installation or new metadata.
        let games = self.games
        keep(revision)
        let installed = filter == .installed || sort == .size ? installedSizes : [:]
        // What only one order reads, such as library dates or the sizes an
        // installation changes, changes only that order.
        let inputs = Inputs(values: revisions[sort, default: 0], sizes: sort == .size ? installed : [:])
        let criteria = Criteria(query: query.trimmingCharacters(in: .whitespacesAndNewlines), sort: sort,
                                filter: filter, installed: installed, inputs: inputs)
        if let kept = narrowed[criteria] { return kept }
        let ordered: [Game]
        if let order = orders[sort], order.inputs == inputs {
            ordered = order.games
        } else {
            ordered = LibraryOrder.order(games, title: \.title, id: \.id, rank: rank(sort, sizes: inputs.sizes))
            orders[sort] = (inputs, ordered)
        }
        let kept = criteria.query.isEmpty && filter == .all ? ordered : ordered.filter { game in
            guard criteria.query.isEmpty || game.title.localizedStandardContains(criteria.query) else { return false }
            switch filter {
            case .all: return true
            case .installed: return criteria.installed[game.id] != nil
            case .played: return playtime(of: game) != nil || lastPlayed(of: game) != nil
            }
        }
        if narrowed.count >= 4 { narrowed.removeAll() }
        narrowed[criteria] = kept
        return kept
    }

    func game(_ id: Game.ID) -> Game? { games.first { $0.id == id } }

    /// How many of the library's games an Installation receipt names: what
    /// the installed filter keeps.
    var installedCount: Int {
        let installed = installedSizes
        return games.count { installed[$0.id] != nil }
    }

    /// The size of each game an Installation receipt names, receipts of one
    /// game added.
    private var installedSizes: [Game.ID: UInt64] {
        Dictionary(installations.receipts.map { ($0.gameID, $0.totalBytes) }, uniquingKeysWith: +)
    }

    /// How long the account played the game; nil until the storefront told
    /// it, or when the game was never played.
    func playtime(of game: Game) -> Duration? {
        (playtimes[game.id] ?? game.playtime).flatMap { $0 > .zero ? $0 : nil }
    }

    /// When the game was last played: on its storefront, or later from
    /// Obelisk by this account.
    func lastPlayed(of game: Game) -> Date? {
        // The cached library's account, while its session connects.
        let launched = account.flatMap { history.lastLaunch(of: game.id, account: $0, in: Store.self) }
        return [game.lastPlayedOnStorefront, launched].compactMap { $0 }.max()
    }

    /// What orders `game` by date added or achievements; nil when nothing
    /// told it. A game the storefront did not answer for has none, as one it
    /// has nothing to tell of.
    func value(of game: Game, for sort: StorefrontLibrarySort) -> Double? {
        values[sort]?[game.id] ?? game.libraryValue(for: sort)
    }

    /// Whether the filter or the order reads the games' playtimes.
    private var usesPlaytime: Bool { filter == .played || sort == .playtime }

    /// What the filter or the order still waits for while it loads.
    var pendingMetadata: String? {
        usesPlaytime && playtimeLoad == .loading ? L10n.string("ui.library.loading_playtime") : nil
    }

    /// What the filter or the order could not load, offered a retry.
    var missingMetadata: String? {
        usesPlaytime && playtimeLoad == .failed ? L10n.string("ui.library.playtime_unavailable") : nil
    }

    // MARK: Loading

    /// Shows the account's Library cache while its session connects, unless
    /// games show. True when shown.
    @discardableResult
    func showCache(of account: Store.AccountID, _ read: (LibraryCache) -> [CachedLibraryGame<Game>]) -> Bool {
        guard games.isEmpty, let cache else { return false }
        let cached = read(cache)
        guard !cached.isEmpty else { return false }
        self.account = account
        isCached = true
        games = cached.map(\.game)
        playtimes = Dictionary(cached.compactMap { entry in entry.playtime.map { (entry.game.id, $0) } },
                               uniquingKeysWith: max)
        for sort in StorefrontLibrarySort.allCases {
            set(Dictionary(cached.compactMap { entry in entry.values[sort].map { (entry.game.id, $0) } },
                           uniquingKeysWith: max), for: sort)
        }
        return true
    }

    /// Hides a cached library whose session did not connect: another account
    /// may sign in next. The cache keeps it. True when one showed.
    @discardableResult
    func dropCache() -> Bool {
        guard isCached else { return false }
        reset()
        account = nil
        isCached = false
        return true
    }

    /// The account's session connected. Keeps the library when it showed this
    /// account's cache, else empties it. True when kept.
    @discardableResult
    func connect(_ account: Store.AccountID) -> Bool {
        let kept = isCached && self.account == account
        if !kept { reset() }
        self.account = account
        isCached = false
        return kept
    }

    /// Shows the games the session listed.
    func show(_ games: [Game]) {
        if self.games != games { self.games = games }
    }

    /// Loads the playtimes through `read`, replacing a load in flight; they
    /// show as loading at once, and a failure keeps what showed and offers
    /// the retry. Playtimes only order and filter the library, so the library
    /// never waits for them. `next` runs once the load ends, unless a newer
    /// load, a connection or the sign-out replaced it.
    func loadPlaytimes(_ read: @escaping @Sendable () async throws -> [Game.ID: Duration],
                       then next: @escaping @MainActor () -> Void) {
        playtimeTask?.cancel()
        playtimeLoad = .loading
        playtimeTask = Task { [weak self] in
            let loaded = try? await read()
            // UniFFI can return after the load was replaced.
            guard !Task.isCancelled, let self else { return }
            self.playtimeTask = nil
            if let loaded {
                if loaded != self.playtimes { self.playtimes = loaded }
                self.playtimeLoad = .idle
            } else {
                self.playtimeLoad = .failed
            }
            next()
        }
    }

    /// Loads `sort`'s values through `read`, unless they are loading.
    func load(_ sort: StorefrontLibrarySort, read: @escaping @Sendable () async throws -> [Game.ID: Double]) {
        guard tasks[sort] == nil else { return }
        tasks[sort] = Task { [weak self] in
            let loaded = try? await read()
            // UniFFI can return after the account was replaced.
            guard !Task.isCancelled, let self else { return }
            self.tasks[sort] = nil
            // A failed load changes nothing: the library's next load asks again.
            if let loaded { self.set(loaded, for: sort) }
        }
    }

    /// Account revocation: forgets the account's Library cache entry and
    /// launches, the games and what is known of them, the search and the
    /// filter. The order stays the player's.
    func forget() {
        if let account {
            cache?.forget(account: Store.account(account))
            history.forget(account: account, in: Store.self)
        }
        reset()
        account = nil
        isCached = false
        searchText = ""
        filter = .all
    }

    // MARK: Implementation

    /// Changes whenever a game, or what every order reads of one, may have.
    private var revision: Int { changes &+ history.revision }

    /// Forgets every kept result once the library's revision changed.
    private func keep(_ revision: Int) {
        guard revision != keptRevision else { return }
        keptRevision = revision
        orders.removeAll()
        narrowed.removeAll()
        featuredGame = nil
    }

    /// What places each game in `sort` order, the greatest first; a game
    /// without one goes after the others. `sizes` are the installed games'.
    private func rank(_ sort: StorefrontLibrarySort, sizes: [Game.ID: UInt64]) -> (Game) -> Double? {
        switch sort {
        case .name: return { _ in nil }
        case .playtime: return { self.playtime(of: $0).map { Double($0.components.seconds) } }
        case .lastPlayed: return { self.lastPlayed(of: $0)?.timeIntervalSince1970 }
        case .size: return { sizes[$0.id].map { Double($0) } }
        case .achievements, .dateAdded: return { self.value(of: $0, for: sort) }
        }
    }

    /// Forgets the games and everything known of them, as another account's
    /// library replaces them.
    private func reset() {
        tasks.values.forEach { $0.cancel() }
        tasks.removeAll()
        playtimeTask?.cancel()
        playtimeTask = nil
        playtimeLoad = .idle
        games = []
        playtimes = [:]
        for sort in values.keys { set([:], for: sort) }
    }

    private func set(_ loaded: [Game.ID: Double], for sort: StorefrontLibrarySort) {
        guard loaded != values[sort, default: [:]] else { return }
        values[sort] = loaded
        revisions[sort, default: 0] &+= 1
    }
}

/// How every library and Home order games: the greatest rank first, a game
/// without one last, equal ones by name, then by ID.
enum LibraryOrder {
    static func order<G, ID: Comparable>(_ games: [G], title: (G) -> String, id: (G) -> ID,
                                         rank: (G) -> Double?) -> [G] {
        keyed(games, title: title, id: id, rank: rank)
            .sorted(by: precedes)
            .map(\.game)
    }

    /// The first of `games` in that order, without ordering the others.
    static func first<G, ID: Comparable>(_ games: [G], title: (G) -> String, id: (G) -> ID,
                                         rank: (G) -> Double?) -> G? {
        keyed(games, title: title, id: id, rank: rank)
            .min(by: precedes)?
            .game
    }

    /// The featured one of `games`: the first by last played when one was
    /// played, else the first by playtime, so every library has one.
    static func featured<G, ID: Comparable>(_ games: [G], title: (G) -> String, id: (G) -> ID,
                                            lastPlayed: (G) -> Date?, playtime: (G) -> Duration?) -> G? {
        if games.contains(where: { lastPlayed($0) != nil }) {
            return first(games, title: title, id: id, rank: { lastPlayed($0)?.timeIntervalSince1970 })
        }
        return first(games, title: title, id: id, rank: { playtime($0).map { Double($0.components.seconds) } })
    }

    /// Merges lists each already in this order; equal games keep the earlier
    /// list's first.
    static func merge<G>(_ orders: [[G]], title: (G) -> String, rank: (G) -> Double?) -> [G] {
        let lists = orders.map { order in order.map { Keyed(game: $0, title: title($0), id: 0, rank: rank($0) ?? -.infinity) } }
        var heads = Array(repeating: 0, count: lists.count)
        var merged: [G] = []
        merged.reserveCapacity(lists.reduce(0) { $0 + $1.count })
        while true {
            var next: Int?
            for list in lists.indices where heads[list] < lists[list].count {
                // Only a game strictly before the earlier lists' heads goes first.
                if let best = next, !precedes(lists[list][heads[list]], lists[best][heads[best]]) { continue }
                next = list
            }
            guard let list = next else { return merged }
            merged.append(lists[list][heads[list]].game)
            heads[list] += 1
        }
    }

    private struct Keyed<G, ID: Comparable> {
        let game: G
        let title: String
        let id: ID
        let rank: Double
    }

    /// Each game's keys, read once rather than at every comparison.
    private static func keyed<G, ID: Comparable>(_ games: [G], title: (G) -> String, id: (G) -> ID,
                                                 rank: (G) -> Double?) -> [Keyed<G, ID>] {
        games.map { Keyed(game: $0, title: title($0), id: id($0), rank: rank($0) ?? -.infinity) }
    }

    private static func precedes<G, ID>(_ left: Keyed<G, ID>, _ right: Keyed<G, ID>) -> Bool {
        if left.rank != right.rank { return left.rank > right.rank }
        let comparison = left.title.localizedStandardCompare(right.title)
        return comparison == .orderedSame ? left.id < right.id : comparison == .orderedAscending
    }
}
