import AppKit
import SwiftUI

/// An in-window modal, centered over the entire root including its sidebar.
/// The native container traps Tab/Escape and restores the previous responder.
struct FriendsModal: NSViewRepresentable {
    let state: FriendsPresentationState
    let avatars: FriendsAvatarCache
    let dismiss: @MainActor () -> Void
    let retry: @MainActor () -> Void
    let signIn: @MainActor () -> Void
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    func makeNSView(context: Context) -> FriendsModalContainer {
        avatars.activate(account: state.account)
        return FriendsModalContainer()
    }

    func updateNSView(_ container: FriendsModalContainer, context: Context) {
        container.dismiss = dismiss
        container.layer?.backgroundColor = NSColor.black.withAlphaComponent(reduceTransparency ? 0.45 : 0.28).cgColor
        container.host.rootView = AnyView(FriendsPanel(state: state, avatars: avatars, focus: container.focus,
                                                      dismiss: dismiss, retry: retry, signIn: signIn)
            .environment(\.self, context.environment)
            .tint(ForgeStyle.accent))
        container.host.setAccessibilityLabel(state.title)
    }

    static func dismantleNSView(_ nsView: FriendsModalContainer, coordinator: ()) {
        nsView.finish()
    }
}

@MainActor
final class FriendsModalContainer: NSView {
    let focus = FriendsPanelFocus()
    let host = NSHostingView(rootView: AnyView(EmptyView()))
    var dismiss: (@MainActor () -> Void)?
    private weak var previousResponder: NSResponder?
    private weak var modalWindow: NSWindow?
    private var keyMonitor: Any?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        identifier = NSUserInterfaceItemIdentifier("friends-modal-container")
        wantsLayer = true
        host.translatesAutoresizingMaskIntoConstraints = false
        host.identifier = NSUserInterfaceItemIdentifier("friends-modal-panel")
        host.setAccessibilityRole(.group)
        host.setAccessibilitySubrole(.dialog)
        host.setAccessibilityModal(true)
        addSubview(host)
        let idealHeight = host.heightAnchor.constraint(equalToConstant: 550)
        idealHeight.priority = .defaultHigh
        NSLayoutConstraint.activate([
            host.centerXAnchor.constraint(equalTo: centerXAnchor),
            host.centerYAnchor.constraint(equalTo: centerYAnchor),
            host.widthAnchor.constraint(equalToConstant: 520),
            host.widthAnchor.constraint(lessThanOrEqualTo: widthAnchor, constant: -48),
            host.heightAnchor.constraint(lessThanOrEqualTo: heightAnchor, constant: -48),
            idealHeight,
        ])
    }

    required init?(coder: NSCoder) { nil }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        guard let window, keyMonitor == nil else { return }
        modalWindow = window
        if let editor = window.firstResponder as? NSTextView, editor.isFieldEditor,
           let field = editor.delegate as? NSResponder {
            previousResponder = field
        } else { previousResponder = window.firstResponder }
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            let consumed = MainActor.assumeIsolated {
                guard let self, event.window === self.modalWindow else { return false }
                if event.keyCode == 53 { self.dismiss?(); return true }
                if event.keyCode == 48 {
                    self.focus.advance(backward: event.modifierFlags.contains(.shift))
                    return true
                }
                return false
            }
            return consumed ? nil : event
        }
        NSAccessibility.post(element: host, notification: .layoutChanged)
    }

    override func hitTest(_ point: NSPoint) -> NSView? {
        super.hitTest(point) ?? (bounds.contains(point) ? self : nil)
    }

    override func mouseDown(with event: NSEvent) {}

    func finish() {
        if let keyMonitor { NSEvent.removeMonitor(keyMonitor) }
        keyMonitor = nil
        let window = modalWindow
        let previous = previousResponder
        previousResponder = nil
        modalWindow = nil
        // SwiftUI re-enables the underlying content in the same update.
        DispatchQueue.main.async { [weak window, weak previous] in
            guard let window, let previous else { return }
            if let view = previous as? NSView, view.window !== window { return }
            window.makeFirstResponder(previous)
        }
    }
}
