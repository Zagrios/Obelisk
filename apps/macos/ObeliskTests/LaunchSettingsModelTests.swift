import XCTest
@testable import Obelisk

final class LaunchSettingsModelTests: XCTestCase {
    private static func settings(choices: LaunchChoices = LaunchChoices(),
                                 engine: LaunchEngine = .rosetta) -> LaunchSettings {
        LaunchSettings(
            engine: engine, graphics: engine == .rosetta ? .d3dMetal : .dxmt, rayTracing: true,
            renderPercent: engine == .linux ? choices.renderPercent ?? RenderPercent.native : RenderPercent.native,
            performanceHud: choices.performanceHud, choices: choices, defaultEngine: .rosetta,
            engines: [
                LaunchEngineOffer(
                    engine: .rosetta, translator: .rosetta, defaultGraphics: .d3dMetal,
                    graphics: [
                        LaunchGraphicsOffer(graphics: .d3dMetal, options: [.rayTracing]),
                        LaunchGraphicsOffer(graphics: .dxmt, options: []),
                    ],
                    lowestRenderPercent: nil
                ),
                LaunchEngineOffer(
                    engine: .linux, translator: .fex, defaultGraphics: .dxmt,
                    graphics: [LaunchGraphicsOffer(graphics: .dxmt, options: [])],
                    lowestRenderPercent: 1
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

        model.setArguments("   ")
        model.setArguments("  -dx11 \"+exec auto exec.cfg\" ")
        try await waitUntil { model.settings?.choices.arguments != nil }
        model.setArguments("-dx11 \"+exec auto exec.cfg\"")
        model.setArguments("")
        try await waitUntil { saved.count == 2 }

        XCTAssertEqual(saved.map(\.arguments), ["-dx11 \"+exec auto exec.cfg\"", nil])
    }

    @MainActor
    private func waitUntil(_ condition: () -> Bool) async throws {
        for _ in 0..<200 where !condition() {
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTAssertTrue(condition())
    }
}
