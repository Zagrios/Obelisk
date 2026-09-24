import AppKit
import Network
import SwiftUI
import XCTest
@testable import Forge

final class EpicPresentationTests: XCTestCase {
    @MainActor
    func testLoadedArtworkSurvivesLibraryDetailAndReturnWithoutAnotherRequest() async throws {
        let bitmap = try XCTUnwrap(NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: 24, pixelsHigh: 24, bitsPerSample: 8,
            samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
            colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
        ))
        let artworkColor = NSColor(deviceRed: 0.1, green: 0.95, blue: 0.3, alpha: 1)
        for y in 0..<24 {
            for x in 0..<24 { bitmap.setColor(artworkColor, atX: x, y: y) }
        }
        let image = try XCTUnwrap(bitmap.representation(using: .png, properties: [:]))
        let server = try EpicArtworkHTTPServer(image: image)
        let url = try await server.start()
        defer { server.stop() }
        let game = EpicGame(
            appName: "cached-artwork", namespace: "presentation", catalogItemId: "cached-artwork",
            title: "Cached artwork", description: "", imageUrl: url.absoluteString,
            headerImageUrl: url.absoluteString, developer: "", windows: false, macos: false
        )
        let session = EpicPresentationSession(games: [game])
        let model = EpicModel(keychain: EpicPresentationCredentials(), restore: { _ in session })
        await model.start()
        let fixture = EpicPresentationWindow(EpicView(model: model), size: NSSize(width: 900, height: 700))
        defer { fixture.close() }
        var navigationPhase = "initial library"
        defer {
            XCTContext.runActivity(named: "Epic artwork navigation") { activity in
                activity.add(XCTAttachment(string: "phase=\(navigationPhase), imageRequests=\(server.requestCount), selectedGame=\(String(describing: model.selectedGame?.title))"))
                if let rendered = fixture.host.bitmapImageRepForCachingDisplay(in: fixture.host.bounds) {
                    fixture.host.cacheDisplay(in: fixture.host.bounds, to: rendered)
                    let image = NSImage(size: fixture.host.bounds.size)
                    image.addRepresentation(rendered)
                    activity.add(XCTAttachment(image: image))
                }
            }
        }

        func artworkIsRendered() -> Bool {
            fixture.layout()
            guard let rendered = fixture.host.bitmapImageRepForCachingDisplay(in: fixture.host.bounds) else { return false }
            fixture.host.cacheDisplay(in: fixture.host.bounds, to: rendered)
            var artworkPixels = 0
            for y in stride(from: 0, to: rendered.pixelsHigh, by: 8) {
                for x in stride(from: 0, to: rendered.pixelsWide, by: 8) {
                    guard let color = rendered.colorAt(x: x, y: y)?.usingColorSpace(.deviceRGB) else { continue }
                    if color.greenComponent > 0.75
                        && color.greenComponent - color.redComponent > 0.2
                        && color.greenComponent - color.blueComponent > 0.3 {
                        artworkPixels += 1
                    }
                }
            }
            return artworkPixels > 50
        }

        // Wait for the actual decoded image, so pending or cancelled downloads
        // cannot masquerade as a loaded library cover.
        try await eventually {
            fixture.layout()
            return self.searchFrame(in: fixture.host) != nil && artworkIsRendered()
        }
        let libraryRequests = server.requestCount
        XCTAssertEqual(libraryRequests, 1)

        model.open(game)
        navigationPhase = "detail"
        try await eventually {
            fixture.layout()
            return self.searchFrame(in: fixture.host) == nil && artworkIsRendered()
        }
        let detailRequests = server.requestCount

        model.closeGame()
        navigationPhase = "returned library"
        try await eventually {
            fixture.layout()
            return self.searchFrame(in: fixture.host) != nil && artworkIsRendered()
        }
        let returnRequests = server.requestCount

