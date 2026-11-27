import SwiftUI

/// What a game's page says of the game at a glance, under its logo: its
/// critics' score, then such facts as its developer, genres and year; and
/// how it plays, under its launch row.
struct GameHeadline {
    /// The critics' score, out of 100.
    var score: Int? = nil
    var facts: [String] = []
    /// How the game plays, as its store names it, such as "Online Co-op".
    var features: [String] = []

    /// The year in `date` as its store writes it, such as "18 apr 2011".
    static func year(in date: String?) -> String? {
        date?.firstMatch(of: /\b(19|20)\d\d\b/).map { String($0.output.0) }
    }
}

/// A game's critics' score and facts in one line over its artwork, light
/// in both appearances, as the library's hero writes its details.
struct GameByline: View {
    let headline: GameHeadline

    var body: some View {
        HStack(spacing: 10) {
            if let score = headline.score {
                Text(score, format: .number)
                    .font(.system(size: 11, weight: .heavy).monospacedDigit())
                    .foregroundStyle(.black.opacity(0.85))
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(Self.color(of: score), in: RoundedRectangle(cornerRadius: 5, style: .continuous))
                    .accessibilityLabel(L10n.string("ui.game.rating"))
                    .accessibilityValue(L10n.string("ui.game.rating_value", Int64(score)))
            }
            if !headline.facts.isEmpty {
                Text(headline.facts.joined(separator: "  ·  "))
                    .font(.system(size: 11, weight: .semibold))
                    .tracking(0.8)
                    .textCase(.uppercase)
                    .foregroundStyle(.white.opacity(0.88))
                    .lineLimit(1)
            }
        }
        .shadow(color: .black.opacity(0.6), radius: 6)
    }

    /// Metacritic's colors: green from 75, yellow from 50, else red.
    private static func color(of score: Int) -> Color {
        switch score {
        case 75...: Color(red: 0.40, green: 0.80, blue: 0.40)
        case 50..<75: Color(red: 1.0, green: 0.80, blue: 0.20)
        default: Color(red: 1.0, green: 0.40, blue: 0.40)
        }
    }
}

/// How a game plays, as small capsules that wrap to a new line.
struct GameFeatureChips: View {
    let features: [String]

    var body: some View {
        GameChipFlow(spacing: 6) {
            ForEach(features, id: \.self) { feature in
                Text(feature)
                    .font(.system(size: 11.5, weight: .medium))
                    .lineLimit(1)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
                    // Facts, not controls: no glass, which marks what responds.
                    .background(.primary.opacity(0.06), in: .capsule)
                    .overlay { Capsule().strokeBorder(.primary.opacity(0.12), lineWidth: 1) }
            }
        }
        .accessibilityElement(children: .combine)
    }
}

/// Places its views in rows, starting a new row where one would overflow.
private struct GameChipFlow: Layout {
    let spacing: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let rows = rows(width: proposal.width ?? .infinity, subviews: subviews)
        let width = rows.map { $0.width }.max() ?? 0
        let height = rows.map(\.height).reduce(0, +) + spacing * CGFloat(max(rows.count - 1, 0))
        return CGSize(width: width, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var y = bounds.minY
        for row in rows(width: bounds.width, subviews: subviews) {
            var x = bounds.minX
            for index in row.indices {
                let size = subviews[index].sizeThatFits(.unspecified)
                subviews[index].place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
                x += size.width + spacing
            }
            y += row.height + spacing
        }
    }

    private struct Row {
        var indices: [Int] = []
        var width: CGFloat = 0
        var height: CGFloat = 0
    }

    private func rows(width: CGFloat, subviews: Subviews) -> [Row] {
        var rows: [Row] = [Row()]
        for index in subviews.indices {
            let size = subviews[index].sizeThatFits(.unspecified)
            if !rows[rows.count - 1].indices.isEmpty, rows[rows.count - 1].width + spacing + size.width > width {
                rows.append(Row())
            }
            var row = rows[rows.count - 1]
            row.width += (row.indices.isEmpty ? 0 : spacing) + size.width
            row.height = max(row.height, size.height)
            row.indices.append(index)
            rows[rows.count - 1] = row
        }
        return rows.filter { !$0.indices.isEmpty }
    }
}
