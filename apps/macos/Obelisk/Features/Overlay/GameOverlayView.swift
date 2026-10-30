import SwiftUI
import WebKit

/// The overlay over the dimmed game: its bar on top, the friends of the
/// game's storefront on the leading side, and the page it shows.
struct GameOverlayView: View {
    let overlay: GameOverlayController
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    @State private var tools: [String] = ["friends"]
    @State private var browserURL = URL(string: "https://www.google.com")!
    @State private var chatFocus = FriendsPanelFocus()

    var body: some View {
        ZStack {
            backdrop
            if overlay.session != nil {
                VStack(spacing: 18) {
                    OverlayTopBar(overlay: overlay)
                    GeometryReader { geometry in
                        ZStack(alignment: .topLeading) {
                            ForEach(tools, id: \.self) { tool in
                                OverlayWorkspaceWindow(key: tool, title: title(tool), bounds: geometry.size,
                                    isFrontmost: tool == tools.last,
                                    close: { tools.removeAll { $0 == tool } },
                                    raise: { raise(tool) }) {
                                    toolContent(tool)
                                }
                            }
                        }
                    }
                    HStack(spacing: 12) {
                        toolButton("friends", symbol: "person.2")
                        toolButton("chat", symbol: "bubble.left.and.bubble.right")
                        toolButton("browser", symbol: "globe")
                        toolButton("achievements", symbol: "trophy")
                    }
                    .padding(12)
                    .background(.regularMaterial, in: Capsule())
                }
                .padding(32)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .onAppear {
                    switch overlay.request {
                    case .webPage(let url): browserURL = url; raise("browser")
                    case .achievements: raise("achievements")
                    case .microTxn: raise("purchase")
                    case .user: chatFocus.conversation = overlay.focus.conversation; raise("chat")
                    default: break
                    }
                }
                .onChange(of: overlay.request) { _, request in
                    switch request {
                    case .webPage(let url): browserURL = url; raise("browser")
                    case .achievements: raise("achievements")
                    case .friends, .invite: raise("friends")
                    case .user: chatFocus.conversation = overlay.focus.conversation; raise("chat")
                    case .microTxn: raise("purchase")
                    case .home: break
                    }
                }
                .onChange(of: overlay.focus.conversation) { _, friend in
                    guard let friend else { return }
                    chatFocus.conversation = friend
                    overlay.focus.conversation = nil
                    raise("chat")
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(L10n.string("ui.overlay.title"))
        .accessibilityIdentifier("game-overlay")
    }

    private func raise(_ tool: String) {
        tools.removeAll { $0 == tool }
        tools.append(tool)
        if tool == "achievements" { overlay.show(.achievements) }
    }

    private func title(_ tool: String) -> String {
        switch tool {
        case "friends": L10n.string("ui.overlay.workspace.friends")
        case "chat": L10n.string("ui.overlay.workspace.chat")
        case "browser": L10n.string("ui.overlay.workspace.browser")
        case "achievements": L10n.string("ui.overlay.workspace.achievements")
        default: L10n.string("ui.overlay.workspace.purchase")
        }
    }

    private func toolButton(_ tool: String, symbol: String) -> some View {
        Button { raise(tool) } label: { Label(title(tool), systemImage: symbol) }
            .buttonStyle(.bordered)
    }

    @ViewBuilder private func toolContent(_ tool: String) -> some View {
        switch tool {
        case "friends":
            if let friends = overlay.friends {
                FriendsPanel(state: friends, avatars: overlay.avatars, chat: overlay.chat,
                    picker: overlay.invitePicker, closable: false,
                    accessory: AnyView(OverlayInvitations(overlay: overlay, compact: true)),
                    openConversation: { friend in
                        chatFocus.target = .composer
                        chatFocus.conversation = friend
                        raise("chat")
                    },
                    focus: overlay.focus,
                    dismiss: overlay.hide, retry: overlay.retryFriends, signIn: overlay.openLibrary)
                    .environment(\.openURL, OpenURLAction { url in
                        guard ["http", "https"].contains(url.scheme?.lowercased() ?? "") else { return .systemAction }
                        browserURL = url
                        raise("browser")
                        return .handled
                    })
            }
        case "chat":
            if let friends = overlay.friends, let chat = overlay.chat {
                OverlayChatWorkspace(state: friends, chat: chat, avatars: overlay.avatars,
                    focus: chatFocus, dismiss: overlay.hide)
                    .environment(\.openURL, OpenURLAction { url in
                        guard ["http", "https"].contains(url.scheme?.lowercased() ?? "") else { return .systemAction }
                        browserURL = url; raise("browser")
                        return .handled
                    })
            }
        case "browser": OverlayWorkspaceBrowser(overlay: overlay, requestedURL: browserURL)
        case "achievements":
            if let achievements = overlay.achievements {
                OverlayAchievementsPage(model: achievements, gameTitle: overlay.gameTitle,
                    back: { tools.removeAll { $0 == "achievements" } }, retry: overlay.retryAchievements)
            }
        case "purchase":
            OverlayPurchasePage(overlay: overlay, purchase: overlay.steam.purchase)
        default: EmptyView()
        }
    }

    private var backdrop: some View {
        Rectangle()
            .fill(reduceTransparency ? AnyShapeStyle(Color.black.opacity(0.82)) : AnyShapeStyle(.ultraThinMaterial))
            .overlay { Color.black.opacity(reduceTransparency ? 0 : 0.45) }
            .ignoresSafeArea()
            .accessibilityHidden(true)
    }

    /// The home page shows the invitations into the game; other pages leave
    /// them to the friends column.
    private var showsHome: Bool {
        switch overlay.request {
        case .achievements, .webPage: false
        case .microTxn(let orderId): overlay.pendingPurchase?.orderId != orderId
        case .home, .friends, .invite, .user: true
        }
    }

    @ViewBuilder private var page: some View {
        switch overlay.request {
        case .microTxn(let orderId) where overlay.pendingPurchase?.orderId == orderId:
            OverlayPurchasePage(overlay: overlay, purchase: overlay.steam.purchase)
        case .achievements:
            if let achievements = overlay.achievements {
                OverlayAchievementsPage(model: achievements, gameTitle: overlay.gameTitle,
                                        back: { overlay.back() }, retry: overlay.retryAchievements)
            }
        case .webPage(let url):
            WebPagePanel(start: { WebPageStart(url: url, cookies: await overlay.webCookies(for: url)) },
                         close: { overlay.back() }, placeholderTitle: url.host() ?? url.absoluteString,
                         identifier: "game-overlay-web-page")
                .id(url)
        case .home, .friends, .invite, .user, .microTxn:
            OverlayHome(overlay: overlay)
        }
    }
}

/// The running game, its storefront, how long it has run, the time, and how
/// to close the overlay.
private struct OverlayTopBar: View {
    let overlay: GameOverlayController

    var body: some View {
        HStack(spacing: 14) {
            OverlayGameArtwork(overlay: overlay, maxPixelSize: 400)
                .frame(width: 96, height: 45)
                .clipShape(RoundedRectangle(cornerRadius: 10))
            VStack(alignment: .leading, spacing: 3) {
                Text(overlay.gameTitle)
                    .font(.system(size: 16, weight: .semibold))
                    .lineLimit(1)
                HStack(spacing: 6) {
                    if let store = overlay.session.flatMap({ LibrarySection(rawValue: $0.storeID) }) {
                        if let logo = store.logoAssetName {
                            Image(logo).resizable().scaledToFit().frame(width: 13, height: 13).accessibilityHidden(true)
                        }
                        Text(store.title)
                    }
                    if let startedAt = overlay.session?.startedAt {
                        Text(verbatim: "·").accessibilityHidden(true)
                        TimelineView(.everyMinute) { context in
                            Text(L10n.string("ui.overlay.playingFor",
                                             OverlayFormatting.duration(from: startedAt, to: context.date)))
                        }
                    }
                }
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .lineLimit(1)
            }
            Spacer(minLength: 16)
            TimelineView(.everyMinute) { context in
                Text(context.date, format: .dateTime.hour().minute().locale(L10n.locale))
            }
            .font(.system(size: 22, weight: .semibold))
            .monospacedDigit()
            Text(L10n.string("ui.overlay.closeHint"))
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 10)
                .frame(height: 26)
                .background(.primary.opacity(0.08), in: Capsule())
            StorefrontGlassButton(systemName: "xmark", size: 13, label: L10n.string("ui.overlay.close"),
                                  identifier: "game-overlay-close", action: overlay.hide)
        }
        .padding(.leading, 12)
        .padding(.trailing, 14)
        .frame(height: 70)
        .modifier(StorefrontPanelGlass(cornerRadius: 24))
    }
}

/// The running game's wide artwork, as its storefront publishes it.
private struct OverlayGameArtwork: View {
    let overlay: GameOverlayController
    let maxPixelSize: Int

    var body: some View {
        if case .steam(let appId)? = overlay.session?.game {
            SteamArtwork(appId: appId, portrait: false)
        } else {
            StorefrontArtwork(imageURL: overlay.gameArtwork, maxPixelSize: maxPixelSize)
        }
    }
}

/// The game's artwork, and what the overlay opens for it.
private struct OverlayHome: View {
    let overlay: GameOverlayController

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            OverlayGameArtwork(overlay: overlay, maxPixelSize: 2048)
                .frame(maxWidth: .infinity)
                .frame(height: 300)
                .clipped()
                .overlay(alignment: .bottomLeading) {
                    Text(overlay.gameTitle)
                        .font(.system(size: 30, weight: .bold))
                        .foregroundStyle(.white)
                        .shadow(color: .black.opacity(0.5), radius: 8, y: 2)
                        .lineLimit(2)
                        .padding(24)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(LinearGradient(colors: [.clear, .black.opacity(0.65)], startPoint: .top, endPoint: .bottom))
                }
            // Friends' invitations come first; many of them scroll.
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    OverlayInvitations(overlay: overlay)
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 190), spacing: 12)], alignment: .leading, spacing: 12) {
                        OverlayTile(title: L10n.string("ui.game.achievements.title"), systemImage: "trophy",
                                    identifier: "game-overlay-achievements") { overlay.show(.achievements) }
                        if case .steam(let appId)? = overlay.session?.game {
                            OverlayTile(title: L10n.string("ui.overlay.storePage"), systemImage: "bag",
                                        identifier: "game-overlay-store") {
                                overlay.show(.webPage(GameOverlayController.storeURL(appId: appId)))
                            }
                        }
                        OverlayTile(title: L10n.string("ui.overlay.library"), systemImage: "square.grid.2x2",
                                    identifier: "game-overlay-library") { overlay.openLibrary() }
                    }
                }
                .padding(20)
            }
            .scrollBounceBehavior(.basedOnSize)
        }
        .clipShape(RoundedRectangle(cornerRadius: 24))
        .modifier(StorefrontPanelGlass(cornerRadius: 24))
    }
}

