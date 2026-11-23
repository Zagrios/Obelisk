import AppKit
import SwiftUI

@main
struct ObeliskApp: App {
    @State private var model: AppModel
    @NSApplicationDelegateAdaptor(AppLifecycle.self) private var lifecycle

    init() {
        // setenv is only safe while no other thread reads the environment:
        // before AppModel, whose Rust side starts threads. The test host
        // starts no game, and its environment stays as it is.
        if !AppIdentity.isTestHost { GameLocale.adopt() }
        NetworkPath.start()
        let model = AppModel()
        // Nor does the test host count as a start of Obelisk.
        if !AppIdentity.isTestHost { model.support.countStart() }
        _model = State(wrappedValue: model)
    }

    var body: some Scene {
        Window(L10n.string("app.library"), id: "main") {
            RootView(model: model)
                .preferredColorScheme(model.appearance.colorScheme)
                .environment(\.locale, L10n.locale)
                .task {
                    // The test host is no Obelisk of its own: it signs no account in.
                    if !AppIdentity.isTestHost { await model.restoreStorefronts() }
                }
                .task {
                    // Nor does it ask for support.
                    if !AppIdentity.isTestHost { await model.offerSupport() }
                }
        }
        .defaultSize(width: 1120, height: 760)
        .windowStyle(.hiddenTitleBar)
        .windowToolbarStyle(.unified)
        .commands {
            CommandGroup(after: .appInfo) {
                Button(L10n.string("app.checkForUpdates"), action: model.updates.check)
                    .disabled(!model.updates.canCheck)
            }
            // Obelisk has a single window and nothing to create.
            CommandGroup(replacing: .newItem) {}
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
        MenuBarExtra {
            AppMenuBarMenu(lifecycle: lifecycle)
        } label: {
            AppMenuBarLabel(model: model, lifecycle: lifecycle)
        }
    }
}
