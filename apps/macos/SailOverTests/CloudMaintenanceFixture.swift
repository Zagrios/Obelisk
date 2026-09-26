import Foundation
@testable import SailOver

/// A Cloud maintenance attempt driven by the test instead of Rust.
final class CloudMaintenanceFixture: CloudMaintenanceProtocol, @unchecked Sendable {
    private let lock = NSLock()
    private var current = CloudMaintenanceStatus(revision: 0, phase: .checking, decisionId: 0,
                                                 allowedChoices: [], error: nil, finished: false)
    private var waiters: [CheckedContinuation<CloudMaintenanceStatus, Never>] = []
    private var recordedAnswers: [(UInt64, CloudChoice)] = []
    private var wasCancelled = false

    var answers: [(UInt64, CloudChoice)] { lock.withLock { recordedAnswers } }
    var cancelled: Bool { lock.withLock { wasCancelled } }

    func publish(_ phase: CloudMaintenancePhase, decisionId: UInt64 = 0,
                 choices: [CloudChoice] = [], error: String? = nil, finished: Bool = false) {
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

    func status(after: UInt64?) async -> CloudMaintenanceStatus {
        await withCheckedContinuation { continuation in
            let ready: CloudMaintenanceStatus? = lock.withLock {
                if current.finished || after != current.revision { return current }
                waiters.append(continuation)
                return nil
            }
            if let ready { continuation.resume(returning: ready) }
        }
    }

    func answer(decisionId: UInt64, choice: CloudChoice) -> Bool {
        lock.withLock {
            recordedAnswers.append((decisionId, choice))
            return decisionId == current.decisionId && current.allowedChoices.contains(choice)
        }
    }

    func cancel() { lock.withLock { wasCancelled = true } }
}
