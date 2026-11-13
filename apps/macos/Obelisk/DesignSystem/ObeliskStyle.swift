import AppKit
import SwiftUI

/// Shared colors and spacing for the native macOS interface.
enum ObeliskStyle {
    /// The violet of Obelisk's icon.
    static let accent = Color(red: 0.77, green: 0.235, blue: 1.0)
    static let sidebarWidth: CGFloat = 216
    static let contentPadding: CGFloat = 32
    /// The floating glass bar's height; a storefront's pages reserve the same space.
    static let titleBarHeight: CGFloat = 58
    /// A library hero's height below the bar it reaches under.
    static let libraryHeroHeight: CGFloat = 330
    /// Steam's colors to launch and to install a game, leading to trailing.
    static let launchGradient = [Color(red: 0.46, green: 0.77, blue: 0.27), Color(red: 0.32, green: 0.63, blue: 0.20)]
    static let installGradient = [Color(red: 0.28, green: 0.55, blue: 0.98), Color(red: 0.40, green: 0.38, blue: 0.88)]
    /// A running game's colors: neutral to quit it, red to force it to quit.
    static let quitGradient = [Color(red: 0.50, green: 0.52, blue: 0.56), Color(red: 0.38, green: 0.40, blue: 0.44)]
    static let forceQuitGradient = [Color(red: 0.93, green: 0.30, blue: 0.27), Color(red: 0.78, green: 0.16, blue: 0.20)]
    /// A game's portrait cover, on its library card and on its page.
    static let coverCornerRadius: CGFloat = 20
    static let coverAspectRatio: CGFloat = 2.0 / 3.0
    /// How a library moves to a new filter or sort order.
    static var selectionAnimation: Animation { .spring(response: 0.32, dampingFraction: 0.82) }

    /// How a game's page replaces its library and back, and reveals itself.
    static func pageAnimation(reduceMotion: Bool) -> Animation {
        reduceMotion ? .easeInOut(duration: 0.2) : .spring(response: 0.38, dampingFraction: 0.86)
    }
}

extension LibrarySection {
    /// The storefront's color, or Obelisk's violet for its own sections.
    var tint: Color {
        switch self {
        case .steam: Color(red: 0.40, green: 0.75, blue: 0.96)
        case .epic: Color(white: 0.85)
        case .gog: Color(red: 0.69, green: 0.43, blue: 0.91)
        default: ObeliskStyle.accent
        }
    }
}

struct ObeliskPalette {
    let colorScheme: ColorScheme

    var background: Color {
        colorScheme == .dark
            ? Color(red: 0.135, green: 0.13, blue: 0.135)
            : Color(red: 0.975, green: 0.97, blue: 0.97)
    }

    var surface: Color {
        colorScheme == .dark
            ? Color.white.opacity(0.035)
            : Color.white.opacity(0.85)
    }

    var border: Color {
        Color.primary.opacity(colorScheme == .dark ? 0.075 : 0.08)
    }
}

extension View {
    /// Shows the pointing hand while the pointer is over a clickable control,
    /// the way `cursor: pointer` behaves on the web. A disabled control, or one
    /// passed `isClickable: false`, keeps the arrow so the cursor never
    /// promises an action that cannot happen.
    func pointerCursor(_ isClickable: Bool = true) -> some View {
        modifier(PointerCursor(isClickable: isClickable))
    }

    /// `buttonStyle`, paired with the pointing hand every clickable control in
    /// Obelisk is meant to show. Prefer it over `.buttonStyle` so the two cannot
    /// drift apart: a button that opts out of the pointer should say so with an
    /// explicit `.pointerCursor(false)`, not by omission.
    ///
    /// Apply `.disabled(_:)` after this, never before — `pointerCursor` reads
    /// the enabled state from the environment.
    func obeliskButtonStyle<S: PrimitiveButtonStyle>(_ style: S) -> some View {
        buttonStyle(style).pointerCursor()
    }
}

private struct PointerCursor: ViewModifier {
    let isClickable: Bool
    @Environment(\.isEnabled) private var isEnabled

    private var pointsAtAction: Bool { isClickable && isEnabled }

    func body(content: Content) -> some View {
        // `.default` rather than `nil`: an unclickable control nested in a
        // clickable one must not inherit its pointing hand.
        content.pointerStyle(pointsAtAction ? .link : .default)
    }
}

struct ObeliskPrimaryButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 13, weight: .semibold))
            .foregroundStyle(.white)
            .padding(.horizontal, 15)
            .padding(.vertical, 10)
            .background(
                ObeliskStyle.accent.opacity(configuration.isPressed ? 0.75 : 1),
                in: RoundedRectangle(cornerRadius: 8)
            )
            .pointerCursor()
    }
}
