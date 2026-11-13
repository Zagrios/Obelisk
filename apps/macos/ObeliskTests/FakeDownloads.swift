import Foundation
@testable import Obelisk

/// The download queue as Rust would publish it, driven by the test. Rust's
/// own suite covers its order; this records what Swift asks.
@MainActor
final class FakeDownloads: DownloadsBackend {
    enum Call: Equatable {
        case remove(UInt64), pause(UInt64), resume(UInt64), moveToTop(UInt64)
    }

    private(set) var current = DownloadQueueState(revision: 0, running: nil, waiting: [], held: false)
    private(set) var calls: [Call] = []
    private(set) var speedLimit: UInt64 = 0
    private var waiters: [CheckedContinuation<Void, Never>] = []

    /// Publishes a change, as Rust does after every one.
    func publish(_ change: (inout DownloadQueueState) -> Void) {
        change(&current)
        current.revision += 1
        let woken = waiters
        waiters.removeAll()
        for waiter in woken { waiter.resume() }
    }

    func state() -> DownloadQueueState { current }
    func changed(after: UInt64) async -> DownloadQueueState {
        while current.revision <= after {
            await withCheckedContinuation { waiters.append($0) }
        }
        return current
    }
    func remove(_ id: UInt64) { calls.append(.remove(id)) }
    func pause(_ id: UInt64) { calls.append(.pause(id)) }
    func resume(_ id: UInt64) { calls.append(.resume(id)) }
    func moveToTop(_ id: UInt64) { calls.append(.moveToTop(id)) }
    func setSpeedLimit(_ bytesPerSecond: UInt64) { speedLimit = bytesPerSecond }
}

extension DownloadItem {
    static func fixture(_ id: UInt64, game: String = "620", kind: DownloadKind = .install,
                        automatic: Bool = false, paused: Bool = false) -> Self {
        DownloadItem(id: id, storefront: .steam, account: "42", game: game, name: "Game \(game)", kind: kind,
                     automatic: automatic, launching: false, paused: paused)
    }
}
