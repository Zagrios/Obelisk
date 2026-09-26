import Foundation

protocol SteamCredentialStore: Sendable {
    func load() async throws -> SteamCredentials?
    func save(_ credentials: SteamCredentials) async throws
    func remove() async throws
}

/// Refresh tokens live only in Keychain. Library files contain no credentials.
struct SteamKeychain: SteamCredentialStore {
    private let store: KeychainStore<StoredCredentials>

    init(service: String = AppIdentity.storageIdentifier + ".steam") {
        store = KeychainStore(service: service, account: "steam-session", label: L10n.string("model.brand.steam"))
    }

    func load() async throws -> SteamCredentials? {
        try await store.load().map {
            SteamCredentials(accountName: $0.accountName, steamId: $0.steamId, refreshToken: $0.refreshToken)
        }
    }

    func save(_ credentials: SteamCredentials) async throws {
        try await store.save(StoredCredentials(
            accountName: credentials.accountName,
            steamId: credentials.steamId,
            refreshToken: credentials.refreshToken
        ))
    }

    func remove() async throws {
        try await store.remove()
    }
}

private struct StoredCredentials: Codable {
    let accountName: String
    let steamId: UInt64
    let refreshToken: String
}