private struct OverlayTile: View {
    let title: String
    let systemImage: String
    let identifier: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Image(systemName: systemImage)
                    .font(.system(size: 15, weight: .semibold))
                    .frame(width: 22)
                    .accessibilityHidden(true)
                Text(title)
                    .font(.system(size: 13, weight: .semibold))
                    .lineLimit(1)
                Spacer(minLength: 0)
            }
            .foregroundStyle(.primary)
            .padding(.horizontal, 16)
            .frame(height: 48)
            .modifier(StorefrontPanelGlass(cornerRadius: 14))
            .contentShape(RoundedRectangle(cornerRadius: 14))
        }
        .obeliskButtonStyle(.plain)
        .accessibilityIdentifier(identifier)
    }
}

private struct OverlayAchievementsPage: View {
    let model: GameAchievementsModel
    let gameTitle: String
    let back: () -> Void
    let retry: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 14) {
                StorefrontGlassButton(systemName: "chevron.left", label: L10n.string("ui.overlay.back"),
                                      identifier: "game-overlay-back", action: back)
                VStack(alignment: .leading, spacing: 4) {
                    Label(L10n.string("ui.game.achievements.title"), systemImage: "trophy")
                        .font(.title2.weight(.semibold))
                    Text(gameTitle).font(.subheadline).foregroundStyle(.secondary)
                }
                Spacer()
                if let fraction = model.fraction {
                    HStack(spacing: 12) {
                        Text(model.countText).monospacedDigit()
                        AchievementProgressBar(fraction: fraction).frame(width: 160)
                    }
                    .accessibilityElement(children: .combine)
                }
            }
            .padding(.horizontal, 24)
            .padding(.top, 20)
            .modifier(OverlayDraggableHeader())
            Divider()
            GameAchievementsList(model: model, retry: retry)
        }
        .modifier(StorefrontPanelGlass(cornerRadius: 24))
        .accessibilityIdentifier("game-overlay-achievements-page")
    }
}

