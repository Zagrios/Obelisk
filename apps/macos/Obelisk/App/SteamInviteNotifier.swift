import Foundation
import UserNotifications

/// What the player answered a friend's invitation with, from its notice.
enum SteamInviteAnswer: Equatable, Sendable {
    case join
    case dismiss
    /// A click on the notice itself.
    case open
}

/// Tells of a friend's invitation while no running game shows Obelisk's
/// overlay. Tests replace it.
@MainActor
protocol SteamInviteNotifying: AnyObject {
    /// Tells that `friend` invited the account into `game`, its name when known.
    func notify(_ invite: SteamReceivedInvite, friend: String, game: String?)
    /// Withdraws the notices of `invites`, which no longer show.
    func withdraw(_ invites: [SteamReceivedInvite])
    /// Receives the player's answers.
    var answer: (@MainActor (SteamInviteAnswer, SteamReceivedInvite) -> Void)? { get set }
    func notifyMessage(friend: UInt64, name: String)
    var openMessage: (@MainActor (UInt64) -> Void)? { get set }
}

extension SteamInviteNotifying {
    func notifyMessage(friend: UInt64, name: String) {}
    var openMessage: (@MainActor (UInt64) -> Void)? {
        get { nil }
        set {}
    }
}

/// macOS notifications with Join and Dismiss buttons. macOS asks the player
/// for permission the first time an invitation arrives.
@MainActor
final class SteamInviteNotifier: NSObject, SteamInviteNotifying, UNUserNotificationCenterDelegate {
    private nonisolated static let category = "com.zagrios.obelisk.steam.invite"
    private nonisolated static let join = "join"
    private nonisolated static let dismiss = "dismiss"

    var answer: (@MainActor (SteamInviteAnswer, SteamReceivedInvite) -> Void)?
    var openMessage: (@MainActor (UInt64) -> Void)?

    func notifyMessage(friend: UInt64, name: String) {
        let content = UNMutableNotificationContent()
        content.title = name
        content.body = L10n.string("ui.chat.newMessage")
        content.sound = .default
        content.userInfo = ["messageFriend": String(friend)]
        let center = center
        Task {
            guard (try? await center.requestAuthorization(options: [.alert, .sound])) == true else { return }
            try? await center.add(UNNotificationRequest(identifier: "steam.message.\(friend)", content: content, trigger: nil))
        }
    }
    private let center = UNUserNotificationCenter.current()

    override init() {
        super.init()
        center.delegate = self
        center.setNotificationCategories([UNNotificationCategory(
            identifier: Self.category,
            actions: [
                UNNotificationAction(identifier: Self.join, title: L10n.string("ui.overlay.invites.join"), options: []),
                UNNotificationAction(identifier: Self.dismiss, title: L10n.string("ui.overlay.invites.dismiss"), options: []),
            ],
            intentIdentifiers: []
        )])
    }

    func notify(_ invite: SteamReceivedInvite, friend: String, game: String?) {
        let content = UNMutableNotificationContent()
        content.title = friend
        content.body = game.map { L10n.string("ui.invites.notification", $0) } ?? L10n.string("ui.invites.notificationUnknown")
        content.sound = .default
        content.categoryIdentifier = Self.category
        content.threadIdentifier = Self.category
        content.userInfo = Self.userInfo(invite)
        let request = UNNotificationRequest(identifier: Self.identifier(invite), content: content, trigger: nil)
        let center = center
        Task {
            guard (try? await center.requestAuthorization(options: [.alert, .sound])) == true else { return }
            try? await center.add(request)
        }
    }

    func withdraw(_ invites: [SteamReceivedInvite]) {
        center.removeDeliveredNotifications(withIdentifiers: invites.map(Self.identifier))
    }

    /// Shows the notice even while Obelisk is in front.
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification)
        async -> UNNotificationPresentationOptions {
        [.banner, .list, .sound]
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse) async {
        let content = response.notification.request.content
        if let value = content.userInfo["messageFriend"] as? String, let friend = UInt64(value) {
            guard response.actionIdentifier == UNNotificationDefaultActionIdentifier else { return }
            await MainActor.run { self.openMessage?(friend) }
            return
        }
        guard content.categoryIdentifier == Self.category, let invite = Self.invite(from: content.userInfo) else { return }
        let answer: SteamInviteAnswer
        switch response.actionIdentifier {
        case Self.join: answer = .join
        case Self.dismiss: answer = .dismiss
        case UNNotificationDefaultActionIdentifier: answer = .open
        default: return
        }
        await MainActor.run { self.answer?(answer, invite) }
    }

    /// One notice per friend, as one invitation per friend lasts.
    private static func identifier(_ invite: SteamReceivedInvite) -> String {
        "\(category).\(invite.friendSteamId)"
    }

    /// Numbers travel as text: a SteamID does not fit a property list integer.
    private static func userInfo(_ invite: SteamReceivedInvite) -> [String: String] {
        var info = [
            "friend": String(invite.friendSteamId),
            "app": String(invite.appId),
            "received": String(invite.receivedAt),
        ]
        switch invite.invite {
        case .lobby(let lobbyId): info["lobby"] = String(lobbyId)
        case .connect(let connect): info["connect"] = connect
        case .none: break
        }
        return info
    }

    private nonisolated static func invite(from info: [AnyHashable: Any]) -> SteamReceivedInvite? {
        guard let friend = (info["friend"] as? String).flatMap(UInt64.init),
              let app = (info["app"] as? String).flatMap(UInt32.init),
              let received = (info["received"] as? String).flatMap(UInt64.init) else { return nil }
        let target: SteamGameInvite
        if let lobby = (info["lobby"] as? String).flatMap(UInt64.init) {
            target = .lobby(lobbyId: lobby)
        } else if let connect = info["connect"] as? String {
            target = .connect(connect: connect)
        } else {
            return nil
        }
        return SteamReceivedInvite(friendSteamId: friend, appId: app, invite: target, receivedAt: received)
    }
}
