import SwiftUI

/// The floating bar over a library: its own controls on the leading edge,
/// such as a storefront's account, and the library's search on the trailing
/// one. On a game's page, a back button comes first and the search leaves;
/// the game's title takes the middle once its page scrolled under the bar.
/// Its backdrop only shows while content scrolls beneath it, never over a
/// hero's artwork.
struct LibraryGlassBar<Leading: View>: View {
    @Binding var searchText: String
    /// Names what the search looks through, for VoiceOver.
    let searchLabel: String
    var showsBackdrop = true
    /// The game whose page shows, if any.
    var game: GameBar?
    @ViewBuilder let leading: Leading
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    /// What the bar shows of a game's page.
    struct GameBar {
        let title: String
        /// Names the library the back button returns to.
        let libraryTitle: String
        /// Whether the page's own title scrolled under the bar.
        var showsTitle: Bool
        let back: () -> Void
    }

    var body: some View {
        GlassEffectContainer(spacing: 6) {
            HStack(spacing: 10) {
                if let game {
                    // Back to the library, also with Command-[.
                    StorefrontGlassButton(systemName: "chevron.left", label: game.libraryTitle,
                                          identifier: "storefront-back", action: game.back)
                        .keyboardShortcut("[", modifiers: .command)
                        .transition(.scale(scale: 0.4).combined(with: .opacity))
                }

                leading

                Spacer(minLength: 12)
                    .overlay { StorefrontWindowDragRegion() }

                if game == nil {
                    LibrarySearchField(label: searchLabel, text: $searchText)
                        .transition(.scale(scale: 0.85, anchor: .trailing).combined(with: .opacity))
                }
            }
        }
        .padding(.horizontal, 20)
        .frame(height: ObeliskStyle.titleBarHeight)
        .overlay {
            ZStack {
                if let game, game.showsTitle {
                    Text(game.title)
                        .font(.system(size: 13, weight: .semibold))
                        .lineLimit(1)
                        // Clear of the controls on both edges.
                        .padding(.horizontal, 250)
                        .transition(.opacity.combined(with: .offset(y: 6)))
                }
            }
            .animation(reduceMotion ? nil : .easeOut(duration: 0.2), value: game?.showsTitle == true)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
        }
        .background {
            Rectangle()
                .fill(.bar)
                .opacity(showsBackdrop ? 1 : 0)
                .animation(reduceMotion ? nil : .easeInOut(duration: 0.2), value: showsBackdrop)
                .allowsHitTesting(false)
        }
    }
}

/// The account's name atop its menu. On a storefront with friends it also
/// tells the account's status, and its submenu picks another.
struct AccountMenuName: View {
    let name: String
    /// Nil on a storefront without friends.
    let status: OnlineStatusChoice?

    var body: some View {
        if let status {
            Picker(selection: Binding(get: { status.status }, set: { status.choose($0) })) {
                ForEach(OnlineStatus.allCases) { Text($0.title).tag($0) }
            } label: {
                Text(L10n.string("ui.account.name_status", name,
                                 status.status.title.lowercased(with: L10n.locale)))
            }
            .pickerStyle(.menu)
            .accessibilityIdentifier("storefront-account-status")
        } else {
            Text(name)
        }
    }
}

/// The store and its signed-in account, opening the account menu.
struct LibraryAccountPill<MenuContent: View>: View {
    let storeTitle: String
    let accountName: String?
    let avatarURL: URL?
    var unreadCount = 0
    @ViewBuilder let menu: MenuContent

    private var name: String? {
        let name = accountName?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return name.isEmpty ? nil : name
    }

    private var accountLabel: String { name ?? L10n.string("ui.account.store", storeTitle) }

    var body: some View {
        Menu { menu } label: {
            HStack(spacing: 9) {
                AccountAvatar(name: name, url: avatarURL)
                    .frame(width: 28, height: 28)
                    .overlay(alignment: .topTrailing) {
                        if unreadCount > 0 {
                            UnreadBadge(count: unreadCount, compact: true)
                                .offset(x: 6, y: -5)
                                .accessibilityHidden(true)
                        }
                    }
                VStack(alignment: .leading, spacing: 0) {
                    Text(storeTitle)
                        .font(.system(size: 13, weight: .semibold))
                    if let name {
                        Text(name)
                            .font(.system(size: 10, weight: .medium))
                            .foregroundStyle(.secondary)
                    }
                }
                .lineLimit(1)
                Image(systemName: "chevron.down")
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
            }
            .foregroundStyle(.primary)
            .padding(.leading, 4)
            .padding(.trailing, 13)
            .frame(height: 36)
            .modifier(StorefrontControlGlass())
            .contentShape(.capsule)
        }
        .menuStyle(.button)
        .obeliskButtonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel(accountLabel)
        .accessibilityValue(unreadCount > 0 ? L10n.string("ui.chat.unreadCount", Int64(unreadCount)) : "")
        .accessibilityIdentifier("storefront-account-menu")
        .help(accountLabel)
    }
}

/// A round account picture, or the account's initials while it has none.
struct AccountAvatar: View {
    let name: String?
    let url: URL?

