import AppKit
import ImageIO
import SwiftUI

struct FriendsPanel: View {
    let state: FriendsPresentationState
    let avatars: FriendsAvatarCache
    let focus: FriendsPanelFocus
    let dismiss: @MainActor () -> Void
    let retry: @MainActor () -> Void
    let signIn: @MainActor () -> Void
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    @State private var search = ""
    @State private var offlineExpanded = false
    @FocusState private var focusedControl: FriendsPanelFocus.Control?

    var body: some View {
        let sections = state.sections(matching: search)
        VStack(spacing: 0) {
            header
            searchField.padding(.horizontal, 22).padding(.bottom, 16)
            Divider()
            if state.connection != .connected && state.connection != .loading {
                HStack(alignment: .center, spacing: 12) {
                    Label(state.summary, systemImage: "exclamationmark.circle")
                        .frame(maxWidth: .infinity, alignment: .leading)
                    if state.connection == .authenticationRequired {
                        Button(L10n.string("ui.friends.signIn"), action: signIn)
                            .pointerCursor()
                            .focused($focusedControl, equals: .reconnect)
                    } else {
                        Button(L10n.string("ui.friends.retry"), action: retry)
                            .pointerCursor()
                            .focused($focusedControl, equals: .reconnect)
                    }
                }
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 22).padding(.vertical, 12)
                    .background(SailOverStyle.accent.opacity(0.06))
                    .accessibilityIdentifier("friends-connection-status")
            }
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 4) {
                    if state.friends.isEmpty {
                        if state.connection == .loading {
                            ProgressView(L10n.string("ui.friends.loading"))
                                .frame(maxWidth: .infinity, minHeight: 200)
                        } else if state.connection == .connected {
                            emptyMessage("ui.friends.empty", symbol: "person.2")
                        } else {
                            emptyMessage("ui.friends.unavailable", symbol: "wifi.exclamationmark")
                        }
                    } else if sections.isEmpty {
                        emptyMessage("ui.friends.noResults", symbol: "magnifyingglass")
                    } else {
                        if !sections.hasOnline && !sections.isSearching && state.connection == .connected {
                            Text(L10n.string("ui.friends.noOnline"))
                                .font(.system(size: 13)).foregroundStyle(.secondary)
                                .padding(.horizontal, 10).padding(.vertical, 14)
                        }
                        rows(sections.playing)
                        rows(sections.online)
                        if !sections.unknown.isEmpty {
                            sectionHeading(L10n.string("ui.friends.presence.unknown"))
                            rows(sections.unknown)
                        }
                        if !sections.offline.isEmpty {
                            Button {
                                offlineExpanded.toggle()
                            } label: {
                                HStack(spacing: 7) {
                                    Image(systemName: sections.showsOffline(isExpanded: offlineExpanded) ? "chevron.down" : "chevron.right")
                                        .font(.system(size: 10, weight: .semibold)).frame(width: 12)
                                    Text(L10n.string("ui.friends.offlineCount", Int64(sections.offline.count)))
                                    Spacer()
                                }
                                .font(.system(size: 12, weight: .semibold))
                                .foregroundStyle(.secondary)
                                .padding(.horizontal, 10).padding(.vertical, 12)
                                .contentShape(Rectangle())
                            }
                            .sailOverButtonStyle(.plain)
                            .focused($focusedControl, equals: .offline)
                            .disabled(sections.isSearching)
                            .accessibilityValue(L10n.string(sections.showsOffline(isExpanded: offlineExpanded) ? "ui.friends.expanded" : "ui.friends.collapsed"))
                            .accessibilityIdentifier("friends-offline-toggle")
                            if sections.showsOffline(isExpanded: offlineExpanded) { rows(sections.offline) }
                        }
                    }
                }
                .padding(.horizontal, 12).padding(.vertical, 10)
            }
            .accessibilityIdentifier("friends-scroll")
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background {
            if reduceTransparency { SailOverPalette(colorScheme: colorScheme).background }
            else {
                Rectangle().fill(.regularMaterial)
                    .overlay { SailOverPalette(colorScheme: colorScheme).background.opacity(0.96) }
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: 18))
        .overlay { RoundedRectangle(cornerRadius: 18).strokeBorder(SailOverPalette(colorScheme: colorScheme).border, lineWidth: 1) }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(state.title)
        .accessibilityIdentifier("friends-panel")
        .onAppear { synchronizeFocus(sections: sections) }
        .onChange(of: search) { _, _ in synchronizeFocus(sections: sections) }
        .onChange(of: sections.offline.isEmpty) { _, _ in synchronizeFocus(sections: sections) }
        .onChange(of: state.connection) { _, _ in synchronizeFocus(sections: sections) }
        .onChange(of: focus.target) { _, target in focusedControl = target }
        .onChange(of: focusedControl) { _, target in if let target { focus.target = target } }
        .onExitCommand(perform: dismiss)
    }

    private var header: some View {
        HStack(spacing: 12) {
            Image(state.store.logoAssetName).resizable().scaledToFit()
                .frame(width: 32, height: 32).accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(state.title).font(.system(size: 20, weight: .semibold))
                Text(state.headerSummary)
                    .font(.system(size: 12)).foregroundStyle(.secondary).lineLimit(2)
            }
            Spacer(minLength: 12)
            Button(action: dismiss) {
                Image(systemName: "xmark").font(.system(size: 12, weight: .semibold))
                    .frame(width: 30, height: 30)
                    .background(.primary.opacity(0.07), in: Circle())
                    .contentShape(Circle())
            }
            .sailOverButtonStyle(.plain)
            .focused($focusedControl, equals: .close)
            .accessibilityLabel(L10n.string("ui.friends.close"))
            .accessibilityIdentifier("friends-close")
            .help(L10n.string("ui.friends.close"))
        }
        .padding(22).padding(.bottom, 2)
    }

    private var searchField: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass").foregroundStyle(.secondary).accessibilityHidden(true)
            TextField(L10n.string("ui.friends.search"), text: $search)
                .textFieldStyle(.plain)
                .focused($focusedControl, equals: .search)
                .accessibilityLabel(L10n.string("ui.friends.search"))
                .accessibilityIdentifier("friends-search")
            if !search.isEmpty {
                Button { search = ""; focus.target = .search } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
                }
                .sailOverButtonStyle(.plain)
                .focused($focusedControl, equals: .clear)
                .accessibilityLabel(L10n.string("ui.search.clear"))
            }
        }
        .font(.system(size: 13))
        .padding(.horizontal, 12).frame(height: 36)
        .background(.primary.opacity(0.05), in: RoundedRectangle(cornerRadius: 9))
        .overlay { RoundedRectangle(cornerRadius: 9).strokeBorder(.primary.opacity(0.08), lineWidth: 1) }
    }

    private func rows(_ friends: [FriendPresentation]) -> some View {
        ForEach(friends) { friend in FriendRow(friend: friend, avatars: avatars) }
    }

    private func sectionHeading(_ title: String) -> some View {
        Text(title).font(.system(size: 12, weight: .semibold)).foregroundStyle(.secondary)
            .padding(.horizontal, 10).padding(.top, 14).padding(.bottom, 6)
    }

    private func emptyMessage(_ key: String, symbol: String) -> some View {
        VStack(spacing: 12) {
            Image(systemName: symbol).font(.system(size: 30)).accessibilityHidden(true)
            Text(L10n.string(key)).font(.system(size: 13)).multilineTextAlignment(.center)
        }
        .foregroundStyle(.secondary)
        .frame(maxWidth: .infinity, minHeight: 200)
        .accessibilityIdentifier("friends-empty-state")
    }

    private func synchronizeFocus(sections: FriendsSections) {
        focus.controls = [.close, .search] + (search.isEmpty ? [] : [.clear])
            + (state.connection != .connected && state.connection != .loading ? [.reconnect] : [])
            + (!sections.offline.isEmpty && !sections.isSearching ? [.offline] : [])
        if !focus.controls.contains(focus.target) { focus.target = .search }
        focusedControl = focus.target
    }
}

