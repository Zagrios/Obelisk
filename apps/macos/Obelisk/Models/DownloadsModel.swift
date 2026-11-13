import Foundation
import Observation

/// The process's download queue, as Rust keeps it. Each action returns at
/// once; the next state tells how it went.
@MainActor
protocol DownloadsBackend: AnyObject {
    func state() -> DownloadQueueState
    /// Waits for a revision newer than `after`.
    func changed(after: UInt64) async -> DownloadQueueState
    func remove(_ id: UInt64)
    func pause(_ id: UInt64)
    func resume(_ id: UInt64)
    func moveToTop(_ id: UInt64)
    /// In bytes per second; zero lifts the limit.
    func setSpeedLimit(_ bytesPerSecond: UInt64)
}

/// The queue every account's installations share in Rust.
final class RustDownloads: DownloadsBackend {
    private let core = Downloads()

    func state() -> DownloadQueueState { core.state() }
    func changed(after: UInt64) async -> DownloadQueueState { await core.changed(after: after) }
    func remove(_ id: UInt64) { core.remove(id: id) }
    func pause(_ id: UInt64) { core.pause(id: id) }
    func resume(_ id: UInt64) { core.resume(id: id) }
    func moveToTop(_ id: UInt64) { core.moveToTop(id: id) }
    func setSpeedLimit(_ bytesPerSecond: UInt64) { core.setSpeedLimit(bytesPerSecond: bytesPerSecond) }
}

/// The unit download speeds read in.
enum DownloadSpeedUnit: String, CaseIterable, Identifiable {
    /// The file sizes Finder shows, per second.
    case bytes
    case bits

    static let bytesPerMegabit = 125_000
    private static let byteLimits = [1, 2, 5, 10, 25, 50, 100].map { $0 * 1_000_000 } + [0]
    private static let bitLimits = [5, 10, 25, 50, 100, 250, 500, 1000].map { $0 * bytesPerMegabit } + [0]

    var id: String { rawValue }
    var title: String {
        switch self {
        case .bytes: L10n.string("model.downloads.speed_unit_bytes")
        case .bits: L10n.string("model.downloads.speed_unit_bits")
        }
    }

    /// The limits the player picks the speed from in this unit, in bytes per
    /// second, ending with none (zero). None goes below the lowest limit Rust
    /// holds to: see `set_speed_limit`.
    var speedLimits: [Int] {
        switch self {
        case .bytes: Self.byteLimits
        case .bits: Self.bitLimits
        }
    }
}

/// The running Transfer's download speed at one moment.
struct DownloadSpeedSample: Identifiable, Equatable {
    let id: Int
    let time: Date
    let bytesPerSecond: Double
}

/// Every storefront's Transfers, waiting in one queue and running one at a
/// time: the player's downloads, updates and repairs, and Obelisk's
/// automatic updates. It also keeps the running Transfer's speed.
@MainActor @Observable
final class DownloadsModel {
    private(set) var running: DownloadItem?
    private(set) var waiting: [DownloadItem] = []
    /// A game runs: automatic updates wait.
    private(set) var held = false
    /// The running Transfer's download speed, one sample a period, for the
    /// last two minutes.
    private(set) var samples: [DownloadSpeedSample] = []
    var isPresented = false
    /// The unit speeds read in, as the player chose.
    var speedUnit: DownloadSpeedUnit {
        didSet { defaults.set(speedUnit.rawValue, forKey: Self.speedUnitKey) }
    }
    /// The most every storefront may download in all, in bytes per second;
    /// zero leaves it unlimited.
    var speedLimit: Int {
        didSet {
            defaults.set(speedLimit, forKey: Self.speedLimitKey)
            backend.setSpeedLimit(UInt64(speedLimit))
        }
    }

    /// The running Transfer's progress, as its storefront's installations show it.
    @ObservationIgnored var progress: @MainActor (DownloadItem) -> GameTransfer? = { _ in nil }

    /// How many samples the speed graph keeps.
    static let sampleLimit = 120
    static let speedUnitKey = "obelisk.downloads.speedUnit"
    static let speedLimitKey = "obelisk.downloads.speedLimit"

