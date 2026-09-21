import AppKit
import SwiftUI
import XCTest
@testable import Forge

final class SteamAchievementsTests: XCTestCase {
    @MainActor
    func testResetAndReplacementDiscardLateAccountOrGameResponses() async throws {
        let old = AchievementResponse()
        let model = GameAchievementsModel(storeID: "steam", describe: SteamModel.message)
        model.load { await old.get() }
        await old.waitUntilRequested()
        model.reset()
        model.load { [Self.achievement("new", unlocked: true)] }
        try await eventually { model.items != nil }
        await old.resolve([Self.achievement("old", unlocked: false)])
        await Task.yield()
        XCTAssertEqual(model.items?.map(\.id), ["new"])
        XCTAssertEqual(model.countText, "1/1")
        XCTAssertEqual(model.fraction, 1)
        model.isPresented = true
        model.reset()
        XCTAssertNil(model.items)
        XCTAssertNil(model.fraction)
        XCTAssertFalse(model.isPresented)
    }

    @MainActor
    func testUnavailableIsDistinctFromZeroAchievementsAndRetryLoadsProgress() async throws {
        let model = GameAchievementsModel(storeID: "steam", describe: SteamModel.message)
        model.load { throw CocoaError(.fileReadNoPermission) }
        try await eventually { !model.isLoading }
        XCTAssertNotNil(model.error)
        XCTAssertNil(model.items)
        XCTAssertNil(model.fraction)
        XCTAssertEqual(model.countText, "—")
        model.load { [] }
        try await eventually { !model.isLoading }
        XCTAssertNil(model.error)
        XCTAssertEqual(model.countText, "0/0")
        XCTAssertNil(model.fraction)
        model.load { [Self.achievement("yes", unlocked: true), Self.achievement("no", unlocked: false)] }
        try await eventually { !model.isLoading }
        XCTAssertEqual(model.countText, "1/2")
        XCTAssertEqual(model.fraction, 0.5)
    }

    func testEpicUnlockDatesAcceptFractionalSecondsAndRejectInvalidDates() {
        func converted(_ date: String?, unlocked: Bool = true) -> GameAchievement {
            GameAchievement(EpicAchievement(id: "test", name: "Title", description: "", iconUrl: "",
                hidden: false, unlocked: unlocked, unlockDate: date))
        }
        let expected = Date(timeIntervalSince1970: 1_663_761_600)
        XCTAssertEqual(converted("2022-09-21T12:00:00Z").unlockDate, expected)
        XCTAssertEqual(converted("2022-09-21T12:00:00.000Z").unlockDate, expected)
        XCTAssertNil(converted("not-a-date").unlockDate)
        XCTAssertNil(converted(nil).unlockDate)
        XCTAssertNil(converted("2022-09-21T12:00:00Z", unlocked: false).unlockDate)
    }

    @MainActor
    func testRenderAchievementHeaderAndPanel() async throws {
        guard let path = ProcessInfo.processInfo.environment["FORGE_RENDER_PREVIEWS"] else {
            throw XCTSkip("Set FORGE_RENDER_PREVIEWS to render achievement fixtures")
        }
        let output = URL(filePath: path)
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        let model = GameAchievementsModel(storeID: "steam", describe: SteamModel.message)
        model.load { (0..<52).map { Self.achievement("\($0)", unlocked: $0 < 13) } }
        try await eventually { !model.isLoading }
        for scheme in [ColorScheme.dark, .light] {
            for width in [CGFloat(860), CGFloat(620)] {
                let view = GameDetailHeader(title: "Thronefall", libraryTitle: "Bibliothèque Steam",
                    playtime: "27,2 heures", lastPlayed: "Aujourd’hui", size: "1,2 Go",
                    back: {}, additionalMetrics: { GameAchievementsButton(model: model) },
                    artwork: { LinearGradient(colors: [.orange.opacity(0.6), .brown], startPoint: .topLeading, endPoint: .bottomTrailing) },
                    action: { Button("Lancer", systemImage: "play.fill") {}.buttonStyle(ForgePrimaryButtonStyle(scale: 1.25)) })
                    .padding(24).frame(width: width).environment(\.colorScheme, scheme)
                    .tint(ForgeStyle.accent).background(ForgePalette(colorScheme: scheme).background)
                try await render(view, to: output.appending(path: "achievements-\(scheme)-\(Int(width)).png"))
            }
        }
        try await render(GameAchievementsView(model: model, gameName: "Thronefall", retry: {})
            .environment(\.colorScheme, .dark).tint(ForgeStyle.accent)
            .background(ForgePalette(colorScheme: .dark).background), to: output.appending(path: "achievements-panel.png"))
    }

    private static func achievement(_ id: String, unlocked: Bool) -> GameAchievement {
        GameAchievement(id: id, name: unlocked ? "Une première victoire" : "Un nouveau défi", description: "Terminer une partie et défendre le royaume.", iconUrl: "", hidden: false, unlocked: unlocked, unlockDate: unlocked ? Date(timeIntervalSince1970: 1_790_000_000) : nil)
    }
    @MainActor
    private func eventually(_ condition: () -> Bool) async throws {
        for _ in 0..<200 {
            if condition() { return }
            try await Task.sleep(for: .milliseconds(5))
        }
        XCTFail("Achievement request did not complete")
    }
    @MainActor
    private func render(_ view: some View, to url: URL) async throws {
        let host = NSHostingView(rootView: view)
        let size = host.fittingSize
        let rect = NSRect(x: -20_000, y: -20_000, width: size.width, height: size.height)
        let window = NSWindow(contentRect: rect, styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.sharingType = .none
        window.contentView = host
        window.setFrameOrigin(rect.origin)
        window.orderBack(nil)
        defer { window.close() }
        host.layoutSubtreeIfNeeded()
        try await Task.sleep(for: .milliseconds(200))
        host.layoutSubtreeIfNeeded()
        window.displayIfNeeded()
        let bitmap = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: bitmap)
        try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).write(to: url)
    }

}

private actor AchievementResponse {
    private var continuation: CheckedContinuation<[GameAchievement], Never>?
    func get() async -> [GameAchievement] { await withCheckedContinuation { continuation = $0 } }
    func waitUntilRequested() async {
        while continuation == nil { await Task.yield() }
    }
    func resolve(_ value: [GameAchievement]) { continuation?.resume(returning: value); continuation = nil }
}
