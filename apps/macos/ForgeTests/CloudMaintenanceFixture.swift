import Foundation
@testable import Forge

/// A Cloud maintenance attempt driven by the test instead of Rust.
final class CloudMaintenanceFixture: CloudMaintenance, @unchecked Sendable {
    private let lock = NSLock()
    private var current = CloudMaintenanceStatus(revision: 0, phase: .checking, decisionId: 0,
                                                 allowedChoices: [], error: nil, finished: false)
    private var waiters: [CheckedContinuation<CloudMaintenanceStatus, Never>] = []
    private var recordedAnswers: [(UInt64, CloudMaintenanceChoice)] = []
    private var wasCancelled = false

    init() { super.init(noHandle: NoHandle()) }

    required init(unsafeFromHandle handle: UInt64) { fatalError("A fixture has no Rust handle") }

    var answers: [(UInt64, CloudMaintenanceChoice)] { lock.withLock { recordedAnswers } }
    var cancelled: Bool { lock.withLock { wasCancelled } }

    func publish(_ phase: CloudMaintenancePhase, decisionId: UInt64 = 0,
                 choices: [CloudMaintenanceChoice] = [], error: String? = nil, finished: Bool = false) {
        let (status, ready) = lock.withLock { () -> (CloudMaintenanceStatus, [CheckedContinuation<CloudMaintenanceStatus, Never>]) in
            current = CloudMaintenanceStatus(revision: current.revision + 1, phase: phase, decisionId: decisionId,
                                             allowedChoices: choices, error: error, finished: finished)
            defer { waiters.removeAll() }
            return (current, waiters)
        }
        for waiter in ready { waiter.resume(returning: status) }
    }

    func finish(_ phase: CloudMaintenancePhase, error: String? = nil) {
        publish(phase, error: error, finished: true)
    }

    override func status(after: UInt64?) async -> CloudMaintenanceStatus {
        await withCheckedContinuation { continuation in
            let ready: CloudMaintenanceStatus? = lock.withLock {
                if current.finished || after != current.revision { return current }
                waiters.append(continuation)
                return nil
            }
            if let ready { continuation.resume(returning: ready) }
        }
    }

    override func answer(decisionId: UInt64, choice: CloudMaintenanceChoice) -> Bool {
        lock.withLock {
            recordedAnswers.append((decisionId, choice))
            return decisionId == current.decisionId && current.allowedChoices.contains(choice)
        }
    }

    override func cancel() { lock.withLock { wasCancelled = true } }
}
