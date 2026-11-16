import Foundation
import Observation

/// The ways to support Obelisk, in a sheet Obelisk shows by itself once, the
/// third time it starts, and Settings shows whenever asked: a star on GitHub,
/// beside how many the repository has, the Discord server, beside how many
/// members it has, and the link a friend gets Obelisk from.
@MainActor
@Observable
final class SupportModel {
    /// The Discord server's public invite.
    nonisolated static let discord = URL(string: "https://discord.gg/h5a7F4rZ6f")!
    /// Where a friend gets Obelisk: its site.
    static let site = URL(string: "https://getobelisk.app")!
    /// The start of Obelisk the sheet shows at.
    static let offeredAtStart = 3
    static let startsKey = "obelisk.support.starts"
    static let offeredKey = "obelisk.support.offered"

    var isPresented = false
    /// How many stars the repository has, once GitHub told.
    private(set) var stars: Int?
    /// How many members the Discord server has, once Discord told.
    private(set) var members: Int?

    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private let readStars: @Sendable () async -> Int?
    @ObservationIgnored private let readMembers: @Sendable () async -> Int?

    /// `stars` and `members` read the counts the sheet shows; tests and the
    /// gallery tell their own.
    init(defaults: UserDefaults = .standard,
         stars: @escaping @Sendable () async -> Int? = { await SupportModel.repositoryStars() },
         members: @escaping @Sendable () async -> Int? = { await SupportModel.discordMembers() }) {
        self.defaults = defaults
        readStars = stars
        readMembers = members
    }

    /// Counts a start of Obelisk, up to the one the sheet shows at.
    func countStart() {
        let starts = defaults.integer(forKey: Self.startsKey)
        if starts < Self.offeredAtStart { defaults.set(starts + 1, forKey: Self.startsKey) }
    }

    /// Whether Obelisk started often enough to show the sheet by itself, and
    /// never did.
    var isDue: Bool {
        !defaults.bool(forKey: Self.offeredKey) && defaults.integer(forKey: Self.startsKey) >= Self.offeredAtStart
    }

    /// Shows the sheet by itself, which Obelisk does once.
    func offer() {
        guard isDue else { return }
        defaults.set(true, forKey: Self.offeredKey)
        present()
    }

    /// Shows the sheet, and reads how many stars the repository has and how
    /// many members the Discord server has, neither waiting for the other.
    func present() {
        isPresented = true
        Task { [weak self, readStars] in
            guard let stars = await readStars() else { return }
            self?.stars = stars
        }
        Task { [weak self, readMembers] in
            guard let members = await readMembers() else { return }
            self?.members = members
        }
    }

    /// How many stars Obelisk's repository has, as GitHub's API tells anyone;
    /// nil when it does not answer.
    nonisolated static func repositoryStars() async -> Int? {
        await count("stargazers_count", at: URL(string: "https://api.github.com/repos\(AppIdentity.repository.path)")!)
    }

    /// How many members the Discord server has, as its public invite tells
    /// anyone; nil when Discord does not answer.
    nonisolated static func discordMembers() async -> Int? {
        let invite = discord.lastPathComponent
        return await count("approximate_member_count",
                           at: URL(string: "https://discord.com/api/v10/invites/\(invite)?with_counts=true")!)
    }

    /// The count a public API's JSON answer holds under `key`; nil when it
    /// does not answer.
    private nonisolated static func count(_ key: String, at url: URL) async -> Int? {
        var request = URLRequest(url: url, timeoutInterval: 10)
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        guard let (body, response) = try? await URLSession.shared.data(for: request),
              (response as? HTTPURLResponse)?.statusCode == 200 else { return nil }
        return count(key, in: body)
    }

    /// The count a JSON object holds under `key`; nil when it holds none.
    nonisolated static func count(_ key: String, in body: Data) -> Int? {
        (try? JSONSerialization.jsonObject(with: body) as? [String: Any])?[key] as? Int
    }
}
