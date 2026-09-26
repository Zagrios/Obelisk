import Foundation

protocol EpicCredentialStore: Sendable {
    func load() async throws -> EpicCredentials?
    func save(_ credentials: EpicCredentials) async throws
    func remove() async throws
}

/// Only the refresh token is persisted. Access tokens remain in the Rust session.
struct EpicKeychain: EpicCredentialStore {
    private let store: KeychainStore<StoredEpicCredentials>

    init(service: String = AppIdentity.storageIdentifier + ".epic") {
        store = KeychainStore(service: service, account: "epic-session", label: L10n.string("model.brand.epic"))
    }

    func load() async throws -> EpicCredentials? {
        try await store.load().map {
            EpicCredentials(accountId: $0.accountId, displayName: $0.displayName, refreshToken: $0.refreshToken)
        }
    }

    func save(_ credentials: EpicCredentials) async throws {
        try await store.save(StoredEpicCredentials(
            accountId: credentials.accountId,
            displayName: credentials.displayName,
            refreshToken: credentials.refreshToken
        ))
    }

    func remove() async throws {
        try await store.remove()
    }
}

private struct StoredEpicCredentials: Codable {
    let accountId: String
    let displayName: String
    let refreshToken: String
}
