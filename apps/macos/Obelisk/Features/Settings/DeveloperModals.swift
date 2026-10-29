#if DEBUG
import AppKit
import SwiftUI

// Debug builds only, like everything in this file: Settings' Developer section
// opens each modal Obelisk presents over made-up data, to check how it looks,
// but for Voile's update, which shows Voile's latest release notes on GitHub.
// Nothing here reaches Rust, a storefront account or getobelisk.app.

/// A modal of Obelisk, and the states the gallery opens it in.
struct DeveloperModal: Identifiable {
    let id: String
    let title: String
    /// Where Obelisk presents it.
    let detail: String
    let symbol: String
    let variants: [Variant]

    struct Variant: Identifiable {
        let title: String
        let open: @MainActor (DeveloperModals) -> Void

        init(_ title: String, open: @escaping @MainActor (DeveloperModals) -> Void) {
            self.title = title
            self.open = open
        }

        var id: String { title }
    }

    @MainActor static var all: [DeveloperModal] {
        [
            DeveloperModal(
                id: "cloud-conflict", title: "Cloud conflict",
                detail: "Over the whole window, when a launch finds the local and Cloud saves both changed.",
                symbol: "arrow.triangle.branch",
                variants: [
                    Variant("Steam") {
                        $0.cloudConflict(storefront: "steam", title: SteamGames.cloudTitle,
                                         message: "backend.steam.cloud.conflict")
                    },
                    Variant("Epic Games") {
                        $0.cloudConflict(storefront: "epic", title: EpicGames.cloudTitle,
                                         message: "backend.epic.cloud_conflict")
                    },
                    Variant("Working") {
                        $0.cloudConflict(storefront: "steam", title: SteamGames.cloudTitle,
                                         message: "backend.steam.cloud.conflict", busy: true)
                    },
                ]),
            DeveloperModal(
                id: "cloud-failure", title: "Cloud failure",
                detail: "Over the whole window, when a launch's Cloud check fails before the game starts.",
                symbol: "icloud.slash",
                variants: [
                    Variant("Steam") {
                        $0.cloudFailure(storefront: "steam", title: SteamGames.cloudTitle,
                                        message: "backend.steam.cloud.unsupported")
                    },
                    Variant("Epic Games") {
                        $0.cloudFailure(storefront: "epic", title: EpicGames.cloudTitle,
                                        message: "backend.epic.cloud.failed")
                    },
                    Variant("No Launch anyway") {
                        $0.cloudFailure(storefront: "steam", title: SteamGames.cloudTitle,
                                        message: "backend.steam.cloud.network", canLaunchAnyway: false)
                    },
                    Variant("Working") {
                        $0.cloudFailure(storefront: "steam", title: SteamGames.cloudTitle,
                                        message: "backend.steam.cloud.network", busy: true)
                    },
                ]),
            DeveloperModal(
                id: "voile-update", title: "Voile update",
                detail: "Over the whole window, when a Windows game's launch finds a newer Voile release. "
                    + "Shows the notes of Voile's latest releases on GitHub.",
                symbol: "sailboat.fill",
                variants: [
                    Variant("Offered") { $0.voileUpdate() },
                    Variant("Required") { $0.voileUpdate(required: true) },
                    Variant("Several releases") { $0.voileUpdate(releases: 2) },
                    Variant("No notes") { $0.voileUpdate(notes: false) },
                ]),
            DeveloperModal(
                id: "rosetta", title: "Rosetta",
                detail: "Over the whole window, when a Windows game needs Rosetta. Install fails after 3 seconds.",
                symbol: "cpu",
                variants: [Variant("Open") { $0.rosetta() }]),
            DeveloperModal(
                id: "bug-report", title: "Bug report",
                detail: "From Settings and a game's SOS button. Send answers after 1.5 seconds, without sending anything.",
                symbol: "ladybug.fill",
                variants: [
                    Variant("Obelisk") { $0.bugReport(.app, answer: 201) },
                    Variant("A game") { $0.bugReport(.game(store: .steam, id: DeveloperModals.gameID,
                                                           name: DeveloperModals.gameName), answer: 201) },
                    Variant("Send fails") { $0.bugReport(.app, answer: 500) },
                    Variant("Too many reports") { $0.bugReport(.app, answer: 429) },
                ]),
            DeveloperModal(
                id: "launch-settings", title: "Launch Settings",
                detail: "From a Windows game's management menu.",
                symbol: "slider.horizontal.3",
                variants: [
                    Variant("Loaded") { $0.launchSettings { DeveloperModals.launchSettings() } },
                    Variant("Loading") {
                        $0.launchSettings {
                            try await Task.sleep(for: .seconds(3600))
                            return nil
                        }
                    },
                    Variant("Not loaded") {
                        $0.launchSettings { throw DeveloperModalFailure("The game's launch settings could not be read.") }
                    },
                ]),
            DeveloperModal(
                id: "achievements", title: "Achievements",
                detail: "From the achievements on a game's page.",
                symbol: "trophy.fill",
                variants: [
                    Variant("List") { $0.achievements { DeveloperModals.achievements } },
                    Variant("Loading") {
                        $0.achievements {
                            try await Task.sleep(for: .seconds(3600))
                            return []
                        }
                    },
                    Variant("Failed") { $0.achievements { throw DeveloperModalFailure("Achievements failed") } },
                    Variant("None") { $0.achievements { [] } },
                ]),
            DeveloperModal(
                id: "friends", title: "Friends",
                detail: "From the friends button of a Steam or Epic library.",
                symbol: "person.2.fill",
                variants: [
                    Variant("Connected") { $0.friends(.connected) },
                    Variant("Loading") { $0.friends(.loading, friends: []) },
                    Variant("Reconnecting") { $0.friends(.interrupted) },
                    Variant("Unavailable") { $0.friends(.failed("model.friends.unavailable"), friends: []) },
                    Variant("Session expired") { $0.friends(.authenticationRequired, friends: []) },
                ]),
            DeveloperModal(
                id: "store", title: "Store",
                detail: "From a library's store button, signed out here.",
                symbol: "bag.fill",
                variants: [
                    Variant("Steam") {
                        $0.store("steam", title: L10n.string("ui.store.steam"), url: "https://store.steampowered.com/")
                    },
                    Variant("Epic Games") {
                        $0.store("epic", title: L10n.string("ui.store.epic"), url: EpicGames.storeURL.absoluteString)
                    },
                    Variant("GOG") { $0.store("gog", title: L10n.string("ui.store.gog"), url: "https://www.gog.com/") },
                ]),
            DeveloperModal(
                id: "games-folder", title: "Games folder",
                detail: "From Settings › Storage, once the player picked another folder. Moves nothing here.",
                symbol: "externaldrive.fill",
                variants: [
                    Variant("External disk") { $0.gamesFolder() },
                    Variant("ExFAT") { $0.gamesFolder(format: "ExFAT") },
                    Variant("Not enough space") { $0.gamesFolder(available: 120_000_000_000) },
                    Variant("Read-only") { $0.gamesFolder(format: "NTFS", readOnly: true) },
                    Variant("No games") { $0.gamesFolder(games: 0) },
                    Variant("Back to this Mac") { $0.gamesFolder(chosen: nil) },
                ]),
            DeveloperModal(
                id: "uninstall", title: "Uninstall",
                detail: "Alert, from a game's management menu: an installation's, or a stopped download's. "
                    + "Deletes nothing here.",
                symbol: "trash.fill",
                variants: [
                    Variant("Open") { $0.confirmsRemoval = .uninstall },
                    Variant("Delete download") { $0.confirmsRemoval = .download },
                ]),
            DeveloperModal(
                id: "force-quit", title: "Force Quit",
                detail: "Alert, from a running game's button once its quit did not end it. Quits nothing here.",
                symbol: "xmark.octagon.fill",
                variants: [Variant("Open") { $0.forceQuit = DeveloperModals.forceQuitAction }]),
            DeveloperModal(
                id: "stop-game", title: "A game still runs",
                detail: "Alert, when Obelisk quits, restarts or updates over a running game. Stops nothing here.",
                symbol: "power",
                variants: [
                    Variant("Quit") { _ in
                        DeveloperModals.stopGame(L10n.string("app.quit.running.message"), L10n.string("app.quit.stop"))
                    },
                    Variant("Restart") { _ in
                        DeveloperModals.stopGame(L10n.string("app.restart.running.message"),
                                                 L10n.string("app.restart.stop"))
                    },
                    Variant("Update") { _ in
                        DeveloperModals.stopGame(L10n.string("app.update.running.message"),
                                                 L10n.string("app.update.stop"))
                    },
                ]),
        ]
    }
}

