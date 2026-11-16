import Foundation

enum AppIdentity {
    // Keep the display name independent of storage identifiers for a future rename.
    static let name = Bundle.main.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String ?? "Obelisk"
    static let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.1.0"
    static let storageIdentifier = "com.zagrios.obelisk"
    /// Obelisk's repository on GitHub.
    static let repository = URL(string: "https://github.com/Zagrios/obelisk")!
    /// Whether this process is the tests' host rather than an Obelisk of its
    /// own: it leaves the player's accounts, games and settings alone.
    static let isTestHost = ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil

    /// Prefix of the Keychain items sessions are saved in. Debug builds are signed by another
    /// team than releases, and macOS asks for the login password each time one team's build
    /// reads an item the other created, so they keep their own items.
    static var keychainService: String {
        #if DEBUG
        storageIdentifier + ".debug"
        #else
        storageIdentifier
        #endif
    }

    static var libraryDirectory: URL {
        // The override is for isolated development and integration tests.
        if let path = ProcessInfo.processInfo.environment["OBELISK_LIBRARY_DIRECTORY"], !path.isEmpty {
            return URL(fileURLWithPath: path, isDirectory: true)
        }
        return URL.applicationSupportDirectory.appending(path: storageIdentifier, directoryHint: .isDirectory)
    }
}

extension ObeliskLocations {
    /// Every path of an Obelisk whose library lives in `library`. Tests pass their own.
    static func of(library: URL) -> ObeliskLocations {
        obeliskLocations(library: library.path, bundle: Bundle.main.bundleURL.path, home: NSHomeDirectory())
    }

    @MainActor static let live = of(library: AppIdentity.libraryDirectory)
}
