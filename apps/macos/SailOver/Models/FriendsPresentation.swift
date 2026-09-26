import Foundation

/// Store/account scoping is part of identity, including for images and SwiftUI rows.
struct FriendsStoreIdentity: Equatable, Sendable {
    let id: String
    let name: String
    let logoAssetName: String
}

struct FriendsAccountIdentity: Hashable, Sendable {
    let storeID: String
    let accountID: String
}

struct FriendIdentity: Hashable, Sendable {
    let account: FriendsAccountIdentity
    let friendID: String
}

enum FriendPresence: Equatable, Sendable {
    case offline, online, busy, away, snooze, lookingToTrade, lookingToPlay, invisible, unknown

    var isOnline: Bool {
        switch self {
        case .online, .busy, .away, .snooze, .lookingToTrade, .lookingToPlay: true
        case .offline, .invisible, .unknown: false
        }
    }

    var title: String {
        switch self {
        case .offline, .invisible: L10n.string("ui.friends.presence.offline")
        case .online: L10n.string("ui.friends.presence.online")
        case .busy: L10n.string("ui.friends.presence.busy")
        case .away: L10n.string("ui.friends.presence.away")
        case .snooze: L10n.string("ui.friends.presence.snooze")
        case .lookingToTrade: L10n.string("ui.friends.presence.lookingToTrade")
        case .lookingToPlay: L10n.string("ui.friends.presence.lookingToPlay")
        case .unknown: L10n.string("ui.friends.presence.unknown")
        }
    }
}

struct FriendGameActivity: Equatable, Sendable {
    let id: String
    let name: String?

    var title: String {
        let value = name?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return value.isEmpty ? L10n.string("ui.friends.inGameUnknown") : L10n.string("ui.friends.inGame", value)
    }
}

struct FriendPresentation: Identifiable, Equatable, Sendable {
    let id: FriendIdentity
    let name: String?
    let avatarURL: URL?
    let presence: FriendPresence
    let activity: FriendGameActivity?

    var displayName: String {
        let value = name?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return value.isEmpty ? L10n.string("ui.friends.unknownName") : value
    }

    var initials: String { name?.initials ?? "" }

    var status: String {
        [presence.title, activity?.title].compactMap { $0 }.joined(separator: " · ")
    }
}

enum FriendsConnectionState: Equatable, Sendable {
    case loading, connected, interrupted, failed(String), authenticationRequired
}

struct FriendsPresentationState: Equatable, Sendable {
    let store: FriendsStoreIdentity
    let account: FriendsAccountIdentity
    var friends: [FriendPresentation]
    var connection: FriendsConnectionState
    var lastKnownOnlineCount: Int? = nil

    var title: String { L10n.string("ui.friends.title", store.name) }
    var onlineCount: Int { friends.lazy.filter { $0.presence.isOnline }.count }
    var onlineSummary: String {
        let count = onlineCount
        return count > 0
            ? L10n.string("ui.friends.onlineCount", Int64(count))
            : L10n.string("ui.friends.totalCount", Int64(friends.count))
    }

    var menuTitle: String {
        switch connection {
        case .connected: onlineCount > 0 ? L10n.string("ui.friends.menu", onlineSummary) : onlineSummary
        case .loading: L10n.string("ui.friends.menuLoading")
        case .interrupted: L10n.string("ui.friends.menuInterrupted")
        case .failed: L10n.string("ui.friends.menuUnavailable")
        case .authenticationRequired: L10n.string("ui.friends.menuAuthenticationRequired")
        }
    }

    var headerSummary: String {
        if connection == .connected { return onlineSummary }
        if connection == .loading { return L10n.string("ui.friends.loading") }
        guard let lastKnownOnlineCount else { return summary }
        return L10n.string("ui.friends.lastKnownCount", L10n.string("ui.friends.onlineCount", Int64(lastKnownOnlineCount)))
    }

    var summary: String {
        switch connection {
        case .connected: onlineSummary
        case .loading: L10n.string("ui.friends.loading")
        case .interrupted: L10n.string("ui.friends.stale")
        case .failed(let message): L10n.message(message)
        case .authenticationRequired: L10n.string("model.friends.authenticationRequired")
        }
    }

    /// Unknown presence is kept separate from offline. Search always includes both.
    func sections(matching search: String) -> FriendsSections {
        let query = search.trimmingCharacters(in: .whitespacesAndNewlines)
        let matches = friends.filter { query.isEmpty || $0.displayName.localizedStandardContains(query) }
        func ordered(_ values: [FriendPresentation]) -> [FriendPresentation] {
            values.sorted {
                let order = $0.displayName.localizedStandardCompare($1.displayName)
                return order == .orderedSame ? $0.id.friendID < $1.id.friendID : order == .orderedAscending
            }
        }
        return FriendsSections(
            playing: ordered(matches.filter { $0.presence.isOnline && $0.activity != nil }),
            online: ordered(matches.filter { $0.presence.isOnline && $0.activity == nil }),
            unknown: ordered(matches.filter { $0.presence == .unknown }),
            offline: ordered(matches.filter { $0.presence == .offline || $0.presence == .invisible }),
            isSearching: !query.isEmpty
        )
    }
}

extension StringProtocol {
    /// Up to two uppercase initials from the leading words, for avatar placeholders.
    var initials: String {
        String(split(whereSeparator: \.isWhitespace).prefix(2).compactMap(\.first).map(String.init).joined().uppercased().prefix(2))
    }
}

struct FriendsSections: Equatable {
    let playing: [FriendPresentation]
    let online: [FriendPresentation]
    let unknown: [FriendPresentation]
    let offline: [FriendPresentation]
    let isSearching: Bool
    var isEmpty: Bool { playing.isEmpty && online.isEmpty && unknown.isEmpty && offline.isEmpty }
    var hasOnline: Bool { !playing.isEmpty || !online.isEmpty }
    func showsOffline(isExpanded: Bool) -> Bool { isSearching || isExpanded }
}
