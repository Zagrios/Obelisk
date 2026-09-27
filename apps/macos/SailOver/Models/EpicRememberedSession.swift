import Foundation
import Observation

/// Keeps one active Epic account's refresh credentials in the Keychain. OAuth
/// renewal stays in Rust, which asks for each rotation to be saved, in order.
@MainActor
@Observable
final class EpicRememberedSession {
    var rememberSession = true {
        didSet {
            guard rememberSession != oldValue else { return }
            // Enqueue synchronously so a later account change also orders this preference change.
            _ = enqueueWrite()
        }
    }
    var storageWarning: String?

    private struct Account: Sendable {
        let session: any EpicSessionProtocol
        let id: String
        let revision: UUID
    }

    @ObservationIgnored private let store: any EpicCredentialStore
    @ObservationIgnored private var account: Account?
    @ObservationIgnored private var revision = UUID()
    @ObservationIgnored private var storageTask: Task<Void, Never>?

    init(store: any EpicCredentialStore) { self.store = store }

    deinit {
        account?.session.shutdownObservers()
    }

    func load() async throws -> EpicCredentials? {
        await storageTask?.value
        return try await store.load()
    }

    /// Makes `session` the active account. Rust saves its credentials now and
    /// after every rotation, through this.
    func activate(_ session: any EpicSessionProtocol, accountID: String) async {
        revision = UUID()
        let active = Account(session: session, id: accountID, revision: revision)
        account = active
        await session.persist(credentials: RememberedCredentials(self, session: session))
        // The account is saved before it is used.
        await persist(active)
    }

    /// Saves the credentials of `session` now, when it is still the active one.
    func save(on session: any EpicSessionProtocol) async {
        guard let active = account, active.session === session else { return }
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
                    let latest: EpicCredentials
                    do {
                        // Resolve only after previous writes, never from a queued prepare result.
                        latest = try await active.session.credentials()
                    } catch {
                        if expected == revision { storageWarning = L10n.string("model.epic.sessionNotSaved") }
                        return
                    }
                    guard expected == revision else { return }
                    if rememberSession {
                        guard latest.accountId == active.id else {
                            storageWarning = L10n.string("model.epic.sessionNotSaved")
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

/// Saves the credentials Epic account calls rotate, such as a launch's
/// exchange before its game starts. Rust waits on its own thread while the
/// save runs on the main actor. Rust holds it, so it leaves the remembered
/// session free to go.
final class RememberedCredentials: EpicCredentialSave, @unchecked Sendable {
    private weak var remembered: EpicRememberedSession?
    private let session: any EpicSessionProtocol

    init(_ remembered: EpicRememberedSession, session: any EpicSessionProtocol) {
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
