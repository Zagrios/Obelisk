import SwiftUI

struct GameCard: View {
    let game: LibraryGame
    let launch: () -> Void
    let remove: () -> Void
    @Environment(\.colorScheme) private var colorScheme
    @State private var isHovered = false

    private var isNative: Bool { game.kind == .nativeMac }
    private var tint: Color {
        isNative ? Color(red: 0.49, green: 0.48, blue: 0.78) : Color(red: 0.30, green: 0.54, blue: 0.66)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ZStack {
                LinearGradient(
                    colors: [tint.opacity(0.32), tint.opacity(0.09)],
                    startPoint: .topLeading,
                    endPoint: .bottomTrailing
                )
                Image(systemName: isNative ? "gamecontroller.fill" : "display")
                    .font(.system(size: 46, weight: .light))
                    .foregroundStyle(tint.opacity(0.85))
            }
            .frame(height: 145)

            VStack(alignment: .leading, spacing: 7) {
                Text(game.title)
                    .font(.system(size: 14, weight: .semibold))
                    .lineLimit(1)
                    .help(game.title)

                Text(isNative ? L10n.string("ui.game.kind.macos") : L10n.string("ui.game.kind.windows"))
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)

                HStack(alignment: .center) {
                    if isNative {
                        if let lastPlayedAt = game.lastPlayedAt {
                            Text(lastPlayedAt, format: .dateTime.day().month(.abbreviated).locale(L10n.locale))
                                .font(.system(size: 10))
                                .foregroundStyle(.tertiary)
                                .help(L10n.string("ui.game.last_launch_app", AppIdentity.name))
                        } else {
                            Text(L10n.string("ui.game.ready_to_launch"))
                                .font(.system(size: 10))
                                .foregroundStyle(.tertiary)
                        }
                    } else {
                        Text(L10n.string("ui.game.engine_not_configured"))
                            .font(.system(size: 10))
                            .foregroundStyle(.secondary)
                    }

                    Spacer(minLength: 8)

                    Button(action: launch) {
                        Label(L10n.string("ui.game.play"), systemImage: "play.fill")
                            .font(.system(size: 11, weight: .semibold))
                    }
                    .forgeButtonStyle(.bordered)
                    .tint(ForgeStyle.accent)
                    .disabled(!isNative)
                    .help(isNative ? L10n.string("ui.game.launch_named", game.title) : L10n.string("ui.game.configure_windows_engine"))
                }
                .padding(.top, 7)
            }
            .padding(14)
        }
        .background(ForgePalette(colorScheme: colorScheme).surface)
        .clipShape(RoundedRectangle(cornerRadius: 11))
        .overlay {
            RoundedRectangle(cornerRadius: 11)
                .strokeBorder(isHovered ? tint.opacity(0.45) : ForgePalette(colorScheme: colorScheme).border)
        }
        .onHover { isHovered = $0 }
        // The card itself only reacts to a double click when the game can run.
        .pointerCursor(isNative)
        .onTapGesture(count: 2) { if isNative { launch() } }
        .contextMenu {
            Button(L10n.string("ui.game.launch"), systemImage: "play.fill", action: launch)
                .disabled(!isNative)
            Divider()
            Button(L10n.string("ui.game.remove"), systemImage: "minus.circle", role: .destructive, action: remove)
        }
    }
}