        XCTAssertEqual(returnRequests, detailRequests, "Returning to Epic's library must reuse its already loaded cover")
        let gameCalls = await session.gameCalls
        XCTAssertEqual(gameCalls, 1, "Artwork navigation must not reload the Epic library")
    }

    @MainActor
    func testMountedDetailRendersArrivingPlaytimeAndSize() async throws {
        let details = EpicGameDetails(playtimeSeconds: 5400, installationBytes: 2_000_000_000)
        let session = EpicPresentationSession(games: epicPresentationLibrary, details: details, holdDetails: true)
        let model = EpicModel(keychain: EpicPresentationCredentials(), restore: { _ in session })
        await model.start()
        model.open(epicPresentationLibrary[0])
        let fixture = EpicPresentationWindow(EpicView(model: model), size: NSSize(width: 900, height: 700))
        defer { fixture.close() }
        try await eventually { await session.waitingForDetails }
        fixture.layout()
        try await Task.sleep(for: .milliseconds(50))
        fixture.layout()

        func renderedImage() throws -> Data {
            let bitmap = try XCTUnwrap(fixture.host.bitmapImageRepForCachingDisplay(in: fixture.host.bounds))
            fixture.host.cacheDisplay(in: fixture.host.bounds, to: bitmap)
            return try XCTUnwrap(bitmap.representation(using: .png, properties: [:]))
        }
        let pending = try renderedImage()
        await session.releaseDetails()
        try await eventually { !model.isLoadingGameDetails }
        try await eventually {
            fixture.layout()
            guard let rendered = try? renderedImage() else { return false }
            return rendered != pending
        }
        XCTAssertEqual(model.selectedGameDetails, details)

        // SwiftUI text can be absent from an offscreen host's accessibility tree.
        // The rendered bitmap above still checks that the mounted view updates.
        if accessibilityElement(in: fixture.host, label: L10n.string("ui.game.metric.playtime")) != nil {
            XCTAssertNotNil(accessibilityElement(in: fixture.host, label: EpicFormatting.playtime(try XCTUnwrap(details.playtimeSeconds))))
            XCTAssertNotNil(accessibilityElement(in: fixture.host, label: SteamFormatting.bytes(2_000_000_000)))
            XCTAssertNil(accessibilityElement(in: fixture.host, label: L10n.string("ui.game.metric.last_played")))
            XCTAssertNil(accessibilityElement(in: fixture.host, label: L10n.string("ui.game.news")))
        }
    }

    @MainActor
    func testMountedViewShowsRestoredLibraryWithoutSwitchingTabs() async throws {
        try await verifyLibraryArrival(restoring: true)
    }

    @MainActor
    func testMountedViewShowsNewlySignedInLibraryWithoutSwitchingTabs() async throws {
        try await verifyLibraryArrival(restoring: false)
    }

    @MainActor
    private func verifyLibraryArrival(restoring: Bool) async throws {
        let store = EpicPresentationCredentials(restoring ? epicPresentationCredentials : nil)
        let session = EpicPresentationSession(games: epicPresentationLibrary, holdGames: true)
        let model = EpicModel(
            keychain: store,
            authorize: { "fixture-browser-code" },
            signIn: { _ in session },
            restore: { _ in session }
        )
        let size = NSSize(width: 900, height: 700)
        // Mount before .task restores credentials, exactly as opening the Epic
        // tab does. Building the host after model.start() hides this regression.
        let fixture = EpicPresentationWindow(EpicView(model: model), size: size)
        defer { fixture.close() }
        fixture.layout()
        XCTAssertFalse(model.isConnected)

        if !restoring {
            try await eventually {
                let loads = await store.loads
                return loads == 1 && !model.isRestoring
            }
            fixture.layout()
            model.signIn()
        }
        try await eventually { await session.waitingForGames }
        fixture.layout()
        XCTAssertTrue(model.isConnected)
        XCTAssertTrue(model.isLoadingGames)
        XCTAssertTrue(model.games.isEmpty)
        await session.releaseGames()
        try await eventually { !model.isLoadingGames && model.games.count == epicPresentationLibrary.count }

        // This second host is a control for the reported workaround: mounting a
        // new tab after data arrived shows the grid even if the old host is stale.
        let remounted = EpicPresentationWindow(EpicView(model: model), size: size)
        defer { remounted.close() }
        try await eventually {
            remounted.layout()
            guard let scroll = self.firstScrollView(in: remounted.host), let document = scroll.documentView else { return false }
            return self.searchFrame(in: remounted.host) != nil
                && document.frame.height > scroll.contentView.bounds.height
        }
        let expectedHeight = try XCTUnwrap(firstScrollView(in: remounted.host)?.documentView).frame.height

        // Check AppKit output, not only the populated model. The pending login
        // and empty-library screens are both short and have no game-card grid.
        let deadline = ContinuousClock.now + .seconds(3)
        var libraryVisible = false
        repeat {
            fixture.layout()
            if let scroll = firstScrollView(in: fixture.host), let document = scroll.documentView {
                libraryVisible = searchFrame(in: fixture.host) != nil
                    && document.frame.height > scroll.contentView.bounds.height
                    && abs(document.frame.height - expectedHeight) < 1
            }
            if libraryVisible { break }
            try await Task.sleep(for: .milliseconds(5))
        } while ContinuousClock.now < deadline

        if !libraryVisible {
            XCTContext.runActivity(named: restoring ? "Stale view after Epic restoration" : "Stale view after Epic sign-in") { activity in
                let actualHeight = firstScrollView(in: fixture.host)?.documentView?.frame.height
                activity.add(XCTAttachment(string: "games=\(model.games.count), connected=\(model.isConnected), loading=\(model.isLoadingGames), mountedDocumentHeight=\(String(describing: actualHeight)), remountedDocumentHeight=\(expectedHeight), mountedSearch=\(String(describing: searchFrame(in: fixture.host)))"))
                if let bitmap = fixture.host.bitmapImageRepForCachingDisplay(in: fixture.host.bounds) {
                    fixture.host.cacheDisplay(in: fixture.host.bounds, to: bitmap)
                    let image = NSImage(size: fixture.host.bounds.size)
                    image.addRepresentation(bitmap)
                    let attachment = XCTAttachment(image: image)
                    attachment.name = restoring ? "epic-restoration-stale-host" : "epic-sign-in-stale-host"
                    attachment.lifetime = .keepAlways
                    activity.add(attachment)
                }
            }
        }
        XCTAssertTrue(libraryVisible, "The mounted Epic tab must render the arriving games without changing tabs or the search query")
        XCTAssertEqual(model.searchText, "")
        XCTAssertNil(model.selectedGame)
        XCTAssertNil(model.errorMessage)
        let gameCalls = await session.gameCalls
        XCTAssertEqual(gameCalls, 1)
    }

    @MainActor
    func testScrollbarAndContentGutterSurviveLibraryDetailAndReturn() async throws {
        for size in [NSSize(width: 900, height: 700), NSSize(width: 903, height: 728)] {
            let session = EpicPresentationSession(games: epicPresentationLibrary)
            let model = EpicModel(keychain: EpicPresentationCredentials(), restore: { _ in session })
            await model.start()
            let fixture = EpicPresentationWindow(EpicView(model: model), size: size)
            defer { fixture.close() }

            try await eventually {
                fixture.layout()
                guard let scroll = self.firstScrollView(in: fixture.host), let document = scroll.documentView else { return false }
                return document.frame.height > scroll.contentView.bounds.height
            }
            let originalScroll = try XCTUnwrap(firstScrollView(in: fixture.host))
            let originalScroller = try XCTUnwrap(originalScroll.verticalScroller)
            let viewportWidth = originalScroll.contentView.bounds.width
            let searchFrameBefore = try XCTUnwrap(searchFrame(in: fixture.host))

            // Compare against the same native container used by Steam, including
            // the custom thumb that remains visible when a document fits.
            let reference = EpicPresentationWindow(StorefrontScrollView { Color.clear.frame(height: 1) }, size: size)
            defer { reference.close() }
            try await eventually {
                reference.layout()
                return self.firstScrollView(in: reference.host)?.verticalScroller != nil
            }
            let referenceScroll = try XCTUnwrap(firstScrollView(in: reference.host))
            let referenceScroller = try XCTUnwrap(referenceScroll.verticalScroller)
            XCTAssertEqual(ObjectIdentifier(type(of: originalScroller)), ObjectIdentifier(type(of: referenceScroller)))
            XCTAssertEqual(viewportWidth, referenceScroll.contentView.bounds.width, accuracy: 0.5)

            func assertStable(_ phase: String) throws {
                let scroll = try XCTUnwrap(firstScrollView(in: fixture.host))
                let scroller = try XCTUnwrap(scroll.verticalScroller)
                XCTAssertTrue(scroll === originalScroll, "The native scroll view changed during \(phase)")
                XCTAssertTrue(scroller === originalScroller, "The persistent scrollbar changed during \(phase)")
                XCTAssertEqual(scroll.scrollerStyle, .legacy)
                XCTAssertFalse(scroll.autohidesScrollers)
                XCTAssertTrue(scroll.hasVerticalScroller)
                XCTAssertFalse(scroller.isHidden, "The scrollbar disappeared during \(phase)")
                XCTAssertGreaterThan(scroller.alphaValue, 0)
                XCTAssertEqual(scroll.contentView.bounds.width, viewportWidth, accuracy: 0.5, "The content gutter changed during \(phase)")
            }
            try assertStable("the full library")

            model.searchText = epicPresentationLibrary[0].title
            try await eventually {
                fixture.layout()
                guard let document = originalScroll.documentView else { return false }
                return document.frame.height <= originalScroll.contentView.bounds.height
            }
            try assertStable("the filtered library")
            let filteredSearch = try XCTUnwrap(searchFrame(in: fixture.host))
            // The clear button takes space at the field's right edge once text exists.
            XCTAssertEqual(filteredSearch.minX, searchFrameBefore.minX, accuracy: 0.5, "Filtering must not move the search field horizontally")
            let filteredHeight = try XCTUnwrap(originalScroll.documentView).frame.height

            model.open(epicPresentationLibrary[0])
            try await eventually {
                fixture.layout()
                guard let document = originalScroll.documentView else { return false }
                return abs(document.frame.height - filteredHeight) > 1
                    && document.frame.height <= originalScroll.contentView.bounds.height
            }
            try assertStable("the short game detail")
            XCTAssertFalse(originalScroller.isEnabled, "A visible scrollbar must not make a short page scrollable")

            model.closeGame()
            model.searchText = ""
            try await eventually {
                fixture.layout()
                guard let document = originalScroll.documentView else { return false }
                return model.selectedGame == nil && document.frame.height > originalScroll.contentView.bounds.height
            }
            try assertStable("returning to the full library")
            let returnedSearch = try XCTUnwrap(searchFrame(in: fixture.host))
            XCTAssertEqual(returnedSearch.maxX, searchFrameBefore.maxX, accuracy: 0.5)
            XCTAssertTrue(originalScroller.isEnabled)
            XCTAssertTrue(originalScroller.target === originalScroll)
            XCTAssertNotNil(originalScroller.action)
            let document = try XCTUnwrap(originalScroll.documentView)
            originalScroll.contentView.scroll(to: NSPoint(x: 0, y: (document.frame.height - originalScroll.contentView.bounds.height) / 2))
            originalScroll.reflectScrolledClipView(originalScroll.contentView)
            XCTAssertEqual(originalScroller.doubleValue, 0.5, accuracy: 0.02)
            let gameCalls = await session.gameCalls
            XCTAssertEqual(gameCalls, 1, "Opening a cached game and returning must not reload the library")
        }
    }

    @MainActor
    func testHeaderAndSearchStayFixedWhileLibraryCardsScroll() async throws {
        let session = EpicPresentationSession(games: epicPresentationLibrary)
        let model = EpicModel(keychain: EpicPresentationCredentials(), restore: { _ in session })
        await model.start()
        let fixture = EpicPresentationWindow(EpicView(model: model), size: NSSize(width: 900, height: 700))
        defer { fixture.close() }
        try await eventually {
            fixture.layout()
            guard let scroll = self.firstScrollView(in: fixture.host), let document = scroll.documentView else { return false }
            return document.frame.height > scroll.contentView.bounds.height
                && self.searchFrame(in: fixture.host) != nil
                && self.nativeViewFrame(in: fixture.host, identifier: "storefront-titlebar-drag-region") != nil
        }
        let scroll = try XCTUnwrap(firstScrollView(in: fixture.host))
        let scroller = try XCTUnwrap(scroll.verticalScroller)
        let document = try XCTUnwrap(scroll.documentView)
        let headerBefore = try XCTUnwrap(nativeViewFrame(in: fixture.host, identifier: "storefront-titlebar-drag-region"))
        let searchBefore = try XCTUnwrap(searchFrame(in: fixture.host))
        let contentBefore = document.convert(NSPoint.zero, to: fixture.host)
        let offsetBefore = scroll.contentView.bounds.origin.y
        let offset = min(CGFloat(180), document.frame.height - scroll.contentView.bounds.height)
        XCTAssertGreaterThan(offset, 100, "The fixture must contain enough cards to scroll behind the header")

        scroll.contentView.scroll(to: NSPoint(x: 0, y: offsetBefore + offset))
        scroll.reflectScrolledClipView(scroll.contentView)
        try await eventually {
            fixture.layout()
            return abs(scroll.contentView.bounds.origin.y - offsetBefore - offset) < 0.5
        }
        let headerAfter = try XCTUnwrap(nativeViewFrame(in: fixture.host, identifier: "storefront-titlebar-drag-region"))
        let searchAfter = try XCTUnwrap(searchFrame(in: fixture.host))
        let contentAfter = document.convert(NSPoint.zero, to: fixture.host)

        for (name, before, after) in [("Header drag region", headerBefore, headerAfter), ("Search field", searchBefore, searchAfter)] {
            XCTAssertEqual(after.minX, before.minX, accuracy: 0.5, "\(name) moved horizontally while scrolling")
            XCTAssertEqual(after.minY, before.minY, accuracy: 0.5, "\(name) scrolled with the game cards")
            XCTAssertEqual(after.width, before.width, accuracy: 0.5)
            XCTAssertEqual(after.height, before.height, accuracy: 0.5)
        }
        XCTAssertEqual(abs(contentAfter.y - contentBefore.y), offset, accuracy: 0.5, "The game-card document must move underneath the fixed controls")
        XCTAssertTrue(firstScrollView(in: fixture.host) === scroll)
        XCTAssertTrue(scroll.verticalScroller === scroller, "Scrolling must keep the same persistent scrollbar")
        XCTAssertFalse(scroller.isHidden)
        XCTAssertEqual(scroll.scrollerStyle, .legacy)
        XCTAssertFalse(scroll.autohidesScrollers)
        XCTAssertEqual(model.searchText, "")
        let gameCalls = await session.gameCalls
        XCTAssertEqual(gameCalls, 1)
    }

    @MainActor
    func testUnsupportedGameDisablesDownloadWhenAccessibilityExposesButton() async throws {
        let session = EpicPresentationSession(games: epicPresentationLibrary)
        let model = EpicModel(keychain: EpicPresentationCredentials(), restore: { _ in session })
        await model.start()
        model.open(epicPresentationLibrary[0])
        let fixture = EpicPresentationWindow(EpicView(model: model), size: NSSize(width: 900, height: 700))
        defer { fixture.close() }
        fixture.layout()
        try await Task.sleep(for: .milliseconds(100))
        fixture.layout()
        XCTAssertFalse(model.installations.primaryActionAvailable)
        guard let button = accessibilityElement(in: fixture.host, label: L10n.string("model.game.download")) else {
            // AppKit may omit SwiftUI virtual buttons from an offscreen host's
            // accessibility tree. Keep this explicit instead of claiming a click.
            throw XCTSkip("This macOS host does not expose the SwiftUI download button through accessibility")
        }
        let selected = model.selectedGame?.libraryID
        let account = model.accountName
        let calls = await session.gameCalls
        if let nativeButton = button as? NSButton {
            XCTAssertFalse(nativeButton.isEnabled)
        } else if let accessibleButton = button as? NSAccessibilityProtocol {
            XCTAssertFalse(accessibleButton.isAccessibilityEnabled())
        } else {
            throw XCTSkip("The virtual button does not provide the typed AppKit accessibility API")
        }
        await Task.yield()
        fixture.layout()
        let subsequentCalls = await session.gameCalls
        let disconnections = await session.disconnections
        XCTAssertEqual(model.selectedGame?.libraryID, selected)
        XCTAssertEqual(model.accountName, account)
        XCTAssertEqual(subsequentCalls, calls)
        XCTAssertEqual(disconnections, 0)
        XCTAssertTrue(model.isConnected)
        XCTAssertFalse(model.isLoadingGames)
        XCTAssertNil(model.errorMessage)
    }

    @MainActor
    func testRenderFixtureLibraryAndDetailWhenRequested() async throws {
        guard let destination = ProcessInfo.processInfo.environment["FORGE_RENDER_PREVIEWS"], !destination.isEmpty else {
            throw XCTSkip("Set FORGE_RENDER_PREVIEWS to render isolated Epic fixtures")
        }
        let session = EpicPresentationSession(games: epicPresentationLibrary)
        let model = EpicModel(keychain: EpicPresentationCredentials(), restore: { _ in session })
        await model.start()
        let output = URL(fileURLWithPath: destination, isDirectory: true)
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        try await render(model, to: output.appending(path: "epic-library-fixture.png"))
        try await render(model, to: output.appending(path: "epic-library-light-fixture.png"), colorScheme: .light)
        try await render(model, to: output.appending(path: "epic-library-scrolled-fixture.png"), scrollOffset: 180)
        try await render(model, to: output.appending(path: "epic-library-light-scrolled-fixture.png"), colorScheme: .light, scrollOffset: 180)
        model.open(epicPresentationLibrary[0])
        try await eventually { !model.isLoadingGameDetails && !model.achievements.isLoading }
        try await render(model, to: output.appending(path: "epic-detail-fixture.png"))

        let game = EpicGame(
            appName: "detail-preview", namespace: "presentation", catalogItemId: "detail-preview",
            title: "Forgotten light", description: "",
            imageUrl: nil, headerImageUrl: nil, developer: "Fixture studio", windows: true, macos: true
        )
        let detailsSession = EpicPresentationSession(
            games: [game], details: EpicGameDetails(playtimeSeconds: 5400, installationBytes: 2_000_000_000),
            achievements: [
                EpicAchievement(id: "first", name: "First steps", description: "", iconUrl: "",
                                hidden: false, unlocked: true, unlockDate: nil),
                EpicAchievement(id: "second", name: "Explorer", description: "", iconUrl: "",
                                hidden: false, unlocked: false, unlockDate: nil)
            ]
        )
        let detailsModel = EpicModel(keychain: EpicPresentationCredentials(), restore: { _ in detailsSession })
        await detailsModel.start()
        detailsModel.open(game)
        try await eventually { !detailsModel.isLoadingGameDetails && !detailsModel.achievements.isLoading }
        try await render(detailsModel, to: output.appending(path: "epic-detail-metrics-fixture.png"))
        try await render(detailsModel, to: output.appending(path: "epic-detail-metrics-light-fixture.png"), colorScheme: .light)
        try await render(detailsModel, to: output.appending(path: "epic-detail-narrow-fixture.png"), width: 580)
    }

    @MainActor
    private func render(_ model: EpicModel, to file: URL, colorScheme: ColorScheme = .dark, scrollOffset: CGFloat = 0, width: CGFloat = 900) async throws {
        let view = EpicView(model: model)
            .environment(\.colorScheme, colorScheme)
            .tint(ForgeStyle.accent)
            .background(ForgePalette(colorScheme: colorScheme).background)
        let fixture = EpicPresentationWindow(view, size: NSSize(width: width, height: 1000))
        defer { fixture.close() }
        fixture.layout()
        try await Task.sleep(for: .milliseconds(200))
        fixture.layout()
        if scrollOffset > 0 {
            let scroll = try XCTUnwrap(firstScrollView(in: fixture.host))
            scroll.contentView.scroll(to: NSPoint(x: 0, y: scrollOffset))
            scroll.reflectScrolledClipView(scroll.contentView)
            fixture.layout()
            try await Task.sleep(for: .milliseconds(100))
            fixture.layout()
        }
        let bitmap = try XCTUnwrap(fixture.host.bitmapImageRepForCachingDisplay(in: fixture.host.bounds))
        fixture.host.cacheDisplay(in: fixture.host.bounds, to: bitmap)
        var luminances: [Double] = []
        for y in stride(from: 0, to: bitmap.pixelsHigh, by: 8) {
            for x in stride(from: 0, to: bitmap.pixelsWide, by: 8) {
                if let color = bitmap.colorAt(x: x, y: y)?.usingColorSpace(.deviceRGB) {
                    luminances.append(Double(0.2126 * color.redComponent + 0.7152 * color.greenComponent + 0.0722 * color.blueComponent))
                }
            }
        }
        let count = Double(max(luminances.count, 1))
        let mean = luminances.reduce(0, +) / count
        let variance = luminances.reduce(0) { $0 + ($1 - mean) * ($1 - mean) } / count
        XCTAssertGreaterThan(variance, 0.002, "The rendered preview must contain visible content")
        guard variance > 0.002 else { throw EpicPresentationFailure.emptyPreview }
        try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).write(to: file)
    }

    @MainActor
    private func firstScrollView(in view: NSView) -> NSScrollView? {
        if let scroll = view as? NSScrollView { return scroll }
        return view.subviews.lazy.compactMap { self.firstScrollView(in: $0) }.first
    }

    @MainActor
    private func searchFrame(in root: NSView) -> NSRect? {
        if let frame = accessibilityFrame(in: root, label: L10n.string("ui.search.store_library", L10n.string("ui.store.epic"))), frame.width > 0 {
            return frame
        }
        // A plain SwiftUI Menu can exist only in the external accessibility
        // tree. Its neighboring TextField still has a native editable control,
        // so measure that real field when virtual labels are absent in-process.
        func field(in view: NSView) -> NSTextField? {
            if let textField = view as? NSTextField, textField.isEditable, textField.placeholderString == L10n.string("ui.search.placeholder") {
                return textField
            }
            return view.subviews.lazy.compactMap { field(in: $0) }.first
        }
        guard let search = field(in: root), let window = search.window else { return nil }
        return window.convertToScreen(search.convert(search.bounds, to: nil))
    }

    @MainActor
    private func nativeViewFrame(in root: NSView, identifier: String) -> NSRect? {
        if root.identifier?.rawValue == identifier, let window = root.window {
            return window.convertToScreen(root.convert(root.bounds, to: nil))
        }
        return root.subviews.lazy.compactMap { self.nativeViewFrame(in: $0, identifier: identifier) }.first
    }

    @MainActor
    private func accessibilityFrame(in root: Any, label: String) -> NSRect? {
        guard let element = accessibilityElement(in: root, label: label),
              element.responds(to: NSSelectorFromString("accessibilityFrame")),
              let frame = element.value(forKey: "accessibilityFrame") as? NSValue else { return nil }
        return frame.rectValue
    }

    @MainActor
    private func accessibilityElement(in root: Any, label: String, depth: Int = 0) -> NSObject? {
        guard depth < 30, let element = root as? NSObject else { return nil }
        func attribute(_ key: String) -> Any? {
            element.responds(to: NSSelectorFromString(key)) ? element.value(forKey: key) : nil
        }
        let names = ["accessibilityLabel", "accessibilityTitle", "accessibilityValue"].compactMap { attribute($0) as? String }
        if names.contains(label) { return element }
        let children = (attribute("accessibilityChildren") as? [Any] ?? [])
            + (attribute("accessibilityChildrenInNavigationOrder") as? [Any] ?? [])
            + ((root as? NSView)?.subviews ?? [])
        for child in children {
            if let match = accessibilityElement(in: child, label: label, depth: depth + 1) { return match }
        }
        return nil
    }

    @MainActor
    private func eventually(_ predicate: @MainActor () async -> Bool) async throws {
        let deadline = ContinuousClock.now + .seconds(3)
        while !(await predicate()) {
            if ContinuousClock.now >= deadline { throw EpicPresentationFailure.timeout }
            try await Task.sleep(for: .milliseconds(5))
        }
    }
}

