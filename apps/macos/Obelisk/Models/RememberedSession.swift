import Foundation
import Observation

/// One storefront account's Keychain item.
protocol CredentialStore<Credentials>: Sendable {
    associatedtype Credentials: Sendable
    func load() async throws -> Credentials?
    func save(_ credentials: Credentials) async throws
    func remove() async throws
}

/// One slot's saved session in the Keychain, and whether it holds one, as
/// its own reads and writes told.
@MainActor
final class SavedSession<Credentials: Sendable> {
    private let store: any CredentialStore<Credentials>
    /// Whether the Keychain holds a session to restore; nil until a read or
    /// a write told, so a read that failed never counts as nothing saved.
    private(set) var isHeld: Bool?

    init(_ store: any CredentialStore<Credentials>) {
        self.store = store
    }

    func load() async throws -> Credentials? {
        let credentials = try await store.load()
        isHeld = credentials != nil
        return credentials
    }

    func save(_ credentials: Credentials) async throws {
        try await store.save(credentials)
        isHeld = true
    }

    func remove() async throws {
        try await store.remove()
        isHeld = false
    }
}

/// A signed-in Rust session whose refresh credentials rotate with its calls,
/// as Epic's and GOG's do. It saves each rotation through `persist`.
struct RotatingSession<Credentials: Sendable>: Sendable {
    /// Tells this session from a later one of the same account.
    let identity: ObjectIdentifier
    let credentials: @Sendable () async throws -> Credentials
    let persist: @Sendable (any CredentialSave) async -> Void
    let shutdownObservers: @Sendable () -> Void
}

/// Keeps one active account's refresh credentials in the Keychain. OAuth
/// renewal stays in Rust, which asks for each rotation to be saved, in order.
@MainActor
@Observable
final class RememberedSession<Credentials: Sendable> {
    var rememberSession = true {
        didSet {
            guard rememberSession != oldValue else { return }
            // Enqueue synchronously so a later account change also orders this preference change.
            _ = enqueueWrite()
        }
    }
    var storageWarning: String?

    private struct Account: Sendable {
        let session: RotatingSession<Credentials>
        let id: String
        let revision: UUID
    }

    @ObservationIgnored private let store: SavedSession<Credentials>
    /// The account the credentials name.
    @ObservationIgnored private let accountID: (Credentials) -> String
    /// Shown when the session could not be saved.
    @ObservationIgnored private let notSaved: String
    @ObservationIgnored private var account: Account?
    @ObservationIgnored private var revision = UUID()
    @ObservationIgnored private var storageTask: Task<Void, Never>?

    init(keychain store: any CredentialStore<Credentials>, accountID: @escaping (Credentials) -> String,
         notSaved: String) {
        self.store = SavedSession(store)
        self.accountID = accountID
        self.notSaved = notSaved
    }

    deinit {
        account?.session.shutdownObservers()
    }

    /// Whether the Keychain holds a session to restore; nil until told.
    var holdsSavedSession: Bool? { store.isHeld }

    func load() async throws -> Credentials? {
        await storageTask?.value
        return try await store.load()
    }

    /// Makes `session` the active account. Rust saves its credentials now and
    /// after every rotation, through this.
    func activate(rotating session: RotatingSession<Credentials>, accountID: String) async {
        revision = UUID()
        let active = Account(session: session, id: accountID, revision: revision)
        account = active
        await session.persist(RememberedCredentials(self, session: session.identity))
        // The account is saved before it is used.
        await persist(active)
    }

    /// Saves the credentials of the session `identity` names now, when it is
    /// still the active one.
    func save(on identity: ObjectIdentifier) async {
        guard let active = account, active.session.identity == identity else { return }
        await persist(active)
    }

    /// Invalidates observations and operations immediately. Removal waits for any in-flight save.
    /// Stopping observation neither revokes OAuth nor cancels a download; the caller owns sign-out.
    @discardableResult
    func forget() -> Task<Void, Never> {
        account?.session.shutdownObservers()
        account = nil
        revision = UUID()
        storageWarning = nil
        return enqueueWrite(removing: true)
    }

    private func persist(_ active: Account) async {
        guard revision == active.revision else { return }
        await enqueueWrite().value
    }

    private func enqueueWrite(removing: Bool = false) -> Task<Void, Never> {
        let previous = storageTask
        let expected = revision
        let task = Task {
            await previous?.value
            guard expected == revision else { return }
            do {
                if rememberSession, let active = account {
                    let latest: Credentials
                    do {
                        // Resolve only after previous writes, never from a queued prepare result.
                        latest = try await active.session.credentials()
                    } catch {
                        if expected == revision { storageWarning = notSaved }
                        return
                    }
                    guard expected == revision else { return }
                    if rememberSession {
                        guard accountID(latest) == active.id else {
                            storageWarning = notSaved
                            return
                        }
                        try await store.save(latest)
                    } else {
                        try await store.remove()
                    }
                } else if removing || !rememberSession {
                    try await store.remove()
                }
                guard expected == revision else { return }
                storageWarning = nil
            } catch {
                guard expected == revision else { return }
                storageWarning = error.localizedDescription
            }
        }
        storageTask = task
        return task
    }
}

/// Saves the credentials account calls rotate, such as a launch's exchange
/// before its game starts. Rust waits on its own thread while the save runs
/// on the main actor. Rust holds it, so it leaves the remembered session free
/// to go.
final class RememberedCredentials<Credentials: Sendable>: CredentialSave, @unchecked Sendable {
    private weak var remembered: RememberedSession<Credentials>?
    private let session: ObjectIdentifier

    init(_ remembered: RememberedSession<Credentials>, session: ObjectIdentifier) {
        self.remembered = remembered
        self.session = session
    }

    func save() {
        let semaphore = DispatchSemaphore(value: 0)
        let (remembered, session) = (remembered, session)
        Task { @MainActor in
            await remembered?.save(on: session)
            semaphore.signal()
        }
        semaphore.wait()
    }
}
