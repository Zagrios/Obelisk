import SwiftUI

/// One of the player's libraries on Home: its storefront, its account and how
/// many games it holds, beside a fan of its last played covers that spreads
/// under the pointer. It opens the library.
struct LibraryTile: View {
    let section: LibrarySection
    let title: String
    let status: AccountStanding
    /// The library's last played covers, the last played on top.
    let covers: [AnyView]
    let open: () -> Void
    @State private var hovered = false
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private static var coverWidth: CGFloat { 58 }

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: 22, style: .continuous)
        let palette = ObeliskPalette(colorScheme: colorScheme)
        return Button(action: open) {
            HStack(alignment: .center, spacing: 12) {
                VStack(alignment: .leading, spacing: 0) {
                    HStack(spacing: 9) {
                        mark(size: 20)
                        Text(title)
                            .font(.system(size: 17, weight: .bold))
                            .lineLimit(1)
                            .minimumScaleFactor(0.85)
                    }
                    Spacer(minLength: 14)
                    details
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                fan
            }
            .padding(.leading, 20)
            .padding(.trailing, 14)
            .padding(.vertical, 18)
            .frame(height: 158)
            .background {
                shape.fill(palette.surface)
                    .overlay {
                        LinearGradient(colors: [section.tint.opacity(colorScheme == .dark ? 0.2 : 0.16), .clear],
                                       startPoint: .topLeading, endPoint: .bottomTrailing)
                            .clipShape(shape)
                    }
            }
            .overlay {
                shape.strokeBorder(hovered ? section.tint.opacity(0.45) : palette.border, lineWidth: 1)
                    .allowsHitTesting(false)
            }
            .contentShape(shape)
        }
        .obeliskButtonStyle(.plain)
        .accessibilityLabel(L10n.string("ui.library.store_accessibility", title))
        .accessibilityValue(summary)
        .accessibilityIdentifier("home-library-\(section.rawValue)")
        .shadow(color: .black.opacity(hovered ? (colorScheme == .dark ? 0.35 : 0.14) : 0), radius: 16, y: 8)
        .scaleEffect(hovered && !reduceMotion ? 1.015 : 1)
        .animation(reduceMotion ? nil : .spring(response: 0.32, dampingFraction: 0.75), value: hovered)
        .onHover { hovered = $0 }
    }

    @ViewBuilder private func mark(size: CGFloat) -> some View {
        if let logo = section.logoAssetName {
            Image(logo)
                .renderingMode(.template)
                .resizable()
                .scaledToFit()
                .frame(width: size, height: size)
        }
    }

    /// What the tile tells of the library, read aloud as its value.
    private var summary: String {
        switch status {
        case let .signedIn(account, _, games, installed):
            return [account, counts(games: games, installed: installed)].compactMap { $0 }.joined(separator: ", ")
        case .restoring: return L10n.string("ui.account.signing_in")
        case .signedOut: return L10n.string("ui.home.signed_out")
        }
    }

    private func counts(games: Int, installed: Int) -> String {
        [L10n.string("ui.library.game_count", Int64(games)),
         installed > 0 ? L10n.string("ui.home.installed_count", Int64(installed)) : nil]
            .compactMap { $0 }
            .joined(separator: " · ")
    }

    @ViewBuilder private var details: some View {
        VStack(alignment: .leading, spacing: 7) {
            switch status {
            case let .signedIn(account, avatarURL, games, installed):
                if let account, !account.isEmpty {
                    HStack(spacing: 7) {
                        AccountAvatar(name: account, url: avatarURL)
                            .frame(width: 22, height: 22)
                        Text(account)
                            .font(.system(size: 13, weight: .medium))
                    }
                }
                Text(counts(games: games, installed: installed))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            case .restoring:
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text(L10n.string("ui.account.signing_in"))
                        .font(.system(size: 13))
                        .foregroundStyle(.secondary)
                }
            case .signedOut:
                Text(L10n.string("ui.home.signed_out"))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                callToAction(L10n.string("ui.account.sign_in"))
            }
        }
        // Each fact on one line, a little smaller rather than cut.
        .lineLimit(1)
        .minimumScaleFactor(0.85)
    }

    private func callToAction(_ title: String) -> some View {
        HStack(spacing: 5) {
            Text(title)
            Image(systemName: "arrow.right")
                .font(.system(size: 11, weight: .bold))
                .offset(x: hovered && !reduceMotion ? 3 : 0)
        }
        .font(.system(size: 13, weight: .semibold))
        .foregroundStyle(ObeliskStyle.accent)
    }

    /// The covers fanned out, the last played in front; the storefront's logo
    /// while it shows none.
    @ViewBuilder private var fan: some View {
        let width = Self.coverWidth
        let height = width / ObeliskStyle.coverAspectRatio
        ZStack {
            if covers.isEmpty {
                mark(size: 48)
                    .foregroundStyle(section.tint.opacity(0.35))
            } else {
                ForEach(Array(covers.prefix(3).enumerated()), id: \.offset) { index, cover in
                    // The front cover, then one on each side behind it.
                    let side: CGFloat = index == 0 ? 0 : index == 1 ? -1 : 1
                    let spread: CGFloat = hovered && !reduceMotion ? 1.35 : 1
                    let shape = RoundedRectangle(cornerRadius: 9, style: .continuous)
                    Color.clear
                        .frame(width: width, height: height)
                        .overlay { cover }
                        .clipShape(shape)
                        .overlay { shape.strokeBorder(.white.opacity(0.14), lineWidth: 0.75) }
                        .shadow(color: .black.opacity(0.35), radius: 6, y: 3)
                        .rotationEffect(.degrees(Double(side * 9 * spread)), anchor: .bottom)
                        .offset(x: side * 21 * spread, y: side == 0 ? 0 : 5)
                        .zIndex(Double(3 - index))
                }
            }
        }
        .frame(width: covers.isEmpty ? 60 : width + 2 * 30, height: height + 8)
        .allowsHitTesting(false)
    }
}
