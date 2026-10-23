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
        if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] == nil { GameLocale.adopt() }
        _model = State(wrappedValue: AppModel())
    }

    var body: some Scene {
        Window(L10n.string("app.library"), id: "main") {
            RootView(model: model)
                .preferredColorScheme(model.appearance.colorScheme)
                .environment(\.locale, L10n.locale)
                .task {
                    // The test host is no Obelisk of its own: it signs no account in.
                    if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] == nil {
                        await model.restoreStorefronts()
                    }
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
