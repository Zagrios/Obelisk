import SwiftUI

struct SteamRosettaPromptView: View {
    @Bindable var model: SteamRosettaModel

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text(L10n.string("ui.steam.rosetta.title"))
                .font(.system(size: 19, weight: .semibold))
                .accessibilityAddTraits(.isHeader)
            Text(L10n.string("ui.steam.rosetta.message"))
                .font(.system(size: 13))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if model.isInstalling {
                ProgressView(L10n.string("ui.steam.rosetta.installing"))
                    .controlSize(.small)
            }
            if model.installationFailed {
                Text(L10n.string("ui.steam.rosetta.failed"))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Button(L10n.string("ui.common.cancel"), role: .cancel, action: model.cancel)
                    .forgeButtonStyle(.bordered)
                    .keyboardShortcut(.cancelAction)
                Spacer(minLength: 8)
                Button(L10n.string("ui.steam.rosetta.install"), action: model.install)
                    .forgeButtonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(model.isInstalling)
            }
        }
        .padding(24)
        .frame(width: 440)
        .tint(ForgeStyle.accent)
        .interactiveDismissDisabled()
        .accessibilityIdentifier("steam-rosetta-prompt")
    }
}
