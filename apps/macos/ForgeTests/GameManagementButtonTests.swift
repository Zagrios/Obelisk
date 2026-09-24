import AppKit
import SwiftUI
import XCTest
@testable import Forge

final class GameManagementButtonTests: XCTestCase {
    @MainActor
    func testManagementControlKeepsASquareFootprint() async {
        let fixture = ManagementButtonWindow(
            GameManagementButton(gameName: "Portal 2", isAvailable: true, uninstall: {}, directory: "/Applications")
        )
        defer { fixture.close() }
        await fixture.prepare()

        XCTAssertEqual(fixture.host.fittingSize.width, 44, accuracy: 0.5)
        XCTAssertEqual(fixture.host.fittingSize.height, 44, accuracy: 0.5)
    }

    @MainActor
    func testManagementAccessibilityNamesTheGameAndReflectsAvailability() async throws {
        for isAvailable in [true, false] {
            let fixture = ManagementButtonWindow(
                GameManagementButton(gameName: "Portal 2", isAvailable: isAvailable, uninstall: {}, directory: "/Applications")
            )
            defer { fixture.close() }
            await fixture.prepare()

            guard let element = accessibilityElement(in: fixture.host, identifier: "game-management-menu") else {
                throw XCTSkip("This macOS host does not expose the SwiftUI menu through in-process accessibility")
            }
            let names = ["accessibilityLabel", "accessibilityTitle"].compactMap { attribute($0, of: element) as? String }
            XCTAssertTrue(names.contains(L10n.string("ui.game.manage_named", "Portal 2")))
            if let control = element as? NSControl {
                XCTAssertEqual(control.isEnabled, isAvailable)
            } else if let enabled = attribute("accessibilityEnabled", of: element) as? NSNumber {
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
                    title: L10n.string("model.game.launch"), symbol: "play.fill", transfer: nil,
                    isTransferring: false, isAvailable: true, action: {}, cancel: {}
                )
                GameManagementButton(gameName: "Portal 2", isAvailable: true, uninstall: {}, directory: "/Applications")
            }
            .padding(18)
            .environment(\.colorScheme, .dark)
            .background(ForgePalette(colorScheme: .dark).background)
        )
        defer { fixture.close() }
        await fixture.prepare()

        let bitmap = try XCTUnwrap(fixture.host.bitmapImageRepForCachingDisplay(in: fixture.host.bounds))
        fixture.host.cacheDisplay(in: fixture.host.bounds, to: bitmap)
        let png = try XCTUnwrap(bitmap.representation(using: .png, properties: [:]))
        try png.write(to: URL(fileURLWithPath: "/tmp/forge-game-management-preview.png"))
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