/// The modal the gallery shows, with the made-up models behind it.
@MainActor @Observable
final class DeveloperModals {
    var sheet: Sheet?
    var confirmsRemoval: GameRemoval?
    var forceQuit: GameAction?

    /// A sheet and the models it shows, kept while it is open.
    struct Sheet: Identifiable {
        let id = UUID()
        let content: @MainActor (_ close: @escaping () -> Void) -> AnyView
    }

    nonisolated static let gameName = "ELDEN RING"
    nonisolated static let gameID = "1245620"

    private func present<Content: View>(_ content: @escaping @MainActor (_ close: @escaping () -> Void) -> Content) {
        sheet = Sheet { AnyView(content($0)) }
    }

    func cloudConflict(storefront: String, title: String, message: String, busy: Bool = false) {
        present { close in
            SteamCloudPromptView(title: "\(title) · \(Self.gameName)", message: message, busy: busy,
                                 onCancel: close, onChooseLocal: close, onChooseCloud: close, storefront: storefront)
                .interactiveDismissDisabled()
        }
    }

    func cloudFailure(storefront: String, title: String, message: String, busy: Bool = false,
                      canLaunchAnyway: Bool = true) {
        present { close in
            SteamCloudFailureView(title: "\(title) · \(Self.gameName)", message: message, busy: busy,
                                  canLaunchAnyway: canLaunchAnyway, onRetry: close, onCancel: close,
                                  onLaunchAnyway: close, storefront: storefront)
        }
    }

