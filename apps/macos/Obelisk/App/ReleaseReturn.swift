import CryptoKit
import Darwin
import Foundation
import Security
import Sparkle

/// A version of Obelisk its Sparkle feed lists, as the return to the latest
/// release reads it.
struct FeedRelease: Equatable, Sendable {
    /// Its CFBundleVersion, which Sparkle compares.
    let version: String
    /// The version players read.
    let displayVersion: String
    /// `ReleaseReturn.betaChannel` for a pre-release, else none.
    let channel: String?
    /// Its DMG.
    let url: URL
    let length: UInt64
    /// The base64 EdDSA signature of its DMG, by the private half of
    /// SUPublicEDKey.
    let signature: String
}

extension FeedRelease {
    /// The version `item` names, unless this Mac cannot run it or it has no
    /// signed DMG.
    init?(_ item: SUAppcastItem) {
        guard let url = item.fileURL, item.minimumOperatingSystemVersionIsOK, item.maximumOperatingSystemVersionIsOK,
              let enclosure = item.propertiesDictionary["enclosure"] as? [AnyHashable: Any],
              let signature = enclosure["sparkle:edSignature"] as? String
        else { return nil }
        self.init(version: item.versionString, displayVersion: item.displayVersionString, channel: item.channel,
                  url: url, length: item.contentLength, signature: signature)
    }
}

/// Takes Obelisk back to its latest release once the player stops taking
/// beta releases while a pre-release runs. Sparkle never installs an older
/// version, so Obelisk gets the release itself and checks it as Sparkle
/// checks an update: its DMG's EdDSA signature, then the code signature of
/// the app inside, which must meet Obelisk's own designated requirement. The
/// app waits beside Obelisk, on its volume, and takes its place in one step
/// as Obelisk quits.
enum ReleaseReturn {
    /// The Sparkle channel of pre-releases in Obelisk's feed.
    static let betaChannel = "beta"

    enum Failure: Error, Equatable {
        /// Obelisk cannot replace itself where it is: a folder it may not
        /// write, or a copy macOS runs from elsewhere.
        case unwritable
        case download
        /// The DMG is not the one the feed signed.
        case signature
        /// The app inside is not this Obelisk's release, signed as it is.
        case damaged
    }

    /// Whether `version` names a beta, X.Y.Z.beta<n>: every beta is a
    /// pre-release, though the feed lists only the newest.
    static func isBeta(_ version: String) -> Bool {
        version.contains(".beta")
    }

    /// The newest release `feed` lists, betas left aside.
    static func latestRelease(in feed: [FeedRelease]) -> FeedRelease? {
        feed.filter { $0.channel == nil && !isBeta($0.version) }
            .max { newer($1.version, than: $0.version) }
    }

    /// Whether Sparkle orders version `a` after version `b`.
    static func newer(_ a: String, than b: String) -> Bool {
        SUStandardVersionComparator.default.compareVersion(a, toVersion: b) == .orderedDescending
    }

    /// The release Obelisk running `version` goes back to: the newest
    /// release `feed` lists, once the player takes no beta releases while
    /// `version`, as Obelisk's own signed Info.plist names it, is a beta newer
    /// than that release. A newer release is Sparkle's own update. `floor`,
    /// the newest release a feed ever listed, keeps a feed, which is not
    /// signed, that lists an older one from taking Obelisk back to it.
    static func target(running version: String, in feed: [FeedRelease], takesBetaReleases: Bool,
                       floor: String?) -> FeedRelease? {
        guard !takesBetaReleases, isBeta(version), let latest = latestRelease(in: feed),
              newer(version, than: latest.version), !(floor.map { newer($0, than: latest.version) } ?? false)
        else { return nil }
        return latest
    }

    /// A new folder on `app`'s volume for `stage` to lay the release in,
    /// unless Obelisk cannot replace itself where it is.
    static func folder(replacing app: URL) throws -> URL {
        let files = FileManager.default
        guard files.isWritableFile(atPath: app.path),
              files.isWritableFile(atPath: app.deletingLastPathComponent().path),
              !app.path.contains("/AppTranslocation/")
        else { throw Failure.unwritable }
        return try files.url(for: .itemReplacementDirectory, in: .userDomainMask, appropriateFor: app, create: true)
    }

    /// The app `stage` lays in `folder`, under `app`'s name.
    static func staged(in folder: URL, replacing app: URL) -> URL {
        folder.appending(path: app.lastPathComponent, directoryHint: .isDirectory)
    }

