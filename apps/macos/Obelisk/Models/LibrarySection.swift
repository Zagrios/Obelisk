import Foundation

enum LibrarySection: String, CaseIterable, Identifiable, Sendable {
    case home, steam, epic, gog, settings

    var id: String { rawValue }

    var title: String {
        switch self {
        case .home: L10n.string("model.section.home.title")
        case .steam: L10n.string("model.section.steam.title")
        case .epic: L10n.string("model.section.epic.title")
        case .gog: L10n.string("model.section.gog.title")
        case .settings: L10n.string("model.section.settings.title")
        }
    }

    var symbol: String {
        switch self {
        case .home: "house"
        case .steam: "gearshape.2"
        case .epic: "shield.lefthalf.filled"
        case .gog: "square.stack.3d.up"
        case .settings: "gearshape"
        }
    }

    var logoAssetName: String? {
        switch self {
        case .steam: "SteamLogo"
        case .epic: "EpicGamesLogo"
        case .gog: "GOGLogo"
        default: nil
        }
    }

    /// What the storefront's library holds, in a line; nil for Obelisk's own sections.
    var storeSubtitle: String? {
        switch self {
        case .steam: L10n.string("model.section.steam.subtitle")
        case .epic: L10n.string("model.section.epic.subtitle")
        case .gog: L10n.string("model.section.gog.subtitle")
        default: nil
        }
    }

    /// The storefront's full name, as "Epic Games"; nil for Obelisk's own sections.
    var storeName: String? {
        switch self {
        case .steam: L10n.string("ui.store.steam")
        case .epic: L10n.string("ui.store.epic")
        case .gog: L10n.string("ui.store.gog")
        default: nil
        }
    }

    /// What names the section where its store is meant: the storefront's full
    /// name, else the section's own title.
    var storeTitle: String { storeName ?? title }

    var isStorefront: Bool { self == .steam || self == .epic || self == .gog }

    /// Store tabs in sidebar order; the single source for every storefront list.
    static let storefronts: [LibrarySection] = allCases.filter(\.isStorefront)
}

extension SessionStorefront {
    /// The section of the storefront's library.
    var section: LibrarySection {
        switch self {
        case .steam: .steam
        case .epic: .epic
        case .gog: .gog
        }
    }

    /// How getobelisk.app names the storefront: a wire value, spelled apart
    /// from the section's.
    var siteName: String {
        switch self {
        case .steam: "steam"
        case .epic: "epic"
        case .gog: "gog"
        }
    }
}
