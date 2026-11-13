import XCTest
@testable import Obelisk

final class LaunchSettingsModelTests: XCTestCase {
    private static func settings(choices: LaunchChoices = LaunchChoices(),
                                 engine: LaunchEngine = .rosetta) -> LaunchSettings {
        LaunchSettings(
            engine: engine, graphics: engine == .rosetta ? .d3dMetal : .dxmt, rayTracing: true,
            renderPercent: engine == .linux ? choices.renderPercent ?? RenderPercent.native : RenderPercent.native,
            performanceHud: choices.performanceHud, gameMode: engine != .linux || choices.gameMode ?? true,
            choices: choices, defaultEngine: .rosetta,
            engines: [
                LaunchEngineOffer(
                    engine: .rosetta, translator: .rosetta, defaultGraphics: .d3dMetal,
                    graphics: [
                        LaunchGraphicsOffer(graphics: .d3dMetal, options: [.rayTracing]),
                        LaunchGraphicsOffer(graphics: .dxmt, options: []),
                    ],
                    lowestRenderPercent: nil, gameMode: false
                ),
                LaunchEngineOffer(
                    engine: .linux, translator: .fex, defaultGraphics: .dxmt,
                    graphics: [LaunchGraphicsOffer(graphics: .dxmt, options: [])],
                    lowestRenderPercent: 1, gameMode: true
                ),
            ]
        )
    }

    @MainActor
    func testAChoiceKeepsTheOthersAndTheOffersFollowTheEngineRustReturns() async throws {
        var saved: [LaunchChoices] = []
        let saving = expectation(description: "saved")
        let model = LaunchSettingsModel(
            load: { Self.settings() },
            save: { choices in
                saved.append(choices)
                saving.fulfill()
                return Self.settings(choices: choices, engine: choices.engine ?? .rosetta)
            }
        )
        await model.appear()
        XCTAssertFalse(model.isCustomized)
        XCTAssertEqual(model.graphicsOffer?.options, [.rayTracing])

        model.choose { $0.engine = .linux }
        await fulfillment(of: [saving], timeout: 2)

        XCTAssertEqual(saved, [LaunchChoices(engine: .linux)])
        try await waitUntil { model.settings?.engine == .linux }
        XCTAssertTrue(model.isCustomized)
        XCTAssertEqual(model.engineOffer?.translator, .fex)
        XCTAssertEqual(model.graphicsOffer?.graphics, .dxmt)
        XCTAssertEqual(model.graphicsOffer?.options, [])
    }

    @MainActor
    func testGameModeIsOfferedByTheLinuxEngineOnlyAndTurnsOff() async throws {
        var saved: [LaunchChoices] = []
        let model = LaunchSettingsModel(
            load: { Self.settings() },
            save: { choices in
                saved.append(choices)
                return Self.settings(choices: choices, engine: choices.engine ?? .rosetta)
            }
        )
        await model.appear()
        XCTAssertEqual(model.engineOffer?.gameMode, false)

        model.choose { $0.engine = .linux }
        try await waitUntil { model.settings?.engine == .linux }
        XCTAssertEqual(model.engineOffer?.gameMode, true)
        XCTAssertEqual(model.settings?.gameMode, true)

        model.choose { $0.gameMode = false }
        try await waitUntil { model.settings?.gameMode == false }
        XCTAssertEqual(saved.last, LaunchChoices(engine: .linux, gameMode: false))
    }

