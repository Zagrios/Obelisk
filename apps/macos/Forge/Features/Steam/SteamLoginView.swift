import CoreImage.CIFilterBuiltins
import SwiftUI

struct SteamLoginView: View {
    @Bindable var model: SteamModel
    @Environment(\.colorScheme) private var colorScheme
    @State private var account = ""
    @State private var password = ""
    @State private var guardCode = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 28) {
            HStack(spacing: 15) {
                StoreMark(section: .steam, size: 54)
                VStack(alignment: .leading, spacing: 6) {
                    Text(L10n.string("ui.steam.sign_in_title")).font(.system(size: 28, weight: .bold)).tracking(-0.7)
                    Text(L10n.string("ui.steam.sign_in_subtitle"))
                        .font(.system(size: 13)).foregroundStyle(.secondary)
                }
            }

            if model.isRestoring || model.isConnecting {
                ProgressView(model.isRestoring ? L10n.string("ui.account.restoring_session") : L10n.string("ui.steam.connecting"))
                    .frame(maxWidth: .infinity, minHeight: 260)
            } else if model.isCredentialChallenge && !model.isSubmittingCredentials {
                guardStep
            } else {
                HStack(alignment: .top, spacing: 30) {
                    credentials
                        .frame(maxWidth: .infinity, alignment: .leading)
                    Divider().frame(height: 240)
                    qrCode.frame(width: 180)
                }
                .padding(26)
                .background(ForgePalette(colorScheme: colorScheme).surface, in: RoundedRectangle(cornerRadius: 14))
                .overlay { RoundedRectangle(cornerRadius: 14).strokeBorder(ForgePalette(colorScheme: colorScheme).border) }
            }
        }
        .frame(maxWidth: 790)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var credentials: some View {
        VStack(alignment: .leading, spacing: 15) {
            Text(L10n.string("ui.steam.account_sign_in")).font(.system(size: 16, weight: .semibold))
            VStack(alignment: .leading, spacing: 7) {
                Text(L10n.string("ui.steam.account_name_caption")).font(.system(size: 10, weight: .semibold)).foregroundStyle(.secondary)
                TextField(L10n.string("ui.steam.account_name_placeholder"), text: $account)
                    .textFieldStyle(.roundedBorder)
                    .textContentType(.username)
                    .accessibilityLabel(L10n.string("ui.steam.account_name_accessibility"))
            }
            VStack(alignment: .leading, spacing: 7) {
                Text(L10n.string("ui.account.password_caption")).font(.system(size: 10, weight: .semibold)).foregroundStyle(.secondary)
                SecureField(L10n.string("ui.account.password"), text: $password)
                    .textFieldStyle(.roundedBorder)
                    .textContentType(.password)
                    .onSubmit(signIn)
            }
            Toggle(L10n.string("ui.account.remember_me"), isOn: $model.rememberSession)
                .toggleStyle(.checkbox)
                .font(.system(size: 12))
            Button(action: signIn) {
                HStack {
                    Spacer()
                    if model.isSubmittingCredentials { ProgressView().controlSize(.small) }
                    Text(model.isSubmittingCredentials ? L10n.string("ui.account.signing_in") : L10n.string("ui.account.sign_in"))
                    Spacer()
                }
            }
            .buttonStyle(ForgePrimaryButtonStyle())
            .disabled(account.trimmingCharacters(in: .whitespaces).isEmpty || password.isEmpty || model.isSubmittingCredentials)
            Link(L10n.string("ui.steam.sign_in_help"), destination: URL(string: "https://help.steampowered.com/\(L10n.languageCode)/wizard/HelpWithLogin")!)
                .font(.system(size: 11))
        }
        .disabled(model.isSubmittingCredentials)
    }

    private var qrCode: some View {
        VStack(spacing: 12) {
            Text(L10n.string("ui.steam.qr_title")).font(.system(size: 15, weight: .semibold))
            if let url = model.qrURL {
                SteamQRCode(value: url)
                    .frame(width: 176, height: 176)
            } else {
                ZStack {
                    RoundedRectangle(cornerRadius: 8).fill(.primary.opacity(0.04))
                    if model.qrError == nil { ProgressView() }
                    else { Image(systemName: "qrcode").font(.system(size: 45)).foregroundStyle(.secondary) }
                }
                .frame(width: 176, height: 176)
            }
            if let error = model.qrError {
                Text(L10n.message(error)).font(.system(size: 11)).foregroundStyle(.secondary).multilineTextAlignment(.center)
                Button(L10n.string("ui.steam.qr_refresh"), action: model.beginQR).buttonStyle(.borderless).font(.system(size: 11))
            } else {
                Text(model.challenge?.remoteInteraction == true ? L10n.string("ui.steam.qr_confirm") : L10n.string("ui.steam.qr_instructions"))
                    .font(.system(size: 12)).foregroundStyle(.secondary).multilineTextAlignment(.center)
            }
        }
    }

    private var guardStep: some View {
        VStack(spacing: 20) {
            Image(systemName: "lock.shield").font(.system(size: 42, weight: .light)).foregroundStyle(ForgeStyle.accent)
            Text(L10n.string("ui.steam.guard_title")).font(.system(size: 23, weight: .semibold))
            if let challenge = model.challenge {
                let methods = challenge.confirmations.filter { [.deviceConfirmation, .emailCode, .deviceCode, .emailConfirmation].contains($0.kind) }
                if methods.count > 1 {
                    Picker(L10n.string("ui.steam.guard_method"), selection: $model.guardKind) {
                        ForEach(Array(methods.enumerated()), id: \.offset) { _, method in
                            Text(guardLabel(method.kind)).tag(method.kind)
                        }
                    }
                    .labelsHidden().frame(maxWidth: 280)
                }
                if model.guardKind == .emailCode || model.guardKind == .deviceCode {
                    Text(model.guardKind == .emailCode ? L10n.string("ui.steam.guard_email_code_instructions") : L10n.string("ui.steam.guard_device_code_instructions"))
                        .font(.system(size: 13)).foregroundStyle(.secondary)
                    if let message = challenge.confirmations.first(where: { $0.kind == model.guardKind })?.message, !message.isEmpty {
                        Text(L10n.message(message)).font(.system(size: 12)).foregroundStyle(.secondary)
                    }
                    TextField(L10n.string("ui.steam.guard_code"), text: $guardCode)
                        .font(.system(size: 22, weight: .medium, design: .monospaced))
                        .textFieldStyle(.roundedBorder)
                        .multilineTextAlignment(.center)
                        .frame(width: 210)
                        .onSubmit(submitGuard)
                    Button(model.isSubmittingGuard ? L10n.string("ui.transfer.verifying_short") : L10n.string("ui.steam.guard_verify_code"), action: submitGuard)
                        .buttonStyle(ForgePrimaryButtonStyle())
                        .disabled(guardCode.trimmingCharacters(in: .whitespacesAndNewlines).count != 5 || model.isSubmittingGuard)
                } else {
                    ProgressView()
                    Text(model.guardKind == .emailConfirmation ? L10n.string("ui.steam.guard_email_instructions") : L10n.string("ui.steam.guard_device_instructions"))
                        .font(.system(size: 13)).foregroundStyle(.secondary).multilineTextAlignment(.center)
                }
            } else { ProgressView(L10n.string("ui.steam.guard_preparing")) }
            Button(L10n.string("ui.steam.back_to_sign_in"), action: model.beginQR)
                .buttonStyle(.borderless).font(.system(size: 12))
        }
        .padding(32)
        .frame(maxWidth: .infinity, minHeight: 290)
        .background(ForgePalette(colorScheme: colorScheme).surface, in: RoundedRectangle(cornerRadius: 14))
    }

    private func signIn() {
        guard !password.isEmpty && !account.trimmingCharacters(in: .whitespaces).isEmpty else { return }
        let suppliedPassword = password
        password = ""
        model.signIn(account: account, password: suppliedPassword)
    }

    private func submitGuard() {
        model.submitGuard(guardCode)
        guardCode = ""
    }

    private func guardLabel(_ kind: SteamGuardKind) -> String {
        switch kind {
        case .deviceConfirmation: L10n.string("ui.steam.guard_method_notification")
        case .deviceCode: L10n.string("ui.steam.guard_method_device_code")
        case .emailCode: L10n.string("ui.steam.guard_method_email_code")
        case .emailConfirmation: L10n.string("ui.steam.guard_method_email_confirmation")
        default: L10n.string("ui.steam.guard_method_default")
        }
    }
}

private struct SteamQRCode: View {
    let value: String
    // Context creation is expensive; one shared instance serves every render.
    private static let context = CIContext()

    private var image: CGImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(value.utf8)
        filter.correctionLevel = "H"
        guard let output = filter.outputImage else { return nil }
        return Self.context.createCGImage(output, from: output.extent)
    }

    var body: some View {
        Group {
            if let image {
                Image(decorative: image, scale: 1)
                    .interpolation(.none).resizable().scaledToFit()
                    .padding(12).background(.white, in: RoundedRectangle(cornerRadius: 8))
            } else { Image(systemName: "qrcode").font(.largeTitle) }
        }
        .accessibilityLabel(L10n.string("ui.steam.qr_accessibility"))
    }
}
