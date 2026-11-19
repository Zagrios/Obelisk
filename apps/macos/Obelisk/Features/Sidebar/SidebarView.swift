import SwiftUI

struct SidebarView: View {
    @Bindable var model: AppModel
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        List(selection: selection(where: { $0 != .settings })) {
            row(.home, title: LibrarySection.home.title)

            Section {
                ForEach(model.accounts, id: \.slot) { row($0.tab, title: model.title(of: $0)) }
            } header: {
                Text(L10n.string("ui.library.title"))
                    .font(.system(size: 13, weight: .semibold))
            }
        }
        .listStyle(.sidebar)
        .environment(\.sidebarRowSize, .large)
        .toolbar(removing: .sidebarToggle)
        // The download queue, then Settings, stay at the bottom of the
        // sidebar, whatever its height.
        .safeAreaInset(edge: .bottom, spacing: 0) {
            VStack(spacing: 0) {
                if model.downloads.isVisible {
                    DownloadsSidebarButton(downloads: model.downloads, game: model.downloadGame,
                                           open: model.openDownload)
                        .padding(.horizontal, 10)
                        .padding(.top, 6)
                        .transition(.move(edge: .bottom).combined(with: .opacity))
                }
                List(selection: selection(where: { $0 == .settings })) {
                    row(.settings, title: LibrarySection.settings.title)
                }
                .listStyle(.sidebar)
                .environment(\.sidebarRowSize, .large)
                .scrollDisabled(true)
                .frame(height: 52)
            }
            .animation(.spring(response: 0.35, dampingFraction: 0.85), value: model.downloads.isVisible)
        }
    }

    /// One list's share of the app's single selection.
    private func selection(where owns: @escaping (LibraryTab) -> Bool) -> Binding<LibraryTab?> {
        Binding(
            get: { owns(model.selection) ? model.selection : nil },
            set: { if let tab = $0 { show(tab) } }
        )
    }

    /// Shows the tab at its first page. Picking the tab already shown
    /// leaves its game page with the page's own motion.
    private func show(_ tab: LibraryTab) {
        let animation = tab == model.selection ? ObeliskStyle.pageAnimation(reduceMotion: reduceMotion) : nil
        withAnimation(animation) { model.show(tab) }
    }

    private func row(_ tab: LibraryTab, title: String) -> some View {
        Label {
            Text(title)
                .lineLimit(1)
                .help(title)
        } icon: {
            sidebarIcon(tab.section)
        }
        .listItemTint(tab == .settings ? .monochrome : .fixed(ObeliskStyle.accent))
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        // The list does not tell a click on the row already selected. A click
        // on another row comes through both.
        .simultaneousGesture(TapGesture().onEnded { show(tab) })
        .pointerCursor()
        .tag(tab)
    }

    @ViewBuilder
    private func sidebarIcon(_ section: LibrarySection) -> some View {
        if let logoAssetName = section.logoAssetName {
            Image(logoAssetName)
                .renderingMode(.template)
                .resizable()
                .scaledToFit()
                .frame(width: 20, height: 20)
                .accessibilityHidden(true)
        } else {
            Image(systemName: section.symbol)
        }
    }
}
