import AVKit
import SwiftUI

/// One fact of a game for its page's Details, such as its developer.
struct GameFact: Identifiable {
    let label: String
    let value: String
    var id: String { label }

    init(label: String, value: String) {
        self.label = label
        self.value = value
    }

    /// The fact `key` names, listing `values` in the player's language; nil
    /// when none is left once empty ones go.
    init?(_ key: String, _ values: [String]) {
        let values = values.filter { !$0.isEmpty }
        guard !values.isEmpty else { return nil }
        self.init(label: L10n.string(key), value: values.formatted(.list(type: .and).locale(L10n.locale)))
    }
}

/// A trailer or a screenshot of a game, as its store shows it.
enum GameMedia: Identifiable, Hashable {
    case trailer(name: String, thumbnail: URL, stream: URL)
    case screenshot(thumbnail: URL, full: URL)

    var id: URL {
        switch self {
        case let .trailer(_, _, stream): stream
        case let .screenshot(_, full): full
        }
    }

    var thumbnail: URL {
        switch self {
        case let .trailer(_, thumbnail, _), let .screenshot(thumbnail, _): thumbnail
        }
    }
}

/// What a game's page tells of it beside what the game's storefront does
/// with it: what the game is and how it was made, its trailers and
/// screenshots, then its details in a column of their own. Shows nothing when
/// it has nothing to tell.
struct GameOverview: View {
    /// What the game is, as Wikipedia or its storefront tells it.
    var summary: String
    /// The Wikipedia article the summary comes from, which its license asks
    /// the page to credit; nil when the storefront wrote it.
    var summarySource: URL? = nil
    /// How the game was made, from its English Wikipedia article.
    var history: [GameHistorySection] = []
    var historySource: URL? = nil
    var media: [GameMedia] = []
    var facts: [GameFact]

    @State private var isWide = true
    @State private var showsWholeHistory = false
    @State private var viewing: GameMedia?

    private var isEmpty: Bool { summary.isEmpty && history.isEmpty && media.isEmpty && facts.isEmpty }

    var body: some View {
        if !isEmpty {
            Group {
                if isWide {
                    HStack(alignment: .top, spacing: 34) {
                        story.frame(maxWidth: .infinity, alignment: .leading)
                        details.frame(width: 300)
                    }
                } else {
                    VStack(alignment: .leading, spacing: 30) {
                        story
                        details
                    }
                }
            }
            .onGeometryChange(for: Bool.self, of: { $0.size.width >= 820 }) { isWide = $0 }
            .sheet(item: $viewing) { media in
                GameMediaViewer(media: self.media, shown: media)
            }
        }
    }

    // MARK: - The game, how it was made, its media

    private var story: some View {
        VStack(alignment: .leading, spacing: 30) {
            if !summary.isEmpty {
                GamePageSection(title: L10n.string("ui.game.the_game")) {
                    VStack(alignment: .leading, spacing: 10) {
                        Text(summary)
                            .font(.system(size: 15))
                            .lineSpacing(4)
                            .frame(maxWidth: 680, alignment: .leading)
                            .fixedSize(horizontal: false, vertical: true)
                            .textSelection(.enabled)
                        if let summarySource {
                            GameSourceLink(title: L10n.string("ui.game.from_wikipedia"), destination: summarySource)
                        }
                    }
                }
            }
            if !history.isEmpty {
                GamePageSection(title: L10n.string("ui.game.how_it_was_made")) {
                    historyText
                }
            }
            if !media.isEmpty {
                GamePageSection(title: L10n.string("ui.game.media")) {
                    GameMediaStrip(media: media) { viewing = $0 }
                }
            }
        }
    }

