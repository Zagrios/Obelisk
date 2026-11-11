import SwiftUI

/// A game of a storefront library: its portrait cover, which lifts under the
/// pointer and then shows a glass bar holding its title and its launch or
/// download button.
struct LibraryCoverCard<Artwork: View>: View {
    let title: String
    let accessibilityLabel: String
    var help: String?
    /// The storefront the game belongs to, where a page mixes several.
    var store: LibrarySection?
    /// What the game's button shows and does. Its button reads the Transfer's
    /// progress alone, so a progress tick redraws only the button.
    let action: GameAction
    /// The game, matching this cover with the one on its page as it opens.
    let matchID: AnyHashable
    let open: () -> Void
    @ViewBuilder let artwork: Artwork
    @State private var hovered = false
    /// The Force Quit its player is asked to confirm. The card asks, not its
    /// bar, which goes as the alert takes the pointer.
    @State private var asking: GameAction?
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private static var cornerRadius: CGFloat { ObeliskStyle.coverCornerRadius }
    private static var barInset: CGFloat { 8 }

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: Self.cornerRadius, style: .continuous)
        return Button(action: open) {
            Color.clear
                .aspectRatio(ObeliskStyle.coverAspectRatio, contentMode: .fit)
                .overlay { artwork.environment(\.showsCoverLabels, hovered) }
                .contentShape(shape)
        }
        .obeliskButtonStyle(.plain)
        .help(help ?? title)
        .accessibilityLabel(accessibilityLabel)
        // The bar only shows under the pointer; its action stays reachable.
        .accessibilityActions {
            if action.isAvailable, let name = label(action.kind, title: title) {
                Button(name) { action.run(asking: $asking) }
            }
        }
        .overlay(alignment: .topLeading) {
            if hovered, let logo = store?.logoAssetName {
                LibraryCardBadge(logo: logo)
                    .padding(Self.barInset + 2)
                    .transition(.opacity.combined(with: .scale(scale: 0.85)))
            }
        }
        .overlay(alignment: .bottom) {
            if hovered {
                LibraryCardBar(
                    title: title,
                    action: action,
                    // Concentric with the card: its radius less the margin.
                    cornerRadius: Self.cornerRadius - Self.barInset,
                    open: open,
                    run: { action.run(asking: $asking) }
                )
                // A point closer to the sides than to the bottom.
                .padding(.horizontal, Self.barInset - 1)
                .padding(.bottom, Self.barInset)
                .transition(.opacity.combined(with: .offset(y: 12)))
            }
        }
        .clipShape(shape)
        .overlay {
            shape.strokeBorder(.white.opacity(hovered ? 0.3 : colorScheme == .dark ? 0.08 : 0.2), lineWidth: 1)
                .allowsHitTesting(false)
        }
        .modifier(GameCoverMatch(game: matchID, isPage: false))
        .shadow(
            color: .black.opacity(colorScheme == .dark ? (hovered ? 0.5 : 0.3) : (hovered ? 0.25 : 0.12)),
            radius: hovered ? 18 : 7,
            y: hovered ? 12 : 4
        )
        .scaleEffect(hovered && !reduceMotion ? 1.03 : 1)
        .animation(reduceMotion ? nil : .spring(response: 0.3, dampingFraction: 0.72), value: hovered)
        .onHover { hovered = $0 }
        .forceQuitConfirmation(title, for: action, asking: $asking)
    }
}

/// What a card's button does for `title`, when it launches, downloads or ends
/// the game.
private func label(_ kind: GameAction.Kind, title: String) -> String? {
    switch kind {
    case .launch: L10n.string("ui.game.launch_named", title)
    case .download: L10n.string("ui.game.download_named", title)
    case let .running(stage): stage?.label(title: title)
    case .queued, .transferring, .waiting, .step: nil
    }
}

/// The card's glass bar: the title opens the game, the button acts on it.
private struct LibraryCardBar: View {
    let title: String
    let action: GameAction
    let cornerRadius: CGFloat
    let open: () -> Void
    let run: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            Button(action: open) {
                Text(title)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(.white)
                    .shadow(color: .black.opacity(0.45), radius: 3, y: 1)
                    .lineLimit(1)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(.rect)
            }
            .obeliskButtonStyle(.plain)
            // The cover already opens the game for assistive technologies.
            .accessibilityHidden(true)

            LibraryCardButton(title: title, action: action, run: run)
        }
        .padding(.leading, 12)
        .padding(.trailing, 6)
        .padding(.vertical, 6)
        // Clear glass, as the Dock's, lets the cover show through; the light
        // text and its shadow keep the title legible over any artwork.
        .glassEffect(.clear, in: .rect(cornerRadius: cornerRadius, style: .continuous))
        .environment(\.colorScheme, .dark)
    }
}

private struct LibraryCardButton: View {
    let title: String
    let action: GameAction
    let run: () -> Void

    private static var diameter: CGFloat { 32 }