struct FriendRow: View {
    let friend: FriendPresentation
    let avatars: FriendsAvatarCache
    @State private var avatar: NSImage?

    var body: some View {
        HStack(spacing: 12) {
            ZStack {
                RoundedRectangle(cornerRadius: 10).fill(.primary.opacity(0.08))
                if let avatar { Image(nsImage: avatar).resizable().scaledToFill() }
                else if friend.initials.isEmpty {
                    Image(systemName: "person.fill").font(.system(size: 19)).foregroundStyle(.secondary)
                } else {
                    Text(friend.initials).font(.system(size: 15, weight: .semibold)).foregroundStyle(.secondary)
                }
            }
            .frame(width: 42, height: 42).clipShape(RoundedRectangle(cornerRadius: 10))
            .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(friend.displayName).font(.system(size: 13, weight: .semibold)).lineLimit(1)
                HStack(spacing: 5) {
                    Circle().fill(presenceColor).frame(width: 5, height: 5).accessibilityHidden(true)
                    Text(friend.presence.title).font(.system(size: 11)).foregroundStyle(.secondary)
                }
                if let activity = friend.activity {
                    Text(activity.title).font(.system(size: 11)).foregroundStyle(presenceColor).lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 10).padding(.vertical, 9)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(friend.displayName)
        .accessibilityValue(friend.status)
        .help([friend.displayName, friend.status].joined(separator: "\n"))
        .task(id: FriendsAvatarCache.Key(identity: friend.id, url: friend.avatarURL)) {
            avatar = nil
            let image = await avatars.image(for: friend.id, url: friend.avatarURL)
            guard !Task.isCancelled else { return }
            avatar = image
        }
    }

    private var presenceColor: Color {
        if friend.presence == .unknown { return .secondary }
        if !friend.presence.isOnline { return .secondary }
        if friend.activity != nil { return Color(red: 0.3, green: 0.68, blue: 0.4) }
        return Color(red: 0.32, green: 0.59, blue: 0.87)
    }
}

@MainActor @Observable
final class FriendsPanelFocus {
    enum Control: Hashable { case close, search, clear, offline, reconnect }
    var target: Control = .search
    var controls: [Control] = [.close, .search]

