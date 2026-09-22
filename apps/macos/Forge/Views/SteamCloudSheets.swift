import SwiftUI

struct SteamCloudPromptView: View {
    let title: String
    let message: String
    let busy: Bool
    let onCancel: () -> Void
    let onChooseLocal: () -> Void
    let onChooseCloud: () -> Void
    var storefront = "steam"

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Label(L10n.message(title), systemImage: "arrow.triangle.branch")
                .font(.system(size: 19, weight: .semibold))
                .accessibilityAddTraits(.isHeader)

            Text(L10n.message(message))
                .font(.system(size: 13))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            if busy {
                ProgressView(L10n.string("ui.cloud.working"))
                    .controlSize(.small)
                    .font(.system(size: 12))
            }

            VStack(spacing: 10) {
                choice("ui.cloud.choose_local", symbol: "internaldrive", action: onChooseLocal)
                choice("ui.cloud.choose_cloud", symbol: "icloud", action: onChooseCloud)
            }
            .disabled(busy)

            HStack(spacing: 10) {
                Button(L10n.string("ui.common.cancel"), role: .cancel, action: onCancel)
                    .forgeButtonStyle(.bordered)
                    .keyboardShortcut(.cancelAction)
                Spacer(minLength: 8)
            }
        }
        .padding(24)
        .frame(width: 490)
        .tint(ForgeStyle.accent)
        .interactiveDismissDisabled()
        .accessibilityIdentifier("\(storefront)-cloud-prompt")
    }

    private func choice(_ key: String, symbol: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Label(L10n.string(key), systemImage: symbol)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 7)
        }
        .forgeButtonStyle(.bordered)
    }
}

struct SteamCloudFailureView: View {
    let title: String
    let message: String
    let busy: Bool
    let canLaunchAnyway: Bool
    let onRetry: () -> Void
    let onCancel: () -> Void
    let onLaunchAnyway: () -> Void
    var storefront = "steam"

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(title, systemImage: "icloud.slash")
                .font(.system(size: 12, weight: .semibold))
            Text(L10n.message(message))
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
            HStack(spacing: 12) {
                Button(L10n.string("ui.common.retry"), action: onRetry)
                if canLaunchAnyway {
                    Button(L10n.string("ui.cloud.launch_anyway"), action: onLaunchAnyway)
                }
                Button(L10n.string("ui.common.cancel"), action: onCancel)
            }
            .forgeButtonStyle(.borderless)
            .controlSize(.small)
            .disabled(busy)
        }
        .accessibilityIdentifier("\(storefront)-cloud-failure")
    }
}
