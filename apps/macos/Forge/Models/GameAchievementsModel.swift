import Foundation
import Observation

@MainActor
@Observable
final class GameAchievementsModel {
    let storeID: String
    /// Each storefront maps its own backend failures, as it does for details
    /// and news, so this model stays free of store-specific error types.
    @ObservationIgnored private let describe: @MainActor (Error) -> String

    init(storeID: String, describe: @escaping @MainActor (Error) -> String) {
        self.storeID = storeID
        self.describe = describe
    }

    private(set) var items: [GameAchievement]?
    private(set) var isLoading = false
    private(set) var error: String?
    var isPresented = false
    @ObservationIgnored private var task: Task<Void, Never>?
    // Counters invalidate late responses, as in the storefront models. The
    // request task is cancelled too, which frees its work rather than gating it.
    @ObservationIgnored private var generation = 0

    private var unlocked: Int { items?.count(where: \.unlocked) ?? 0 }
    var fraction: Double? {
        guard let items, !items.isEmpty else { return nil }
        return Double(unlocked) / Double(items.count)
    }
    var countText: String {
        guard let items else { return "—" }
        return "\(unlocked)/\(items.count)"
    }
    /// A game with no achievements has nothing to show, and neither has one whose
    /// list has not been requested; a load in flight or a failure both do.
    var hasContent: Bool { isLoading || error != nil || items?.isEmpty == false }

    func reset() {
        generation += 1
        task?.cancel()
        task = nil
        items = nil
        error = nil
        isLoading = false
        isPresented = false
    }

    func load(_ fetch: @escaping @MainActor @Sendable () async throws -> [GameAchievement]) {
        generation += 1
        let request = generation
        task?.cancel()
        items = nil
        error = nil
        isLoading = true
        task = Task { [weak self] in
            do {
                let items = try await fetch()
                guard let self, self.generation == request else { return }
                self.items = items
                self.isLoading = false
            } catch {
                guard let self, self.generation == request else { return }
                self.error = self.describe(error)
                self.isLoading = false
            }
        }
    }
    deinit { task?.cancel() }
}


struct GameAchievement: Sendable {
    let id: String
    let name: String
    let description: String
    let iconUrl: String
    let hidden: Bool
    let unlocked: Bool
    let unlockDate: Date?
}

extension GameAchievement {
    init(_ value: SteamAchievement) {
        self.init(id: value.id, name: value.name, description: value.description, iconUrl: value.iconUrl,
                  hidden: value.hidden, unlocked: value.unlocked,
                  unlockDate: value.unlocked && value.unlockTime > 0 ? Date(timeIntervalSince1970: TimeInterval(value.unlockTime)) : nil)
    }

    init(_ value: EpicAchievement) {
        let date = value.unlockDate.flatMap {
            (try? Date.ISO8601FormatStyle(includingFractionalSeconds: true).parse($0))
                ?? (try? Date.ISO8601FormatStyle().parse($0))
        }
        self.init(id: value.id, name: value.name, description: value.description, iconUrl: value.iconUrl,
                  hidden: value.hidden, unlocked: value.unlocked, unlockDate: value.unlocked ? date : nil)
    }
}
