import SwiftUI

enum AppAppearance: String, CaseIterable, Identifiable, Sendable {
    case system, light, dark

    var id: String { rawValue }
    var title: String {
        switch self {
        case .system: L10n.string("model.appearance.system")
        case .light: L10n.string("model.appearance.light")
        case .dark: L10n.string("model.appearance.dark")
        }
    }

    var colorScheme: ColorScheme? {
        switch self {
        case .system: nil
        case .light: .light
        case .dark: .dark
        }
    }
}
