import SwiftUI

struct SidebarView: View {
    @Bindable var model: AppModel
    @Environment(\.colorScheme) private var colorScheme

    private let stores: [LibrarySection] = [.steam, .epic, .gog, .other]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 10) {
                Image(systemName: "gamecontroller.fill")
                    .font(.system(size: 19, weight: .medium))
                    .foregroundStyle(ForgeStyle.accent)
                Text(AppIdentity.name)
                    .font(.system(size: 21, weight: .bold, design: .rounded))
            }
            .padding(.horizontal, 19)
            .padding(.top, 25)
            .padding(.bottom, 32)

            VStack(alignment: .leading, spacing: 4) {
                sidebarRow(.recent)

                Text(L10n.string("ui.library.title"))
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(.tertiary)
                    .padding(.horizontal, 11)
                    .padding(.top, 23)
                    .padding(.bottom, 5)

                ForEach(stores) { section in
                    sidebarRow(section)
                }
            }
            .padding(.horizontal, 10)

            Spacer(minLength: 24)

            Rectangle()
                .fill(ForgePalette(colorScheme: colorScheme).border)
                .frame(height: 1)
                .padding(.horizontal, 20)
                .padding(.bottom, 12)

            sidebarRow(.settings)
                .padding(.horizontal, 10)
                .padding(.bottom, 18)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background {
            ForgePalette(colorScheme: colorScheme).sidebar
                .overlay(.ultraThinMaterial.opacity(0.2))
                .ignoresSafeArea(.container, edges: .top)
        }
    }

    private func sidebarRow(_ section: LibrarySection) -> some View {
        let isSelected = model.selection == section

        return Button {
            model.selection = section
        } label: {

            HStack(spacing: 10) {
                sidebarIcon(section)
                    .foregroundStyle(section == .settings && !isSelected ? Color.secondary : ForgeStyle.accent)
                    .frame(width: 20)
                Text(section.title)
                    .font(.system(size: 13, weight: isSelected ? .semibold : .medium))
                    .foregroundStyle(.primary)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 11)
            .frame(height: 35)
            .contentShape(RoundedRectangle(cornerRadius: 7))
            .background(
                isSelected ? ForgePalette(colorScheme: colorScheme).selection : .clear,
                in: RoundedRectangle(cornerRadius: 7)
            )
        }
        .forgeButtonStyle(.plain)
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }

    @ViewBuilder
    private func sidebarIcon(_ section: LibrarySection) -> some View {
        if let logoAssetName = section.logoAssetName {
            Image(logoAssetName)
                .renderingMode(.template)
                .resizable()
                .scaledToFit()
                .frame(width: 18, height: 18)
                .accessibilityHidden(true)
        } else {
            Image(systemName: section.symbol)
                .font(.system(size: 16, weight: .regular))
        }
    }
}
