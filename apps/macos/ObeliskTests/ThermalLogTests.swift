import Foundation
import XCTest
@testable import Obelisk

/// What a thermal log wrote, in order.
@MainActor
final class ThermalLines {
    var values: [String] = []
}

@MainActor
final class ThermalLogTests: XCTestCase {
    private let center = NotificationCenter()
    private let lines = ThermalLines()
    private var state = ThermalSnapshot(thermalState: .nominal, lowPowerMode: false, onBattery: false)

    private func makeLog(heartbeat: Duration = .seconds(300)) -> ThermalLog {
        ThermalLog(snapshot: { [unowned self] in state }, write: { [lines] in lines.values.append($0) },
                   center: center, heartbeat: heartbeat)
    }

    func testASnapshotNamesEachThermalStateAndWhereThePowerComesFrom() {
        let states: [(ProcessInfo.ThermalState, String)] = [
            (.nominal, "nominal"), (.fair, "fair"), (.serious, "serious"), (.critical, "critical"),
        ]
        for (state, name) in states {
            XCTAssertEqual(ThermalSnapshot(thermalState: state, lowPowerMode: false, onBattery: false).text,
                           "thermal=\(name) low_power_mode=false power=ac")
        }
        XCTAssertEqual(ThermalSnapshot(thermalState: .serious, lowPowerMode: true, onBattery: true).text,
                       "thermal=serious low_power_mode=true power=battery")
        XCTAssertEqual(ThermalSnapshot(thermalState: .fair, lowPowerMode: false, onBattery: nil).text,
                       "thermal=fair low_power_mode=false power=unknown")
    }

    func testTheLiveSnapshotReadsThisMac() {
        let text = ThermalSnapshot.current.text
        XCTAssertTrue(text.hasPrefix("thermal="), text)
        XCTAssertTrue(text.contains(" low_power_mode="), text)
        XCTAssertFalse(text.hasSuffix("power=unknown"), "macOS tells where this Mac draws its power from")
    }

    func testStartNotesTheStateAndEachChangeMacOSAnnounces() async throws {
        // The observers hold the log weakly: the test keeps it.
        let log = makeLog()
        defer { withExtendedLifetime(log) {} }
        log.start()
        XCTAssertEqual(lines.values, ["Obelisk started: thermal=nominal low_power_mode=false power=ac"])

        state = ThermalSnapshot(thermalState: .serious, lowPowerMode: false, onBattery: false)
        center.post(name: ProcessInfo.thermalStateDidChangeNotification, object: nil)
        try await eventually { lines.values.count == 2 }
        XCTAssertEqual(lines.values.last, "changed: thermal=serious low_power_mode=false power=ac")

        state = ThermalSnapshot(thermalState: .serious, lowPowerMode: true, onBattery: true)
        center.post(name: .NSProcessInfoPowerStateDidChange, object: nil)
        try await eventually { lines.values.count == 3 }
        XCTAssertEqual(lines.values.last, "changed: thermal=serious low_power_mode=true power=battery")
    }

    func testAGameThatRunsIsNotedWhenItStartsAndExitsAndWhileItRuns() async throws {
        let log = makeLog(heartbeat: .milliseconds(20))
        log.follow(running: false)
        XCTAssertTrue(lines.values.isEmpty, "no game ran")

        log.follow(running: true)
        log.follow(running: true)
        try await eventually { lines.values.filter { $0.hasPrefix("game running") }.count >= 2 }
        log.follow(running: false)
        log.follow(running: false)
        XCTAssertEqual(lines.values.first, "game started: thermal=nominal low_power_mode=false power=ac")
        XCTAssertEqual(lines.values.filter { $0.hasPrefix("game started") }.count, 1)
        XCTAssertEqual(lines.values.last, "game ended: thermal=nominal low_power_mode=false power=ac")
        XCTAssertEqual(lines.values.filter { $0.hasPrefix("game ended") }.count, 1)

        let count = lines.values.count
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(lines.values.count, count, "a game that exited is no longer noted")
    }
}