    func rosetta() {
        let sessions = DeveloperRosettaSessions { [weak self] in self?.sheet = nil }
        // Rust never confirms Rosetta here: the installation fails once the
        // first readiness check comes.
        let model = RosettaModel(sessions: sessions, install: { try await Task.sleep(for: .seconds(3600)) },
                                 readinessDelay: { try await Task.sleep(for: .seconds(3)) })
        model.connect(accountId: 0)
        present { _ in RosettaPromptView(model: model).interactiveDismissDisabled() }
    }

    /// Voile's latest `releases` on GitHub, newest first, offered as a game
    /// that needs the newest would when `required`; without their notes
    /// unless `notes`. A failed read shows as the notes.
    func voileUpdate(required: Bool = false, releases count: Int = 1, notes: Bool = true) {
        Task {
            var releases: [VoileReleaseNotes]
            do {
                releases = try await Self.voileReleases(count)
            } catch {
                releases = [VoileReleaseNotes(version: "v?", notes: "**GitHub's releases could not be read:** "
                                                + error.localizedDescription)]
            }
            if !notes { releases = releases.map { VoileReleaseNotes(version: $0.version, notes: "") } }
            let offer = VoileUpdateOffer(version: releases[0].version, required: required, releases: releases)
            present { close in VoileUpdateSheet(offer: offer) { _ in close() } }
        }
    }

    /// Voile's latest `count` runtime releases on GitHub, newest first, with
    /// their notes as GitHub keeps them: the releases Voile's feed offers,
    /// whose tags read v26.3.0-17, not the archives its build pins.
    nonisolated static func voileReleases(_ count: Int) async throws -> [VoileReleaseNotes] {
        struct Release: Decodable {
            let tagName: String
            let body: String?
            let draft: Bool
        }
        var request = URLRequest(url: URL(string: "https://api.github.com/repos/Zagrios/Voile/releases?per_page=30")!,
                                 timeoutInterval: 20)
        request.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        let (data, response) = try await URLSession.shared.data(for: request)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard status == 200 else { throw DeveloperModalFailure("GitHub answered \(status).") }
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let releases = try decoder.decode([Release].self, from: data)
            .filter { !$0.draft && $0.tagName.wholeMatch(of: /v\d+\.\d+\.\d+-\d+/) != nil }
            .prefix(count)
            .map { VoileReleaseNotes(version: $0.tagName, notes: ($0.body ?? "").replacingOccurrences(of: "\r", with: "")) }
        guard !releases.isEmpty else { throw DeveloperModalFailure("Voile has no runtime release on GitHub.") }
        return Array(releases)
    }

