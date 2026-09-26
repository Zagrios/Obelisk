import Foundation
@testable import SailOver

/// Records Cloud indicator reads and maintenance starts for both storefronts.
@MainActor
final class CloudServicesFixture {
    struct IndicatorRequest: Sendable { let root: String; let accountId: UInt64 }
    struct SteamStart { let credentials: SteamCredentials; let request: SteamCloudMaintenanceRequest; let attempt: CloudMaintenanceFixture }
    struct EpicStart { let configuration: EpicCloudConfiguration; let attempt: CloudMaintenanceFixture }

    /// Indicator reads run off the main actor, so they are recorded under a lock.
    private let reads = LockedList<IndicatorRequest>()
    var steamRequests: [IndicatorRequest] { reads.values }
    private(set) var steamMaintenance: [SteamStart] = []
    private(set) var epicMaintenance: [EpicStart] = []
    var steamIndicator = CloudIndicator(state: .synced, gameSession: false, syncedAt: 1_789_000_000,
                                        pendingUpload: false, installationRemoved: false)

    var steam: SteamCloudServices {
        let indicator = steamIndicator
        let reads = reads
        return SteamCloudServices(indicators: { root, account, apps in
            reads.append(IndicatorRequest(root: root, accountId: account))
            return apps.map { SteamCloudIndicator(appId: $0, indicator: indicator) }
        }, startMaintenance: { [weak self] credentials, request in
            let attempt = CloudMaintenanceFixture()
            self?.steamMaintenance.append(SteamStart(credentials: credentials, request: request, attempt: attempt))
            return attempt
        })
    }

    func epic(_ indicator: CloudIndicator, configuration: EpicCloudConfiguration? = nil) -> EpicCloudServices {
        EpicCloudServices(indicators: { _, _, games in
            games.map { EpicCloudIndicator(namespace: $0.namespace, appName: $0.appName, indicator: indicator,
                                           configuration: configuration) }
        })
    }

    func startEpicMaintenance(_ configuration: EpicCloudConfiguration) -> any CloudMaintenanceProtocol {
        let attempt = CloudMaintenanceFixture()
        epicMaintenance.append(EpicStart(configuration: configuration, attempt: attempt))
        return attempt
    }
}

final class LockedList<Value: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: [Value] = []
    var values: [Value] { lock.withLock { stored } }
    func append(_ value: Value) { lock.withLock { stored.append(value) } }
}
