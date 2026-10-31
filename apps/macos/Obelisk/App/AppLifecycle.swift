import AppKit
import SwiftUI

/// Quitting Obelisk, or closing its window, hides it: it keeps running in the
/// menu bar, as Steam does, so its Game sessions and Transfers go on. It really
/// quits from its menu bar item, after stopping a running game, when macOS
/// logs out, restarts or shuts down, or for Sparkle to install a new version.
@MainActor
final class AppLifecycle: NSObject, NSApplicationDelegate {
    /// Asks the menu bar item, which can open windows, to show the library.
    static let showLibrary = Notification.Name("com.zagrios.obelisk.showLibrary")
    /// How long quitting waits for stopped games to settle their Cloud saves.
    private static let settleTimeout: Duration = .seconds(60)

    /// Whether a game runs, and how to stop every one of them.
    var games: (running: @MainActor () -> Bool, stop: @MainActor () async -> Void) = ({ false }, {})
    /// The library cache, whose last changes are still being written.
    var libraryCache: LibraryCache?
    /// Games are being stopped before Obelisk ends.
    private var stopping = false
    /// Obelisk is ending: its termination is let through.
    private var quitting = false
    /// Obelisk opens again once it has exited.
    private var reopens = false
    private var windowClosing: NSObjectProtocol?

    func applicationDidFinishLaunching(_ notification: Notification) {
        // An Obelisk that died took its games' sessions along, but Wine's
        // Windows processes froze on their own. None of them can belong to
        // this Obelisk yet.
        // The test host is no Obelisk of its own: it leaves every game alone.
        if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] == nil {
            let locations = ObeliskLocations.live
            Task.detached(priority: .utility) { _ = endOrphanedWindowsGames(locations: locations) }
            // A release build reads the game notes and profiles from Obelisk's
            // main, else the copies it kept last.
            Task.detached(priority: .utility) { await refreshNotesAndProfiles(locations: locations) }
        }
        // Closing the last library window hides Obelisk like quitting does.
        windowClosing = NotificationCenter.default.addObserver(
            forName: NSWindow.willCloseNotification, object: nil, queue: .main
        ) { notification in
            let closing = notification.object as? NSWindow
            MainActor.assumeIsolated {
                let others = NSApp.windows.filter { $0 !== closing && $0.isVisible && $0.canBecomeMain }
                if others.isEmpty { NSApp.setActivationPolicy(.accessory) }
            }
        }
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        if quitting { return .terminateNow }
        guard Self.systemQuit else {
            hide()
            return .terminateCancel
        }
        // macOS waits while the games settle, within its own logout deadline.
        quitting = true
        Task {
            await stopGames()
            NSApp.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }

    func applicationWillTerminate(_ notification: Notification) {
        libraryCache?.flush()
        if reopens { Self.reopenOnceExited() }
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        show()
        return false
    }

    /// Shows the library window and Obelisk's Dock icon.
    func show() {
        NSApp.setActivationPolicy(.regular)
        NotificationCenter.default.post(name: Self.showLibrary, object: nil)
        NSApp.activate()
    }

    /// Closes Obelisk's windows and hides its Dock icon; it keeps running.
    func hide() {
        for window in NSApp.windows where window.canBecomeMain { window.close() }
        NSApp.setActivationPolicy(.accessory)
    }

    /// Really quits: a running game is stopped first, once the player agrees,
    /// and its Cloud saves settle before Obelisk exits.
    func quit() {
        quit(running: L10n.string("app.quit.running.message"), stop: L10n.string("app.quit.stop")) { [self] in
            terminate()
        }
    }

    /// Quits as `quit()` does, then opens Obelisk again once it has exited, so
    /// a new language applies everywhere.
    func restart() {
        quit(running: L10n.string("app.restart.running.message"), stop: L10n.string("app.restart.stop")) { [self] in
            reopens = true
            terminate()
        }
    }

    /// Quits as `quit()` does for Sparkle to install Obelisk's new version:
    /// `install` lets Sparkle end Obelisk, then open the new one.
    func update(_ install: @escaping () -> Void) {
        quit(running: L10n.string("app.update.running.message"), stop: L10n.string("app.update.stop"), then: install)
    }

    private func quit(running message: String, stop: String, then exit: @escaping () -> Void) {
        guard !stopping else { return }
        if games.running(), !Self.confirmsStoppingGames(running: message, stop: stop) { return }
        stopping = true
        Task {
            await stopGames()
            stopping = false
            quitting = true
            exit()
        }
    }

    /// Ends Obelisk. While a sheet is open AppKit refuses with a beep, without
    /// asking the delegate: Obelisk stays as it was, so the player can quit
    /// again once the sheet is closed.
    private func terminate() {
        NSApp.terminate(nil)
        quitting = false
        reopens = false
    }

    /// Asks whether to stop the running game: `message` tells why, `stop`
    /// is the button that agrees.
    static func confirmsStoppingGames(running message: String, stop: String) -> Bool {
        NSApp.activate()
        let alert = NSAlert()
        alert.messageText = L10n.string("app.quit.running.title")
        alert.informativeText = message
        alert.addButton(withTitle: stop)
        alert.addButton(withTitle: L10n.string("ui.common.cancel"))
        return alert.runModal() == .alertFirstButtonReturn
    }

    /// A shell that outlives this Obelisk opens its bundle again once its
    /// process is gone: two Obelisks never run at once.
    private static func reopenOnceExited() {
        let shell = Process()
        shell.executableURL = URL(fileURLWithPath: "/bin/sh")
        shell.arguments = [
            "-c", "while /bin/kill -0 \"$1\" 2>/dev/null; do /bin/sleep 0.2; done; exec /usr/bin/open \"$2\"",
            "obelisk-restart", String(ProcessInfo.processInfo.processIdentifier), Bundle.main.bundlePath,
        ]
        try? shell.run()
    }

    private func stopGames() async {
        guard games.running() else { return }
        await games.stop()
        let deadline = ContinuousClock.now + Self.settleTimeout
        while games.running() && ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(250))
        }
    }

    /// Whether macOS itself asks Obelisk to quit, to log out, restart or
    /// shut down, rather than the player.
    private static var systemQuit: Bool {
        // The 'why?' attribute of the quit Apple event names that reason.
        let reason = AEKeyword(0x7768_793F)
        return NSAppleEventManager.shared().currentAppleEvent?.attributeDescriptor(forKeyword: reason) != nil
    }
}

/// Obelisk's menu bar item, which stays while its windows are closed. It
/// tells the lifecycle which games run.
struct AppMenuBarLabel: View {
    let model: AppModel
    let lifecycle: AppLifecycle
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Image("MenuBarIcon")
            .onAppear {
                lifecycle.libraryCache = model.libraryCache
                model.overlay.showLibrary = { lifecycle.show() }
                model.restart = { lifecycle.restart() }
                model.updates.quitToInstall = { install in lifecycle.update(install) }
                // The model holds the lifecycle, so the lifecycle holds it weakly.
                lifecycle.games = (
                    running: { [weak app = model] in app?.runsGame ?? false },
                    stop: { [weak app = model] in await app?.stopGames() }
                )
            }
            .onReceive(NotificationCenter.default.publisher(for: AppLifecycle.showLibrary)) { _ in
                openWindow(id: "main")
            }
    }
}

struct AppMenuBarMenu: View {
    let lifecycle: AppLifecycle

    var body: some View {
        Button(L10n.string("app.menu.open")) { lifecycle.show() }
        Divider()
        Button(L10n.string("app.menu.quit")) { lifecycle.quit() }
    }
}