    /// The history's opening, fading out, until the player reads on.
    private var historyText: some View {
        VStack(alignment: .leading, spacing: 12) {
            VStack(alignment: .leading, spacing: 12) {
                ForEach(Array(shownHistory.enumerated()), id: \.offset) { _, section in
                    if let heading = section.heading {
                        Text(heading)
                            .font(.system(size: 13, weight: .semibold))
                            .padding(.top, 6)
                    }
                    ForEach(Array(section.paragraphs.enumerated()), id: \.offset) { _, paragraph in
                        Text(paragraph)
                            .font(.system(size: 13))
                            .foregroundStyle(.secondary)
                            .lineSpacing(3)
                            .fixedSize(horizontal: false, vertical: true)
                            .textSelection(.enabled)
                    }
                }
            }
            .frame(maxWidth: 680, alignment: .leading)
            .mask {
                LinearGradient(stops: showsWholeHistory || !historyContinues
                               ? [.init(color: .black, location: 0)]
                               : [.init(color: .black, location: 0.6), .init(color: .clear, location: 1)],
                               startPoint: .top, endPoint: .bottom)
            }
            HStack(spacing: 16) {
                if historyContinues || showsWholeHistory {
                    Button(L10n.string(showsWholeHistory ? "ui.game.show_less" : "ui.game.continue_reading")) {
                        withAnimation(.easeInOut(duration: 0.25)) { showsWholeHistory.toggle() }
                    }
                    .obeliskButtonStyle(.plain)
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(.tint)
                    .pointerCursor()
                }
                Spacer(minLength: 0)
                if let historySource {
                    GameSourceLink(title: L10n.string("ui.game.from_english_wikipedia"), destination: historySource)
                }
            }
            .frame(maxWidth: 680)
        }
    }

    /// The history as shown: whole, or its first paragraph.
    private var shownHistory: [GameHistorySection] {
        guard !showsWholeHistory, let first = history.first else { return history }
        return [GameHistorySection(heading: first.heading, paragraphs: Array(first.paragraphs.prefix(1)))]
    }

    private var historyContinues: Bool {
        history.count > 1 || (history.first?.paragraphs.count ?? 0) > 1
    }

    // MARK: - Details

    @ViewBuilder private var details: some View {
        if !facts.isEmpty {
            GamePageSection(title: L10n.string("ui.game.details")) {
                VStack(spacing: 0) {
                    ForEach(Array(facts.enumerated()), id: \.element.id) { index, fact in
                        if index > 0 { Divider() }
                        HStack(alignment: .firstTextBaseline, spacing: 12) {
                            Text(fact.label)
                                .foregroundStyle(.secondary)
                            Spacer(minLength: 8)
                            Text(fact.value)
                                .multilineTextAlignment(.trailing)
                                .fixedSize(horizontal: false, vertical: true)
                                .textSelection(.enabled)
                        }
                        .font(.system(size: 12))
                        .padding(.vertical, 10)
                    }
                }
                .modifier(GamePageCard(padding: 16))
            }
        }
    }
}

/// Credits the text above it to its source, linking it.
private struct GameSourceLink: View {
    let title: String
    let destination: URL

    var body: some View {
        Link(title, destination: destination)
            .font(.system(size: 11))
            .foregroundStyle(.secondary)
            .pointerCursor()
            .help(destination.absoluteString)
    }
}

/// A game's trailers, then its screenshots, in a row the player scrolls
/// sideways within its column, fading out at an edge it continues past;
/// one opens larger.
private struct GameMediaStrip: View {
    let media: [GameMedia]
    let open: (GameMedia) -> Void

    /// Whether the row continues past its leading and trailing edges.
    private struct Overflow: Equatable {
        var leading = false
        var trailing = false
    }

    @State private var overflow = Overflow()

    var body: some View {
        ScrollView(.horizontal) {
            LazyHStack(spacing: 12) {
                ForEach(Array(media.enumerated()), id: \.element.id) { index, item in
                    Button { open(item) } label: {
                        thumbnail(item)
                    }
                    .obeliskButtonStyle(.plain)
                    .pointerCursor()
                    .accessibilityLabel(label(item, index: index))
                }
            }
            .padding(.vertical, 6)
        }
        .scrollIndicators(.hidden)
        // The thumbnails' shadows spill above and below the row, but the row
        // stays in its column, beside the details.
        .scrollClipDisabled()
        .onScrollGeometryChange(for: Overflow.self) { geometry in
            Overflow(
                leading: geometry.contentOffset.x > 1,
                trailing: geometry.contentOffset.x + geometry.containerSize.width < geometry.contentSize.width - 1
            )
        } action: { _, overflow in
            withAnimation(.easeOut(duration: 0.2)) { self.overflow = overflow }
        }
        .mask {
            HStack(spacing: 0) {
                LinearGradient(colors: [overflow.leading ? .clear : .black, .black], startPoint: .leading, endPoint: .trailing)
                    .frame(width: 56)
                Color.black
                LinearGradient(colors: [.black, overflow.trailing ? .clear : .black], startPoint: .leading, endPoint: .trailing)
                    .frame(width: 56)
            }
            .padding(.vertical, -24)
        }
    }

