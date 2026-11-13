import SwiftUI

struct RosettaPromptView<Sessions: RosettaSessions>: View {
    @Bindable var model: RosettaModel<Sessions>
    var storefront = LibrarySection.steam.rawValue

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            SheetHeader(title: L10n.string("ui.rosetta.title"), subtitle: L10n.string("ui.rosetta.message"),
                        symbol: "cpu", colors: TileColors.violet)
            if model.isInstalling {
                SheetNotice(kind: .progress, text: L10n.string("ui.rosetta.installing"))
            } else if model.installationFailed {
                SheetNotice(kind: .warning, text: L10n.string("ui.rosetta.failed"))
            }
            HStack(spacing: 10) {
                Spacer(minLength: 0)
                GlassCapsuleButton(title: L10n.string("ui.common.cancel"), size: .large,
                                   identifier: "\(storefront)-rosetta-cancel", action: model.cancel)
                    .keyboardShortcut(.cancelAction)
                AccentCapsuleButton(title: L10n.string("ui.rosetta.install"), size: .large,
                                    isBusy: model.isInstalling, identifier: "\(storefront)-rosetta-install",
                                    action: model.install)
                    .keyboardShortcut(.defaultAction)
                    .disabled(model.isInstalling)
            }
        }
        .padding(24)
        .frame(width: 460)
        .tint(ObeliskStyle.accent)
        .animation(.easeInOut(duration: 0.2), value: model.isInstalling)
        .animation(.easeInOut(duration: 0.2), value: model.installationFailed)
        .interactiveDismissDisabled()
        .accessibilityIdentifier("\(storefront)-rosetta-prompt")
    }
}
