import Foundation
@testable import Forge

// Older library/download fixtures have no social events. Suspend until the
// model cancels instead of making a network request or polling a fake cache.
extension SteamSessionProtocol {
    func archiveCloud(config: SteamCloudConfiguration) async throws {}
    func platforms(appIds: [UInt32]) async throws -> [SteamGamePlatforms] { [] }
    #if DEBUG
    func cloudAutocloudMetadata() async throws -> String {
        throw CocoaError(.featureUnsupported)
    }
    func cloudSdkCandidates() async throws -> String {
        throw CocoaError(.featureUnsupported)
    }
    func cloudInterruptionProbe(appId: UInt32, directory: String) async throws -> String {
        throw CocoaError(.featureUnsupported)
    }

    func cloudTransferProbe(appId: UInt32, directory: String) async throws -> String {
        throw CocoaError(.featureUnsupported)
    }
    #endif

    func cloudPreferences(appId: UInt32) async throws -> String {
        throw CocoaError(.featureUnsupported)
    }

    func cloudBackup(appId: UInt32, directory: String) async throws -> String {
        throw CocoaError(.featureUnsupported)
    }

    func nextFriends(afterRevision: UInt64) async -> SteamFriendsSnapshot {
        let stream = AsyncStream<Void>(bufferingPolicy: .bufferingNewest(1)) { _ in }
        for await _ in stream {}
        return SteamFriendsSnapshot(revision: afterRevision + 1, accountId: 0, connection: .disconnected, friends: [])
    }
    func shutdown() {}
}
