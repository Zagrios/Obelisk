import Foundation

/// Only the refresh token is persisted. Access tokens remain in the Rust session.
struct GogKeychain: CredentialStore {
    private let store: KeychainStore<StoredGogCredentials>

    /// The item of `slot`'s account.
    init(slot: AccountSlot = AccountSlot(.gog), service: String = AppIdentity.keychainService + ".gog") {
        store = KeychainStore(service: service, account: "gog-session" + slot.suffix,
                              label: L10n.string("model.brand.gog"))
    }

    func load() async throws -> WebAccountCredentials? {
        try await store.load().map {
            WebAccountCredentials(accountId: $0.userId, name: $0.username, refreshToken: $0.refreshToken)
        }
    }

    func save(_ credentials: WebAccountCredentials) async throws {
        try await store.save(StoredGogCredentials(
            userId: credentials.accountId,
            username: credentials.name,
            refreshToken: credentials.refreshToken
        ))
    }

    func remove() async throws {
        try await store.remove()
    }
}

/// The item as the Keychain keeps it, under GOG's own names.
private struct StoredGogCredentials: Codable {
    let userId: String
    let username: String
    let refreshToken: String
}
