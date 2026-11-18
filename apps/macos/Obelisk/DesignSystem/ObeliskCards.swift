import SwiftUI

/// A card of rows, as a game page's card, its rows divided: Settings' cards
/// and the sheets' own.
struct ObeliskCard<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 0) { content }
            .modifier(GamePageCard(padding: 0))
    }
}

/// The line between two rows, from their text's leading edge.
struct ObeliskCardDivider: View {
    var body: some View {
        Divider().padding(.leading, 64)
    }
}

/// A row of a card: its symbol, its title over what it does, then its control.
struct ObeliskRow<Icon: View, Detail: View, Accessory: View>: View {
    let title: String
    @ViewBuilder let icon: Icon
    @ViewBuilder let detail: Detail
    @ViewBuilder let accessory: Accessory

    var body: some View {
        HStack(alignment: .center, spacing: 14) {
            icon
            VStack(alignment: .leading, spacing: 3) {
                Text(title)
                    .font(.system(size: 13, weight: .semibold))
                detail
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            accessory
                .padding(.leading, 10)
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 14)
    }
}

extension ObeliskRow where Detail == EmptyView {
    init(title: String, @ViewBuilder icon: () -> Icon, @ViewBuilder accessory: () -> Accessory) {
        self.init(title: title, icon: icon, detail: { EmptyView() }, accessory: accessory)
    }
}

/// A symbol, white on a tile of its colors, as System Settings shows its
/// panes: a row's at 32 points, a sheet's header at 44.
struct SymbolTile: View {
    let name: String
    let colors: [Color]
    var size: CGFloat = 32

    var body: some View {
        Image(systemName: name)
            .font(.system(size: size * 0.44, weight: .semibold))
            .onTile(colors: colors, size: size)
    }
}

private extension View {
    /// This glyph, white on a rounded tile of `colors`, top to bottom.
    func onTile(colors: [Color], size: CGFloat) -> some View {
        let shape = RoundedRectangle(cornerRadius: size * 0.28, style: .continuous)
        return foregroundStyle(.white)
            .frame(width: size, height: size)
            .background(LinearGradient(colors: colors, startPoint: .top, endPoint: .bottom), in: shape)
            .overlay { shape.strokeBorder(.white.opacity(0.16), lineWidth: 0.5) }
            .accessibilityHidden(true)
    }
}

/// The colors of a symbol's tile, top to bottom.
enum TileColors {
    static let violet = [ObeliskStyle.accent, Color(red: 0.45, green: 0.2, blue: 0.85)]
    static let grey = [Color(red: 0.6, green: 0.6, blue: 0.64), Color(red: 0.42, green: 0.42, blue: 0.46)]
    static let red = [Color(red: 0.98, green: 0.45, blue: 0.42), Color(red: 0.86, green: 0.22, blue: 0.3)]
    static let gold = [Color(red: 1, green: 0.8, blue: 0.3), Color(red: 0.95, green: 0.6, blue: 0.1)]
    static let blue = [Color(red: 0.38, green: 0.68, blue: 0.98), Color(red: 0.2, green: 0.5, blue: 0.93)]
    static let green = [Color(red: 0.4, green: 0.82, blue: 0.45), Color(red: 0.2, green: 0.64, blue: 0.3)]
    static let graphite = [Color(red: 0.36, green: 0.37, blue: 0.4), Color(red: 0.2, green: 0.21, blue: 0.24)]
}

/// A storefront's logo, white on a dark tile in its colors.
struct StoreTile: View {
    let section: LibrarySection
    var size: CGFloat = 32

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: size * 0.28, style: .continuous)
        ZStack {
            if let logo = section.logoAssetName {
                Image(logo)
                    .renderingMode(.template)
                    .resizable()
                    .scaledToFit()
                    .frame(width: size * 0.56, height: size * 0.56)
            }
        }
        .foregroundStyle(.white)
        .frame(width: size, height: size)
        .background {
            shape.fill(Color(white: 0.14))
                .overlay {
                    LinearGradient(colors: [section.tint.opacity(0.5), .clear], startPoint: .topLeading,
                                   endPoint: .bottomTrailing)
                        .clipShape(shape)
                }
        }
        .overlay { shape.strokeBorder(.white.opacity(0.14), lineWidth: 0.5) }
        .accessibilityHidden(true)
    }
}

/// A service's logo, white on a tile of its colors, as `SymbolTile` shows a
/// symbol.
struct LogoTile: View {
    let asset: String
    let colors: [Color]
    var size: CGFloat = 32

    var body: some View {
        Image(asset)
            .renderingMode(.template)
            .resizable()
            .scaledToFit()
            .frame(width: size * 0.56, height: size * 0.56)
            .onTile(colors: colors, size: size)
    }
}

