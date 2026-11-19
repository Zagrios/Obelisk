import AppKit
import SwiftUI
import WebKit

/// Puts the overlay on screen and gives the game its focus back.
@MainActor
protocol OverlayPresenter: AnyObject {
    /// Covers the screen of `game`'s window, or the main screen.
    func show(_ overlay: GameOverlayController, over game: OverlayApplication?)
    /// Brings `application` back in front, when it still runs.
    func hide(returningTo application: OverlayApplication?)
}

/// The overlay's panel, above a game's fullscreen window and in every Space.
/// Obelisk activates as it opens, as Command-Tab would: Wine then releases
/// the pointer it confines, and shows it.
@MainActor
final class GameOverlayPanelPresenter: OverlayPresenter {
    private var panel: GameOverlayPanel?

    func show(_ overlay: GameOverlayController, over game: OverlayApplication?) {
        let panel = self.panel ?? GameOverlayPanel(overlay: overlay)
        self.panel = panel
        if let screen = Self.screen(showing: game?.pid) ?? NSScreen.main {
            panel.setFrame(screen.frame, display: true)
        }
        // Key before activation, so the activation keeps it key. A game's own
        // request comes with no input to Obelisk, and macOS refuses the
        // cooperative activation then: Wine would keep the pointer confined.
        panel.orderFrontRegardless()
        panel.makeKey()
        NSApp.activate(ignoringOtherApps: true)
    }

    func hide(returningTo application: OverlayApplication?) {
        panel?.orderOut(nil)
        guard let application, let running = NSRunningApplication(processIdentifier: application.pid),
              !running.isTerminated else { return }
        NSApp.yieldActivation(to: running)
        running.activate(from: .current, options: [])
    }

    /// The screen holding the largest window of `pid`. Quartz tells every
    /// window's owner and bounds without the Screen Recording permission.
    static func screen(showing pid: pid_t?) -> NSScreen? {
        guard let pid,
              let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements],
                                                       kCGNullWindowID) as? [[String: Any]] else { return nil }
        let bounds = windows
            .filter { ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid }
            .compactMap { ($0[kCGWindowBounds as String] as? NSDictionary).flatMap { CGRect(dictionaryRepresentation: $0) } }
            .max { $0.width * $0.height < $1.width * $1.height }
        guard let bounds, let primary = NSScreen.screens.first else { return nil }
        // Quartz measures from the primary screen's top edge, AppKit from its bottom.
        let center = CGPoint(x: bounds.midX, y: primary.frame.maxY - bounds.midY)
        return NSScreen.screens.first { $0.frame.contains(center) }
    }
}

@MainActor
final class GameOverlayPanel: NSPanel {
    private var keyMonitor: Any?

    init(overlay: GameOverlayController) {
        super.init(contentRect: .zero, styleMask: [.borderless], backing: .buffered, defer: false)
        level = .gameOverlay
        collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .ignoresCycle]
        hidesOnDeactivate = false
        isReleasedWhenClosed = false
        isOpaque = false
        backgroundColor = .clear
        hasShadow = false
        isMovable = false
        animationBehavior = .none
        identifier = NSUserInterfaceItemIdentifier("game-overlay")
        setAccessibilityLabel(L10n.string("ui.overlay.title"))
        contentView = NSHostingView(rootView: GameOverlayView(overlay: overlay)
            .environment(\.locale, L10n.locale)
            .environment(\.colorScheme, .dark)
            .tint(ObeliskStyle.accent))
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self, weak overlay] event in
            let consumed = MainActor.assumeIsolated {
                guard let self, let overlay, event.window === self else { return false }
                return Self.handle(event, overlay: overlay)
            }
            return consumed ? nil : event
        }
    }

    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }

    /// Escape leaves a conversation, then a page, then the overlay. A web
    /// page keeps its own Tab and Return.
    private static func handle(_ event: NSEvent, overlay: GameOverlayController) -> Bool {
        let responder = event.window?.firstResponder as? NSView
        if event.keyCode != 53, responder?.enclosing(WKWebView.self) != nil { return false }
        return overlay.focus.handle(event) {
            if !overlay.back() { overlay.hide() }
        }
    }

    isolated deinit {
        if let keyMonitor { NSEvent.removeMonitor(keyMonitor) }
    }
}

extension NSWindow.Level {
    /// Above everything a game shows. In exclusive fullscreen, Wine captures
    /// the displays: a shield covers every other window, and the game's own
    /// window sits just above it.
    static var gameOverlay: NSWindow.Level { NSWindow.Level(rawValue: Int(CGShieldingWindowLevel()) + 2) }
}

private extension NSView {
    func enclosing<View: NSView>(_ type: View.Type) -> View? {
        var view: NSView? = self
        while let current = view {
            if let match = current as? View { return match }
            view = current.superview
        }
        return nil
    }
}
