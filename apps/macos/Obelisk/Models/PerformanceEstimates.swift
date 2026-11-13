import Foundation
import Observation

/// How each game runs on this Mac's chip, as other Obelisk players measured
/// it: getobelisk.app's numbers for the chip, else for the nearest weaker one.
/// The whole table comes in one answer, kept for an hour, so the site never
/// learns which game the player looks at.
@MainActor @Observable
final class PerformanceEstimates {
    struct Estimate: Decodable, Equatable {
        let store: String
        let id: String
        let name: String
        /// Names the game's page on the site.
        let slug: String
        /// The chip these numbers come from: this Mac's, or a weaker one.
        let measuredOn: String
        let sessions: Int
        let fpsMedian: Double?
        let fpsLow1Percent: Double?
        let crashRate: Double

        enum CodingKeys: String, CodingKey {
            case store, id, name, slug, sessions
            case measuredOn = "measured_on"
            case fpsMedian = "fps_median"
            case fpsLow1Percent = "fps_low_1pct"
            case crashRate = "crash_rate"
        }

        /// The game's page on the site, with every chip's numbers.
        var page: URL? { URL(string: "https://getobelisk.app/games/\(slug)") }
    }

    private struct Answer: Decodable {
        let games: [Estimate]
    }

    nonisolated static let endpoint = URL(string: "https://getobelisk.app/api/performance")!
    /// How long an answer stays fresh.
    static let lifetime: TimeInterval = 3600

    private var estimates: [String: Estimate] = [:]
    @ObservationIgnored private var fetchedAt: Date?
    @ObservationIgnored private var loading: Task<Void, Never>?
    @ObservationIgnored private let chip: String
    @ObservationIgnored private let load: @Sendable (String) async -> [Estimate]?

    init(chip: String = MacHardware.current.chip,
         load: @escaping @Sendable (String) async -> [Estimate]? = { await PerformanceEstimates.fetch(chip: $0) }) {
        self.chip = chip
        self.load = load
    }

    /// The estimate of the game `store` names `id`, once an answer came.
    func estimate(store: SessionStorefront, id: String) -> Estimate? {
        estimates["\(store.siteName):\(id)"]
    }

    /// Asks the site again when the last answer is older than `lifetime`. A
    /// failed answer keeps the previous one until the next try.
    func refresh(now: Date = .now) {
        guard loading == nil, fetchedAt.map({ now.timeIntervalSince($0) >= Self.lifetime }) ?? true else { return }
        fetchedAt = now
        loading = Task {
            if let games = await load(chip) {
                estimates = Dictionary(games.map { ("\($0.store):\($0.id)", $0) }, uniquingKeysWith: { first, _ in first })
            }
            loading = nil
        }
    }

    private nonisolated static let session = URLSession(configuration: .ephemeral)

    nonisolated static func fetch(chip: String) async -> [Estimate]? {
        var components = URLComponents(url: endpoint, resolvingAgainstBaseURL: false)
        components?.queryItems = [URLQueryItem(name: "chip", value: chip)]
        guard let url = components?.url,
              let (data, response) = try? await session.data(for: URLRequest(url: url, timeoutInterval: 20)),
              (response as? HTTPURLResponse)?.statusCode == 200 else { return nil }
        return try? JSONDecoder().decode(Answer.self, from: data).games
    }
}