    /// `answer` is the HTTP status getobelisk.app answers with: 201 gives the
    /// report a code, 429 turns away more reports, anything else fails.
    func bugReport(_ subject: BugReportSubject, answer: Int) {
        let model = BugReportModel(subject: subject, sources: Self.bugReportSources()) { request in
            try await Task.sleep(for: .seconds(1.5))
            let response = HTTPURLResponse(url: request.url ?? BugReportPayload.endpoint, statusCode: answer,
                                           httpVersion: nil, headerFields: nil)
            return (Data(#"{"code":"K7F3-9QX2"}"#.utf8), response ?? URLResponse())
        }
        present { close in BugReportSheet(model: model, close: close) }
    }

    /// A few made-up diagnostics, which the report lists: a game's logs and
    /// launch report, and for Obelisk the report alone. Nothing reads
    /// Obelisk's own log or crash reports.
    private static func bugReportSources() -> BugReportSources {
        let folder = FileManager.default.temporaryDirectory.appending(path: "obelisk-developer-modals",
                                                                       directoryHint: .isDirectory)
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let logs = ["game.log", "game.previous.log", "input.log"].map { folder.appending(path: $0) }
        let report = folder.appending(path: "launch-report.json")
        for url in logs + [report] {
            try? Data("Made up for the modal gallery.\n".utf8).write(to: url)
        }
        let diagnostics: @Sendable (BugReportSubject, Date) -> [URL] = { subject, _ in
            if case .app = subject { [report] } else { logs + [report] }
        }
        return BugReportSources(diagnostics: diagnostics, crashReports: folder,
                                process: "obelisk-developer-modals", home: NSHomeDirectory(), unifiedLog: { "" },
                                voile: { nil })
    }

    /// A folder the player picked: by default 23 games, 271 GB of them to
    /// copy to an APFS disk with room for them.
    func gamesFolder(chosen: URL? = URL(filePath: "/Volumes/Games SSD/Jeux"), games: Int = 23,
                     available: UInt64 = 912_000_000_000, format: String? = nil, readOnly: Bool = false) {
        let path = chosen.map { $0.appending(path: "Obelisk Games").path }
            ?? AppIdentity.libraryDirectory.appending(path: "Games").path
        let proposal = GamesFolderProposal(
            chosen: chosen, path: path, games: games, copiedBytes: games > 0 ? 271_000_000_000 : 0,
            volume: chosen == nil ? "Macintosh HD" : "Games SSD", availableBytes: available, format: format,
            isReadOnly: readOnly)
        present { close in GamesFolderSheet(proposal: proposal, cancel: close, confirm: close) }
    }

    func launchSettings(_ load: @escaping () async throws -> LaunchSettings?) {
        let model = LaunchSettingsModel(load: load) { Self.launchSettings($0) }
        present { _ in LaunchSettingsSheet(model: model, gameName: Self.gameName) }
    }

    /// What Rust offers a Windows game, `choices` applied.
    nonisolated static func launchSettings(_ choices: LaunchChoices = LaunchChoices()) -> LaunchSettings {
        let engines = [
            LaunchEngineOffer(engine: .rosetta, translator: .rosetta, defaultGraphics: .d3dMetal, graphics: [
                LaunchGraphicsOffer(graphics: .d3dMetal, options: [.rayTracing]),
                LaunchGraphicsOffer(graphics: .dxmt, options: []),
            ]),
            LaunchEngineOffer(engine: .linux, translator: .fex, defaultGraphics: .dxmt,
                              graphics: [LaunchGraphicsOffer(graphics: .dxmt, options: [])]),
        ]
        let engine = choices.engine ?? .rosetta
        let offer = engines.first { $0.engine == engine } ?? engines[0]
        let graphics = choices.graphics.flatMap { chosen in offer.graphics.contains { $0.graphics == chosen } ? chosen : nil }
            ?? offer.defaultGraphics
        return LaunchSettings(engine: engine, graphics: graphics, rayTracing: choices.rayTracing ?? false,
                              performanceHud: choices.performanceHud,
                              steamClient: engine == .rosetta ? choices.steamClient ?? .obelisk : .obelisk,
                              choices: choices, defaultEngine: .rosetta, defaultSteamClient: .obelisk,
                              engines: engines)
    }

    func achievements(_ fetch: @escaping @MainActor @Sendable () async throws -> [GameAchievement]) {
        let model = GameAchievementsModel(storeID: "steam") { _ in
            L10n.message("backend.steam.achievements.invalidResponse")
        }
        model.load(fetch)
        present { _ in GameAchievementsView(model: model, gameName: Self.gameName) { model.load(fetch) } }
    }

    static var achievements: [GameAchievement] {
        let day: TimeInterval = 24 * 3600
        let unlocked = Date(timeIntervalSinceNow: -40 * day)
        let items: [(name: String, description: String, unlocked: Date?)] = [
            ("The Roundtable Hold", "Arrived at the Roundtable Hold", unlocked),
            ("Shardbearer Godrick", "Defeated Shardbearer Godrick", unlocked + 2 * day),
            ("Great Rune", "Restored the power of a Great Rune", unlocked + 2 * day),
            ("Legendary Armaments", "Acquired all legendary armaments", nil),
            ("Elden Lord", "Achieved the \"Elden Lord\" ending", nil),
        ]
        return items.enumerated().map { index, item in
            GameAchievement(id: String(index), name: item.name, description: item.description, iconUrl: "",
                            hidden: false, unlocked: item.unlocked != nil, unlockDate: item.unlocked)
        } + [GameAchievement(id: "hidden", name: "", description: "", iconUrl: "", hidden: true, unlocked: false,
                             unlockDate: nil)]
    }

    func friends(_ connection: FriendsConnectionState, friends: [FriendPresentation]? = nil) {
        let account = FriendsAccountIdentity(storeID: "steam", accountID: "0")
        let state = FriendsPresentationState(
            store: FriendsStoreIdentity(id: "steam", name: L10n.string("ui.store.steam"), logoAssetName: "SteamLogo"),
            account: account, friends: friends ?? Self.friends(of: account), connection: connection,
            lastKnownOnlineCount: connection == .interrupted ? 4 : nil)
        let avatars = FriendsAvatarCache()
        present { close in
            FriendsModal(state: state, avatars: avatars, dismiss: { close() }, retry: {}, signIn: { close() })
                .frame(width: 520, height: 550)
        }
    }

    private static func friends(of account: FriendsAccountIdentity) -> [FriendPresentation] {
        let friends: [(String, FriendPresence, String?)] = [
            ("Mira Okafor", .online, "ELDEN RING"),
            ("Jonas Lindqvist", .online, "Rocket League"),
            ("Ana Sousa", .online, nil),
            ("Theo Marchand", .away, nil),
            ("Kenji Watanabe", .busy, nil),
            ("Lea Brandt", .snooze, nil),
            ("Sam Carter", .offline, nil),
            ("Priya Raman", .offline, nil),
            ("Noah Fischer", .unknown, nil),
        ]
        return friends.enumerated().map { index, friend in
            FriendPresentation(id: FriendIdentity(account: account, friendID: String(index)), name: friend.0,
                               avatarURL: nil, presence: friend.1,
                               activity: friend.2.map { FriendGameActivity(id: $0, name: $0) })
        }
    }

    func store(_ id: String, title: String, url: String) {
        guard let url = URL(string: url) else { return }
        let store = StorePresentation(storeID: id, storeTitle: title) { WebPageStart(url: url) }
        present { close in
            StorefrontStoreModal(store: store, close: close)
                .frame(width: 1000, height: 680)
        }
    }

    static var forceQuitAction: GameAction {
        GameAction(kind: .running(.forceQuit), title: L10n.string("model.game.forceQuit"), isAvailable: true,
                   launches: false)
    }

    static func stopGame(_ message: String, _ stop: String) {
        _ = AppLifecycle.confirmsStoppingGames(running: message, stop: stop)
    }
}

extension View {
    /// Presents the gallery's modals over this view.
    func developerModals(_ modals: DeveloperModals) -> some View {
        modifier(DeveloperModalsPresentation(modals: modals))
    }
}

private struct DeveloperModalsPresentation: ViewModifier {
    @Bindable var modals: DeveloperModals

    func body(content: Content) -> some View {
        content
            .sheet(item: $modals.sheet) { sheet in sheet.content { modals.sheet = nil } }
            .removalConfirmation(modals.confirmsRemoval ?? .uninstall, DeveloperModals.gameName,
                                 isPresented: Binding(get: { modals.confirmsRemoval != nil },
                                                      set: { if !$0 { modals.confirmsRemoval = nil } }),
                                 canRemove: true) {}
            .forceQuitConfirmation(DeveloperModals.gameName, for: DeveloperModals.forceQuitAction,
                                   asking: $modals.forceQuit)
    }
}

/// A Rosetta requirement always waiting; its cancellation closes the sheet.
@MainActor
private final class DeveloperRosettaSessions: RosettaSessions {
    struct Prompt: Equatable {}
    private let close: () -> Void

    init(close: @escaping () -> Void) {
        self.close = close
    }

    func rosettaPrompt(accountId: Int) -> Prompt? { Prompt() }
    func resumeAfterRosetta(_ prompt: Prompt) async throws { throw DeveloperModalFailure("Rosetta is not ready") }
    func cancelRosetta(_ prompt: Prompt) { close() }
    static func rosettaUnavailable(_ error: Error) -> Bool { false }
}

private struct DeveloperModalFailure: LocalizedError {
    let errorDescription: String?

    init(_ description: String) {
        errorDescription = description
    }
}
#endif
