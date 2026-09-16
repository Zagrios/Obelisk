import Foundation
import Security

/// One generic-password Keychain item holding a JSON-encoded value. Items are
/// device-bound (`AfterFirstUnlockThisDeviceOnly`) and never synchronised.
actor KeychainStore<Stored: Codable & Sendable> {
    private let service: String
    private let account: String
    private let label: String

    /// `label` names the stored session in error messages ("Steam", "Epic").
    init(service: String, account: String, label: String) {
        self.service = service
        self.account = account
        self.label = label
    }

    func load() throws -> Stored? {
        var query = baseQuery
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = item as? Data else {
            throw KeychainFailure(label: label, operation: .read, status: status)
        }
        do {
            return try JSONDecoder().decode(Stored.self, from: data)
        } catch {
            throw KeychainFailure(label: label, operation: .read, status: errSecDecode)
        }
    }

    func save(_ value: Stored) throws {
        let data = try JSONEncoder().encode(value)
        let update = [kSecValueData as String: data]
        var status = SecItemUpdate(baseQuery as CFDictionary, update as CFDictionary)
        if status == errSecItemNotFound {
            var query = baseQuery
            query[kSecValueData as String] = data
            query[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
            status = SecItemAdd(query as CFDictionary, nil)
        }
        guard status == errSecSuccess else {
            throw KeychainFailure(label: label, operation: .save, status: status)
        }
    }

    func remove() throws {
        let status = SecItemDelete(baseQuery as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw KeychainFailure(label: label, operation: .remove, status: status)
        }
    }

    private var baseQuery: [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecAttrSynchronizable as String: false,
        ]
    }
}

private struct KeychainFailure: LocalizedError {
    enum Operation {
        case read, save, remove
    }

    let label: String
    let operation: Operation
    let status: OSStatus

    var errorDescription: String? {
        switch operation {
        case .read: L10n.string("model.keychain.readFailed", label, Int64(status))
        case .save: L10n.string("model.keychain.saveFailed", label, Int64(status))
        case .remove: L10n.string("model.keychain.removeFailed", label, Int64(status))
        }
    }
}