enum OverlayFormatting {
    /// How long the game has run: minutes, then hours and minutes.
    static func duration(from start: Date, to now: Date) -> String {
        let seconds = Int64(max(0, now.timeIntervalSince(start)))
        return Duration.seconds(seconds - seconds % 60)
            .formatted(.units(allowed: [.hours, .minutes], width: .abbreviated).locale(L10n.locale))
    }
}

/// Chat owns conversation navigation; Friends never swaps its list for a chat.
struct OverlayChatWorkspace: View {
    let state: FriendsPresentationState
    let chat: SteamChatModel
    let avatars: FriendsAvatarCache
    let focus: FriendsPanelFocus
    let dismiss: () -> Void

    private var recent: [FriendPresentation] {
        state.friends.filter { friend in
            friend.steamID.flatMap { chat.lastActivity(for: $0) } != nil
        }.sorted {
            let left = chat.lastActivity(for: $0.steamID ?? 0) ?? 0
            let right = chat.lastActivity(for: $1.steamID ?? 0) ?? 0
            return left == right ? $0.id.friendID < $1.id.friendID : left > right
        }
    }

    var body: some View {
        if let identity = focus.conversation,
           let friend = state.friends.first(where: { $0.id == identity }), let steamID = friend.steamID {
            FriendConversationView(friend: friend, steamID: steamID, chat: chat, avatars: avatars,
                focus: focus, dismiss: dismiss, closable: false)
                .id(identity)
        } else {
            VStack(spacing: 0) {
                HStack {
                    Label(L10n.string("ui.overlay.workspace.chat"), systemImage: "bubble.left.and.bubble.right")
                        .font(.title2.bold())
                    Spacer()
                }
                .padding(22)
                .modifier(OverlayDraggableHeader())
                Divider()
                if recent.isEmpty {
                    ContentUnavailableView(L10n.string("ui.chat.noRecent"), systemImage: "bubble.left.and.bubble.right",
                        description: Text(L10n.string("ui.chat.selectFriend")))
                } else {
                    ScrollView {
                        LazyVStack(spacing: 4) {
                            ForEach(recent) { friend in
                                if let steamID = friend.steamID {
                                    Button {
                                        focus.target = .composer
                                        focus.conversation = friend.id
                                    } label: {
                                        HStack(spacing: 12) {
                                            FriendAvatar(friend: friend, avatars: avatars, size: 40)
                                            VStack(alignment: .leading, spacing: 5) {
                                                HStack {
                                                    Text(friend.displayName).fontWeight(.semibold)
                                                    Spacer()
                                                    if let timestamp = chat.lastActivity(for: steamID) {
                                                        Text(Date(timeIntervalSince1970: TimeInterval(timestamp)),
                                                            format: .dateTime.hour().minute().locale(L10n.locale))
                                                            .font(.caption).foregroundStyle(.secondary)
                                                    }
                                                }
                                                Text(chat.preview(for: steamID) ?? "—")
                                                    .font(.subheadline).foregroundStyle(.secondary).lineLimit(1)
                                            }
                                            if let unread = chat.unread[steamID], unread > 0 {
                                                Text(unread.formatted()).font(.caption.bold())
                                                    .padding(7).background(ObeliskStyle.accent, in: Capsule())
                                                    .accessibilityLabel(L10n.string("ui.chat.unreadCount", Int64(unread)))
                                            }
                                        }
                                        .padding(12).contentShape(Rectangle())
                                    }
                                    .buttonStyle(.plain)
                                    .task { chat.loadPreview(for: steamID) }
                                }
                            }
                        }.padding(10)
                    }
                }
            }
            .accessibilityIdentifier("overlay-recent-conversations")
        }
    }
}

