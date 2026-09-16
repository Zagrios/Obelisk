import Foundation
@testable import Forge

// Older library/download fixtures have no social events. Suspend until the
// model cancels instead of making a network request or polling a fake cache.
extension SteamSessionProtocol {
    func nextFriends(afterRevision: UInt64) async -> SteamFriendsSnapshot {
        let stream = AsyncStream<Void>(bufferingPolicy: .bufferingNewest(1)) { _ in }
        for await _ in stream {}
        return SteamFriendsSnapshot(revision: afterRevision + 1, accountId: 0, connection: .disconnected, friends: [])
    }
    func shutdown() {}
}