/// A choice that is a card of its own: it outlines itself in the accent color
/// under the pointer.
struct ChoiceCard<Label: View>: View {
    let identifier: String
    let action: () -> Void
    @ViewBuilder let label: Label
    @State private var isHovered = false
    @Environment(\.isEnabled) private var isEnabled
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        let palette = ObeliskPalette(colorScheme: colorScheme)
        let shape = RoundedRectangle(cornerRadius: 18, style: .continuous)
        let highlighted = isHovered && isEnabled
        Button(action: action) {
            label
                .background(highlighted ? AnyShapeStyle(ObeliskStyle.accent.opacity(0.1))
                                        : AnyShapeStyle(palette.surface),
                            in: shape)
                .overlay {
                    shape.strokeBorder(highlighted ? AnyShapeStyle(ObeliskStyle.accent.opacity(0.7))
                                                    : AnyShapeStyle(palette.border),
                                       lineWidth: highlighted ? 1.5 : 1)
                }
                .contentShape(shape)
                .opacity(isEnabled ? 1 : 0.55)
        }
        .obeliskButtonStyle(.plain)
        .onHover { isHovered = $0 }
        .animation(.easeOut(duration: 0.15), value: highlighted)
        .accessibilityIdentifier(identifier)
    }
}

/// How large a capsule button is: a row's, or a sheet's own answers.
enum CapsuleButtonSize {
    case regular, large

    var height: CGFloat { self == .large ? 36 : 32 }
    var fontSize: CGFloat { self == .large ? 13 : 12 }
    var padding: CGFloat { self == .large ? 18 : 14 }
}

/// An action on a glass capsule, as the libraries' controls.
struct GlassCapsuleButton: View {
    let title: String
    var systemImage: String?
    var size = CapsuleButtonSize.regular
    var accessibilityLabel: String?
    let identifier: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 6) {
                if let systemImage {
                    Image(systemName: systemImage)
                        .font(.system(size: size.fontSize - 1, weight: .semibold))
                }
                Text(title)
            }
        }
        .buttonStyle(GlassCapsuleStyle(size: size))
        .pointerCursor()
        .fixedSize()
        .accessibilityLabel(accessibilityLabel ?? title)
        .accessibilityIdentifier(identifier)
    }
}

/// A button's glass capsule, for a button that keeps its own label.
struct GlassCapsuleStyle: ButtonStyle {
    var size = CapsuleButtonSize.regular
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: size.fontSize, weight: .semibold))
            .foregroundStyle(isEnabled ? .primary : .tertiary)
            .lineLimit(1)
            .padding(.horizontal, size.padding)
            .frame(height: size.height)
            .modifier(StorefrontControlGlass())
            .contentShape(.capsule)
    }
}

/// The main action, on a capsule of the accent color. While `isBusy`, a
/// spinner leads its title.
struct AccentCapsuleButton: View {
    let title: String
    var systemImage: String?
    var size = CapsuleButtonSize.regular
    var isBusy = false
    var accessibilityLabel: String?
    let identifier: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 7) {
                if isBusy {
                    ProgressView().controlSize(.mini).tint(.white)
                } else if let systemImage {
                    Image(systemName: systemImage)
                        .font(.system(size: size.fontSize - 1, weight: .semibold))
                }
                Text(title)
            }
            .font(.system(size: size.fontSize, weight: .semibold))
            .lineLimit(1)
            .padding(.horizontal, size.padding)
            .frame(height: size.height)
        }
        .buttonStyle(AccentCapsuleStyle())
        .pointerCursor()
        .fixedSize()
        .accessibilityLabel(accessibilityLabel ?? title)
        .accessibilityIdentifier(identifier)
    }
}

private struct AccentCapsuleStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(.white.opacity(isEnabled ? 1 : 0.6))
            .background(ObeliskStyle.accent.opacity(isEnabled ? (configuration.isPressed ? 0.75 : 1) : 0.4),
                        in: .capsule)
            .contentShape(.capsule)
    }
}

/// A few actions behind a glass capsule, which opens their menu.
struct GlassCapsuleMenu<Items: View>: View {
    let title: String
    /// What the capsule shows instead of its title, such as a picker's value.
    var value: String?
    var chevron = "chevron.down"
    let identifier: String
    @ViewBuilder let items: Items
    @Environment(\.isEnabled) private var isEnabled

    private var size: CapsuleButtonSize { .regular }

    var body: some View {
        Menu {
            items
        } label: {
            HStack(spacing: 6) {
                Text(value ?? title)
                Image(systemName: chevron)
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(.secondary)
            }
            .font(.system(size: size.fontSize, weight: .semibold))
            .foregroundStyle(isEnabled ? .primary : .tertiary)
            .lineLimit(1)
            .padding(.horizontal, size.padding)
            .frame(height: size.height)
            .modifier(StorefrontControlGlass())
            .contentShape(.capsule)
        }
        .menuStyle(.button)
        .obeliskButtonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel(title)
        .accessibilityValue(value ?? "")
        .accessibilityIdentifier(identifier)
    }
}

/// A choice among a few values, as a glass capsule naming the chosen one; its
/// menu checks it.
struct GlassMenuPicker<Value: Hashable, Options: View>: View {
    let title: String
    let value: String
    @Binding var selection: Value
    let identifier: String
    @ViewBuilder let options: Options

    var body: some View {
        GlassCapsuleMenu(title: title, value: value, chevron: "chevron.up.chevron.down", identifier: identifier) {
            Picker(title, selection: $selection) { options }
                .pickerStyle(.inline)
                .labelsHidden()
        }
    }
}