private struct OverlayWorkspaceWindow<Content: View>: View {
    let key: String
    let title: String
    let bounds: CGSize
    let isFrontmost: Bool
    let close: () -> Void
    let raise: () -> Void
    let content: Content

    init(key: String, title: String, bounds: CGSize, isFrontmost: Bool, close: @escaping () -> Void,
         raise: @escaping () -> Void, @ViewBuilder content: () -> Content) {
        self.key = key
        self.title = title
        self.bounds = bounds
        self.isFrontmost = isFrontmost
        self.close = close
        self.raise = raise
        self.content = content()
    }

    var body: some View {
        NativeOverlayTool(key: key, title: title,
            initialSize: CGSize(width: min(bounds.width, key == "friends" ? 400 : 620), height: min(bounds.height, 550)),
            isFrontmost: isFrontmost, close: close, content: AnyView(content))
    }
}

/// Each tool is an AppKit child panel. AppKit moves and resizes its existing
/// backing surface, without relaying every pointer movement through SwiftUI.
struct NativeOverlayTool: NSViewRepresentable {
    let key: String
    let title: String
    let initialSize: CGSize
    let isFrontmost: Bool
    let close: () -> Void
    let content: AnyView

    func makeNSView(context: Context) -> Anchor {
        let anchor = Anchor()
        anchor.coordinator = context.coordinator
        return anchor
    }

