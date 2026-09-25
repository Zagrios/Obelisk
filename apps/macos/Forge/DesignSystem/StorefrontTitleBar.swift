import AppKit
import SwiftUI

struct StorefrontSortOption: Identifiable {
    let id: String
    let title: String
    let isSelected: Bool
    let action: @MainActor () -> Void
}

struct StorefrontTitleBar<AccountMenu: View>: View {
    let title: String

    let accountName: String?
    let avatarURL: URL?
    @Binding private var searchText: String
    let sortTitle: String
    let sortOptions: [StorefrontSortOption]
    @Binding private var filter: StorefrontLibraryFilter
    let availableFilters: [StorefrontLibraryFilter]
    let showsLibraryControls: Bool
    let isConnected: Bool
    private let accountMenu: AccountMenu
    private let sortButtonWidth: CGFloat

    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @FocusState private var searchIsFocused: Bool
    @State private var sortIsPresented = false

    init(
        title: String,
        accountName: String?,
        avatarURL: URL? = nil,
        searchText: Binding<String>,
        sortTitle: String = L10n.string("ui.sort.name"),
        sortOptions: [StorefrontSortOption] = [],
        filter: Binding<StorefrontLibraryFilter> = .constant(.all),
        availableFilters: [StorefrontLibraryFilter] = [],
        showsLibraryControls: Bool = true,
        isConnected: Bool,
        @ViewBuilder accountMenu: () -> AccountMenu
    ) {
        self.title = title
        self.accountName = accountName
        self.avatarURL = avatarURL
        self._searchText = searchText
        self.sortTitle = sortTitle
        self.sortOptions = sortOptions
        self._filter = filter
        self.availableFilters = availableFilters
        self.showsLibraryControls = showsLibraryControls
        self.isConnected = isConnected
        self.accountMenu = accountMenu()
        // Measured once per title change rather than on every body evaluation.
        let textWidth = (sortTitle as NSString).size(withAttributes: [
            .font: NSFont.systemFont(ofSize: 13, weight: .medium),
        ]).width
        sortButtonWidth = ceil(textWidth) + 20 + 7 + 24
    }

    var body: some View {
        HStack(spacing: 12) {
            if isConnected {
                Menu { accountMenu } label: { avatar }
                    .menuStyle(.button)
                    .forgeButtonStyle(.plain)
                    .menuIndicator(.hidden)
                    .fixedSize()
                    .accessibilityLabel(accountLabel)
                    .accessibilityIdentifier("storefront-account-menu")
                    .help(accountLabel)
            } else {
                avatar.accessibilityLabel(accountLabel)
            }

            Text(title)
                .font(.system(size: 13, weight: .semibold))
                .lineLimit(1)

            Spacer(minLength: 12)
                .overlay { StorefrontWindowDragRegion() }

            if showsLibraryControls {
                Button { sortIsPresented.toggle() } label: {
                    HStack(spacing: 7) {
                        Image(systemName: "arrow.up.arrow.down")
                            .font(.system(size: 14, weight: .semibold))
                            .foregroundStyle(.white)
                            .frame(width: 20)
                        Text(sortTitle)
                            .font(.system(size: 13, weight: .medium))
                            .lineLimit(1)
                    }
                    .foregroundStyle(.primary)
                    .padding(.horizontal, 12)
                    .frame(width: sortButtonWidth, height: 38)
                    .modifier(StorefrontControlGlass())
                    .contentShape(Capsule())
                }
                .forgeButtonStyle(.plain)
                .fixedSize()
                .animation(reduceMotion ? nil : .easeInOut(duration: 0.2), value: sortTitle)
                .popover(isPresented: $sortIsPresented, arrowEdge: .bottom) {
                    StorefrontSortPopover(options: sortOptions) { sortIsPresented = false }
                }
                .disabled(sortOptions.isEmpty)
                .accessibilityLabel(L10n.string("ui.sort.library_named", title))
                .accessibilityValue(sortTitle)
                .accessibilityIdentifier("storefront-sort-menu")
                .help(L10n.string("ui.sort.library"))

                if !availableFilters.isEmpty {
                    filterMenu
                }

                searchField
            }
        }
        .padding(.horizontal, 20)
        .frame(height: ForgeStyle.titleBarHeight)
        .background {
            if reduceTransparency {
                ForgePalette(colorScheme: colorScheme).background
            } else {
                StorefrontTitleBarMaterial()
            }
        }
        .overlay(alignment: .bottom) {
            Rectangle()
                .fill(ForgePalette(colorScheme: colorScheme).border)
                .frame(height: 1)
                .allowsHitTesting(false)
        }
    }

    private var accountLabel: String {
        let name = accountName?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return name.isEmpty ? L10n.string("ui.account.store", title) : name
    }

    private var initials: String { (accountName ?? "").initials }

    private var avatar: some View {
        ZStack {
            Circle().fill(.primary.opacity(0.09))
            if let avatarURL {
                AsyncImage(url: avatarURL) { phase in
                    if let image = phase.image {
                        image.resizable().scaledToFill()
                    } else {
                        avatarPlaceholder
                    }
                }
            } else {
                avatarPlaceholder
            }
        }
        .frame(width: 38, height: 38)
        .clipShape(Circle())
        .modifier(StorefrontControlGlass())
        .overlay {
            if avatarURL != nil {
                Circle().strokeBorder(.white.opacity(0.2), lineWidth: 0.75)
                    .allowsHitTesting(false)
            }
        }
        .contentShape(Circle())
    }

    @ViewBuilder
    private var avatarPlaceholder: some View {
        if initials.isEmpty {
            Image(systemName: "person.fill")
                .font(.system(size: 15, weight: .medium))
                .foregroundStyle(.primary)
        } else {
            Text(initials)
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(.primary)
        }
    }

