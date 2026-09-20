import Foundation
import Observation

@MainActor
@Observable
final class SteamLibraryPlatforms {
    private(set) var platforms: [UInt32: [String]] = [:]
    private(set) var isLoading = false
    private(set) var hasFailures = false

    @ObservationIgnored private var loadID: UUID?
    @ObservationIgnored private var task: Task<Void, Never>?

    deinit {
        task?.cancel()
    }

    func load(appIds: [UInt32], session: any SteamSessionProtocol) {
        reset()
        var seen: Set<UInt32> = []
        let appIds = appIds.filter { seen.insert($0).inserted }
        guard !appIds.isEmpty else { return }

        let loadID = UUID()
        self.loadID = loadID
        isLoading = true
        task = Task { [weak self] in
            defer {
                if self?.loadID == loadID {
                    self?.loadID = nil
                    self?.task = nil
                    self?.isLoading = false
                }
            }
            for start in stride(from: 0, to: appIds.count, by: 50) {
                guard !Task.isCancelled, self?.loadID == loadID else { return }
                let batch = Array(appIds[start..<min(start + 50, appIds.count)])
                do {
                    let result = try await session.platforms(appIds: batch)
                    // UniFFI can return after cancellation or account replacement.
                    guard !Task.isCancelled, let self, self.loadID == loadID else { return }
                    for game in result { self.platforms[game.appId] = game.platforms }
                } catch {
                    guard !Task.isCancelled, let self, self.loadID == loadID else { return }
                    self.hasFailures = true
                }
            }
        }
    }

    func reset() {
        task?.cancel()
        task = nil
        loadID = nil
        platforms.removeAll()
        isLoading = false
        hasFailures = false
    }
}