    func makeCoordinator() -> Coordinator { Coordinator(key: key, initialSize: initialSize) }

    func updateNSView(_ view: Anchor, context: Context) {
        let coordinator = context.coordinator
        coordinator.panel.title = title
        coordinator.host.rootView = AnyView(content
            .environment(\.overlayWindowDrag, OverlayWindowDrag(
                register: { [weak coordinator] view in coordinator?.header = view }))
            .environment(\.colorScheme, .dark)
            .environment(\.locale, L10n.locale)
            .tint(ObeliskStyle.accent)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(Color(white: 0.12))
            .overlay(alignment: .topTrailing) {
                Button(action: close) { Image(systemName: "xmark").padding(8) }
                    .buttonStyle(.plain)
                    .background(Color(white: 0.2), in: Circle())
                    .padding(8)
                    .accessibilityLabel(L10n.string("ui.common.close"))
            }
            .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
            // Restore the outer environment before applying tool-specific
            // values. A nearer `\.self` override would erase the drag hook.
            .environment(\.self, context.environment))
        coordinator.attach(to: view.window)
        if view.window?.isVisible == true, isFrontmost && !coordinator.wasFrontmost {
            coordinator.panel.makeKeyAndOrderFront(nil)
        }
        coordinator.wasFrontmost = isFrontmost
    }

    static func dismantleNSView(_ view: Anchor, coordinator: Coordinator) { coordinator.detach() }

