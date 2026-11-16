import Foundation

/// Obelisk's GitHub issue forms, in the repository's .github/ISSUE_TEMPLATE,
/// opened with what Obelisk knows filled in: GitHub fills a form's fields
/// from query items named after their ids.
enum GitHubIssues {
    static let newIssue = AppIdentity.repository.appending(path: "issues/new")
    /// How long the description's start may be in a title, after its prefix.
    static let titleLimit = 80
    /// GitHub refuses very long URLs; the report keeps the whole description.
    static let textLimit = 2000

    /// The bug form for `report`, which the site gave `code`, titled "BUG : "
    /// then the game and the description's start. The contact the player gave
    /// stays out: an issue is public.
    static func bug(_ report: BugReport, code: String) -> URL {
        let summary = shortened(report.description.prefix { !$0.isNewline }, to: titleLimit)
        var fields = [
            ("template", "bug_report.yml"),
            ("title", "BUG : " + (report.game.map { "\($0.name): \(summary)" } ?? summary)),
            ("report-code", code),
        ]
        if let game = report.game {
            fields.append(("game", "\(game.name) (\(storeName(game.store)) \(game.id))"))
        }
        fields += [
            ("what-happened", shortened(report.description, to: textLimit)),
            ("obelisk-version", report.voileVersion.map { "\(report.obeliskVersion) (Voile \($0))" }
                ?? report.obeliskVersion),
            ("mac", "\(report.chip), \(report.macModel), macOS \(report.macOSVersion)"),
        ]
        return url(fields)
    }

    /// The feature form, its title started for the player.
    static func feature(obeliskVersion: String) -> URL {
        url([("template", "feature_request.yml"), ("title", "FEATURE : "), ("obelisk-version", obeliskVersion)])
    }

    private static func storeName(_ store: String) -> String {
        switch store {
        case "steam": "Steam"
        case "epic": "Epic Games"
        case "gog": "GOG"
        default: store
        }
    }

    /// `text` in at most `limit` UTF-16 units, cut after a word with "…".
    private static func shortened(_ text: some StringProtocol, to limit: Int) -> String {
        var text = String(text)
        guard text.utf16.count > limit else { return text }
        while text.utf16.count > limit - 1 { text.removeLast() }
        if let space = text.lastIndex(of: " "), text.distance(from: text.startIndex, to: space) > limit / 2 {
            text = String(text[..<space])
        }
        return text.trimmingCharacters(in: .whitespaces) + "…"
    }

    /// A query keeps `+`, `&`, `=` and `#` encoded: a `+` would read as a space.
    private static let allowed = CharacterSet.urlQueryAllowed.subtracting(CharacterSet(charactersIn: "+&=#"))

    private static func url(_ fields: [(String, String)]) -> URL {
        var components = URLComponents(url: newIssue, resolvingAgainstBaseURL: false)!
        components.percentEncodedQueryItems = fields.map {
            URLQueryItem(name: $0.0, value: $0.1.addingPercentEncoding(withAllowedCharacters: allowed))
        }
        return components.url!
    }
}
