import Foundation

/// One language choice for SwiftUI, AppKit and messages received from Rust.
enum L10n {
    /// The languages Obelisk is translated into, as its lproj folders name them.
    static let available = Bundle.main.localizations.filter { $0 != "Base" }
    static let languageCode = resolvedLanguage(preferredLanguages: Locale.preferredLanguages)
    static let locale = formattingLocale(language: languageCode, region: Locale.current.region?.identifier)
    private static let strings = LocalizedStrings(language: languageCode)

    /// Regional variants share translations. An unsupported system language uses English.
    static func resolvedLanguage(preferredLanguages: [String], available: [String] = L10n.available) -> String {
        guard let first = preferredLanguages.first,
              let code = Locale(identifier: first).language.languageCode?.identifier,
              available.contains(code) else { return "en" }
        return code
    }

    static func formattingLocale(language: String, region: String?) -> Locale {
        Locale(identifier: region.map { "\(language)_\($0)" } ?? language)
    }

    static func string(_ key: String, _ arguments: CVarArg...) -> String {
        strings.string(key, arguments: arguments, locale: locale)
    }

    static func message(_ value: String) -> String {
        strings.message(value, locale: locale)
    }
}

/// The language games start in. A Finder-launched app has no `LANG`, and a
/// game, Windows ones in Wine above all, then takes English whatever the
/// player's language: Obelisk gives every game its own, the Mac's or the one
/// the player chose for Obelisk, as a `LANG` such as `it_IT.UTF-8`.
enum GameLocale {
    /// The `LANG` of the first of `preferredLanguages`, such as `it_IT.UTF-8`
    /// for `it-IT`. A language named without a region takes the one it is
    /// spoken in most, as `it` does Italy, else `region`; nil when it names
    /// no language.
    static func variable(preferredLanguages: [String], region: String? = Locale.current.region?.identifier) -> String? {
        guard let first = preferredLanguages.first else { return nil }
        let language = Locale.Language(identifier: first)
        guard let code = language.languageCode?.identifier, code != "und",
              let place = Locale.Language(identifier: language.maximalIdentifier).region?.identifier ?? region
        else { return nil }
        return "\(code)_\(place).UTF-8"
    }

    /// Gives the games Obelisk starts the player's language, unless Obelisk
    /// was given a `LANG`, as from Terminal or Xcode.
    static func adopt(preferredLanguages: [String] = Locale.preferredLanguages) {
        if let variable = variable(preferredLanguages: preferredLanguages) { setenv("LANG", variable, 0) }
    }
}

/// The language the player chose for Obelisk, or the Mac's own. macOS keeps
/// the choice as Obelisk's `AppleLanguages`, as its per-app language setting
/// does, so every part of Obelisk, AppKit's menus and the storefronts' content
/// included, takes it when Obelisk next starts.
enum AppLanguage: Hashable, Identifiable {
    case system
    case language(String)

    static let preferenceKey = "AppleLanguages"

    var id: String { code ?? "system" }

    /// The translation chosen, none for the Mac's language.
    var code: String? {
        switch self {
        case .system: nil
        case .language(let code): code
        }
    }

    /// The Mac's language first, then every translation by its own name.
    static var choices: [AppLanguage] {
        [.system] + L10n.available
            .sorted { name(of: $0).localizedStandardCompare(name(of: $1)) == .orderedAscending }
            .map(AppLanguage.language)
    }

    /// The countries whose flags stand for a language several of them speak;
    /// any other language shows the flag its own code names.
    static let countries = ["en": ["us", "gb"]]

    /// The flags that stand for this translation, as asset names: one, or two
    /// split across the diagonal. The Mac's language has none.
    var flags: [String] {
        guard let code else { return [] }
        return Self.countries[code, default: [code]].map { "Flag-\($0)" }
    }

    /// A language's name in itself, as language menus show it: "Français".
    static func name(of code: String) -> String {
        let locale = Locale(identifier: code)
        return locale.localizedString(forLanguageCode: code)?.capitalized(with: locale) ?? code
    }

    /// The Mac's own languages, whatever Obelisk's choice.
    static var systemLanguages: [String] {
        UserDefaults.standard.persistentDomain(forName: UserDefaults.globalDomain)?[preferenceKey] as? [String]
            ?? Locale.preferredLanguages
    }

    /// The choice `defaults` keeps for the app whose preferences are `domain`;
    /// one macOS's own setting made reads as the translation Obelisk shows for it.
    init(defaults: UserDefaults, domain: String) {
        let languages = defaults.persistentDomain(forName: domain)?[Self.preferenceKey] as? [String]
        self = languages?.first.map { .language(L10n.resolvedLanguage(preferredLanguages: [$0])) } ?? .system
    }

    func save(in defaults: UserDefaults) {
        if let code {
            defaults.set([code], forKey: Self.preferenceKey)
        } else {
            defaults.removeObject(forKey: Self.preferenceKey)
        }
    }

    /// The translation Obelisk shows with this choice, `system` being the
    /// Mac's languages.
    func resolved(system: [String]) -> String {
        L10n.resolvedLanguage(preferredLanguages: code.map { [$0] } ?? system)
    }
}

/// An explicit bundle makes language fallback deterministic and testable.
struct LocalizedStrings {
    let language: String
    private let bundle: Bundle
    private let fallback: Bundle

    init(language: String, bundle: Bundle = .main) {
        self.language = L10n.resolvedLanguage(preferredLanguages: [language])
        fallback = bundle.path(forResource: "en", ofType: "lproj").flatMap(Bundle.init(path:)) ?? bundle
        self.bundle = bundle.path(forResource: self.language, ofType: "lproj").flatMap(Bundle.init(path:)) ?? fallback
    }

    func string(_ key: String, arguments: [CVarArg] = [], locale: Locale? = nil) -> String {
        let english = fallback.localizedString(forKey: key, value: key, table: nil)
        let format = bundle.localizedString(forKey: key, value: english, table: nil)
        guard !arguments.isEmpty else { return format }
        return String(format: format, locale: locale ?? Locale(identifier: language), arguments: arguments)
    }

    /// Rust passes stable keys and JSON arguments, never a sentence to translate by matching it,
    /// and models reuse storefront errors for their own `model.` keys. Other values are
    /// filenames or diagnostics supplied by the OS or a remote service.
    func message(_ value: String, locale: Locale? = nil) -> String {
        if value.hasPrefix("backend.") || value.hasPrefix("model."), !value.contains(where: \.isWhitespace) {
            return string(value, locale: locale)
        }
        guard value.hasPrefix("{"), let data = value.data(using: .utf8),
              let payload = try? JSONDecoder().decode(Message.self, from: data),
              payload.key.hasPrefix("backend.") else { return value }
        let arguments: [CVarArg] = payload.args.map { message($0, locale: locale) }
        return string(payload.key, arguments: arguments, locale: locale)
    }

    private struct Message: Decodable {
        let key: String
        let args: [String]
    }
}
