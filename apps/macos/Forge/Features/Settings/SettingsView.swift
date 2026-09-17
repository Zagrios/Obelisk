import SwiftUI

struct SettingsView: View {
    @Bindable var model: AppModel
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 28) {
                VStack(alignment: .leading, spacing: 7) {
                    Text(L10n.string("ui.settings.title"))
                        .font(.system(size: 34, weight: .bold))
                        .tracking(-1)
                        .accessibilityAddTraits(.isHeader)
                    Text(L10n.string("ui.settings.subtitle", AppIdentity.name))
                        .font(.system(size: 13))
                        .foregroundStyle(.secondary)
                }
                .padding(.bottom, 7)

                settingsSection(L10n.string("ui.settings.appearance")) {
                    HStack(spacing: 24) {
                        Label(L10n.string("ui.settings.theme"), systemImage: "circle.lefthalf.filled")
                            .font(.system(size: 13, weight: .medium))
                        Spacer(minLength: 12)
                        Picker(L10n.string("ui.settings.theme"), selection: $model.appearance) {
                            ForEach(AppAppearance.allCases) { appearance in
                                Text(appearance.title).tag(appearance)
                            }
                        }
                        .pickerStyle(.segmented)
                        .pointerCursor()
                        .labelsHidden()
                        .frame(width: 260)
                    }
                    .padding(18)
                }

                settingsSection(L10n.string("ui.settings.steam_games")) {
                    Toggle(isOn: Binding(
                        get: { model.steam.verifyBeforeLaunch },
                        set: { model.steam.verifyBeforeLaunch = $0 }
                    )) {
                        VStack(alignment: .leading, spacing: 6) {
                            Text(L10n.string("ui.settings.verify_before_launch"))
                                .font(.system(size: 13, weight: .medium))
                            Text(L10n.string("ui.settings.verify_before_launch_description"))
                                .font(.system(size: 12))
                                .foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                    .toggleStyle(.switch)
                    .pointerCursor()
                    .padding(18)
                }

                settingsSection(L10n.string("ui.library.title")) {
                    VStack(alignment: .leading, spacing: 10) {
                        Label(L10n.string("ui.settings.catalog_location"), systemImage: "folder")
                            .font(.system(size: 13, weight: .medium))
                        Text(model.libraryDirectory)
                            .font(.system(size: 12, design: .monospaced))
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                            .fixedSize(horizontal: false, vertical: true)
                        Text(L10n.string("ui.settings.games_original_location"))
                            .font(.system(size: 12))
                            .foregroundStyle(.tertiary)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(18)
                }

                settingsSection(L10n.string("ui.settings.stores")) {
                    VStack(spacing: 0) {
                        ForEach(LibrarySection.storefronts) { section in

                            HStack(spacing: 12) {
                                StoreMark(section: section, size: 30)
                                Text(section.title)
                                    .font(.system(size: 13, weight: .medium))
                                Spacer()
                                if section == .steam || section == .epic {
                                    Button(L10n.string("ui.settings.open_store", section.title)) { model.selection = section }
                                        .forgeButtonStyle(.bordered)
                                } else {
                                    ComingSoonBadge()
                                }
                            }
                            .padding(.horizontal, 18)
                            .padding(.vertical, 12)

                            if section != .gog {
                                Divider().padding(.leading, 60)
                            }
                        }
                    }
                }

                HStack(spacing: 8) {
                    Text(AppIdentity.name)
                        .fontWeight(.semibold)
                    Text(L10n.string("ui.settings.version", AppIdentity.version))
                        .textSelection(.enabled)
                }
                .font(.system(size: 11))
                .foregroundStyle(.tertiary)
                .frame(maxWidth: .infinity, alignment: .center)
                .padding(.top, 3)
            }
            .padding(ForgeStyle.contentPadding)
            .padding(.top, 6)
            .frame(maxWidth: 820, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func settingsSection<Content: View>(
        _ title: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 11) {
            Text(title)
                .font(.system(size: 15, weight: .semibold))
                .accessibilityAddTraits(.isHeader)
            content()
                .background(
                    ForgePalette(colorScheme: colorScheme).surface,
                    in: RoundedRectangle(cornerRadius: 11)
                )
                .overlay {
                    RoundedRectangle(cornerRadius: 11)
                        .strokeBorder(ForgePalette(colorScheme: colorScheme).border)
                }
        }
    }
}
