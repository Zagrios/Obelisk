import Foundation
@testable import SailOver

// Older library/download fixtures have no social events. Suspend until the
// model cancels instead of making a network request or polling a fake cache.
extension SteamSessionProtocol {
    func achievements(appId: UInt32, language: String) async throws -> [SteamAchievement] {
        throw CocoaError(.featureUnsupported)
    }
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

// Models take launches and Cloud maintenance as injected closures, so a fake
// session never has to build a Rust-backed object.
extension EpicSessionProtocol {
    func archiveCloud(configuration: EpicCloudConfiguration) async throws {}
    func maintainCloud(configuration: EpicCloudConfiguration) async -> CloudMaintenance {
        fatalError("Tests inject Cloud maintenance into EpicModel")
    }

    func achievements(namespace: String, locale: String) async throws -> [EpicAchievement] {
        throw CocoaError(.featureUnsupported)
    }
}
