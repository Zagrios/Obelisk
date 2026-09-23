import Foundation

/// One Cloud maintenance attempt as a storefront model presents it. While it
/// exists, the game holds its Reservation.
struct CloudMaintenanceState<Game: Equatable>: Equatable {
    let game: Game
    var status: CloudMaintenanceStatus?

    /// The conflict question the attempt waits on, if any.
    var conflict: CloudMaintenanceStatus? {
        guard let status, status.phase == .conflict, !status.allowedChoices.isEmpty else { return nil }
        return status
    }

    var isSyncing: Bool { conflict == nil }
}

extension CloudMaintenanceChoice {
    init?(_ choice: SteamCloudChoice) {
        switch choice {
        case .local: self = .local
        case .cloud: self = .cloud
        case .cancel: self = .cancel
        case .retry, .launchAnyway: return nil
        }
    }

    init?(_ choice: EpicCloudChoice) {
        switch choice {
        case .local: self = .local
        case .cloud: self = .cloud
        case .cancel: self = .cancel
        case .retry, .launchAnyway: return nil
        }
    }
}

/// Follows an attempt's revisioned status until it finishes, or until
/// `update` reports that the model no longer presents it.
@MainActor
func followCloudMaintenance(_ attempt: any CloudMaintenanceProtocol,
                            update: @MainActor (CloudMaintenanceStatus) -> Bool) async {
    var after: UInt64?
    while true {
        let status = await attempt.status(after: after)
        guard update(status), !status.finished else { return }
        after = status.revision
    }
}