    private func thumbnail(_ item: GameMedia) -> some View {
        let shape = RoundedRectangle(cornerRadius: 12, style: .continuous)
        return Color.clear
            .frame(width: 236, height: 133)
            .overlay {
                CachedArtwork(urls: [item.thumbnail], maxPixelSize: 600) { image in
                    image.resizable().scaledToFill()
                } placeholder: {
                    ArtworkPlaceholder()
                } failure: {
                    ArtworkPlaceholder()
                }
            }
            .overlay {
                if case .trailer = item {
                    Image(systemName: "play.fill")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(.white)
                        .frame(width: 44, height: 44)
                        .glassEffect(.regular, in: .circle)
                }
            }
            .overlay(alignment: .bottomLeading) {
                if case let .trailer(name, _, _) = item, !name.isEmpty {
                    Text(name)
                        .font(.system(size: 10.5, weight: .semibold))
                        .foregroundStyle(.white)
                        .shadow(color: .black.opacity(0.8), radius: 3)
                        .lineLimit(1)
                        .padding(8)
                }
            }
            .clipShape(shape)
            .shadow(color: .black.opacity(0.25), radius: 8, y: 4)
    }

    private func label(_ item: GameMedia, index: Int) -> String {
        switch item {
        case let .trailer(name, _, _): L10n.string("ui.game.play_trailer", name)
        case .screenshot:
            L10n.string("ui.game.screenshot", Int64(media[...index].filter { if case .screenshot = $0 { true } else { false } }.count))
        }
    }
}

/// A game's trailers and screenshots, one at a time and large: a trailer
/// plays as it shows, and the arrows go through them.
private struct GameMediaViewer: View {
    let media: [GameMedia]
    @State private var index: Int
    @State private var player: AVPlayer?
    @Environment(\.dismiss) private var dismiss

    init(media: [GameMedia], shown: GameMedia) {
        self.media = media
        _index = State(initialValue: media.firstIndex(of: shown) ?? 0)
    }

    var body: some View {
        VStack(spacing: 14) {
            ZStack {
                Color.black
                switch media[index] {
                case .trailer:
                    if let player { VideoPlayer(player: player) }
                case let .screenshot(thumbnail, full):
                    CachedArtwork(urls: [full, thumbnail], maxPixelSize: 2560) { image in
                        image.resizable().scaledToFit()
                    } placeholder: {
                        ProgressView().controlSize(.small)
                    } failure: {
                        ArtworkPlaceholder()
                    }
                }
            }
            .aspectRatio(16 / 9, contentMode: .fit)
            .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
            HStack {
                Button { show(index - 1) } label: { Image(systemName: "chevron.left") }
                    .keyboardShortcut(.leftArrow, modifiers: [])
                    .disabled(index == 0)
                    .accessibilityLabel(L10n.string("ui.game.previous_media"))
                Text(position)
                    .font(.system(size: 12).monospacedDigit())
                    .foregroundStyle(.secondary)
                    .frame(minWidth: 60)
                Button { show(index + 1) } label: { Image(systemName: "chevron.right") }
                    .keyboardShortcut(.rightArrow, modifiers: [])
                    .disabled(index == media.count - 1)
                    .accessibilityLabel(L10n.string("ui.game.next_media"))
                Spacer()
                Button(L10n.string("ui.common.close")) { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            .obeliskButtonStyle(.glass)
        }
        .padding(18)
        .frame(minWidth: 720, idealWidth: 1040, minHeight: 470)
        .onAppear { show(index) }
        .onDisappear { player?.pause() }
    }

    /// Which one shows, out of how many, such as 3 / 12.
    private var position: String {
        "\(index + 1) / \(media.count)"
    }

    private func show(_ position: Int) {
        guard media.indices.contains(position) else { return }
        index = position
        player?.pause()
        if case let .trailer(_, _, stream) = media[position] {
            let player = AVPlayer(url: stream)
            player.play()
            self.player = player
        } else {
            player = nil
        }
    }
}
