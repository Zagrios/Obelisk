import Foundation

/// Only the refresh token is persisted. Access tokens remain in the Rust session.
struct EpicKeychain: CredentialStore {
    private let store: KeychainStore<StoredEpicCredentials>

    init(service: String = AppIdentity.keychainService + ".epic") {
        store = KeychainStore(service: service, account: "epic-session", label: L10n.string("model.brand.epic"))
    }

    func load() async throws -> WebAccountCredentials? {
        try await store.load().map {
            WebAccountCredentials(accountId: $0.accountId, name: $0.displayName, refreshToken: $0.refreshToken)
        }
    }

    func save(_ credentials: WebAccountCredentials) async throws {
        try await store.save(StoredEpicCredentials(
            accountId: credentials.accountId,
            displayName: credentials.name,
            refreshToken: credentials.refreshToken
        ))
    }

    func remove() async throws {
        try await store.remove()
    }
}

/// The item as the Keychain keeps it, under Epic's own names.
private struct StoredEpicCredentials: Codable {
    let accountId: String
    let displayName: String
    let refreshToken: String
}
