import CoreImage.CIFilterBuiltins
import SwiftUI

struct SteamLoginView: View {
    @Bindable var model: SteamModel
    @State private var account = ""
    @State private var password = ""
    @State private var guardCode = ""
    @FocusState private var isGuardCodeFocused: Bool

    var body: some View {
        StorefrontSignInPage(
            section: .steam,
            title: L10n.string("ui.steam.sign_in_title"),
            subtitle: L10n.string("ui.steam.sign_in_subtitle"),
            cardWidth: isWaiting || isGuardStep ? 480 : 720,
            warning: $model.storageWarning
        ) {
            if isWaiting {
                SignInProgress(title: model.isRestoring ? L10n.string("ui.account.restoring_session") : L10n.string("ui.steam.connecting"))
            } else if isGuardStep {
                guardStep
            } else {
                HStack(alignment: .center, spacing: 30) {
                    credentials
                        .frame(maxWidth: .infinity, alignment: .leading)
                    Rectangle()
                        .fill(.primary.opacity(0.1))
                        .frame(width: 1)
                        .padding(.vertical, 8)
                    // As wide as the code, so both columns keep the card's margins.
                    qrCode
                        .frame(width: 176)
                }
                .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var isWaiting: Bool { model.isRestoring || model.isConnecting }
    private var isGuardStep: Bool { model.isCredentialChallenge && !model.isSubmittingCredentials }

    private var credentials: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(L10n.string("ui.steam.account_sign_in"))
                .font(.system(size: 16, weight: .semibold))
                .padding(.bottom, 2)
            SignInField(
                symbol: "person",
                placeholder: L10n.string("ui.steam.account_name_placeholder"),
                text: $account,
                contentType: .username,
                accessibilityLabel: L10n.string("ui.steam.account_name_accessibility")
            )
            SignInField(
                symbol: "lock",
                placeholder: L10n.string("ui.account.password"),
                text: $password,
                isSecure: true,
                contentType: .password,
                onSubmit: signIn
            )
            SignInRememberToggle(isOn: $model.rememberSession)
                .padding(.horizontal, 2)
            Button(action: signIn) {
                HStack(spacing: 8) {
                    if model.isSubmittingCredentials { ProgressView().controlSize(.small) }
                    Text(model.isSubmittingCredentials ? L10n.string("ui.account.signing_in") : L10n.string("ui.account.sign_in"))
                }
                .font(.system(size: 13, weight: .semibold))
                .frame(maxWidth: .infinity)
            }
            .obeliskButtonStyle(.glassProminent)
            .controlSize(.extraLarge)
            .disabled(account.trimmingCharacters(in: .whitespaces).isEmpty || password.isEmpty || model.isSubmittingCredentials)
            Link(L10n.string("ui.steam.sign_in_help"), destination: URL(string: "https://help.steampowered.com/\(L10n.languageCode)/wizard/HelpWithLogin")!)
                .font(.system(size: 12))
                .frame(maxWidth: .infinity)
                .pointerCursor()
        }
        .disabled(model.isSubmittingCredentials)
    }

    private var qrCode: some View {
        VStack(spacing: 14) {
            Text(L10n.string("ui.steam.qr_title"))
                .font(.system(size: 16, weight: .semibold))
            ZStack {
                if let url = model.qrURL {
                    SteamQRCode(value: url)
                } else {
                    RoundedRectangle(cornerRadius: 16).fill(.primary.opacity(0.05))
                    if model.qrError == nil { ProgressView() }
                    else { Image(systemName: "qrcode").font(.system(size: 45)).foregroundStyle(.secondary) }
                }
            }
            .frame(width: 176, height: 176)
            if let error = model.qrError {
                Text(L10n.message(error))
                    .font(.system(size: 12)).foregroundStyle(.secondary).multilineTextAlignment(.center)
                Button(L10n.string("ui.steam.qr_refresh"), systemImage: "arrow.clockwise", action: model.beginQR)
                    .obeliskButtonStyle(.glass)
            } else {
                Text(model.challenge?.remoteInteraction == true ? L10n.string("ui.steam.qr_confirm") : L10n.string("ui.steam.qr_instructions"))
                    .font(.system(size: 12)).foregroundStyle(.secondary).multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var guardStep: some View {
        VStack(spacing: 18) {
            Image(systemName: "lock.shield")
                .font(.system(size: 28, weight: .medium))
                .foregroundStyle(ObeliskStyle.accent)
                .frame(width: 64, height: 64)
                .background(ObeliskStyle.accent.opacity(0.14), in: Circle())
                .accessibilityHidden(true)
            Text(L10n.string("ui.steam.guard_title"))
                .font(.system(size: 22, weight: .semibold))
            if let challenge = model.challenge {
                let methods = challenge.confirmations.filter { [.deviceConfirmation, .emailCode, .deviceCode, .emailConfirmation].contains($0.kind) }
                if methods.count > 1 {
                    Picker(L10n.string("ui.steam.guard_method"), selection: $model.guardKind) {
                        ForEach(Array(methods.enumerated()), id: \.offset) { _, method in
                            Text(guardLabel(method.kind)).tag(method.kind)
                        }
                    }
                    .labelsHidden()
                    .pointerCursor()
                    .frame(maxWidth: 280)
                }
                if model.guardKind == .emailCode || model.guardKind == .deviceCode {
                    Text(model.guardKind == .emailCode ? L10n.string("ui.steam.guard_email_code_instructions") : L10n.string("ui.steam.guard_device_code_instructions"))
                        .font(.system(size: 13)).foregroundStyle(.secondary).multilineTextAlignment(.center)
                    if let message = challenge.confirmations.first(where: { $0.kind == model.guardKind })?.message, !message.isEmpty {
                        Text(L10n.message(message)).font(.system(size: 12)).foregroundStyle(.secondary)
                    }
                    // The field's own placeholder would take the code's large type.
                    TextField("", text: $guardCode)
                        .textFieldStyle(.plain)
                        .accessibilityLabel(L10n.string("ui.steam.guard_code"))
                        .font(.system(size: 24, weight: .semibold, design: .monospaced))
                        .tracking(6)
                        .multilineTextAlignment(.center)
                        .focused($isGuardCodeFocused)
                        .onSubmit(submitGuard)
                        .frame(width: 220, height: 54)
                        .overlay {
                            if guardCode.isEmpty {
                                Text(L10n.string("ui.steam.guard_code"))
                                    .font(.system(size: 14))
                                    .foregroundStyle(.tertiary)
                                    .allowsHitTesting(false)
                                    .accessibilityHidden(true)
                            }
                        }
                        .modifier(SignInWell(isFocused: isGuardCodeFocused))
                        .onTapGesture { isGuardCodeFocused = true }
                    Button(action: submitGuard) {
                        Text(model.isSubmittingGuard ? L10n.string("ui.transfer.verifying_short") : L10n.string("ui.steam.guard_verify_code"))
                            .font(.system(size: 13, weight: .semibold))
                            .frame(width: 196)
                    }
                    .obeliskButtonStyle(.glassProminent)
                    .controlSize(.extraLarge)
                    .disabled(guardCode.trimmingCharacters(in: .whitespacesAndNewlines).count != 5 || model.isSubmittingGuard)
                } else {
                    ProgressView()
                    Text(model.guardKind == .emailConfirmation ? L10n.string("ui.steam.guard_email_instructions") : L10n.string("ui.steam.guard_device_instructions"))
                        .font(.system(size: 13)).foregroundStyle(.secondary).multilineTextAlignment(.center)
                }
            } else { ProgressView(L10n.string("ui.steam.guard_preparing")) }
            Button(L10n.string("ui.steam.back_to_sign_in"), systemImage: "chevron.left", action: model.beginQR)
                .obeliskButtonStyle(.borderless)
                .font(.system(size: 12))
        }
        .frame(maxWidth: .infinity)
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

struct SteamQRCode: View {
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
                    .padding(12).background(.white, in: RoundedRectangle(cornerRadius: 16))
            } else { Image(systemName: "qrcode").font(.largeTitle) }
        }
        .accessibilityLabel(L10n.string("ui.steam.qr_accessibility"))
    }
}