    private var searchField: some View {
        HStack(spacing: 7) {
            Image(systemName: "magnifyingglass")
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            TextField(L10n.string("ui.search.placeholder"), text: $searchText)
                .textFieldStyle(.plain)
                .focused($searchIsFocused)
                .accessibilityLabel(L10n.string("ui.search.store_library", title))
                .accessibilityIdentifier("storefront-search")
                .onExitCommand {
                    if searchText.isEmpty { searchIsFocused = false }
                    else { searchText = "" }
                }
            if !searchText.isEmpty {
                Button { searchText = "" } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
                }
                .forgeButtonStyle(.plain)
                .accessibilityLabel(L10n.string("ui.search.clear"))
            }
        }
        .font(.system(size: 13))
        .foregroundStyle(.primary)
        .padding(.horizontal, 13)
        .frame(height: 38)
        .modifier(StorefrontControlGlass())
        .frame(minWidth: 160, idealWidth: 240, maxWidth: 300)
    }

    private var filterMenu: some View {
        Menu {
            Picker(L10n.string("ui.filter.library"), selection: $filter) {
                ForEach(availableFilters) { option in
                    Text(option.title)
                        .tag(option)
                        .accessibilityIdentifier("storefront-filter-option-\(option.id)")
                }
            }
            .pickerStyle(.inline)
            .labelsHidden()
        } label: {
            Image(systemName: filter == .all ? "line.3.horizontal.decrease" : "line.3.horizontal.decrease.circle.fill")
                .font(.system(size: 18, weight: .medium))
                .foregroundStyle(.primary)
                .frame(width: 38, height: 38)
                .modifier(StorefrontControlGlass())
                .contentShape(Circle())
        }
        .menuStyle(.button)
        .forgeButtonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel(L10n.string("ui.filter.library_named", title))
        .accessibilityValue(filter.title)
        .accessibilityIdentifier("storefront-filter-menu")
        .help(L10n.string("ui.filter.library"))
    }
}

struct StorefrontSortPopover: View {
    let options: [StorefrontSortOption]
    let dismiss: @MainActor () -> Void
    @FocusState private var focusedOption: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(options) { option in
                Button { select(option) } label: {
                    HStack(spacing: 8) {
                        Image(systemName: "checkmark")
                            .font(.system(size: 11, weight: .semibold))
                            .opacity(option.isSelected ? 1 : 0)
                            .frame(width: 14)
                            .accessibilityHidden(true)
                        Text(option.title).lineLimit(1)
                        Spacer(minLength: 0)
                    }
                    .font(.system(size: 13))
                    .foregroundStyle(.primary)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(focusedOption == option.id ? Color.primary.opacity(0.09) : .clear, in: RoundedRectangle(cornerRadius: 6))
                    .contentShape(Rectangle())
                }
                .forgeButtonStyle(.plain)
                .focusable()
                .focused($focusedOption, equals: option.id)
                .focusEffectDisabled()
                .onHover { if $0 { focusedOption = option.id } }
                .accessibilityLabel(option.title)
                .accessibilityAddTraits(option.isSelected ? .isSelected : [])
                .accessibilityIdentifier("storefront-sort-option-\(option.id)")
            }
        }
        .padding(6)
        .frame(minWidth: 230)
        .fixedSize(horizontal: true, vertical: true)
        .onAppear { focusedOption = options.first(where: \.isSelected)?.id ?? options.first?.id }
        .onMoveCommand { direction in
            if direction == .up { moveFocus(by: -1) }
            else if direction == .down { moveFocus(by: 1) }
        }
        .onKeyPress(.return) {
            guard let option = options.first(where: { $0.id == focusedOption }) else { return .ignored }
            select(option)
            return .handled
        }
        .onExitCommand(perform: dismiss)
    }

    private func select(_ option: StorefrontSortOption) {
        option.action()
        dismiss()
    }

    private func moveFocus(by offset: Int) {
        guard !options.isEmpty else { return }
        let current = options.firstIndex(where: { $0.id == focusedOption }) ?? 0
        focusedOption = options[(current + offset + options.count) % options.count].id
    }
}

private struct StorefrontControlGlass: ViewModifier {
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    @ViewBuilder
    func body(content: Content) -> some View {
        if reduceTransparency {
            content.background {
                Capsule()
                    .fill(Color(nsColor: .controlBackgroundColor))
                    .overlay { outline }
            }
        } else {
            content.glassEffect(.regular.interactive(), in: Capsule())
        }
    }

    private var outline: some View {
        Capsule().strokeBorder(
            LinearGradient(
                colors: [.white.opacity(colorScheme == .dark ? 0.24 : 0.65), .primary.opacity(0.1)],
                startPoint: .top,
                endPoint: .bottom
            ),
            lineWidth: 0.75
        )
    }
}

private struct StorefrontTitleBarMaterial: NSViewRepresentable {
    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = .headerView
        view.blendingMode = .withinWindow
        view.state = .followsWindowActiveState
        return view
    }

    func updateNSView(_ nsView: NSVisualEffectView, context: Context) {}
}

private struct StorefrontWindowDragRegion: NSViewRepresentable {
    func makeNSView(context: Context) -> DragRegion {
        let view = DragRegion()
        view.identifier = NSUserInterfaceItemIdentifier("storefront-titlebar-drag-region")
        return view
    }
    func updateNSView(_ nsView: DragRegion, context: Context) {}

    final class DragRegion: NSView {
        override func mouseDown(with event: NSEvent) {
            window?.performDrag(with: event)
        }
    }
}
