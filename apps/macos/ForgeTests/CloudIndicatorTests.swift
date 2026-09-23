import Foundation
import XCTest
@testable import Forge

final class CloudIndicatorTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_790_000_000)

    private func saved(_ state: CloudSavedState, syncedAt: UInt64? = nil, gameSession: Bool = false,
                       pendingUpload: Bool = false) -> CloudIndicator {
        CloudIndicator(state: state, gameSession: gameSession, syncedAt: syncedAt,
                       pendingUpload: pendingUpload, installationRemoved: false)
    }

    private func shown(_ indicator: CloudIndicator?, syncing: Bool = false, playing: Bool = false,
                       installed: Bool = true) -> CloudIndicatorPresentation? {
        CloudIndicatorPresentation.make(saved: indicator, isSyncing: syncing, isPlaying: playing,
                                        isInstalled: installed, now: now)
    }

    func testEachSavedStateHasItsSymbolTextAndTone() {
        let twoHoursAgo = UInt64(now.timeIntervalSince1970) - 7_200
        let lastSync = CloudSyncDate.text(Date(timeIntervalSince1970: TimeInterval(twoHoursAgo)), now: now)
        let pending = L10n.string("ui.cloud.indicator.pending")
        let cases: [(CloudIndicator, String?, String, CloudIndicatorPresentation.Tone)] = [
            (saved(.recoveryNeeded), "exclamationmark.icloud", pending, .warning),
            (saved(.uploadUncertain), "exclamationmark.icloud", pending, .warning),
            (saved(.conflict), "exclamationmark.icloud", L10n.string("ui.cloud.indicator.conflict"), .warning),
            (saved(.pending, syncedAt: twoHoursAgo), "exclamationmark.icloud",
             L10n.string("ui.cloud.indicator.pending_since", lastSync), .warning),
            (saved(.pending), "exclamationmark.icloud", pending, .warning),
            (saved(.unavailable), "icloud.slash", L10n.string("ui.cloud.indicator.unavailable"), .normal),
            (saved(.localChanges), "exclamationmark.icloud", pending, .warning),
            (saved(.lastFailed), "exclamationmark.icloud", L10n.string("ui.cloud.indicator.failed"), .warning),
            (saved(.synced, syncedAt: twoHoursAgo), "icloud",
             L10n.string("ui.cloud.indicator.synced_at", lastSync), .normal),
            (saved(.synced), "icloud", L10n.string("ui.cloud.indicator.synced"), .normal),
            (saved(.neverSynced), "icloud", L10n.string("ui.cloud.indicator.never"), .muted),
        ]
        for (index, (indicator, symbol, value, tone)) in cases.enumerated() {
            let presentation = shown(indicator)
            XCTAssertEqual(presentation?.symbol, symbol, "case \(index)")
            XCTAssertEqual(presentation?.value, value, "case \(index)")
            XCTAssertEqual(presentation?.tone, tone, "case \(index)")
        }
        XCTAssertEqual(shown(nil)?.value, L10n.string("ui.cloud.indicator.never"), "No journal yet means never synced")
    }

    func testLiveStatesComeFirst() {
        let synced = saved(.synced, syncedAt: UInt64(now.timeIntervalSince1970) - 30, gameSession: true)
        let syncing = shown(synced, syncing: true, playing: true)
        XCTAssertNil(syncing?.symbol, "A sync in progress shows progress")
        XCTAssertEqual(syncing?.value, L10n.string("ui.cloud.indicator.syncing"))

        let playing = shown(synced, playing: true)
        XCTAssertEqual(playing?.symbol, "cloud")
        XCTAssertEqual(playing?.value, L10n.string("ui.cloud.date.just_now"))
        XCTAssertEqual(playing?.tone, .normal)

        let stale = shown(synced)
        XCTAssertEqual(stale?.value, L10n.string("ui.cloud.indicator.pending"), "A guard with no live game is stale")
        XCTAssertEqual(stale?.tone, .warning)

        let withoutGuard = shown(saved(.unavailable), playing: true)
        XCTAssertEqual(withoutGuard?.value, L10n.string("ui.cloud.indicator.unavailable"))
    }

    func testUninstalledGamesShowTheIndicatorOnlyWhileAnUploadIsPending() {
        XCTAssertNil(shown(saved(.synced), installed: false))
        XCTAssertNil(shown(nil, installed: false))
        XCTAssertEqual(shown(saved(.pending, pendingUpload: true), installed: false)?.tone, .warning)
        XCTAssertNotNil(shown(saved(.neverSynced), installed: true))
    }

    func testLastSyncDatesAreRelativeForAWeekThenAbsolute() {
        let french = Locale(identifier: "fr_FR")
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        func text(_ offset: TimeInterval) -> String {
            CloudSyncDate.text(now.addingTimeInterval(offset), now: now, locale: french, calendar: calendar)
        }
        // `now` is 21 September 2026, 14:13 UTC.
        XCTAssertEqual(text(-20), L10n.string("ui.cloud.date.just_now"))
        XCTAssertEqual(text(5), L10n.string("ui.cloud.date.just_now"), "A slightly ahead clock is not a future sync")
        XCTAssertEqual(text(-7_200), "il y a 2\u{00A0}h", "French puts a no-break space before the unit")
        XCTAssertEqual(text(-6 * 86_400), "il y a 6\u{00A0}j")
        XCTAssertEqual(text(-10 * 86_400), "11 sept.")
        XCTAssertEqual(text(-400 * 86_400), "17 août 2025")
    }
}
