import Foundation

/// A game added by hand: a Windows or Linux program Obelisk runs in Wine or
/// the Linux engine, or an app macOS opens, optionally with a file.
struct CustomGame: Identifiable, Codable, Equatable, Sendable {
    let id: String
    var title: String
    var executablePath: String
    var launchArguments: String
    /// The file an app opens, as Finder's Open With does, such as an
    /// emulator's game. Nil for a program, and for an app opened alone; a
    /// catalog saved before it existed reads as nil.
    var documentPath: String?
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
         documentPath: String? = nil,
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
        self.documentPath = documentPath
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

    /// Whether macOS opens this game as an app rather than Obelisk running a
    /// program: its path names an app bundle, such as `Example Game.app`.
    var isApp: Bool { Self.isApp(path: executablePath) }

    /// The file the app opens, when one is set.
    var document: URL? {
        guard let documentPath, !documentPath.isEmpty else { return nil }
        return URL(fileURLWithPath: documentPath)
    }

    /// Read from the name, so a moved app still reads as one, unless a plain
    /// file has it, such as a program named `Example Game.app`. Not
    /// `UTType(filenameExtension:)`, which takes `app` for data, not a bundle.
    static func isApp(path: String) -> Bool {
        guard URL(fileURLWithPath: path).pathExtension.caseInsensitiveCompare("app") == .orderedSame else {
            return false
        }
        var isDirectory: ObjCBool = false
        return !FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory) || isDirectory.boolValue
    }

    /// The game as saved: an app has no Wine, so what only a program uses,
    /// such as its prefix, runner, engine and graphics options, goes back to
    /// how a new game starts rather than linger. Only for an app that is
    /// there: a program whose file reads like an app only because its disk
    /// is not connected keeps its settings.
    func forgettingProgramSettings() -> CustomGame {
        var isDirectory: ObjCBool = false
        guard isApp, FileManager.default.fileExists(atPath: executablePath, isDirectory: &isDirectory),
              isDirectory.boolValue else { return self }
        var game = self
        let fresh = CustomGame(title: title, executablePath: executablePath)
        game.winePrefixPath = fresh.winePrefixPath
        game.runnerPath = fresh.runnerPath
        game.engine = fresh.engine
        game.graphics = fresh.graphics
        game.rayTracing = fresh.rayTracing
        game.performanceHud = fresh.performanceHud
        game.renderPercent = fresh.renderPercent
        game.environmentVariables = fresh.environmentVariables
        return game
    }
}

// MARK: - Apps

extension CustomGame {
    /// Why the app at this path cannot be added or opened, as a catalog key;
    /// nil when it can. As for a storefront's Mac build, an app whose
    /// executable has no arm64 slice is refused; one whose executable is a
    /// script, or an iPhone or iPad app wrapped for the Mac, is opened as
    /// macOS would.
    static func appProblem(at path: String) -> String? {
        let url = URL(fileURLWithPath: path)
        // An iPhone or iPad app keeps its own bundle inside a wrapper.
        if wrappedBundle(of: url) != nil { return nil }
        guard FileManager.default.fileExists(atPath: url.appendingPathComponent("Contents/Info.plist").path),
              let bundle = Bundle(url: url),
              let executable = bundle.executableURL,
              FileManager.default.isExecutableFile(atPath: executable.path) else {
            return "ui.custom.app_invalid"
        }
        // Not a Mach-O, such as a script: macOS runs it as it is.
        guard let architectures = bundle.executableArchitectures else { return nil }
        let arm64 = NSNumber(value: NSBundleExecutableArchitectureARM64)
        guard architectures.contains(arm64) else {
            return "ui.custom.app_not_native"
        }
        return nil
    }

    /// The bundle an iPhone or iPad app wrapped for the Mac keeps inside its
    /// `Wrapper` folder, which names the app.
    private static func wrappedBundle(of app: URL) -> Bundle? {
        let wrapped = app.appendingPathComponent("WrappedBundle")
        if FileManager.default.fileExists(atPath: wrapped.path) {
            return Bundle(url: wrapped.resolvingSymlinksInPath())
        }
        let wrapper = app.appendingPathComponent("Wrapper")
        let inner = (try? FileManager.default.contentsOfDirectory(at: wrapper, includingPropertiesForKeys: nil)) ?? []
        return inner.first { isApp(path: $0.path) }.flatMap(Bundle.init(url:))
    }

    /// The bundle that describes the app: its own, or the one it wraps.
    private static func describingBundle(at path: String) -> Bundle? {
        let url = URL(fileURLWithPath: path)
        return wrappedBundle(of: url) ?? Bundle(url: url)
    }

    /// The name an app shows for itself: `Example Game` rather than its
    /// file's `Example Game (v1.2)`.
    static func appTitle(at path: String) -> String {
        let url = URL(fileURLWithPath: path)
        let bundle = describingBundle(at: path)
        for key in ["CFBundleDisplayName", "CFBundleName"] {
            if let name = bundle?.object(forInfoDictionaryKey: key) as? String,
               !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                return name
            }
        }
        return url.deletingPathExtension().lastPathComponent
    }
}

extension LaunchEngine: Codable {
    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        let raw = try container.decode(String.self)
        switch raw {
        case "linux": self = .linux
        default: self = .rosetta
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .rosetta: try container.encode("rosetta")
        case .linux: try container.encode("linux")
        }
    }
}

extension LaunchGraphics: Codable {
    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        let raw = try container.decode(String.self)
        switch raw {
        case "dxmt": self = .dxmt
        default: self = .d3dMetal
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .d3dMetal: try container.encode("d3dMetal")
        case .dxmt: try container.encode("dxmt")
        }
    }
}