    @MainActor
    func testARenderResolutionIsOfferedByTheLinuxEngineOnlyAndIsKeptThenForgotten() async throws {
        var saved: [LaunchChoices] = []
        let model = LaunchSettingsModel(
            load: { Self.settings() },
            save: { choices in
                saved.append(choices)
                return Self.settings(choices: choices, engine: choices.engine ?? .rosetta)
            }
        )
        await model.appear()
        XCTAssertNil(model.engineOffer?.lowestRenderPercent)
        XCTAssertEqual(model.settings?.renderPercent, RenderPercent.native)

        model.choose { $0.engine = .linux }
        try await waitUntil { model.settings?.engine == .linux }
        XCTAssertEqual(model.engineOffer?.lowestRenderPercent, 1)

        model.choose { $0.renderPercent = 50 }
        try await waitUntil { model.settings?.renderPercent == 50 }
        XCTAssertEqual(saved.last, LaunchChoices(engine: .linux, renderPercent: 50))
        XCTAssertTrue(model.isCustomized)

        model.reset()
        try await waitUntil { !model.isCustomized }
        XCTAssertEqual(saved.last, LaunchChoices())
        XCTAssertEqual(model.settings?.renderPercent, RenderPercent.native)
        XCTAssertNil(model.engineOffer?.lowestRenderPercent)
    }

    @MainActor
    func testTheRenderResolutionIsNamedByItsShareAndAllThePixelsAreNative() {
        let share = RenderPercent.name(of: 50)

        XCTAssertEqual(RenderPercent.name(of: RenderPercent.native),
                       L10n.string("ui.launch_settings.render_percent.native"))
        XCTAssertEqual(share, DownloadFormatting.percent(0.5), "the percent is written the way the app writes any")
        XCTAssertTrue(share.contains("50"))
        XCTAssertTrue(share.contains("%"))
    }

    @MainActor
    func testTheRenderResolutionSnapsToARoundShareWithinTwoPoints() {
        XCTAssertEqual(RenderPercent.stops, [25, 50, 75, RenderPercent.native])

        for stop in RenderPercent.stops {
            for offset in -2...2 {
                let percent = UInt32(Int(stop) + offset)
                XCTAssertEqual(RenderPercent.snapped(percent), stop, "\(percent) %")
            }
            for offset in [-3, 3] {
                let percent = UInt32(Int(stop) + offset)
                XCTAssertEqual(RenderPercent.snapped(percent), percent, "\(percent) %")
            }
        }
        XCTAssertEqual(RenderPercent.snapped(1), 1)
    }

    @MainActor
    func testRestoringDefaultsForgetsEveryChoice() async throws {
        var saved: [LaunchChoices] = []
        let model = LaunchSettingsModel(
            load: { Self.settings(choices: LaunchChoices(arguments: "-windowed", engine: .linux, rayTracing: false,
                                                                 renderPercent: 50, performanceHud: true),
                                  engine: .linux) },
            save: { choices in
                saved.append(choices)
                return Self.settings(choices: choices)
            }
        )
        await model.appear()
        XCTAssertTrue(model.isCustomized)

        model.reset()

        try await waitUntil { !model.isCustomized }
        XCTAssertEqual(saved, [LaunchChoices()])
    }

    @MainActor
    func testAChangeRustCouldNotKeepIsReportedAndTheSettingsStay() async throws {
        struct NotSaved: LocalizedError { var errorDescription: String? { "not saved" } }
        let model = LaunchSettingsModel(load: { Self.settings() }, save: { _ in throw NotSaved() })
        await model.appear()

        model.choose { $0.rayTracing = false }

        try await waitUntil { model.error != nil }
        XCTAssertEqual(model.error, "not saved")
        XCTAssertEqual(model.settings, Self.settings())
    }

    @MainActor
    func testTypedArgumentsAreKeptTrimmedOnlyWhenTheyChange() async throws {
        var saved: [LaunchChoices] = []
        let model = LaunchSettingsModel(
            load: { Self.settings() },
            save: { choices in
                saved.append(choices)
                return Self.settings(choices: choices)
            }
        )
        await model.appear()

        model.arguments = "   "
        XCTAssertEqual(model.arguments, "   ", "the field shows what is typed")
        model.keepArguments()
        XCTAssertEqual(model.arguments, "")

        model.arguments = "  -dx11 \"+exec auto exec.cfg\" "
        model.keepArguments()
        XCTAssertEqual(model.arguments, "-dx11 \"+exec auto exec.cfg\"", "before Rust kept them")
        model.arguments = "-dx11 \"+exec auto exec.cfg\""
        model.keepArguments()
        try await waitUntil { model.settings?.choices.arguments != nil }
        XCTAssertEqual(saved.map(\.arguments), ["-dx11 \"+exec auto exec.cfg\""])

        model.arguments = ""
        model.keepArguments()
        try await waitUntil { saved.count == 2 }
        XCTAssertEqual(saved.map(\.arguments), ["-dx11 \"+exec auto exec.cfg\"", nil])
    }

