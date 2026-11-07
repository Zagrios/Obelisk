import Foundation

/// A standalone Windows game or application manually added to Obelisk.
struct CustomGame: Identifiable, Codable, Equatable, Sendable {
    let id: String
    var title: String
    var executablePath: String
    var launchArguments: String
    var customCoverPath: String?
    var customBannerPath: String?
    var winePrefixPath: String?
    var runnerPath: String?
    var engine: LaunchEngine
    var graphics: LaunchGraphics
    var rayTracing: Bool
    var performanceHud: Bool
    var renderPercent: UInt32
    var environmentVariables: [String: String]
    var dateAdded: Date
    var lastPlayed: Date?
    var playtimeSeconds: UInt64

    init(id: String = UUID().uuidString,
         title: String,
         executablePath: String,
         launchArguments: String = "",
         customCoverPath: String? = nil,
         customBannerPath: String? = nil,
         winePrefixPath: String? = nil,
         runnerPath: String? = nil,
         engine: LaunchEngine = .rosetta,
         graphics: LaunchGraphics = .d3dMetal,
         rayTracing: Bool = true,
         performanceHud: Bool = false,
         renderPercent: UInt32 = 100,
         environmentVariables: [String: String] = [:],
         dateAdded: Date = .now,
         lastPlayed: Date? = nil,
         playtimeSeconds: UInt64 = 0) {
        self.id = id
        self.title = title
        self.executablePath = executablePath
        self.launchArguments = launchArguments
        self.customCoverPath = customCoverPath
        self.customBannerPath = customBannerPath
        self.winePrefixPath = winePrefixPath
        self.runnerPath = runnerPath
        self.engine = engine
        self.graphics = graphics
        self.rayTracing = rayTracing
        self.performanceHud = performanceHud
        self.renderPercent = renderPercent
        self.environmentVariables = environmentVariables
        self.dateAdded = dateAdded
        self.lastPlayed = lastPlayed
        self.playtimeSeconds = playtimeSeconds
    }

    /// Formatted playtime string (e.g. "2 hrs 15 mins" or "35 mins" or "Not played yet").
    var formattedPlaytime: String {
        if playtimeSeconds == 0 {
            return L10n.string("ui.game.never_played")
        }
        let hours = playtimeSeconds / 3600
        let minutes = (playtimeSeconds % 3600) / 60
        if hours > 0 {
            return "\(hours)h \(minutes)m"
        } else {
            return "\(max(1, minutes))m"
        }
    }
}