    var body: some View {
        switch action.kind {
        case .launch:
            button(symbol: "play.fill", colors: ObeliskStyle.launchGradient)
        case .download:
            button(symbol: "square.and.arrow.down", colors: ObeliskStyle.installGradient)
        case .waiting, .step:
            ProgressView()
                .controlSize(.small)
                .frame(width: Self.diameter, height: Self.diameter)
                .accessibilityLabel(L10n.string("ui.transfer.preparing_accessibility"))
        case .transferring:
            LibraryCardProgress(progress: { action.transfer()?.progressFraction }, diameter: Self.diameter)
        case .queued:
            Image(systemName: "clock")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(.white.opacity(0.85))
                .frame(width: Self.diameter, height: Self.diameter)
                .background(.white.opacity(0.14), in: Circle())
                .help(L10n.string("model.game.queued"))
                .accessibilityLabel(L10n.string("model.game.queued"))
        case .running(.quit):
            button(symbol: "stop.fill", colors: ObeliskStyle.quitGradient)
        case .running(.closing):
            ProgressView()
                .controlSize(.small)
                .frame(width: Self.diameter, height: Self.diameter)
                .help(L10n.string("ui.game.closing_help"))
                .accessibilityLabel(L10n.string("model.game.closing"))
        case .running(.forceQuit):
            button(symbol: "stop.fill", colors: ObeliskStyle.forceQuitGradient,
                   help: L10n.string("ui.game.force_quit_help"))
        case .running(nil):
            Image(systemName: "play.fill")
                .font(.system(size: 12, weight: .bold))
                .foregroundStyle(.green)
                .frame(width: Self.diameter, height: Self.diameter)
                .help(L10n.string("model.game.running"))
                .accessibilityLabel(L10n.string("model.game.running"))
        }
    }

    /// Green launches, blue downloads, grey quits and red forces a quit, as on
    /// the game's page, through glass.
    private func button(symbol: String, colors: [Color], help: String? = nil) -> some View {
        let name = label(action.kind, title: title) ?? title
        return Button(action: run) {
            Image(systemName: symbol)
                .font(.system(size: 12, weight: .bold))
                .foregroundStyle(.white)
                .shadow(color: .black.opacity(0.25), radius: 1.5, y: 0.5)
                .frame(width: Self.diameter, height: Self.diameter)
                .background {
                    Circle()
                        .fill(LinearGradient(colors: colors, startPoint: .leading, endPoint: .trailing))
                        .opacity(0.75)
                }
                .glassEffect(.regular.interactive(), in: .circle)
                .contentShape(.circle)
        }
        .obeliskButtonStyle(.plain)
        .disabled(!action.isAvailable)
        .help(help ?? name)
        .accessibilityLabel(name)
    }
}

/// A Transfer's progress around the card's button.
private struct LibraryCardProgress: View {
    let progress: @MainActor () -> Double?
    let diameter: CGFloat

    var body: some View {
        let fraction = progress()
        ZStack {
            Circle().stroke(.primary.opacity(0.15), lineWidth: 3)
            if let fraction {
                Circle()
                    .trim(from: 0, to: fraction)
                    .stroke(LinearGradient(colors: ObeliskStyle.installGradient, startPoint: .leading, endPoint: .trailing),
                            style: StrokeStyle(lineWidth: 3, lineCap: .round))
                    .rotationEffect(.degrees(-90))
            } else {
                ProgressView().controlSize(.mini)
            }
            Image(systemName: "square.and.arrow.down")
                .font(.system(size: 10, weight: .bold))
                .opacity(fraction == nil ? 0 : 1)
        }
        .padding(4)
        .frame(width: diameter, height: diameter)
        .help(L10n.string("ui.transfer.downloading"))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(L10n.string("ui.transfer.downloading"))
        .accessibilityValue(fraction.map(DownloadFormatting.percent) ?? "")
    }
}

/// A game's storefront, in glass over its cover.
struct LibraryCardBadge: View {
    let logo: String

    var body: some View {
        Image(logo)
            .renderingMode(.template)
            .resizable()
            .scaledToFit()
            .frame(width: 12, height: 12)
            .frame(width: 26, height: 26)
            .glassEffect(.regular, in: .circle)
            .environment(\.colorScheme, .dark)
            .accessibilityHidden(true)
            .allowsHitTesting(false)
    }
}

extension EnvironmentValues {
    /// Whether a cover shows its labels: a library card's only under the
    /// pointer, as its title; a game page's always.
    @Entry var showsCoverLabels = true
}

/// What a cover says of its game, such as that it is a demo, in the colors
/// of the button that installs it.
struct LibraryCoverLabel: View {
    let text: String

    var body: some View {
        // As tall as a card's storefront badge, level with it.
        Text(text)
            .font(.system(size: 12, weight: .bold))
            .foregroundStyle(.white)
            .padding(.horizontal, 10)
            .frame(height: 26)
            .background(LinearGradient(colors: ObeliskStyle.installGradient, startPoint: .leading, endPoint: .trailing),
                        in: Capsule())
            .accessibilityHidden(true)
            .allowsHitTesting(false)
    }
}
