import Foundation

enum AppIdentity {
    // Keep the display name independent of storage identifiers for a future rename.
    static let name = Bundle.main.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String ?? "Forge"
    static let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.1.0"
    static let storageIdentifier = "com.zagrios.forge"

    static var libraryDirectory: URL {
        // The override is for isolated development and integration tests.
        if let path = ProcessInfo.processInfo.environment["FORGE_LIBRARY_DIRECTORY"], !path.isEmpty {
            return URL(fileURLWithPath: path, isDirectory: true)
        }
        return URL.applicationSupportDirectory.appending(path: storageIdentifier, directoryHint: .isDirectory)
    }
}

extension ForgeLocations {
    /// Every path of a Forge whose library lives in `library`. Tests pass their own.
    static func of(library: URL) -> ForgeLocations {
        forgeLocations(library: library.path, bundle: Bundle.main.bundleURL.path, home: NSHomeDirectory())
    }

    @MainActor static let live = of(library: AppIdentity.libraryDirectory)
}
