import AppKit
import Observation
import SwiftUI
import UniformTypeIdentifiers

@MainActor
@Observable
final class AppModel {
    var selection: LibrarySection = .recent {
        didSet { searchText = "" }
    }
    var searchText = ""
    private(set) var games: [LibraryGame] = []
    private(set) var isLoading = false
    var errorMessage: String?
    private(set) var isImporting = false
    let libraryDirectory: String
    let steam: SteamModel
    let epic: EpicModel
    private(set) var activeFriendsAccount: FriendsAccountIdentity?
    var appearance: AppAppearance {
        didSet { defaults.set(appearance.rawValue, forKey: "appearance") }
    }

    @ObservationIgnored private let core: CoreClient
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private var revision = 0

    init(directory: URL = AppIdentity.libraryDirectory, defaults: UserDefaults = .standard, steam: SteamModel? = nil, epic: EpicModel? = nil) {
        libraryDirectory = directory.path
        self.defaults = defaults
        appearance = AppAppearance(rawValue: defaults.string(forKey: "appearance") ?? "system") ?? .system
        core = CoreClient(storageDirectory: directory.path)
        self.steam = steam ?? SteamModel(directory: directory, preferences: defaults)
        self.epic = epic ?? EpicModel(directory: directory)
    }

    /// The Cloud or Rosetta question shown over the whole app, whichever tab is
    /// open. One sheet at a time, in a fixed order.
    enum StorefrontPrompt: Equatable {
        case steamRosetta, steamCloud, epicRosetta, epicCloud
    }

    var presentedPrompt: StorefrontPrompt? {
        if steam.rosetta.prompt != nil { return .steamRosetta }
        if steam.cloud.promptPresented { return .steamCloud }
        if epic.rosetta.prompt != nil { return .epicRosetta }
        if epic.cloud.promptPresented { return .epicCloud }
        return nil
    }

    var presentedFriends: FriendsPresentationState? {
        guard let activeFriendsAccount else { return nil }
        let state = activeFriendsAccount.storeID == "steam" ? steam.friends : epic.friends
        return state?.account == activeFriendsAccount ? state : nil
    }

    func presentFriends(storeID: String) {
        let state = storeID == "steam" ? steam.friends : epic.friends
        guard let state else { return }
        activeFriendsAccount = state.account
        if storeID == "steam" { epic.closeFriends() }
        else { steam.closeFriends() }
    }

    func closeFriends(storeID: String? = nil) {
        if let storeID, activeFriendsAccount?.storeID != storeID { return }
        activeFriendsAccount = nil
        steam.closeFriends()
        epic.closeFriends()
    }

    func validateFriendsAccount() {
        if activeFriendsAccount != nil && presentedFriends == nil { closeFriends() }
    }

    func retryPresentedFriends() {
        if activeFriendsAccount?.storeID == "steam" { steam.retryFriends() }
        else if activeFriendsAccount?.storeID == "epic" { epic.retryFriends() }
    }

    func signInPresentedFriends() {
        if activeFriendsAccount?.storeID == "steam" { steam.signOut() }
        else if activeFriendsAccount?.storeID == "epic" { epic.signOut() }
        closeFriends()
    }

    var filteredGames: [LibraryGame] {
        guard selection == .recent || selection == .other else { return [] }
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        let matching = games.filter { query.isEmpty || $0.title.localizedStandardContains(query) }
        if selection == .other {
            return matching.sorted { $0.title.localizedStandardCompare($1.title) == .orderedAscending }
        }
        return matching.sorted {
            let lhs = $0.lastPlayedAt ?? $0.addedAt
            let rhs = $1.lastPlayedAt ?? $1.addedAt
            return lhs == rhs ? $0.id < $1.id : lhs > rhs
        }
    }

    func refresh() {
        if selection == .steam {
            steam.refresh()
        } else if selection == .epic {
            epic.refresh()
        }
        isLoading = true
        Task { await reload(after: {}, failure: L10n.string("model.app.readLibrary")) }
    }

    func importGame() {
        guard !isImporting else { return }
        let panel = NSOpenPanel()
        panel.title = L10n.string("model.app.addGame")
        panel.message = L10n.string("model.app.chooseGame")
        panel.prompt = L10n.string("model.app.add")
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.treatsFilePackagesAsDirectories = false
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = [.applicationBundle] + [UTType(filenameExtension: "exe")].compactMap { $0 }
        isImporting = true
        panel.begin { [weak self] response in
            guard let self else { return }
            guard response == .OK, let url = panel.url else {
                self.isImporting = false
                return
            }
            Task { await self.addGame(at: url) }
        }
    }

    func addGame(at url: URL) async {
        isImporting = true
        defer { isImporting = false }
        let path = url.path
        if await reload(after: { _ = try await self.core.addLocalGame(path: path) }, failure: L10n.string("model.app.addFailed")) {
            selection = .other
        }
    }

    func removeGame(_ game: LibraryGame) {
        let id = game.id
        Task { await reload(after: { try await self.core.removeGame(id: id) }, failure: L10n.string("model.app.removeFailed")) }
    }

    /// Runs `mutation`, then reloads the catalogue. Only the newest call may
    /// publish its result, so an older reload cannot overwrite a newer one.
    /// Returns whether this call's result was published.
    @discardableResult
    private func reload(after mutation: () async throws -> Void, failure action: String) async -> Bool {
        revision += 1
        let currentRevision = revision
        do {
            try await mutation()
            let loaded = try await core.games()
            guard currentRevision == revision else { return false }
            games = loaded
            isLoading = false
            return true
        } catch {
            guard currentRevision == revision else { return false }
            isLoading = false
            report(error, action: action)
            return false
        }
    }

    func launchGame(_ game: LibraryGame) {
        guard game.canLaunch else {
            errorMessage = L10n.string("model.app.windowsUnavailable")
            return
        }
        let configuration = NSWorkspace.OpenConfiguration()
        NSWorkspace.shared.openApplication(at: URL(fileURLWithPath: game.path), configuration: configuration) { [weak self] _, error in
            let message = error?.localizedDescription
            Task { @MainActor in
                guard let self else { return }
                if let message {
                    self.errorMessage = L10n.string("model.app.launchFailed", message)
                    return
                }
                do {
                    try await self.core.recordLaunch(id: game.id)
                    self.refresh()
                } catch {
                    self.report(error, action: L10n.string("model.app.historyFailed"))
                }
            }
        }
    }

    private func report(_ error: Error, action: String) {
        errorMessage = L10n.string("model.app.actionFailed", action, error.localizedDescription)
    }
}
