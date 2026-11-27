import Foundation
import Network

/// Whether the Mac has a usable network path, as macOS tells it.
enum NetworkPath {
    private static let monitor = NWPathMonitor()

    /// Starts following the path, whose first state arrives shortly after.
    /// Before that, and when never started, the Mac counts as offline.
    static func start() {
        monitor.start(queue: DispatchQueue(label: "com.zagrios.obelisk.network-path"))
    }

    static var isOnline: Bool { monitor.currentPath.status == .satisfied }

    /// `message`, a store's that it did not answer, with what may block
    /// Obelisk while the Mac is online.
    static func blocked(_ message: String, online: Bool = isOnline) -> String {
        guard online else { return message }
        // Some reasons end in a transport error, without a full stop.
        let open = message.last.map { $0.isLetter || $0.isNumber } ?? false
        return L10n.string("model.network.blocked", open ? message + "." : message)
    }
}
