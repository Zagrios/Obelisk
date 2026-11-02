import Foundation
import Observation

extension OnlineStatus: CaseIterable, Identifiable {
    /// In the order the account's menu offers them.
    public static let allCases: [OnlineStatus] = [.online, .away, .invisible]
    public var id: Self { self }

    var title: String {
        switch self {
        case .online: L10n.string("ui.friends.presence.online")
        case .away: L10n.string("ui.friends.presence.away")
        case .invisible: L10n.string("ui.status.invisible")
        }
    }

    /// How it is kept across launches.
    fileprivate var stored: String {
        switch self {
        case .online: "online"
        case .away: "away"
        case .invisible: "invisible"
        }
    }
}

/// How the player shows to their friends on a storefront, the same choice
/// on every storefront that has friends: kept across launches, and told to
/// whatever shows it, the account's session and the storefront's games.
@MainActor
@Observable
final class OnlineStatusChoice {
    private(set) var status: OnlineStatus
    @ObservationIgnored private var followers: [@MainActor (OnlineStatus) -> Void] = []
    @ObservationIgnored private let key: String
    @ObservationIgnored private let defaults: UserDefaults

    init(storeID: String, defaults: UserDefaults = .standard) {
        key = "onlineStatus.\(storeID)"
        self.defaults = defaults
        let stored = defaults.string(forKey: key)
        status = OnlineStatus.allCases.first { $0.stored == stored } ?? .online
    }

    /// Tells `follower` the status now, then each one the player picks.
    func follow(_ follower: @escaping @MainActor (OnlineStatus) -> Void) {
        followers.append(follower)
        follower(status)
    }

    func choose(_ status: OnlineStatus) {
        guard status != self.status else { return }
        self.status = status
        defaults.set(status.stored, forKey: key)
        for follower in followers { follower(status) }
    }
}
