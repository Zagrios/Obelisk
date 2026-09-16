import AppKit
import Foundation

/// UniFFI 0.32 async wrappers do not propagate Swift cancellation to Rust.
/// This explicit token cancels directory lookup, TLS connection and CM logon.
enum SteamSessionConnection {
    static func connect(_ credentials: SteamCredentials) async throws -> any SteamSessionProtocol {
        let attempt = SteamConnectionAttempt()
        return try await withTaskCancellationHandler {
            try Task.checkCancellation()
            let session = try await attempt.connect(credentials: credentials)
            if Task.isCancelled {
                session.shutdown()
                throw CancellationError()
            }
            return session
        } onCancel: {
            attempt.cancel()
        }
    }
}

extension FriendPresence {
    init(_ presence: SteamFriendPresence) {
        switch presence {
        case .unknown: self = .unknown
        case .offline: self = .offline
        case .online: self = .online
        case .busy: self = .busy
        case .away: self = .away
        case .snooze: self = .snooze
        case .lookingToTrade: self = .lookingToTrade
        case .lookingToPlay: self = .lookingToPlay
        case .invisible: self = .invisible
        }
    }
}

/// NotificationCenter retains observer blocks, so removal follows the model's lifetime.
final class SystemWakeObservation: @unchecked Sendable {
    private let token: NSObjectProtocol
    init(action: @escaping @Sendable () -> Void) {
        token = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didWakeNotification, object: nil, queue: .main
        ) { _ in action() }
    }
    deinit { NSWorkspace.shared.notificationCenter.removeObserver(token) }
}