@MainActor
struct EpicPresentationWindow {
    let host: NSView
    private let window: NSWindow

    init<Content: View>(_ content: Content, size: NSSize) {
        host = NSHostingView(rootView: content.frame(width: size.width, height: size.height))
        let rect = NSRect(x: -20_000, y: -20_000, width: size.width, height: size.height)
        window = NSWindow(contentRect: rect, styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.sharingType = .none
        window.contentView = host
        window.setFrameOrigin(rect.origin)
        window.orderBack(nil)
    }

    func layout() {
        host.layoutSubtreeIfNeeded()
        window.displayIfNeeded()
    }

    func close() { window.close() }

    var sheetContent: NSView? { window.sheets.first?.contentView }
}

private actor EpicPresentationCredentials: EpicCredentialStore {
    private var value: EpicCredentials?
    private(set) var loads = 0
    init(_ value: EpicCredentials? = epicPresentationCredentials) { self.value = value }
    func load() -> EpicCredentials? { loads += 1; return value }
    func save(_ credentials: EpicCredentials) { value = credentials }
    func remove() { value = nil }
}

private actor EpicPresentationSession: EpicSessionProtocol {
    func startFriends() async {}
    func reconnectFriends() async {}
    nonisolated func stopFriends() {}
    nonisolated func shutdownObservers() { credentialRevisions.stop() }
    nonisolated func friends() -> EpicFriendsSnapshot {
        EpicFriendsSnapshot(accountId: "unused", revision: 1, connection: .stopped, friends: [], message: nil)
    }
    func nextFriends(afterRevision: UInt64) async -> EpicFriendsSnapshot { friends() }
    nonisolated let credentialRevisions = EpicCredentialRevisionFeed()
    func nextCredentialsRevision(afterRevision: UInt64) async -> UInt64? {
        await credentialRevisions.next(after: afterRevision)
    }

    func launchPlan(namespace: String, catalogItemId: String, appName: String, platform: String, directory: String, language: String) async throws -> EpicLaunchPlan {
        throw EpicError.InvalidResponse
    }
    nonisolated func cancelDownload() {}
    nonisolated func downloadProgress() -> GameTransfer? { nil }
    func download(namespace: String, catalogItemId: String, appName: String, platform: String, directory: String) async throws -> EpicInstallation {
        throw EpicError.InvalidResponse
    }
    private let library: [EpicGame]
    private let details: EpicGameDetails
    private let achievementItems: [EpicAchievement]
    private var shouldHoldDetails: Bool
    private var detailsContinuation: CheckedContinuation<EpicGameDetails, Never>?
    private(set) var waitingForDetails = false
    private var shouldHoldGames: Bool
    private var gamesContinuation: CheckedContinuation<[EpicGame], Never>?
    private(set) var waitingForGames = false
    private(set) var gameCalls = 0
    private(set) var disconnections = 0

    init(
        games: [EpicGame],
        holdGames: Bool = false,
        details: EpicGameDetails = EpicGameDetails(playtimeSeconds: nil, installationBytes: nil),
        holdDetails: Bool = false,
        achievements: [EpicAchievement] = []
    ) {
        library = games
        shouldHoldGames = holdGames
        self.details = details
        shouldHoldDetails = holdDetails
        achievementItems = achievements
    }
    func achievements(namespace: String, locale: String) async throws -> [EpicAchievement] { achievementItems }
    func credentials() -> EpicCredentials { epicPresentationCredentials }
    func prepare() -> EpicCredentials { epicPresentationCredentials }
    func gameDetails(namespace: String, catalogItemId: String, appName: String, platform: String) async -> EpicGameDetails {
        if shouldHoldDetails {
            shouldHoldDetails = false
            waitingForDetails = true
            let result = await withCheckedContinuation { detailsContinuation = $0 }
            waitingForDetails = false
            return result
        }
        return details
    }
    func releaseDetails() {
        detailsContinuation?.resume(returning: details)
        detailsContinuation = nil
    }
    func games() async -> [EpicGame] {
        gameCalls += 1
        if shouldHoldGames {
            shouldHoldGames = false
            waitingForGames = true
            return await withCheckedContinuation { gamesContinuation = $0 }
        }
        return library
    }
    func releaseGames() {
        shouldHoldGames = false
        waitingForGames = false
        gamesContinuation?.resume(returning: library)
        gamesContinuation = nil
    }
    func disconnect() { disconnections += 1 }
}

private let epicPresentationCredentials = EpicCredentials(accountId: "epic-presentation", displayName: "Test account", refreshToken: "fixture-only-never-sent")
private let epicPresentationLibrary: [EpicGame] = (0..<24).map { index in
    EpicGame(appName: "fixture-\(index)", namespace: "presentation", catalogItemId: "catalog-\(index)", title: index == 0 ? "Forgotten light" : "Test game \(index)", description: "", imageUrl: nil, headerImageUrl: nil, developer: "", windows: false, macos: false)
}
private enum EpicPresentationFailure: Error { case timeout, emptyPreview }

// A real loopback response exercises SwiftUI's image loader without changing
// URLSession globals or touching an Epic account. Disabling HTTP caching makes
// this check cover decoded-image reuse, independent of server cache headers.
private final class EpicArtworkHTTPServer: @unchecked Sendable {
    private let listener: NWListener
    private let queue = DispatchQueue(label: "ForgeTests.EpicArtworkHTTPServer")
    private let lock = NSLock()
    private var requests = 0
    private let response: Data

