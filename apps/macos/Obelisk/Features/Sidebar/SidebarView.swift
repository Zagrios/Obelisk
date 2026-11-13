import SwiftUI

struct SidebarView: View {
    @Bindable var model: AppModel
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        List(selection: selection(where: { $0 != .settings })) {
            row(.home)

            Section {
                ForEach(LibrarySection.storefronts) { row($0) }
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
                    row(.settings)
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
    private func selection(where owns: @escaping (LibrarySection) -> Bool) -> Binding<LibrarySection?> {
        Binding(
            get: { owns(model.selection) ? model.selection : nil },
            set: { if let section = $0 { show(section) } }
        )
    }

    /// Shows the section at its first page. Picking the tab already shown
    /// leaves its game page with the page's own motion.
    private func show(_ section: LibrarySection) {
        let animation = section == model.selection ? ObeliskStyle.pageAnimation(reduceMotion: reduceMotion) : nil
        withAnimation(animation) { model.show(section) }
    }

    private func row(_ section: LibrarySection) -> some View {
        Label {
            Text(section.title)
        } icon: {
            sidebarIcon(section)
        }
        .listItemTint(section == .settings ? .monochrome : .fixed(ObeliskStyle.accent))
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        // The list does not tell a click on the row already selected. A click
        // on another row comes through both.
        .simultaneousGesture(TapGesture().onEnded { show(section) })
        .pointerCursor()
        .tag(section)
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
