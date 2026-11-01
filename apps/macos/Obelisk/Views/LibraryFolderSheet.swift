import SwiftUI

/// Asks the player to move the library to the folder they picked: where and
/// what the disk can hold, then the move's progress until Obelisk restarts.
struct LibraryFolderSheet: View {
    let model: LibraryFolderModel
    let proposal: LibraryFolderProposal

    private var title: String {
        proposal.chosen == nil ? L10n.string("ui.library_folder.title_mac")
                               : L10n.string("ui.library_folder.title_move", proposal.volume)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            SheetHeader(title: title, subtitle: proposal.path,
                        symbol: proposal.chosen == nil ? "laptopcomputer" : "externaldrive.fill",
                        colors: TileColors.blue)
            VStack(alignment: .leading, spacing: 8) {
                Text(L10n.string(proposal.copiedBytes > 0 ? "ui.library_folder.copies" : "ui.library_folder.moves",
                                 AppIdentity.name))
                if proposal.copiedBytes > 0, let available = proposal.availableBytes {
                    Text(L10n.string("ui.games_folder.space", GameFormatting.bytes(proposal.copiedBytes),
                                     GameFormatting.bytes(available)))
                        .monospacedDigit()
                }
                Text(L10n.string("ui.library_folder.restarts", AppIdentity.name))
                    .foregroundStyle(.secondary)
            }
            .font(.system(size: 13))
            .fixedSize(horizontal: false, vertical: true)
            if proposal.isReadOnly {
                SheetNotice(kind: .failure, text: L10n.string("ui.games_folder.read_only", proposal.volume))
            } else if proposal.lacksSpace, let available = proposal.availableBytes {
                SheetNotice(kind: .failure, text: L10n.string("ui.games_folder.no_space", proposal.volume,
                                                              GameFormatting.bytes(proposal.copiedBytes),
                                                              GameFormatting.bytes(available)))
            } else if let format = proposal.format {
                SheetNotice(kind: .failure, text: L10n.string("ui.library_folder.format", proposal.volume, format,
                                                              AppIdentity.name))
            } else if proposal.isSynced {
                SheetNotice(kind: .failure, text: L10n.string("ui.library_folder.synced", AppIdentity.name))
            } else if proposal.chosen != nil {
                SheetNotice(kind: .warning, text: L10n.string("ui.library_folder.connected", AppIdentity.name))
            }
            if proposal.isCaseSensitive, proposal.canChoose {
                SheetNotice(kind: .warning, text: L10n.string("ui.library_folder.case_sensitive", proposal.volume))
            }
            if let moving = model.moving {
                progress(moving)
            }
            HStack(spacing: 10) {
                Spacer(minLength: 0)
                GlassCapsuleButton(title: L10n.string("ui.common.cancel"), size: .large,
                                   identifier: "library-folder-cancel", action: model.cancel)
                    .keyboardShortcut(.cancelAction)
                AccentCapsuleButton(title: L10n.string("ui.library_folder.move"), size: .large,
                                    identifier: "library-folder-confirm") {
                    Task { await model.confirm() }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!proposal.canChoose || model.moving != nil)
            }
        }
        .padding(24)
        .frame(width: 480)
        .tint(ObeliskStyle.accent)
        .interactiveDismissDisabled(model.moving != nil)
        .accessibilityIdentifier("library-folder-sheet")
    }

    private func progress(_ moving: LibraryMoveProgress) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            ProgressView(value: Double(moving.copiedBytes), total: Double(max(moving.totalBytes, 1)))
            Text(moving.totalBytes > 0
                 ? L10n.string("ui.library_folder.copied", GameFormatting.bytes(moving.copiedBytes),
                               GameFormatting.bytes(moving.totalBytes))
                 : L10n.string("ui.library_folder.moving"))
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .monospacedDigit()
        }
    }
}