    @MainActor
    func testAChoiceMadeBeforeRustKeptTheLastOneAddsToIt() async throws {
        var saved: [LaunchChoices] = []
        let model = LaunchSettingsModel(
            load: { Self.settings() },
            save: { choices in
                saved.append(choices)
                return Self.settings(choices: choices, engine: choices.engine ?? .rosetta)
            }
        )
        await model.appear()

        model.choose { $0.performanceHud = true }
        model.choose { $0.engine = .linux }

        let both = LaunchChoices(engine: .linux, performanceHud: true)
        try await waitUntil { model.settings?.choices == both }
        XCTAssertEqual(saved.last, both)
    }

    @MainActor
    func testRustKeepsOneChangeAtATimeAndOnlyTheLatestOfThoseThatWaited() async throws {
        var saved: [LaunchChoices] = []
        var keeping = 0
        var release: CheckedContinuation<Void, Never>?
        let model = LaunchSettingsModel(
            load: { Self.settings() },
            save: { choices in
                keeping += 1
                XCTAssertEqual(keeping, 1, "an earlier save could come back over a later one")
                saved.append(choices)
                if saved.count == 1 { await withCheckedContinuation { release = $0 } }
                keeping -= 1
                return Self.settings(choices: choices, engine: choices.engine ?? .rosetta)
            }
        )
        await model.appear()

        model.choose { $0.engine = .linux }
        try await waitUntil { release != nil }
        model.choose { $0.renderPercent = 75 }
        model.choose { $0.renderPercent = 50 }
        model.choose { $0.performanceHud = true }
        XCTAssertEqual(saved, [LaunchChoices(engine: .linux)])
        release?.resume()

        let latest = LaunchChoices(engine: .linux, renderPercent: 50, performanceHud: true)
        try await waitUntil { model.settings?.choices == latest }
        XCTAssertEqual(saved, [LaunchChoices(engine: .linux), latest])
    }

    @MainActor
    func testAChangeRustCouldNotKeepIsNotSentAgainWithTheNextOne() async throws {
        struct NotSaved: LocalizedError { var errorDescription: String? { "not saved" } }
        var saved: [LaunchChoices] = []
        let model = LaunchSettingsModel(
            load: { Self.settings() },
            save: { choices in
                saved.append(choices)
                if choices.rayTracing == false { throw NotSaved() }
                return Self.settings(choices: choices)
            }
        )
        await model.appear()

        model.choose { $0.rayTracing = false }
        try await waitUntil { model.error != nil }
        model.choose { $0.performanceHud = true }

        try await waitUntil { model.settings?.choices == LaunchChoices(performanceHud: true) }
        XCTAssertEqual(saved.last, LaunchChoices(performanceHud: true))
        XCTAssertNil(model.error)
    }

    @MainActor
    func testArgumentsRustCouldNotKeepStayInTheFieldToBeKeptAgain() async throws {
        struct NotSaved: LocalizedError { var errorDescription: String? { "not saved" } }
        var saved: [LaunchChoices] = []
        var fails = true
        let model = LaunchSettingsModel(
            load: { Self.settings() },
            save: { choices in
                saved.append(choices)
                if fails { throw NotSaved() }
                return Self.settings(choices: choices)
            }
        )
        await model.appear()

        model.arguments = "-windowed"
        model.keepArguments()
        try await waitUntil { model.error != nil }
        XCTAssertEqual(model.arguments, "-windowed")
        XCTAssertNil(model.settings?.choices.arguments)

        fails = false
        model.keepArguments()
        try await waitUntil { model.settings?.choices.arguments == "-windowed" }
        XCTAssertEqual(saved.map(\.arguments), ["-windowed", "-windowed"])
    }