    func advance(backward: Bool) {
        guard !controls.isEmpty else { return }
        let index = controls.firstIndex(of: target) ?? 0
        target = controls[(index + (backward ? controls.count - 1 : 1)) % controls.count]
    }
}

/// Images stay in memory for this account only. URLSession uses no disk cache.
@MainActor
final class FriendsAvatarCache {
    struct Key: Hashable { let identity: FriendIdentity; let url: URL? }
    private var account: FriendsAccountIdentity?
    private var images: [Key: NSImage] = [:]
    private var order: [Key] = []
    private var pending: [Key: Task<NSImage?, Never>] = [:]
    private let load: @Sendable (URL) async throws -> Data
    private nonisolated static let session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.urlCache = nil
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        return URLSession(configuration: configuration)
    }()

    init(load: @escaping @Sendable (URL) async throws -> Data = FriendsAvatarCache.fetch) {
        self.load = load
    }

    private nonisolated static func fetch(_ url: URL) async throws -> Data {
        let (bytes, response) = try await session.bytes(for: URLRequest(url: url, timeoutInterval: 20))
        guard let response = response as? HTTPURLResponse, response.statusCode == 200,
              response.expectedContentLength <= 2_097_152 else { return Data() }
        var data = Data()
        for try await byte in bytes {
            guard data.count < 2_097_152, !Task.isCancelled else { return Data() }
            data.append(byte)
        }
        return data
    }
    private var generation = UUID()

    func activate(account: FriendsAccountIdentity?) {
        guard self.account != account else { return }
        pending.values.forEach { $0.cancel() }
        pending.removeAll()
        images.removeAll()
        order.removeAll()
        generation = UUID()
        self.account = account
    }

    func image(for identity: FriendIdentity, url: URL?) async -> NSImage? {
        guard account == identity.account, let url, url.scheme == "https" else { return nil }
        let key = Key(identity: identity, url: url)
        if let image = images[key] { return image }
        let requestGeneration = generation
        // Scrolling can cancel a row before its shared request finishes. Wait
        // for a real completion instead of creating unbounded network tasks.
        while pending[key] == nil && pending.count >= 8 {
            guard let active = pending.values.first else { break }
            _ = await active.value
            guard !Task.isCancelled, generation == requestGeneration, account == identity.account else { return nil }
            // Let the request owner remove its completed entry.
            await Task.yield()
        }
        guard !Task.isCancelled else { return nil }
        if let task = pending[key] {
            let image = await task.value
            return generation == requestGeneration && account == identity.account ? image : nil
        }
        let task = Task<NSImage?, Never> { [load] in
            do {
                let data = try await load(url)
                guard !Task.isCancelled, let image = ImageDecoding.thumbnail(from: data, maxPixelSize: 96) else { return nil }
                return NSImage(cgImage: image, size: NSSize(width: image.width, height: image.height))
            } catch { return nil }
        }
        pending[key] = task
        let image = await task.value
        guard generation == requestGeneration, account == identity.account else { return nil }
        pending.removeValue(forKey: key)
        if let image {
            images[key] = image
            order.append(key)
            while order.count > 128 { images.removeValue(forKey: order.removeFirst()) }
        }
        return image
    }
}
