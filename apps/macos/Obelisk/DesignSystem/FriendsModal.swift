import AppKit
import SwiftUI

/// The friends panel, as the window's sheet shows it. The native container
/// takes Tab, Return and Escape and restores the previous responder.
struct FriendsModal: NSViewRepresentable {
    let state: FriendsPresentationState
    let avatars: FriendsAvatarCache
    var chat: SteamChatModel?
    let dismiss: @MainActor () -> Void
    let retry: @MainActor () -> Void
    let signIn: @MainActor () -> Void
    func makeNSView(context: Context) -> FriendsModalContainer {
        avatars.activate(account: state.account)
        return FriendsModalContainer()
    }

    func updateNSView(_ container: FriendsModalContainer, context: Context) {
        container.dismiss = dismiss
        if let requested = chat?.requestedFriend,
           let friend = state.friends.first(where: { $0.steamID == requested }) {
            container.focus.target = .composer
            container.focus.conversation = friend.id
            Task { @MainActor in chat?.requestedFriend = nil }
        }
        container.host.rootView = AnyView(FriendsPanel(state: state, avatars: avatars, chat: chat, focus: container.focus,
                                                      dismiss: dismiss, retry: retry, signIn: signIn)
            .environment(\.self, context.environment)
            .tint(ObeliskStyle.accent))
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
        host.translatesAutoresizingMaskIntoConstraints = false
        host.identifier = NSUserInterfaceItemIdentifier("friends-modal-panel")
        host.setAccessibilityRole(.group)
        host.setAccessibilitySubrole(.dialog)
        host.setAccessibilityModal(true)
        addSubview(host)
        NSLayoutConstraint.activate([
            host.leadingAnchor.constraint(equalTo: leadingAnchor),
            host.trailingAnchor.constraint(equalTo: trailingAnchor),
            host.topAnchor.constraint(equalTo: topAnchor),
            host.bottomAnchor.constraint(equalTo: bottomAnchor),
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
                return self.focus.handle(event) { self.dismiss?() }
            }
            return consumed ? nil : event
        }
        NSAccessibility.post(element: host, notification: .layoutChanged)
    }

    func finish() {
        if let keyMonitor { NSEvent.removeMonitor(keyMonitor) }
        keyMonitor = nil
        let window = modalWindow
        let previous = previousResponder
        previousResponder = nil
        modalWindow = nil
        // Once SwiftUI has taken the modal away.
        DispatchQueue.main.async { [weak window, weak previous] in
            guard let window, let previous else { return }
            if let view = previous as? NSView, view.window !== window { return }
            window.makeFirstResponder(previous)
        }
    }
}
