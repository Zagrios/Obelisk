import AppKit
import SwiftUI

@main
struct ForgeApp: App {
    @State private var model = AppModel()

    var body: some Scene {
        Window(L10n.string("app.library"), id: "main") {
            RootView(model: model)
                .preferredColorScheme(model.appearance.colorScheme)
                .environment(\.locale, L10n.locale)
                .task { model.refresh() }
        }
        .defaultSize(width: 1120, height: 760)
        .windowStyle(.hiddenTitleBar)
        .windowToolbarStyle(.unified)
        .commands {
            CommandGroup(replacing: .newItem) {
                Button(L10n.string("app.addGame"), action: model.importGame)
                    .keyboardShortcut("o")
                    .disabled(model.presentedFriends != nil)
            }
            CommandGroup(replacing: .appSettings) {
                Button(L10n.string("app.settings")) { model.selection = .settings }
                    .keyboardShortcut(",")
                    .disabled(model.presentedFriends != nil)
            }
            CommandGroup(after: .sidebar) {
                Button(L10n.string("app.refreshLibrary"), action: model.refresh)
                    .keyboardShortcut("r")
                    .disabled(model.presentedFriends != nil)
            }
        }
    }
}