    final class Anchor: NSView {
        weak var coordinator: Coordinator?
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            coordinator?.attach(to: window)
        }
        override func hitTest(_ point: NSPoint) -> NSView? { nil }
    }

    @MainActor final class Coordinator: NSObject, NSWindowDelegate {
        let panel: ToolPanel
        let host = NSHostingView(rootView: AnyView(EmptyView()))
        private let key: String
        private var mouseMonitor: Any?
        /// SwiftUI may replace the AppKit view used for a header while a chat
        /// update is rendering. Keep the last registered region and its
        /// panel-relative bounds so that brief replacement gaps cannot make a
        /// live tool window stop being draggable.
        var header: NSView? {
            didSet {
                panel.dragRegion = header
                updateDragRegionFrame()
            }
        }
        var wasFrontmost = false

        init(key: String, initialSize: CGSize) {
            self.key = key
            panel = ToolPanel(contentRect: NSRect(origin: .zero, size: initialSize),
                styleMask: [.borderless, .resizable, .nonactivatingPanel], backing: .buffered, defer: false)
            super.init()
            panel.delegate = self
            panel.identifier = NSUserInterfaceItemIdentifier("game-overlay-tool.\(key)")
            panel.contentView = host
            host.wantsLayer = true
            host.layer?.cornerRadius = 18
            host.layer?.masksToBounds = true
            panel.contentMinSize = NSSize(width: 340, height: 280)
            panel.level = .gameOverlay
            panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .ignoresCycle]
            panel.hidesOnDeactivate = false
            panel.becomesKeyOnlyIfNeeded = false
            panel.isReleasedWhenClosed = false
            panel.isOpaque = false
            panel.backgroundColor = .clear
            panel.hasShadow = true
            panel.isMovable = true
            panel.animationBehavior = .none
            panel.setFrameAutosaveName("overlay.native.\(key)")
            mouseMonitor = NSEvent.addLocalMonitorForEvents(matching: .leftMouseDown) { [weak self] event in
                let consumed = MainActor.assumeIsolated { () -> Bool in
                    guard let self, event.window === self.panel,
                          self.panel.acceptsDrag(at: event.locationInWindow) else { return false }
                    let screenPoint = self.panel.convertPoint(toScreen: event.locationInWindow)
                    let element = self.host.accessibilityHitTest(screenPoint)
                    let role = (element as? NSAccessibilityElement)?.accessibilityRole()
                        ?? (element as? NSView)?.accessibilityRole()
                    if let role,
                       [NSAccessibility.Role.button, .textField, .textArea, .popUpButton, .checkBox, .link].contains(role) {
                        return false
                    }
                    // The close button floats above the header's content.
                    if event.locationInWindow.x > self.panel.frame.width - 48 { return false }
                    // Raise before AppKit enters its drag tracking loop so the
                    // window follows the pointer above its sibling tools.
                    if let parent = self.panel.parent {
                        parent.removeChildWindow(self.panel)
                        parent.addChildWindow(self.panel, ordered: .above)
                    }
                    self.panel.makeKeyAndOrderFront(nil)
                    NSCursor.closedHand.push()
                    defer { NSCursor.pop() }
                    self.panel.performDrag(with: event)
                    self.panel.saveFrame(usingName: "overlay.native.\(self.key)")
                    return true
                }
                return consumed ? nil : event
            }
        }

        func attach(to parent: NSWindow?) {
            guard let parent, panel.parent !== parent else { return }
            panel.parent?.removeChildWindow(panel)
            if !panel.setFrameUsingName("overlay.native.\(key)") {
                panel.setFrameTopLeftPoint(NSPoint(x: parent.frame.minX + 70, y: parent.frame.maxY - 130))
            }
            parent.addChildWindow(panel, ordered: .above)
            if parent.isVisible { panel.orderFrontRegardless() }
        }

        func windowDidResize(_ notification: Notification) {
            updateDragRegionFrame()
            panel.saveFrame(usingName: "overlay.native.\(key)")
        }

        func updateDragRegionFrame() {
            guard let header, header.window === panel, let content = panel.contentView else { return }
            let headerFrame = header.convert(header.bounds, to: content)
            // Headers always span the tool width. Storing only the vertical
            // strip keeps the fallback accurate after a native resize too.
            panel.dragRegionFrame = NSRect(x: 0, y: headerFrame.minY,
                                           width: content.bounds.width, height: headerFrame.height)
        }

        func detach() {
            if let mouseMonitor { NSEvent.removeMonitor(mouseMonitor) }
            mouseMonitor = nil
            panel.parent?.removeChildWindow(panel)
            panel.orderOut(nil)
            panel.identifier = nil
            host.rootView = AnyView(EmptyView())
        }
    }

    final class ToolPanel: NSPanel {
        /// Strong deliberately: this avoids a drag outage while SwiftUI swaps
        /// the hosting tree during a state update. The next header replaces it.
        var dragRegion: NSView?
        var dragRegionFrame: NSRect?

        func acceptsDrag(at point: NSPoint) -> Bool {
            if let dragRegion, dragRegion.window === self,
               dragRegion.bounds.contains(dragRegion.convert(point, from: nil)) {
                return true
            }
            return dragRegionFrame?.contains(point) == true
        }
        override var canBecomeKey: Bool { true }
        override var canBecomeMain: Bool { false }
    }
}

/// Tool headers gain dragging only when hosted in the overlay workspace.
struct OverlayWindowDrag: Sendable {
    var register: @MainActor @Sendable (NSView) -> Void
}

extension EnvironmentValues {
    @Entry var overlayWindowDrag: OverlayWindowDrag? = nil
}

struct OverlayDraggableHeader: ViewModifier {
    @Environment(\.overlayWindowDrag) private var drag

    func body(content: Content) -> some View {
        if let drag {
            content.background(OverlayHeaderRegion(register: drag.register).padding(-8))
        } else { content }
    }
}

private struct OverlayHeaderRegion: NSViewRepresentable {
    let register: @MainActor (NSView) -> Void
    func makeNSView(context: Context) -> NSView {
        let region = Region()
        region.register = register
        return region
    }
    func updateNSView(_ view: NSView, context: Context) {
        (view as? Region)?.register = register
        register(view)
        view.window?.invalidateCursorRects(for: view)
    }

