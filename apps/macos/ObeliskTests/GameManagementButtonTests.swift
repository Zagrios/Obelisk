import AppKit
import SwiftUI
import XCTest
@testable import Obelisk

final class GameManagementButtonTests: XCTestCase {
    @MainActor
    func testManagementControlKeepsASquareFootprint() async {
        let fixture = ManagementButtonWindow(
            GameManagementButton(gameName: "Portal 2", canVerify: true, canRemove: true, verify: {}, remove: {}, directory: "/Applications")
        )
        defer { fixture.close() }
        await fixture.prepare()

        XCTAssertEqual(fixture.host.fittingSize.width, 36, accuracy: 0.5)
        XCTAssertEqual(fixture.host.fittingSize.height, 36, accuracy: 0.5)
    }

    @MainActor
    func testManagementAccessibilityNamesTheGameAndReflectsAvailability() async throws {
        for isAvailable in [true, false] {
            let fixture = ManagementButtonWindow(
                GameManagementButton(gameName: "Portal 2", canVerify: isAvailable, canRemove: isAvailable, verify: {}, remove: {}, directory: "/Applications")
            )
            defer { fixture.close() }
            await fixture.prepare()

            guard let element = accessibilityElement(in: fixture.host, identifier: "game-management-menu") else {
                throw XCTSkip("This macOS host does not expose the SwiftUI menu through in-process accessibility")
            }
            // A SwiftUI menu names itself in its title, as an attributed string.
            let names = ["accessibilityLabel", "accessibilityTitle"].compactMap { name in
                let value = attribute(name, of: element)
                return value as? String ?? (value as? NSAttributedString)?.string
            }
            XCTAssertTrue(names.contains(L10n.string("ui.game.manage_named", "Portal 2")))
            if let control = element as? NSControl {
                XCTAssertEqual(control.isEnabled, isAvailable)
            } else if element.responds(to: NSSelectorFromString("isAccessibilityEnabled")),
                      let enabled = element.value(forKey: "accessibilityEnabled") as? NSNumber {
                // The getter is isAccessibilityEnabled: the key finds it, the plain selector does not.
                XCTAssertEqual(enabled.boolValue, isAvailable)
            } else {
                throw XCTSkip("The SwiftUI menu does not expose its enabled state through in-process accessibility")
            }
        }
    }

    @MainActor
    func testRenderManagementControlBesideLaunch() async throws {
        let fixture = ManagementButtonWindow(
            HStack(spacing: 10) {
                GameActionButton(
                    action: GameAction(kind: .launch, title: L10n.string("model.game.launch"), isAvailable: true,
                                       launches: true),
                    gameName: "Portal 2"
                )
                GameManagementButton(gameName: "Portal 2", canVerify: true, canRemove: true, verify: {}, remove: {}, directory: "/Applications")
            }
            .padding(18)
            .environment(\.colorScheme, .dark)
            .background(ObeliskPalette(colorScheme: .dark).background)
        )
        defer { fixture.close() }
        await fixture.prepare()

        let bitmap = try XCTUnwrap(fixture.host.bitmapImageRepForCachingDisplay(in: fixture.host.bounds))
        fixture.host.cacheDisplay(in: fixture.host.bounds, to: bitmap)
        let png = try XCTUnwrap(bitmap.representation(using: .png, properties: [:]))
        try png.write(to: URL(fileURLWithPath: "/tmp/obelisk-game-management-preview.png"))
    }

    @MainActor
    func testTheLaunchRowShowsTheGameSNote() async throws {
        let metrics = GameMetrics(isInstalled: true, size: "33,4 Go", lastPlayed: nil, playtime: nil,
                                  achievements: GameAchievementsModel(storeID: "steam", describe: SteamModel.message))
        func row(_ note: String?) -> some View {
            GameLaunchRow(metrics: metrics, note: note) {
                GameActionButton(
                    action: GameAction(kind: .launch, title: L10n.string("model.game.launch"), isAvailable: true,
                                       launches: true),
                    gameName: "Team Fortress 2"
                )
            }
            .padding(18)
            .frame(width: 760)
            .environment(\.colorScheme, .dark)
            .background(ObeliskPalette(colorScheme: .dark).background)
        }
        let plain = ManagementButtonWindow(row(nil))
        defer { plain.close() }
        await plain.prepare()
        let noted = ManagementButtonWindow(row("Plays offline and on LAN servers that allow insecure players."))
        defer { noted.close() }
        await noted.prepare()

        // The note adds its caption and a line under the metrics.
        XCTAssertGreaterThan(noted.host.fittingSize.height, plain.host.fittingSize.height + 20)
    }

    @MainActor
    private func attribute(_ name: String, of element: NSObject) -> Any? {
        element.responds(to: NSSelectorFromString(name)) ? element.value(forKey: name) : nil
    }

    @MainActor
    private func accessibilityElement(in root: Any, identifier: String, depth: Int = 0) -> NSObject? {
        guard depth < 30, let element = root as? NSObject else { return nil }
        if attribute("accessibilityIdentifier", of: element) as? String == identifier { return element }
        let children = (attribute("accessibilityChildren", of: element) as? [Any] ?? [])
            + (attribute("accessibilityChildrenInNavigationOrder", of: element) as? [Any] ?? [])
            + ((root as? NSView)?.subviews ?? [])
        for child in children {
            if let match = accessibilityElement(in: child, identifier: identifier, depth: depth + 1) { return match }
        }
        return nil
    }
}

@MainActor
private final class ManagementButtonWindow {
    let host: NSView
    private let window: NSWindow

    init<Content: View>(_ content: Content) {
        host = NSHostingView(rootView: content)
        let rect = NSRect(x: -20_000, y: -20_000, width: 300, height: 80)
        window = NSWindow(contentRect: rect, styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.sharingType = .none
        window.contentView = host
        window.setFrameOrigin(rect.origin)
        window.orderBack(nil)
    }

    func prepare() async {
        for _ in 0..<3 {
            await withCheckedContinuation { continuation in
                DispatchQueue.main.async { continuation.resume() }
            }
            host.layoutSubtreeIfNeeded()
            let size = host.fittingSize
            if size.width > 0 && size.height > 0 { window.setContentSize(size) }
            window.displayIfNeeded()
        }
    }

    func close() { window.close() }
}
