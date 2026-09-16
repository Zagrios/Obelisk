import SwiftUI

struct RootView: View {
    @Bindable var model: AppModel
    @Environment(\.colorScheme) private var colorScheme
    @State private var friendsAvatars = FriendsAvatarCache()

    var body: some View {
        HStack(spacing: 0) {
            SidebarView(model: model)
                .frame(width: ForgeStyle.sidebarWidth)

            Rectangle()
                .fill(ForgePalette(colorScheme: colorScheme).border)
                .frame(width: 1)
                .ignoresSafeArea(.container, edges: .top)

            Group {
                if model.selection == .settings {
                    SettingsView(model: model)
                } else if model.selection == .steam {
                    SteamView(model: model.steam)
                } else if model.selection == .epic {
                    EpicView(model: model.epic)
                } else {
                    LibraryView(model: model)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .frame(minWidth: 860, minHeight: 600)
        .background(ForgePalette(colorScheme: colorScheme).background)
        .tint(ForgeStyle.accent)
        .disabled(model.presentedFriends != nil)
        .allowsHitTesting(model.presentedFriends == nil)
        .accessibilityHidden(model.presentedFriends != nil)
        .overlay {
            if let friends = model.presentedFriends {
                FriendsModal(state: friends, avatars: friendsAvatars, dismiss: { model.closeFriends() },
                             retry: model.retryPresentedFriends, signIn: model.signInPresentedFriends)
                    .ignoresSafeArea(.container, edges: .top)
                    .id(friends.account)
            }
        }
        .onChange(of: model.steam.isFriendsPresented, initial: true) { _, presented in
            if presented { model.presentFriends(storeID: "steam") }
            else { model.closeFriends(storeID: "steam") }
        }
        .onChange(of: model.epic.isFriendsPresented, initial: true) { _, presented in
            if presented { model.presentFriends(storeID: "epic") }
            else { model.closeFriends(storeID: "epic") }
        }
        .onChange(of: model.steam.friends?.account) { _, _ in model.validateFriendsAccount() }
        .onChange(of: model.epic.friends?.account) { _, _ in model.validateFriendsAccount() }
        .onChange(of: model.presentedFriends?.account, initial: true) { _, account in
            friendsAvatars.activate(account: account)
        }
        .errorAlert(L10n.string("ui.common.error"), message: $model.errorMessage)
    }
}
