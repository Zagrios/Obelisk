import Foundation
import IOKit.ps
import OSLog

/// How hot and how powered the Mac was at one moment, as a bug report's log
/// shows it: macOS's thermal state, Low Power Mode and where the Mac draws
/// its power.
struct ThermalSnapshot {
    let thermalState: ProcessInfo.ThermalState
    let lowPowerMode: Bool
    /// Nil when macOS does not say.
    let onBattery: Bool?

    /// As `thermal=serious low_power_mode=false power=battery`.
    var text: String {
        let state = switch thermalState {
        case .nominal: "nominal"
        case .fair: "fair"
        case .serious: "serious"
        case .critical: "critical"
        @unknown default: "unknown"
        }
        let power = onBattery.map { $0 ? "battery" : "ac" } ?? "unknown"
        return "thermal=\(state) low_power_mode=\(lowPowerMode) power=\(power)"
    }

    static var current: ThermalSnapshot {
        ThermalSnapshot(thermalState: ProcessInfo.processInfo.thermalState,
                        lowPowerMode: ProcessInfo.processInfo.isLowPowerModeEnabled,
                        onBattery: isOnBattery())
    }

    private static func isOnBattery() -> Bool? {
        guard let info = IOPSCopyPowerSourcesInfo()?.takeRetainedValue(),
              let source = IOPSGetProvidingPowerSourceType(info)?.takeUnretainedValue() else { return nil }
        return (source as String) == kIOPSBatteryPowerValue
    }
}

/// Writes the Mac's thermal and power state to the unified log, where a bug
/// report finds it: at launch, whenever macOS changes either, when a game
/// starts and exits, and every few minutes while one runs. A game that drops
/// frames on a Mac that throttles then shows the throttling beside it.
@MainActor
final class ThermalLog {
    static let shared = ThermalLog()
    private static let logger = Logger(subsystem: AppIdentity.storageIdentifier, category: "Thermal")

    private let snapshot: () -> ThermalSnapshot
    private let write: (String) -> Void
    private let center: NotificationCenter
    private let heartbeat: Duration
    /// Notes the running game; there is one while a game runs.
    private var beating: Task<Void, Never>?

    /// A game that runs is noted every `heartbeat`, so that the last hour of
    /// the log a report carries says how the Mac was even if nothing changed.
    init(snapshot: @escaping () -> ThermalSnapshot = { .current },
         write: @escaping (String) -> Void = { ThermalLog.logger.notice("\($0, privacy: .public)") },
         center: NotificationCenter = .default, heartbeat: Duration = .seconds(300)) {
        self.snapshot = snapshot
        self.write = write
        self.center = center
        self.heartbeat = heartbeat
    }

    /// Notes the state now and each time macOS changes it.
    func start() {
        note("Obelisk started")
        for name in [ProcessInfo.thermalStateDidChangeNotification, .NSProcessInfoPowerStateDidChange] {
            center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.note("changed") }
            }
        }
    }

    /// Notes when a game starts running and when none does, and in between.
    func follow(running: Bool) {
        guard running != (beating != nil) else { return }
        if running {
            note("game started")
            let interval = heartbeat
            beating = Task { [weak self] in
                while (try? await Task.sleep(for: interval)) != nil {
                    guard let self else { return }
                    note("game running")
                }
            }
        } else {
            beating?.cancel()
            beating = nil
            note("game ended")
        }
    }

    private func note(_ event: String) {
        write("\(event): \(snapshot().text)")
    }
}
