import AppKit
import SwiftUI
import XCTest
@testable import SailOver

final class StorefrontSortPopoverTests: XCTestCase {
    @MainActor
    func testClickAppliesSortBeforeDismissingInTheSameEvent() async throws {
        var selected = "recent"
        var events: [String] = []
        let option = StorefrontSortOption(id: "name", title: SteamLibrarySort.name.title, isSelected: false) {
            selected = "name"
            events.append("selected")
        }
        let fixture = SortPopoverFixture(options: [option]) {
            events.append("dismissed:\(selected)")
        }
        defer { fixture.close() }
        try await fixture.prepare()

        // With one row, the fitted content's center is inside the actual button.
        // Send input to the mounted view instead of calling the supplied action.
        fixture.clickCenter()

        XCTAssertEqual(selected, "name")
        XCTAssertEqual(events, ["selected", "dismissed:name"], "Selection and dismissal must finish in the input handler, in that order")
    }

    @MainActor
    func testEscapeDismissesWithoutChangingTheSort() async throws {
        var selected = "recent"
        var events: [String] = []
        let options = ["recent", "name", "playtime"].map { id in
            StorefrontSortOption(id: id, title: id, isSelected: id == selected) {
                selected = id
                events.append("selected:\(id)")
            }
        }
        let fixture = SortPopoverFixture(options: options) { events.append("dismissed") }
        defer { fixture.close() }
        try await fixture.prepare()

        fixture.press(keyCode: 53, characters: "\u{1b}")

        XCTAssertEqual(selected, "recent")
        XCTAssertEqual(events, ["dismissed"])
    }

    @MainActor
    func testArrowKeysThenReturnApplyTheFocusedOption() async throws {
        var selected = "recent"
        var events: [String] = []
        let options = ["recent", "name", "playtime"].map { id in
            StorefrontSortOption(id: id, title: id, isSelected: id == selected) {
                selected = id
                events.append("selected:\(id)")
            }
        }
        let fixture = SortPopoverFixture(options: options) { events.append("dismissed:\(selected)") }
        defer { fixture.close() }
        try await fixture.prepare()

        fixture.press(keyCode: 125, characters: "\u{f701}")
        await fixture.settleUI()
        fixture.press(keyCode: 125, characters: "\u{f701}")
        await fixture.settleUI()
        fixture.press(keyCode: 126, characters: "\u{f700}")
        await fixture.settleUI()
        XCTAssertEqual(selected, "recent", "Moving focus must not apply a sort")
        XCTAssertTrue(events.isEmpty)
        fixture.press(keyCode: 36, characters: "\r")

        XCTAssertEqual(selected, "name")
        XCTAssertEqual(events, ["selected:name", "dismissed:name"])
    }
}

@MainActor
private final class SortPopoverFixture {
    let host: NSView
    let window: SortPopoverWindow
    private let mountState: SortPopoverMountState

    init(options: [StorefrontSortOption], dismiss: @escaping @MainActor () -> Void) {
        let mountState = SortPopoverMountState()
        self.mountState = mountState
        host = NSHostingView(rootView: StorefrontSortPopover(options: options, dismiss: dismiss)
            .onAppear { mountState.appeared = true })
        window = SortPopoverWindow(contentRect: NSRect(x: -20_000, y: -20_000, width: 260, height: 180), styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.sharingType = .none
        window.contentView = host
        window.makeKeyAndOrderFront(nil)
    }

    func prepare() async throws {
        // Let onAppear establish the selected row's real SwiftUI focus target.
        // No sleep or deadline is used to judge input latency.
        let deadline = ContinuousClock.now + .seconds(3)
        repeat {
            host.layoutSubtreeIfNeeded()
            let size = host.fittingSize
            if size.width > 0 && size.height > 0 { window.setContentSize(size) }
            await settleUI()
            if mountState.appeared && window.firstResponder is NSView { return }
            try await Task.sleep(for: .milliseconds(5))
        } while ContinuousClock.now < deadline
        throw SortPopoverFixtureError.missingFocus
    }

    func settleUI() async {
        // Real key events arrive on separate AppKit turns. Give FocusState its
        // deferred update before delivering the next key in the fixture.
        for _ in 0..<2 {
            await withCheckedContinuation { continuation in
                DispatchQueue.main.async { continuation.resume() }
            }
            host.layoutSubtreeIfNeeded()
            window.displayIfNeeded()
        }
    }

    func clickCenter() {
        let location = host.convert(NSPoint(x: host.bounds.midX, y: host.bounds.midY), to: nil)
        let timestamp = ProcessInfo.processInfo.systemUptime
        let mouseDown = NSEvent.mouseEvent(with: .leftMouseDown, location: location, modifierFlags: [], timestamp: timestamp, windowNumber: window.windowNumber, context: nil, eventNumber: 9_123, clickCount: 1, pressure: 1)!
        let mouseUp = NSEvent.mouseEvent(with: .leftMouseUp, location: location, modifierFlags: [], timestamp: timestamp + 0.001, windowNumber: window.windowNumber, context: nil, eventNumber: 9_124, clickCount: 1, pressure: 0)!
        // A native control may track the mouse synchronously inside mouseDown.
        // Queue its release first, and deliver it ourselves if no tracking loop
        // consumed it. Both events stay inside this test application's queue.
        NSApp.postEvent(mouseUp, atStart: true)
        window.sendEvent(mouseDown)
        if let release = NSApp.nextEvent(matching: .leftMouseUp, until: Date(), inMode: .default, dequeue: true) {
            window.sendEvent(release)
        }
    }

    func press(keyCode: UInt16, characters: String) {
        for type in [NSEvent.EventType.keyDown, .keyUp] {
            let event = NSEvent.keyEvent(with: type, location: .zero, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil, characters: characters, charactersIgnoringModifiers: characters, isARepeat: false, keyCode: keyCode)!
            window.sendEvent(event)
        }
    }

    func close() { window.close() }
}

private final class SortPopoverWindow: NSWindow {
    override var canBecomeKey: Bool { true }
}

@MainActor
private final class SortPopoverMountState {
    var appeared = false
}

private enum SortPopoverFixtureError: Error { case missingFocus }
