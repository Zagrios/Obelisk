import SwiftUI

struct EpicLoginView: View {
    @Bindable var model: EpicModel
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        VStack(alignment: .leading, spacing: 28) {
            HStack(spacing: 15) {
                StoreMark(section: .epic, size: 54)
                VStack(alignment: .leading, spacing: 6) {
                    Text(L10n.string("ui.epic.sign_in_title")).font(.system(size: 28, weight: .bold)).tracking(-0.7)
                    Text(L10n.string("ui.epic.sign_in_subtitle"))
                        .font(.system(size: 13)).foregroundStyle(.secondary)
                }
            }

            if model.isRestoring || model.isConnecting || model.isSigningOut {
                VStack(spacing: 18) {
                    ProgressView(model.isSigningOut ? L10n.string("ui.account.signing_out") : model.isRestoring ? L10n.string("ui.account.restoring_session") : L10n.string("ui.epic.connecting"))
                    if !model.isSigningOut {
                        Button(L10n.string("ui.common.cancel"), action: model.cancelSignIn)
                            .sailOverButtonStyle(.borderless)
                    }
                }
                .frame(maxWidth: .infinity, minHeight: 260)
            } else {
                VStack(alignment: .leading, spacing: 18) {
                    Text(L10n.string("ui.epic.account_sign_in")).font(.system(size: 16, weight: .semibold))
                    Text(L10n.string("ui.epic.sign_in_instructions"))
                        .font(.system(size: 13)).foregroundStyle(.secondary)
                    Toggle(L10n.string("ui.account.remember_me"), isOn: $model.rememberSession)
                        .toggleStyle(.checkbox)
                        .pointerCursor()
                        .font(.system(size: 12))
                    Button(L10n.string("ui.epic.sign_in")) { model.signIn() }
                        .buttonStyle(SailOverPrimaryButtonStyle())
                    if model.canRetryRestore {
                        Button(L10n.string("ui.account.retry_saved_session"), action: model.retryRestore)
                            .sailOverButtonStyle(.borderless).font(.system(size: 12))
                    }
                }
                .padding(26)
                .frame(maxWidth: 570, alignment: .leading)
                .background(SailOverPalette(colorScheme: colorScheme).surface, in: RoundedRectangle(cornerRadius: 14))
                .overlay { RoundedRectangle(cornerRadius: 14).strokeBorder(SailOverPalette(colorScheme: colorScheme).border) }
            }
        }
        .frame(maxWidth: 790)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