    /// Gets `release`, checks it and lays its app in `folder`, which
    /// `folder(replacing:)` made, as `staged(in:replacing:)`, ready for
    /// `install`. `publicKey` is SUPublicEDKey. A failure leaves `folder` to
    /// `abandon`, the DMG within it.
    static func stage(_ release: FeedRelease, in folder: URL, replacing app: URL, publicKey: String) async throws {
        let files = FileManager.default
        let (downloaded, response) = try await URLSession.shared.download(from: release.url)
        let download = folder.appending(path: "Obelisk.dmg")
        try files.moveItem(at: downloaded, to: download)
        defer { try? files.removeItem(at: download) }
        guard (response as? HTTPURLResponse)?.statusCode == 200 else { throw Failure.download }
        let dmg = try Data(contentsOf: download, options: .alwaysMapped)
        guard UInt64(dmg.count) == release.length, signs(release.signature, dmg, key: publicKey) else {
            throw Failure.signature
        }
        try Task.checkCancellation()

        let mount = mountPoint(in: folder)
        try files.createDirectory(at: mount, withIntermediateDirectories: false)
        // Its signature already proved every byte: no checksum to verify again.
        try await run("/usr/bin/hdiutil", "attach", "-nobrowse", "-readonly", "-noautoopen", "-noverify", "-quiet",
                      "-mountpoint", mount.path, download.path)
        let staged = staged(in: folder, replacing: app)
        let copied: Result<Void, any Error>
        do {
            try await run("/usr/bin/ditto", mount.appending(path: "Obelisk.app").path, staged.path)
            copied = .success(())
        } catch {
            copied = .failure(error)
        }
        try? await run("/usr/bin/hdiutil", "detach", "-force", "-quiet", mount.path)
        try copied.get()
        try? files.removeItem(at: mount)
        guard holds(staged, version: release.version) else { throw Failure.damaged }
        try Task.checkCancellation()
    }

    /// Where `stage` attaches the release's DMG, in its folder.
    private static func mountPoint(in folder: URL) -> URL {
        folder.appending(path: "dmg", directoryHint: .isDirectory)
    }

    /// Removes `folder`, which `folder(replacing:)` made, at once: Obelisk may
    /// be quitting while `stage` still has the DMG attached there.
    static func abandon(_ folder: URL) {
        let mount = mountPoint(in: folder)
        if FileManager.default.fileExists(atPath: mount.appending(path: "Obelisk.app").path) {
            let detach = process("/usr/bin/hdiutil", ["detach", "-force", "-quiet", mount.path])
            if (try? detach.run()) != nil { detach.waitUntilExit() }
        }
        try? FileManager.default.removeItem(at: folder)
    }

    /// Puts the app `stage` laid in `folder` in place of `app` in one step,
    /// then removes the version it replaced with `folder`.
    static func install(from folder: URL, over app: URL) throws {
        guard renamex_np(staged(in: folder, replacing: app).path, app.path, UInt32(RENAME_SWAP)) == 0 else {
            throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
        }
        abandon(folder)
    }

    /// Whether `signature`, base64, is the EdDSA signature of `data` by
    /// `key`, the base64 of an Ed25519 public key, as Sparkle's sign_update
    /// makes it.
    static func signs(_ signature: String, _ data: Data, key: String) -> Bool {
        guard let key = Data(base64Encoded: key), let signature = Data(base64Encoded: signature),
              let publicKey = try? Curve25519.Signing.PublicKey(rawRepresentation: key)
        else { return false }
        return publicKey.isValidSignature(signature, for: data)
    }

    /// Whether `app` is this Obelisk's `version`, validly signed with
    /// Obelisk's own designated requirement.
    private static func holds(_ app: URL, version: String) -> Bool {
        guard let info = NSDictionary(contentsOf: app.appending(path: "Contents/Info.plist")),
              info["CFBundleIdentifier"] as? String == Bundle.main.bundleIdentifier,
              info["CFBundleVersion"] as? String == version
        else { return false }
        var running: SecCode?
        var runningStatic: SecStaticCode?
        var requirement: SecRequirement?
        var code: SecStaticCode?
        guard SecCodeCopySelf([], &running) == errSecSuccess, let running,
              SecCodeCopyStaticCode(running, [], &runningStatic) == errSecSuccess, let runningStatic,
              SecCodeCopyDesignatedRequirement(runningStatic, [], &requirement) == errSecSuccess, let requirement,
              SecStaticCodeCreateWithPath(app as CFURL, [], &code) == errSecSuccess, let code
        else { return false }
        let flags = SecCSFlags(rawValue: kSecCSCheckAllArchitectures | kSecCSCheckNestedCode | kSecCSStrictValidate)
        return SecStaticCodeCheckValidity(code, flags, requirement) == errSecSuccess
    }

    /// `tool` with `arguments`, silent, ready to run.
    private static func process(_ tool: String, _ arguments: [String]) -> Process {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: tool)
        process.arguments = arguments
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        return process
    }

    /// Runs `tool` with `arguments` until it ends, which must be a success.
    private static func run(_ tool: String, _ arguments: String...) async throws {
        let process = process(tool, arguments)
        let status: Int32 = try await withCheckedThrowingContinuation { continuation in
            process.terminationHandler = { continuation.resume(returning: $0.terminationStatus) }
            do {
                try process.run()
            } catch {
                process.terminationHandler = nil
                continuation.resume(throwing: error)
            }
        }
        guard status == 0 else { throw Failure.damaged }
    }
}
