import Foundation
@testable import SailOver

/// Cloud maintenance syncs a test runs in place of a storefront's. Each one
/// shows and asks through Rust's `CloudSync`, and runs until the test ends it.
final class CloudMaintenanceFixture: @unchecked Sendable {
    struct Start {
        let appId: UInt32?
        let steam: SteamCloudConfiguration?
        let epic: EpicCloudConfiguration?
        let sync: CloudSync
    }

    private let lock = NSLock()
    private var recorded: [Start] = []
    private var ending: CheckedContinuation<Error?, Never>?
    private var recordedAnswers: [CloudChoice?] = []

    var starts: [Start] { lock.withLock { recorded } }
    /// Each question's answer: nil once the player cancelled or signed out.
    var answers: [CloudChoice?] { lock.withLock { recordedAnswers } }

    var steam: SteamCloudMaintenanceStandIn {
        { [self] appId, config, sync in try await run(Start(appId: appId, steam: config, epic: nil, sync: sync)) }
    }
    var epic: EpicCloudMaintenanceStandIn {
        { [self] configuration, sync in try await run(Start(appId: nil, steam: nil, epic: configuration, sync: sync)) }
    }

    private func run(_ start: Start) async throws {
        let failure: Error? = await withCheckedContinuation { continuation in
            lock.withLock {
                recorded.append(start)
                ending = continuation
            }
        }
        if let failure { throw failure }
    }

    /// Shows progress in the running sync.
    func show(_ phase: CloudPhase, error: String? = nil) {
        starts.last?.sync.show(phase: phase, error: error)
    }

    /// Asks about a conflict in the running sync and records its answer.
    @discardableResult
    func askConflict(error: String? = nil) -> Task<CloudChoice?, Never> {
        let sync = starts.last?.sync
        return Task { [self] in
            let answer = await sync?.askConflict(error: error)
            lock.withLock { recordedAnswers.append(answer) }
            return answer
        }
    }

    /// Ends the running sync, failed with `failure`.
    func finish(_ failure: Error? = nil) {
        let ending: CheckedContinuation<Error?, Never>? = lock.withLock {
            defer { self.ending = nil }
            return self.ending
        }
        ending?.resume(returning: failure)
    }
}