    var body: some View {
        ZStack {
            Circle().fill(.primary.opacity(0.12))
            CachedArtwork(url: url, maxPixelSize: 64) { image in
                image.resizable().scaledToFill()
            } placeholder: {
                placeholder
            }
        }
        .clipShape(.circle)
        .overlay { Circle().strokeBorder(.white.opacity(0.18), lineWidth: 0.75) }
        .accessibilityHidden(true)
    }

    @ViewBuilder
    private var placeholder: some View {
        let initials = (name ?? "").initials
        if initials.isEmpty {
            Image(systemName: "person.fill")
                .font(.system(size: 12, weight: .medium))
        } else {
            Text(initials)
                .font(.system(size: 11, weight: .semibold))
        }
    }
}

struct LibrarySearchField: View {
    /// Names what the search looks through, for VoiceOver.
    let label: String
    @Binding var text: String
    @FocusState private var isFocused: Bool

    var body: some View {
        HStack(spacing: 7) {
            Image(systemName: "magnifyingglass")
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            TextField(L10n.string("ui.search.placeholder"), text: $text)
                .textFieldStyle(.plain)
                .focused($isFocused)
                .accessibilityLabel(label)
                .accessibilityIdentifier("storefront-search")
                .onExitCommand {
                    if text.isEmpty { isFocused = false }
                    else { text = "" }
                }
            if !text.isEmpty {
                Button { text = "" } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
                }
                .obeliskButtonStyle(.plain)
                .accessibilityLabel(L10n.string("ui.search.clear"))
            }
        }
        .font(.system(size: 13))
        .padding(.horizontal, 13)
        .frame(height: 36)
        .frame(minWidth: 160, idealWidth: 250, maxWidth: 280)
        .modifier(StorefrontControlGlass(interactive: false))
    }
}

/// The library's filters as one glass capsule; the selection slides between them.
struct LibraryFilterPicker: View {
    let storeTitle: String
    let filters: [StorefrontLibraryFilter]
    @Binding var selection: StorefrontLibraryFilter
    @Namespace private var namespace
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        HStack(spacing: 2) {
            ForEach(filters) { filter in
                let isSelected = filter == selection
                Button {
                    withAnimation(reduceMotion ? nil : ObeliskStyle.selectionAnimation) {
                        selection = filter
                    }
                } label: {
                    Text(filter.title)
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(isSelected ? .primary : .secondary)
                        .lineLimit(1)
                        .padding(.horizontal, 12)
                        .frame(height: 28)
                        .background {
                            if isSelected {
                                Capsule()
                                    .fill(.primary.opacity(0.13))
                                    .matchedGeometryEffect(id: "selection", in: namespace)
                            }
                        }
                        .contentShape(.capsule)
                }
                .obeliskButtonStyle(.plain)
                .accessibilityAddTraits(isSelected ? .isSelected : [])
                .accessibilityIdentifier("storefront-filter-option-\(filter.id)")
            }
        }
        .padding(4)
        .modifier(StorefrontControlGlass(interactive: false))
        .fixedSize()
        .accessibilityElement(children: .contain)
        .accessibilityLabel(L10n.string("ui.filter.library_named", storeTitle))
        .accessibilityIdentifier("storefront-filter-menu")
    }
}

/// The library's sort order, chosen from a popover.
struct LibrarySortButton: View {
    let storeTitle: String
    let sortTitle: String
    let options: [StorefrontSortOption]
    @State private var isPresented = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        Button { isPresented.toggle() } label: {
            HStack(spacing: 7) {
                Image(systemName: "arrow.up.arrow.down")
                    .font(.system(size: 12, weight: .semibold))
                Text(sortTitle)
                    .font(.system(size: 12, weight: .semibold))
                    .lineLimit(1)
                    .contentTransition(.interpolate)
            }
            .foregroundStyle(.primary)
            .padding(.horizontal, 14)
            .frame(height: 36)
            // The outgoing title stays inside while the capsule resizes.
            .clipShape(.capsule)
            .modifier(StorefrontControlGlass())
            .contentShape(.capsule)
        }
        .obeliskButtonStyle(.plain)
        .fixedSize()
        .popover(isPresented: $isPresented, arrowEdge: .bottom) {
            StorefrontSortPopover(options: options.map(animated)) { isPresented = false }
        }
        .disabled(options.isEmpty)
        .accessibilityLabel(L10n.string("ui.sort.library_named", storeTitle))
        .accessibilityValue(sortTitle)
        .accessibilityIdentifier("storefront-sort-menu")
        .help(L10n.string("ui.sort.library"))
    }

    /// Applies the option with the filters' motion: the capsule resizes and
    /// the games slide to their new places.
    private func animated(_ option: StorefrontSortOption) -> StorefrontSortOption {
        let animation = reduceMotion ? nil : ObeliskStyle.selectionAnimation
        return StorefrontSortOption(id: option.id, title: option.title, isSelected: option.isSelected) {
            withAnimation(animation) { option.action() }
        }
    }
}
