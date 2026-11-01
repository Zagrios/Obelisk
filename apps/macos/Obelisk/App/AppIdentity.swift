import Foundation

enum AppIdentity {
    // Keep the display name independent of storage identifiers for a future rename.
    static let name = Bundle.main.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String ?? "Obelisk"
    static let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.1.0"
    static let storageIdentifier = "com.zagrios.obelisk"

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

    /// The library Obelisk runs with until it restarts: a move to another
    /// folder applies from the next start.
    static let libraryDirectory = libraryDirectory(environment: ProcessInfo.processInfo.environment,
                                                   defaults: .standard)

    /// Obelisk's own library folder on this Mac.
    static var defaultLibraryDirectory: URL {
        URL.applicationSupportDirectory.appending(path: storageIdentifier, directoryHint: .isDirectory)
    }

    /// The folder the player moved the library to; absent while it is
    /// Obelisk's own. It lives outside the library, which may be on a disk
    /// that is not connected.
    static let libraryDirectoryKey = "obelisk.library.directory"
    /// The library Obelisk left for the one it moved to, which goes once
    /// Obelisk started there.
    static let leftLibraryKey = "obelisk.library.left"

    static func libraryDirectory(environment: [String: String], defaults: UserDefaults) -> URL {
        // The override is for isolated development and integration tests.
        if let path = environment["OBELISK_LIBRARY_DIRECTORY"], !path.isEmpty {
            return URL(fileURLWithPath: path, isDirectory: true)
        }
        if let path = defaults.string(forKey: libraryDirectoryKey), path.hasPrefix("/") {
            return URL(fileURLWithPath: path, isDirectory: true)
        }
        return defaultLibraryDirectory
    }
}

extension ObeliskLocations {
    /// Every path of an Obelisk whose library lives in `library`. Tests pass their own.
    static func of(library: URL) -> ObeliskLocations {
        obeliskLocations(library: library.path, bundle: Bundle.main.bundleURL.path, home: NSHomeDirectory())
    }

    @MainActor static let live = of(library: AppIdentity.libraryDirectory)
}
