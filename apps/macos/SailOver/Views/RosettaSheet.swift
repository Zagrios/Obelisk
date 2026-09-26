import SwiftUI

struct RosettaPromptView<Sessions: RosettaSessions>: View {
    @Bindable var model: RosettaModel<Sessions>
    var storefront = "steam"

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text(L10n.string("ui.rosetta.title"))
                .font(.system(size: 19, weight: .semibold))
                .accessibilityAddTraits(.isHeader)
            Text(L10n.string("ui.rosetta.message"))
                .font(.system(size: 13))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if model.isInstalling {
                ProgressView(L10n.string("ui.rosetta.installing"))
                    .controlSize(.small)
            }
            if model.installationFailed {
                Text(L10n.string("ui.rosetta.failed"))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Button(L10n.string("ui.common.cancel"), role: .cancel, action: model.cancel)
                    .sailOverButtonStyle(.bordered)
                    .keyboardShortcut(.cancelAction)
                Spacer(minLength: 8)
                Button(L10n.string("ui.rosetta.install"), action: model.install)
                    .sailOverButtonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(model.isInstalling)
            }
        }
        .padding(24)
        .frame(width: 440)
        .tint(SailOverStyle.accent)
        .interactiveDismissDisabled()
        .accessibilityIdentifier("\(storefront)-rosetta-prompt")
    }
}
