import SwiftUI

enum GamePlatform {
    case macOS, windows

    static func preferred(macos: Bool, windows: Bool) -> Self? {
        if macos { return .macOS }
        if windows { return .windows }
        return nil
    }

    static func preferred(platforms: [String]) -> Self? {
        preferred(macos: platforms.contains("macos"), windows: platforms.contains("windows"))
    }

    var title: String {
        switch self {
        case .macOS: L10n.string("model.platform.macos")
        case .windows: L10n.string("model.platform.windows")
        }
    }

    var logoAssetName: String {
        switch self {
        case .macOS: "AppleLogo"
        case .windows: "WindowsLogo"
        }
    }
}

/// A compact system logo that stays legible over both light and dark artwork.
struct GamePlatformBadge: View {
    let platform: GamePlatform

    var body: some View {
        Image(platform.logoAssetName)
            .resizable()
            .scaledToFit()
            .frame(width: 15, height: 15)
            .foregroundStyle(.white)
            .frame(width: 30, height: 30)
            .background(.black.opacity(0.7), in: RoundedRectangle(cornerRadius: 7))
            .accessibilityHidden(true)
            .allowsHitTesting(false)
    }
}
