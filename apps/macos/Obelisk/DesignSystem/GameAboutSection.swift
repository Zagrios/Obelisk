import SwiftUI

/// What a storefront tells of a game on its page: its summary, then facts
/// such as its developer or release date. Shows nothing when the storefront
/// tells neither.
struct GameAboutSection: View {
    struct Fact: Identifiable {
        let label: String
        let value: String
        var id: String { label }

        init(label: String, value: String) {
            self.label = label
            self.value = value
        }

        /// The fact `key` names, listing `values` in the player's language;
        /// nil when none is left once empty ones go.
        init?(_ key: String, _ values: [String]) {
            let values = values.filter { !$0.isEmpty }
            guard !values.isEmpty else { return nil }
            self.init(label: L10n.string(key), value: values.formatted(.list(type: .and).locale(L10n.locale)))
        }
    }

    let summary: String
    let facts: [Fact]
    /// The article the summary comes from, which its license asks the page
    /// to credit, as Wikipedia's.
    var summarySource: URL? = nil

    var body: some View {
        if !summary.isEmpty || !facts.isEmpty {
            GamePageSection(title: L10n.string("ui.game.about")) {
                VStack(alignment: .leading, spacing: 18) {
                    if !summary.isEmpty {
                        Text(summary)
                            .font(.system(size: 13))
                            .foregroundStyle(.secondary)
                            .lineSpacing(3)
                            .fixedSize(horizontal: false, vertical: true)
                            .textSelection(.enabled)
                    }
                    if !summary.isEmpty, let summarySource {
                        Link(L10n.string("ui.game.from_wikipedia"), destination: summarySource)
                            .font(.system(size: 11))
                            .foregroundStyle(.secondary)
                            .pointerCursor()
                    }
                    if !facts.isEmpty {
                        Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 24, verticalSpacing: 10) {
                            ForEach(facts) { fact in
                                GridRow {
                                    Text(fact.label)
                                        .foregroundStyle(.secondary)
                                        .gridColumnAlignment(.leading)
                                    Text(fact.value)
                                        .fixedSize(horizontal: false, vertical: true)
                                        .textSelection(.enabled)
                                }
                            }
                        }
                        .font(.system(size: 12))
                    }
                }
                .modifier(GamePageCard())
            }
        }
    }
}
