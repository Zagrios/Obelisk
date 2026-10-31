import SwiftUI

struct RootView: View {
    @Bindable var model: AppModel
    @Environment(\.colorScheme) private var colorScheme
    @State private var friendsAvatars = FriendsAvatarCache()
    @State private var shownFriends: FriendsPresentationState?
    @State private var windowSize = CGSize(width: 1120, height: 760)

    var body: some View {
        NavigationSplitView {
            SidebarView(model: model)
                .navigationSplitViewColumnWidth(ObeliskStyle.sidebarWidth)
        } detail: {
            Group {
                switch model.selection {
                case .home: HomeView(model: model)
                case .steam: SteamView(model: model.steam)
                case .epic: EpicView(model: model.epic)
                case .gog: GogView(model: model.gog)
                case .settings: SettingsView(model: model)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(ObeliskPalette(colorScheme: colorScheme).background)
        }
        .frame(minWidth: 860, minHeight: 600)
        .tint(ObeliskStyle.accent)
        .onGeometryChange(for: CGSize.self) { $0.size } action: { windowSize = $0 }
        .sheet(isPresented: Binding(get: { model.presentedFriends != nil }, set: { if !$0 { model.closeFriends() } })) {
            // The last friends shown, while the sheet goes away.
            if let friends = model.presentedFriends ?? shownFriends {
                FriendsModal(state: friends, avatars: friendsAvatars,
                             chat: friends.store.id == LibrarySection.steam.rawValue ? model.steam.chat : nil,
                             dismiss: { model.closeFriends() },
                             retry: model.retryPresentedFriends, signIn: model.signInPresentedFriends)
                    .frame(width: 520, height: 550)
                    .id(friends.account)
            }
        }
        .sheet(item: $model.presentedStore) { store in
            // As large as the window leaves room for: a sheet does not keep
            // within it.
            StorefrontStoreModal(store: store) { model.presentedStore = nil }
                .frame(width: min(1100, max(640, windowSize.width - 120)),
                       height: max(460, windowSize.height - 50))
        }
        .environment(\.presentStore) { model.presentedStore = $0 }
        .sheet(item: $model.bugReport) { report in
            BugReportSheet(model: report) { model.bugReport = nil }
        }
        .environment(\.reportBug) { model.reportBug($0) }
        .environment(model.estimates)
        .environment(model.steam.libraryArtwork)
        // A store belongs to the library it opened from.
        .onChange(of: model.selection) { model.presentedStore = nil }
        .onChange(of: model.presentedFriends?.account, initial: true) { _, account in
            friendsAvatars.activate(account: account)
            if let friends = model.presentedFriends { shownFriends = friends }
        }
        .sheet(isPresented: Binding(get: { model.presentedPrompt != nil }, set: { _ in })) {
            if let prompt = model.presentedPrompt {
                // Each storefront's question is a view of its own, though two
                // storefronts may ask with the same sheet.
                model.promptSheet(for: prompt).id(prompt).interactiveDismissDisabled()
            }
        }
    }
}