    @ObservationIgnored private let backend: any DownloadsBackend
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private let period: Duration
    @ObservationIgnored private var revision: UInt64?
    @ObservationIgnored private var observation: Task<Void, Never>?
    @ObservationIgnored private var sampling: Task<Void, Never>?
    /// The running Transfer's bytes when last sampled.
    @ObservationIgnored private var lastSample: (time: Date, bytes: UInt64)?
    @ObservationIgnored private var nextSample = 0

    init(backend: any DownloadsBackend = RustDownloads(), period: Duration = .seconds(1),
         defaults: UserDefaults = .standard) {
        self.backend = backend
        self.period = period
        self.defaults = defaults
        speedUnit = defaults.string(forKey: Self.speedUnitKey).flatMap(DownloadSpeedUnit.init) ?? .bytes
        speedLimit = max(0, defaults.integer(forKey: Self.speedLimitKey))
        backend.setSpeedLimit(UInt64(speedLimit))
        apply(backend.state())
        observation = Task { [weak self, backend] in
            var after = self?.revision ?? 0
            while !Task.isCancelled {
                let state = await backend.changed(after: after)
                guard let self else { return }
                after = state.revision
                apply(state)
            }
        }
    }

    /// Where the limit stands among the unit's limits: the nearest, since the
    /// other unit may have offered one between two. Changing the unit leaves
    /// the limit as it is until the player picks another.
    var speedLimitChoice: Int {
        get {
            let limits = speedUnit.speedLimits
            guard speedLimit != 0 else { return limits.count - 1 }
            return limits.dropLast().indices.min { abs(limits[$0] - speedLimit) < abs(limits[$1] - speedLimit) } ?? 0
        }
        set { speedLimit = speedUnit.speedLimits[newValue] }
    }

    /// Whether anything runs or waits: the sidebar shows the queue then.
    var isVisible: Bool { running != nil || !waiting.isEmpty }

    var count: Int { waiting.count + (running == nil ? 0 : 1) }

    var currentTransfer: GameTransfer? { running.flatMap(progress) }

    /// The latest speed, once two samples are known.
    var currentSpeed: Double? { samples.last?.bytesPerSecond }

    var peakSpeed: Double { samples.map(\.bytesPerSecond).max() ?? 0 }

    /// Only automatic updates wait, for the game that runs.
    var waitsForGame: Bool { held && running == nil && !waiting.isEmpty && waiting.allSatisfy(\.automatic) }

    func remove(_ item: DownloadItem) { backend.remove(item.id) }
    func pause(_ item: DownloadItem) { backend.pause(item.id) }
    func resume(_ item: DownloadItem) { backend.resume(item.id) }
    func moveToTop(_ item: DownloadItem) { backend.moveToTop(item.id) }

    private func apply(_ state: DownloadQueueState) {
        if let revision, state.revision <= revision { return }
        revision = state.revision
        if running?.id != state.running?.id {
            samples = []
            lastSample = nil
        }
        if running != state.running { running = state.running }
        if waiting != state.waiting { waiting = state.waiting }
        if held != state.held { held = state.held }
        if running == nil {
            sampling?.cancel()
            sampling = nil
        } else if sampling == nil {
            sampling = Task { [weak self, period] in
                // Ends with the model, or once nothing runs.
                repeat {
                    try? await Task.sleep(for: period)
                } while !Task.isCancelled && self?.sampleRunning() == true
            }
        }
    }

    private func sampleRunning() -> Bool {
        sample()
        return running != nil
    }

    /// Adds the speed since the last sample of the running Transfer.
    func sample(at now: Date = Date()) {
        guard let running, let transfer = progress(running) else { return }
        defer { lastSample = (now, transfer.networkBytes) }
        guard let last = lastSample else { return }
        let elapsed = now.timeIntervalSince(last.time)
        guard elapsed > 0 else { return }
        let bytes = transfer.networkBytes >= last.bytes ? transfer.networkBytes - last.bytes : 0
        nextSample += 1
        samples.append(DownloadSpeedSample(id: nextSample, time: now, bytesPerSecond: Double(bytes) / elapsed))
        if samples.count > Self.sampleLimit { samples.removeFirst(samples.count - Self.sampleLimit) }
    }
}
