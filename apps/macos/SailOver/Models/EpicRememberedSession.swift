import Foundation
import Observation

/// Coordinates refresh credentials for one active Epic account. OAuth renewal stays in Rust.
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
    @ObservationIgnored private var observation: Task<Void, Never>?
    @ObservationIgnored private var storageTask: Task<Void, Never>?

    init(store: any EpicCredentialStore) { self.store = store }

    deinit {
        observation?.cancel()
        account?.session.shutdownObservers()
    }

    func load() async throws -> EpicCredentials? {
        await storageTask?.value
        return try await store.load()
    }

    func activate(_ session: any EpicSessionProtocol, accountID: String) async {
        observation?.cancel()
        revision = UUID()
        let active = Account(session: session, id: accountID, revision: revision)
        account = active
        observation = Task { [weak self] in
            await withTaskCancellationHandler {
                var after: UInt64 = 0
                while !Task.isCancelled {
                    guard let updated = await session.nextCredentialsRevision(afterRevision: after),
                          self?.revision == active.revision, !Task.isCancelled else { return }
                    after = updated
                    await self?.persist(active)
                }
            } onCancel: { session.shutdownObservers() }
        }
        await persist(active)
    }

    /// Prepare and save before the request unless its session was already prepared by the transfer.
    /// Save on every exit, including cancellation.
    /// The request keeps its caller's cancellation, while storage runs in its own ordered tasks.
    func perform<Value>(
        on session: any EpicSessionProtocol,
        preparing: Bool = true,
        _ operation: @MainActor () async throws -> Value
    ) async throws -> Value {
        guard let active = account, active.session === session else { throw CancellationError() }
        let result: Result<Value, Error>
        do {
            try requireCurrent(active)
            if preparing {
                _ = try await session.prepare()
                await persist(active)
                try requireCurrent(active)
            }
            result = .success(try await operation())
        } catch {
            result = .failure(error)
        }
        await persist(active)
        try requireCurrent(active)
        return try result.get()
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
        observation?.cancel()
        observation = nil
        account?.session.shutdownObservers()
        account = nil
        revision = UUID()
        storageWarning = nil
        return enqueueWrite(removing: true)
    }

    private func requireCurrent(_ active: Account) throws {
        guard revision == active.revision, !Task.isCancelled else { throw CancellationError() }
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

/// Saves an Epic launch's rotated credentials before its game starts. Rust
/// waits on its own thread while the save runs on the main actor.
final class RememberedCredentials: BeforeGameStart, @unchecked Sendable {
    private let remembered: EpicRememberedSession
    private let session: any EpicSessionProtocol

    init(_ remembered: EpicRememberedSession, session: any EpicSessionProtocol) {
        self.remembered = remembered
        self.session = session
    }

    func run() {
        let semaphore = DispatchSemaphore(value: 0)
        let (remembered, session) = (remembered, session)
        Task { @MainActor in
            await remembered.save(on: session)
            semaphore.signal()
        }
        semaphore.wait()
    }
}
