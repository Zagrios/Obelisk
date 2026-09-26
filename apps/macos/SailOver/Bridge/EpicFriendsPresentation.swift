import Foundation

extension FriendsPresentationState {
    static func epic(accountID: String) -> Self {
        FriendsPresentationState(
            store: FriendsStoreIdentity(id: "epic", name: L10n.string("ui.store.epic"), logoAssetName: "EpicGamesLogo"),
            account: FriendsAccountIdentity(storeID: "epic", accountID: accountID),
            friends: [], connection: .loading
        )
    }

    mutating func apply(_ snapshot: EpicFriendsSnapshot) {
        guard account.storeID == "epic", snapshot.accountId == account.accountID else { return }
        // The Rust service keeps its last full snapshot during reconnects.
        if snapshot.connection != .loading || friends.isEmpty {
            friends = snapshot.friends.map { friend in
                let presence: FriendPresence = switch friend.presence {
                case .unknown: .unknown
                case .offline: .offline
                case .online: .online
                case .away: .away
                case .busy: .busy
                case .extendedAway: .snooze
                case .invisible: .invisible
                }
                return FriendPresentation(
                    id: FriendIdentity(account: account, friendID: friend.accountId),
                    name: friend.displayName, avatarURL: friend.avatarUrl.flatMap(URL.init(string:)),
                    presence: presence, activity: friend.activity.map { FriendGameActivity(id: $0.id, name: $0.name) }
                )
            }
        }
        connection = switch snapshot.connection {
        case .loading: friends.isEmpty ? .loading : .interrupted
        case .ready: .connected
        case .interrupted, .stopped: .interrupted
        case .failed, .forbidden: .failed(snapshot.message ?? "model.friends.unavailable")
        case .authenticationRequired: .authenticationRequired
        }
        if snapshot.connection == .ready && !friends.contains(where: { $0.presence == .unknown }) {
            lastKnownOnlineCount = onlineCount
        }
    }
}
