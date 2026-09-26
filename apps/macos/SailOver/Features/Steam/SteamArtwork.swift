import SwiftUI

struct SteamArtwork: View {
    let appId: UInt32
    var portrait = true
    var iconHash = ""

    private var base: String { "https://shared.fastly.steamstatic.com/store_item_assets/steam/apps/\(appId)" }

    var body: some View {
        AsyncImage(url: URL(string: "\(base)/\(portrait ? "library_600x900.jpg" : "library_hero_2x.jpg")")) { phase in
            if let image = phase.image {
                image.resizable().scaledToFill()
            } else if phase.error != nil {
                if portrait { headerFallback }
                else { heroFallback }
            }
            else { placeholder }
        }
        .accessibilityHidden(true)
    }

    // Some games only publish the standard library hero or the store header.
    private var heroFallback: some View {
        AsyncImage(url: URL(string: "\(base)/library_hero.jpg")) { phase in
            if let image = phase.image { image.resizable().scaledToFill() }
            else if phase.error != nil { headerFallback }
            else { placeholder }
        }
    }

    private var headerFallback: some View {
        AsyncImage(url: URL(string: "\(base)/header.jpg")) { phase in
            if let image = phase.image { image.resizable().scaledToFill() }
            else if phase.error != nil { iconFallback }
            else { placeholder }
        }
    }

    @ViewBuilder private var iconFallback: some View {
        if iconHash.count == 40 && iconHash.allSatisfy(\.isHexDigit) {
            AsyncImage(url: URL(string: "https://media.steampowered.com/steamcommunity/public/images/apps/\(appId)/\(iconHash).jpg")) { phase in
                if let image = phase.image {
                    placeholder.overlay { image.resizable().scaledToFit().padding(30) }
                } else { placeholder }
            }
        } else { placeholder }
    }

    private var placeholder: ArtworkPlaceholder { ArtworkPlaceholder() }
}

enum SteamFormatting {
    static func playtime(_ minutes: UInt32) -> String {
        if minutes == 0 { return L10n.string("ui.game.never_played") }
        if minutes < 60 { return L10n.string("ui.game.playtime_minutes", Int64(minutes)) }
        let hours = Double(minutes) / 60
        return L10n.string("ui.game.playtime_hours", hours.formatted(.number.precision(.fractionLength(0...1)).locale(L10n.locale)))
    }

    static func lastPlayed(_ timestamp: UInt64) -> String {
        guard timestamp > 0 else { return L10n.string("ui.game.never_launched") }
        return Date(timeIntervalSince1970: TimeInterval(timestamp)).formatted(.dateTime.day().month(.abbreviated).year().locale(L10n.locale))
    }

    static func bytes(_ value: UInt64) -> String {
        Int64(clamping: value).formatted(.byteCount(style: .file).locale(L10n.locale))
    }
}
