import Foundation

/// One language choice for SwiftUI, AppKit and messages received from Rust.
enum L10n {
    static let languageCode = resolvedLanguage(preferredLanguages: Locale.preferredLanguages)
    static let locale = formattingLocale(language: languageCode, region: Locale.current.region?.identifier)
    private static let strings = LocalizedStrings(language: languageCode)

    /// Regional variants share translations. An unsupported system language uses English.
    static func resolvedLanguage(preferredLanguages: [String]) -> String {
        guard let first = preferredLanguages.first else { return "en" }
        return Locale(identifier: first).language.languageCode?.identifier == "fr" ? "fr" : "en"
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

    /// Rust passes stable keys and JSON arguments, never a sentence to translate by matching it.
    /// Other values are filenames or diagnostics supplied by the OS or a remote service.
    func message(_ value: String, locale: Locale? = nil) -> String {
        if value.hasPrefix("backend."), !value.contains(where: \.isWhitespace) {
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