    init(image: Data) throws {
        listener = try NWListener(using: .tcp, on: .any)
        let headers = "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: \(image.count)\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
        response = Data(headers.utf8) + image
    }

    var requestCount: Int { lock.withLock { requests } }

    func start() async throws -> URL {
        try await withCheckedThrowingContinuation { continuation in
            listener.stateUpdateHandler = { [self] state in
                switch state {
                case .ready:
                    listener.stateUpdateHandler = nil
                    guard let port = listener.port,
                          let url = URL(string: "http://127.0.0.1:\(port.rawValue)/\(UUID().uuidString).png") else {
                        continuation.resume(throwing: EpicPresentationFailure.timeout)
                        return
                    }
                    continuation.resume(returning: url)
                case .failed(let error):
                    listener.stateUpdateHandler = nil
                    continuation.resume(throwing: error)
                default: break
                }
            }
            listener.newConnectionHandler = { [self] connection in
                connection.start(queue: queue)
                receiveRequest(connection)
            }
            listener.start(queue: queue)
        }
    }

    func stop() {
        listener.newConnectionHandler = nil
        listener.cancel()
    }

    private func receiveRequest(_ connection: NWConnection, accumulated: Data = Data()) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 16_384) { [self] data, _, complete, error in
            let request = accumulated + (data ?? Data())
            if request.range(of: Data("\r\n\r\n".utf8)) != nil {
                lock.withLock { requests += 1 }
                connection.send(content: response, completion: .contentProcessed { _ in connection.cancel() })
            } else if complete || error != nil {
                connection.cancel()
            } else {
                receiveRequest(connection, accumulated: request)
            }
        }
    }
}