    @MainActor
    func testAHeldThumbShowsItsSnappedShareAndKeepsItOnceLetGo() async throws {
        var saved: [LaunchChoices] = []
        var release: CheckedContinuation<Void, Never>?
        let model = LaunchSettingsModel(
            load: { Self.settings(choices: LaunchChoices(engine: .linux), engine: .linux) },
            save: { choices in
                saved.append(choices)
                await withCheckedContinuation { release = $0 }
                return Self.settings(choices: choices, engine: .linux)
            }
        )
        await model.appear()
        XCTAssertEqual(model.renderPercent, RenderPercent.native)

        model.holdRenderPercent()
        model.moveRenderPercent(to: 62)
        XCTAssertEqual(model.renderPercent, 62)
        model.moveRenderPercent(to: 51)
        XCTAssertEqual(model.renderPercent, 50, "within two points of a stop")
        XCTAssertEqual(saved, [], "nothing is kept while the thumb is held")

        model.letGoOfRenderPercent()
        try await waitUntil { release != nil }
        XCTAssertEqual(saved, [LaunchChoices(engine: .linux, renderPercent: 50)])
        XCTAssertEqual(model.settings?.renderPercent, RenderPercent.native)
        XCTAssertEqual(model.renderPercent, 50, "the thumb does not jump back while Rust keeps the share")

        release?.resume()
        try await waitUntil { model.settings?.renderPercent == 50 }
        XCTAssertEqual(model.renderPercent, 50)
        XCTAssertFalse(model.holdsRenderPercent)
    }

    @MainActor
    func testAKeyboardStepIsKeptAtOnceAndTheNextOneStartsFromIt() async throws {
        var saved: [LaunchChoices] = []
        var release: CheckedContinuation<Void, Never>?
        let model = LaunchSettingsModel(
            load: { Self.settings(choices: LaunchChoices(engine: .linux), engine: .linux) },
            save: { choices in
                saved.append(choices)
                if saved.count == 1 { await withCheckedContinuation { release = $0 } }
                return Self.settings(choices: choices, engine: .linux)
            }
        )
        await model.appear()

        model.moveRenderPercent(to: 90)
        try await waitUntil { release != nil }
        XCTAssertEqual(model.renderPercent, 90)
        model.moveRenderPercent(to: 80)
        XCTAssertEqual(model.renderPercent, 80, "the second step shows before the first is kept")
        release?.resume()

        try await waitUntil { model.settings?.renderPercent == 80 }
        XCTAssertEqual(saved.map(\.renderPercent), [90, 80])
        XCTAssertEqual(model.renderPercent, 80)
    }

    @MainActor
    func testRestoringDefaultsWhileAStepWaitsBringsTheShareBackToNative() async throws {
        var saved: [LaunchChoices] = []
        var release: CheckedContinuation<Void, Never>?
        let model = LaunchSettingsModel(
            load: { Self.settings(choices: LaunchChoices(engine: .linux), engine: .linux) },
            save: { choices in
                saved.append(choices)
                if saved.count == 1 { await withCheckedContinuation { release = $0 } }
                return Self.settings(choices: choices, engine: choices.engine ?? .rosetta)
            }
        )
        await model.appear()

        model.moveRenderPercent(to: 50)
        try await waitUntil { release != nil }
        model.reset()
        release?.resume()

        try await waitUntil { saved.count == 2 && !model.isCustomized }
        XCTAssertEqual(saved.last, LaunchChoices())
        XCTAssertEqual(model.renderPercent, RenderPercent.native)
    }

    @MainActor
    private func waitUntil(_ condition: () -> Bool) async throws {
        for _ in 0..<200 where !condition() {
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTAssertTrue(condition())
    }
}
