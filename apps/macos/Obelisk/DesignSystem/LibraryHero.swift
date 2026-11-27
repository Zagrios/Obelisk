import SwiftUI

/// A game shown wide at the top of a library, its artwork reaching under the
/// library's floating bar: Home's and each storefront's. Its text stays light,
/// with its own shadow, in both appearances.
struct LibraryHero<Artwork: View, Logo: View>: View {
    /// The game's name, read aloud as the hero's heading.
    let title: String
    let details: [Detail]
    /// What the game's button shows and does, as on its cover and its page.
    let action: GameAction
    /// Opens the game's page.
    let open: () -> Void
    @ViewBuilder let artwork: Artwork
    /// What shows the game's name over its artwork, if the artwork does not.
    @ViewBuilder let logo: Logo

    typealias Detail = LibraryHeroDetail

    var body: some View {
        // How far the artwork reaches up under the library's bar.
        let reach = ObeliskStyle.titleBarHeight
        return HeroBackdrop { artwork }
            .overlay(alignment: .bottomLeading) {
                VStack(alignment: .leading, spacing: 14) {
                    logo
                        .shadow(color: .black.opacity(0.35), radius: 14, y: 4)
                        .accessibilityHidden(true)
                    HStack(spacing: 16) {
                        ForEach(details) { detail in
                            Label {
                                Text(detail.text)
                            } icon: {
                                if let symbol = detail.symbol {
                                    Image(systemName: symbol)
                                } else if let logo = detail.logo {
                                    Image(logo)
                                        .renderingMode(.template)
                                        .resizable()
                                        .scaledToFit()
                                        .frame(width: 13, height: 13)
                                }
                            }
                        }
                    }
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(.white)
                    .shadow(color: .black.opacity(0.6), radius: 6)
                    HStack(spacing: 12) {
                        GameActionButton(action: action, gameName: title)
                        LibraryHeroOpenButton(open: open)
                    }
                    .padding(.top, 4)
                }
                .padding(.horizontal, ObeliskStyle.contentPadding)
                .padding(.bottom, 28)
                .accessibilityElement(children: .contain)
                .accessibilityLabel(title)
                .accessibilityAddTraits(.isHeader)
                // The text fades out as it slides under the bar's controls.
                .visualEffect { [reach] content, geometry in
                    // Fully visible until its top nears the bar, gone a little
                    // past the bar's bottom edge.
                    let top = geometry.frame(in: .scrollView).minY
                    return content.opacity(min(max((top - reach + 40) / 80, 0), 1))
                }
            }
            .heroFrame()
    }
}

/// One fact of a hero's game under its logo, such as when it was last played.
struct LibraryHeroDetail: Identifiable {
    let text: String
    /// An SF Symbol, or an asset such as a storefront's logo.
    let symbol: String?
    let logo: String?
    var id: String { symbol ?? logo ?? text }

    init(symbol: String, text: String) {
        self.symbol = symbol
        logo = nil
        self.text = text
    }

    init(logo: String, text: String) {
        symbol = nil
        self.logo = logo
        self.text = text
    }
}

extension EnvironmentValues {
    /// How tall a hero's logo stands: a game's page shows it smaller than a
    /// library's hero, beside the game's cover.
    @Entry var heroLogoHeight: CGFloat = 120
}

extension View {
    /// Frames a page's hero: a library hero's height below the floating bar,
    /// reaching up under it, light on its dark artwork in both appearances.
    func heroFrame() -> some View {
        frame(height: ObeliskStyle.libraryHeroHeight + ObeliskStyle.titleBarHeight)
            .padding(.top, -ObeliskStyle.titleBarHeight)
            .environment(\.colorScheme, .dark)
    }
}

/// A page's wide artwork at the top of its scroll view: it lags behind the page
/// as it scrolls, stretches over an overscroll and reaches beyond the sidebar.
struct HeroBackdrop<Artwork: View>: View {
    @ViewBuilder let artwork: Artwork
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        // How much of the scrolling the artwork follows less than the page.
        let parallax = reduceMotion ? 0 : 0.7
        return Color.clear
            .overlay {
                artwork
                    .visualEffect { content, geometry in
                        let top = geometry.frame(in: .scrollView).minY
                        let height = max(geometry.size.height, 1)
                        // Scrolled down, the artwork lags behind the page; pulled
                        // past the top, it stretches over the gap above it.
                        return content
                            .offset(y: top < 0 ? -top * parallax : 0)
                            .scaleEffect(top > 0 ? 1 + top / height : 1, anchor: .bottom)
                    }
            }
            .clipShape(OpenTopRectangle())
            .backgroundExtensionEffect()
    }
}

/// The hero's frame, open above so the stretched artwork can fill an overscroll.
private struct OpenTopRectangle: Shape {
    func path(in rect: CGRect) -> Path {
        Path(CGRect(x: rect.minX, y: rect.minY - 10_000, width: rect.width, height: rect.height + 10_000))
    }
}

/// Opens the hero's game page: plain glass as tall as the game's button
/// beside it.
private struct LibraryHeroOpenButton: View {
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            Label(L10n.string("ui.library.hero.open"), systemImage: "arrow.right")
                .labelStyle(TrailingIconLabelStyle())
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(.white)
                .padding(.horizontal, 20)
                .frame(height: 44)
                .modifier(StorefrontControlGlass())
                .contentShape(.capsule)
        }
        .obeliskButtonStyle(.plain)
    }
}

/// The title, then its icon, as in "View game →".
private struct TrailingIconLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 7) {
            configuration.title
            configuration.icon
        }
    }
}

/// The featured game's logo, or its title when its store publishes none.
struct LibraryHeroLogo: View {
    let url: URL?
    let title: String
    @Environment(\.heroLogoHeight) private var height

    var body: some View {
        Group {
            if let url {
                // Cached, so a game's page shows the logo its library hero loaded.
                CachedArtwork(urls: [url], maxPixelSize: 960) { image in
                    image.resizable().scaledToFit()
                } placeholder: {
                    Color.clear
                } failure: {
                    titleText
                }
            } else {
                titleText
            }
        }
        .frame(width: height * 3, height: height, alignment: .bottomLeading)
    }

    private var titleText: some View {
        Text(title)
            .font(.system(size: 38, weight: .bold))
            .foregroundStyle(.white)
            .lineLimit(2)
            .minimumScaleFactor(0.6)
    }
}
