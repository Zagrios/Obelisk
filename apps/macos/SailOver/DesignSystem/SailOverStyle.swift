import AppKit
import SwiftUI

/// Shared colors and spacing for the native macOS interface.
enum SailOverStyle {
    static let accent = Color(red: 1.0, green: 0.22, blue: 0.38)
    static let sidebarWidth: CGFloat = 216
    static let contentPadding: CGFloat = 32
    /// Fixed storefront title bar height; `StorefrontPage` reserves the same space.
    static let titleBarHeight: CGFloat = 58
}

struct SailOverPalette {
    let colorScheme: ColorScheme

    var background: Color {
        colorScheme == .dark
            ? Color(red: 0.135, green: 0.13, blue: 0.135)
            : Color(red: 0.975, green: 0.97, blue: 0.97)
    }

    var sidebar: Color {
        colorScheme == .dark
            ? Color(red: 0.16, green: 0.145, blue: 0.15)
            : Color(red: 0.94, green: 0.925, blue: 0.93)
    }

    var surface: Color {
        colorScheme == .dark
            ? Color.white.opacity(0.035)
            : Color.white.opacity(0.85)
    }

    var border: Color {
        Color.primary.opacity(colorScheme == .dark ? 0.075 : 0.08)
    }

    var selection: Color {
        Color.primary.opacity(colorScheme == .dark ? 0.12 : 0.075)
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
    /// SailOver is meant to show. Prefer it over `.buttonStyle` so the two cannot
    /// drift apart: a button that opts out of the pointer should say so with an
    /// explicit `.pointerCursor(false)`, not by omission.
    ///
    /// Apply `.disabled(_:)` after this, never before — `pointerCursor` reads
    /// the enabled state from the environment.
    func sailOverButtonStyle<S: PrimitiveButtonStyle>(_ style: S) -> some View {
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

struct SailOverPrimaryButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 13, weight: .semibold))
            .foregroundStyle(.white)
            .padding(.horizontal, 15)
            .padding(.vertical, 10)
            .background(
                SailOverStyle.accent.opacity(configuration.isPressed ? 0.75 : 1),
                in: RoundedRectangle(cornerRadius: 8)
            )
            .pointerCursor()
    }
}

struct StoreMark: View {
    let section: LibrarySection
    var size: CGFloat = 40

    private var tint: Color {
        switch section {
        case .steam: Color(red: 0.36, green: 0.64, blue: 0.88)
        case .epic: Color.secondary
        case .gog: Color(red: 0.69, green: 0.43, blue: 0.91)
        default: SailOverStyle.accent
        }
    }

    var body: some View {
        Image(systemName: section.symbol)
            .font(.system(size: size * 0.45, weight: .medium))
            .foregroundStyle(tint)
            .frame(width: size, height: size)
            .background(tint.opacity(0.12), in: RoundedRectangle(cornerRadius: size * 0.25))
            .accessibilityHidden(true)
    }
}

struct ComingSoonBadge: View {
    var body: some View {
        Text(L10n.string("ui.common.coming_soon"))
            .font(.system(size: 10, weight: .medium))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 7)
            .padding(.vertical, 4)
            .background(.primary.opacity(0.055), in: Capsule())
    }
}