    @MainActor final class Region: NSView {
        var register: (@MainActor (NSView) -> Void)?
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            register?(self)
        }
        override func layout() {
            super.layout()
            register?(self)
        }
        override func hitTest(_ point: NSPoint) -> NSView? { nil }
        override func resetCursorRects() {
            super.resetCursorRects()
            addCursorRect(bounds, cursor: .openHand)
        }
    }
}

private struct OverlayWorkspaceBrowser: View {
    let overlay: GameOverlayController
    let requestedURL: URL
    @State private var tabs: [BrowserTab] = []
    @State private var selected: UUID?
    @State private var address = ""
    private var current: BrowserTab? { tabs.first { $0.id == selected } }

    var body: some View {
        VStack(spacing: 0) {
            ScrollView(.horizontal) {
                HStack {
                    ForEach(tabs) { tab in
                        HStack {
                            Button { selected = tab.id; address = tab.browser.url?.absoluteString ?? "" } label: {
                                Text(tab.browser.title ?? L10n.string("ui.overlay.workspace.browser"))
                                    .lineLimit(1).frame(maxWidth: 150)
                            }
                            .tint(selected == tab.id ? ObeliskStyle.accent : .secondary)
                            Button {
                                tabs.removeAll { $0.id == tab.id }
                                if selected == tab.id { selected = tabs.last?.id }
                            } label: { Image(systemName: "xmark") }
                            .accessibilityLabel(L10n.string("ui.common.close"))
                        }
                    }
                    Button { add(URL(string: "https://www.google.com")!) } label: { Image(systemName: "plus") }
                        .accessibilityLabel(L10n.string("ui.overlay.workspace.newTab"))
                }.padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .modifier(OverlayDraggableHeader())
            HStack {
                Button { current?.browser.webView.goBack() } label: { Image(systemName: "chevron.left") }
                    .disabled(current?.browser.canGoBack != true)
                    .accessibilityLabel(L10n.string("ui.overlay.back"))
                Button { current?.browser.webView.goForward() } label: { Image(systemName: "chevron.right") }
                    .accessibilityLabel(L10n.string("ui.overlay.workspace.forward"))
                Button { current?.browser.webView.reload() } label: { Image(systemName: "arrow.clockwise") }
                    .accessibilityLabel(L10n.string("ui.overlay.web.reload"))
                TextField(L10n.string("ui.overlay.workspace.address"), text: $address)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit(navigate)
            }.padding(8)
            ZStack {
                ForEach(tabs) { tab in
                    WorkspaceWebView(webView: tab.browser.webView)
                        .opacity(selected == tab.id ? 1 : 0)
                        .allowsHitTesting(selected == tab.id)
                        .accessibilityHidden(selected != tab.id)
                }
            }
        }
        .onAppear { if tabs.isEmpty { add(requestedURL) } }
        .onChange(of: requestedURL) { _, url in add(url) }
        .onChange(of: current?.browser.url) { _, url in address = url?.absoluteString ?? "" }
    }

    private func add(_ url: URL) {
        Task { @MainActor in
            let cookies = await overlay.webCookies(for: url)
            guard !Task.isCancelled else { return }
            let tab = BrowserTab(browser: WebPageBrowser(WebPageStart(url: url, cookies: cookies)))
            tabs.append(tab); selected = tab.id; address = url.absoluteString
        }
    }

    private func navigate() {
        let text = address.trimmingCharacters(in: .whitespacesAndNewlines)
        let url: URL?
        if text.contains(" ") || !text.contains(".") {
            var components = URLComponents(string: "https://www.google.com/search")!
            components.queryItems = [URLQueryItem(name: "q", value: text)]
            url = components.url
        } else { url = URL(string: text.contains("://") ? text : "https://\(text)") }
        guard let url, ["http", "https"].contains(url.scheme?.lowercased() ?? "") else { return }
        if let current { current.browser.webView.load(URLRequest(url: url)) } else { add(url) }
    }

    private struct BrowserTab: Identifiable {
        let id = UUID()
        let browser: WebPageBrowser
    }
}

private struct WorkspaceWebView: NSViewRepresentable {
    let webView: WKWebView
    func makeNSView(context: Context) -> WKWebView { webView }
    func updateNSView(_ view: WKWebView, context: Context) {}
}
