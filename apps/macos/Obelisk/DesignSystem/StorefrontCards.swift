import SwiftUI

/// The three storefronts' cards in one row, or, short of room, the first two
/// side by side over the last, which takes their whole width.
struct StorefrontRows<Card: View>: View {
    var inOneRow = false
    var spacing: CGFloat = 16
    /// A storefront's card, told whether it spans a row of its own.
    @ViewBuilder let card: (LibrarySection, _ spansRow: Bool) -> Card

    var body: some View {
        let stores = LibrarySection.storefronts
        if inOneRow {
            HStack(spacing: spacing) {
                ForEach(stores) { card($0, false) }
            }
            .fixedSize(horizontal: false, vertical: true)
        } else {
            VStack(spacing: spacing) {
                HStack(spacing: spacing) {
                    ForEach(stores.dropLast()) { card($0, false) }
                }
                .fixedSize(horizontal: false, vertical: true)
                ForEach(stores.suffix(1)) { card($0, true).fixedSize(horizontal: false, vertical: true) }
            }
        }
    }
}

/// A storefront on a sign-in page: its logo, its name and what its library
/// holds, leading to its own sign-in. Spanning a row, it lays out in one line.
struct StorefrontSignInCard: View {
    let section: LibrarySection
    /// Whether the storefront is restoring or signing in an account.
    let isConnecting: Bool
    var spansRow = false
    let open: () -> Void
    @State private var hovered = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var name: String { section.storeTitle }

    var body: some View {
        Button(action: open) {
            Group {
                if spansRow {
                    HStack(spacing: 18) {
                        logo
                        texts
                        Spacer(minLength: 12)
                        status
                    }
                } else {
                    VStack(alignment: .leading, spacing: 18) {
                        logo
                        texts
                        Spacer(minLength: 0)
                        status
                    }
                    .frame(minHeight: 190, alignment: .top)
                }
            }
            .padding(24)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .modifier(StorefrontPanelGlass(cornerRadius: 26, tint: section.tint.opacity(hovered ? 0.18 : 0.08)))
            .contentShape(.rect(cornerRadius: 26))
        }
        .obeliskButtonStyle(.plain)
        .disabled(isConnecting)
        .accessibilityLabel(L10n.string("ui.home.sign_in_store", name))
        .accessibilityValue(isConnecting ? L10n.string("ui.account.signing_in") : "")
        .accessibilityIdentifier("home-sign-in-\(section.rawValue)")
        .scaleEffect(hovered && !reduceMotion ? 1.015 : 1)
        .animation(reduceMotion ? nil : .spring(response: 0.32, dampingFraction: 0.75), value: hovered)
        .onHover { hovered = $0 }
    }

    @ViewBuilder private var logo: some View {
        if let logo = section.logoAssetName {
            Image(logo)
                .renderingMode(.template)
                .resizable()
                .scaledToFit()
                .frame(width: 34, height: 34)
                .accessibilityHidden(true)
        }
    }

    private var texts: some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(name)
                .font(.system(size: 19, weight: .bold))
            if let subtitle = section.storeSubtitle {
                Text(subtitle)
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    @ViewBuilder private var status: some View {
        if isConnecting {
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text(L10n.string("ui.account.signing_in"))
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
            }
            .frame(height: 34)
        } else {
            HStack(spacing: 6) {
                Text(L10n.string("ui.account.sign_in"))
                Image(systemName: "arrow.right")
                    .font(.system(size: 11, weight: .bold))
                    .offset(x: hovered && !reduceMotion ? 3 : 0)
            }
            .font(.system(size: 13, weight: .semibold))
            .foregroundStyle(.white)
            .padding(.horizontal, 16)
            .frame(height: 34)
            .background(ObeliskStyle.accent, in: .capsule)
        }
    }
}
