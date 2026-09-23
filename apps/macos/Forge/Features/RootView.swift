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
        .sheet(isPresented: Binding(get: { model.presentedPrompt != nil }, set: { _ in })) {
            prompt.interactiveDismissDisabled()
        }
        .errorAlert(L10n.string("ui.common.error"), message: $model.errorMessage)
    }

    @ViewBuilder private var prompt: some View {
        switch model.presentedPrompt {
        case .steamRosetta:
            RosettaPromptView(model: model.steam.rosetta)
        case .steamCloud:
            let cloud = model.steam.cloud
            SteamCloudPromptView(
                title: model.steam.cloudTitle(for: cloud.promptAppId),
                message: L10n.string(cloud.promptMessage),
                busy: cloud.promptWorking,
                onCancel: { cloud.respond(.cancel) },
                onChooseLocal: { cloud.respond(.local) },
                onChooseCloud: { cloud.respond(.cloud) }
            )
        case .epicRosetta:
            RosettaPromptView(model: model.epic.rosetta, storefront: "epic")
        case .epicCloud:
            let cloud = model.epic.cloud
            SteamCloudPromptView(
                title: model.epic.cloudTitle(for: cloud.promptLibraryID),
                message: cloud.promptMessage,
                busy: cloud.promptWorking,
                onCancel: { cloud.respond(.cancel) },
                onChooseLocal: { cloud.respond(.local) },
                onChooseCloud: { cloud.respond(.cloud) },
                storefront: "epic"
            )
        case nil:
            EmptyView()
        }
    }
}
