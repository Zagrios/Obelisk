import Foundation
import Observation

/// One storefront account's place in Obelisk, whether or not an account is
/// signed in there: its row in the sidebar, and where its saved session is
/// kept. Each storefront has one from the start, the one every Obelisk had
/// while a storefront held a single account; the player adds the others.
struct AccountSlot: Hashable, Sendable, Identifiable {
    let section: LibrarySection
    /// Empty for the storefront's original slot; a UUID for one the player added.
    let name: String

    init(section: LibrarySection, name: String) {
        self.section = section
        self.name = name
    }

    /// The storefront's original slot.
    init(_ section: LibrarySection) {
        self.init(section: section, name: "")
    }

    /// A slot the player adds to `section`.
    static func added(to section: LibrarySection) -> AccountSlot {
        AccountSlot(section: section, name: UUID().uuidString)
    }

    /// Names the slot among every storefront's: as its storefront, for the
    /// original one.
    var id: String { section.rawValue + suffix }

    /// What tells the slot's Keychain item from the original slot's, which
    /// keeps the name it always had.
    var suffix: String { name.isEmpty ? "" : "-" + name }
}

/// What the sidebar shows: Home, one storefront account's library, or Settings.
enum LibraryTab: Hashable, Sendable {
    case home
    case account(AccountSlot)
    case settings

    /// The tab's section: its account's storefront, or Obelisk's own.
    var section: LibrarySection {
        switch self {
        case .home: .home
        case .account(let slot): slot.section
        case .settings: .settings
        }
    }

    /// The slot whose library the tab shows.
    var slot: AccountSlot? {
        if case .account(let slot) = self { slot } else { nil }
    }
}

/// One storefront's accounts, whichever storefront it is: `AccountList`,
/// which the app also reaches without naming the storefront's model.
@MainActor
protocol AccountListing: AnyObject {
    associatedtype Model: StorefrontAccount
    /// In the order the player added them, which is the sidebar's. There is
    /// always one, signed in or not.
    var accounts: [Model] { get }
    /// The account of `slot`; nil once its slot went.
    subscript(slot: AccountSlot) -> Model? { get }
    /// The account signed in as `id`, as its storefront names it.
    func account(signedInAs id: String) -> Model?
    /// The name the account of `slot` last showed, while it shows none.
    func rememberedName(of slot: AccountSlot) -> String?
    /// Told each slot that went, once its account signed out.
    var onRemoved: (@MainActor (AccountSlot) -> Void)? { get set }
    /// A slot for another account, signed out: a new one, or the one that
    /// already holds nothing.
    func add() -> Model
    /// The player signs the account of `slot` out. Its slot goes with it,
    /// unless it is the storefront's last, which then offers its sign-in.
    func signOut(_ slot: AccountSlot)
    /// Takes away the slots that hold nothing: an account the player added
    /// and left without signing in, or one whose saved session is gone.
    /// `shown`, the slot whose tab shows, stays, as does a slot whose
    /// failure the player has not read yet, and the storefront's last slot.
    func removeVacant(keeping shown: AccountSlot?)
}

/// One storefront's accounts and the rules of their slots. The slots are
/// kept across launches, each with the name its account last showed, so the
/// sidebar names every account before its session restores.
@MainActor
@Observable
final class AccountList<Model: StorefrontAccount & SendableMetatype>: AccountListing {
    let section: LibrarySection
    private(set) var accounts: [Model]
    /// The name each slot's account last showed.
    private var names: [AccountSlot: String]
    @ObservationIgnored var onRemoved: (@MainActor (AccountSlot) -> Void)?

    @ObservationIgnored private let make: @MainActor (AccountSlot) -> Model
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private let key: String

    /// The storefront's saved slots, each with the account `make` gives it.
    /// `first` stands for the account of its own slot, which comes first:
    /// tests pass the one they drive.
    init(_ section: LibrarySection, first: Model? = nil, defaults: UserDefaults = .standard,
         make: @escaping @MainActor (AccountSlot) -> Model) {
        self.section = section
        self.make = make
        self.defaults = defaults
        key = "obelisk.accounts.\(section.rawValue)"
        let saved = (defaults.array(forKey: key) as? [[String: String]] ?? []).compactMap { entry in
            entry["slot"].map { (slot: AccountSlot(section: section, name: $0), name: entry["name"]) }
        }
        names = Dictionary(saved.compactMap { entry in entry.name.map { (entry.slot, $0) } },
                           uniquingKeysWith: { first, _ in first })
        var seen: Set<AccountSlot> = []
        var slots = saved.map(\.slot).filter { seen.insert($0).inserted }
        if let first {
            accounts = [first] + slots.filter { $0 != first.slot }.map(make)
        } else {
            if slots.isEmpty { slots = [AccountSlot(section)] }
            accounts = slots.map(make)
        }
        for account in accounts { adopt(account) }
        followNames()
    }

    /// The storefront's first account: there is always one.
    var first: Model { accounts[0] }

    subscript(slot: AccountSlot) -> Model? {
        accounts.first { $0.slot == slot }
    }

    func account(signedInAs id: String) -> Model? {
        accounts.first { $0.accountID == id }
    }

    func rememberedName(of slot: AccountSlot) -> String? { names[slot] }

    func add() -> Model {
        if let vacant = accounts.first(where: \.isVacant) { return vacant }
        let account = make(.added(to: section))
        adopt(account)
        accounts.append(account)
        save()
        return account
    }

    func signOut(_ slot: AccountSlot) {
        guard let account = self[slot] else { return }
        if accounts.count > 1 { remove(account) } else { account.signOut() }
    }

    func removeVacant(keeping shown: AccountSlot?) {
        for account in accounts where accounts.count > 1 {
            guard account.isVacant, account.errorMessage == nil, account.slot != shown else { continue }
            remove(account)
        }
    }

    /// Signs `account` out and takes its slot away.
    private func remove(_ account: Model) {
        let slot = account.slot
        accounts.removeAll { $0 === account }
        names[slot] = nil
        save()
        account.signOut()
        account.retire()
        onRemoved?(slot)
    }

    /// An account signs in once among its storefront's slots.
    private func adopt(_ account: Model) {
        account.isSignedInElsewhere = { [weak self, weak account] id in
            self?.accounts.contains { $0 !== account && $0.accountID == id } ?? false
        }
    }

    /// Remembers the name each account shows, as it shows it.
    private func followNames() {
        let shown = withObservationTracking {
            accounts.map { (slot: $0.slot, name: $0.accountName) }
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in self?.followNames() }
        }
        var names = names
        for case let (slot, name?) in shown where !name.isEmpty { names[slot] = name }
        guard names != self.names else { return }
        self.names = names
        save()
    }

    private func save() {
        defaults.set(accounts.map { account in
            var entry = ["slot": account.slot.name]
            entry["name"] = names[account.slot]
            return entry
        }, forKey: key)
    }
}

extension AccountList where Model: LibraryStorefront {
    /// The storefront's Game sessions, which its accounts share: one of its
    /// games runs at a time.
    var gameSessions: GameSessions<Model.Store> { first.gameSessions }

    /// The account whose game runs, as its Game session names it, else the
    /// storefront's first: one that signed out while its game runs is not
    /// among them any more.
    var playing: Model {
        guard let account = gameSessions.activeGame?.account else { return first }
        return self.account(signedInAs: "\(account)") ?? first
    }
}
