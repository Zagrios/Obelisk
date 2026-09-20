import AppKit
import Observation
import OSLog

/// Presents a supervisor-owned launch requirement. Only macOS installs Rosetta;
/// only the supervisor decides whether the waiting game can proceed.
@MainActor @Observable
final class SteamRosettaModel {
    private let sessions: SteamGameSessions
    private var accountId: UInt64?
    private var generation = 0
    private var dismissed: SteamGameSessions.RosettaPrompt?
    private var working: SteamGameSessions.RosettaPrompt?
    private var failed: SteamGameSessions.RosettaPrompt?
    @ObservationIgnored private var work: Task<Void, Never>?
    @ObservationIgnored private let installRosetta: @MainActor () async throws -> Void
    @ObservationIgnored private let readinessAttempts: Int
    @ObservationIgnored private let readinessDelay: @MainActor () async throws -> Void
    @ObservationIgnored private let readinessTimeout: Duration
    @ObservationIgnored private let now: @MainActor () -> ContinuousClock.Instant

    init(sessions: SteamGameSessions,
         install: @escaping @MainActor () async throws -> Void = { try await RosettaInstaller.install() },
         readinessAttempts: Int = 600,
         readinessDelay: @escaping @MainActor () async throws -> Void = { try await Task.sleep(for: .seconds(1)) },
         readinessTimeout: Duration = .seconds(600),
         now: @escaping @MainActor () -> ContinuousClock.Instant = { .now }) {
        self.sessions = sessions
        installRosetta = install
        self.readinessAttempts = max(1, readinessAttempts)
        self.readinessDelay = readinessDelay
        self.readinessTimeout = readinessTimeout
        self.now = now
    }

    var prompt: SteamGameSessions.RosettaPrompt? {
        guard let accountId, let request = sessions.rosettaPrompt(accountId: accountId), request != dismissed else { return nil }
        return request
    }
    var isInstalling: Bool { prompt != nil && prompt == working }
    var installationFailed: Bool { prompt != nil && prompt == failed }

    func connect(accountId: UInt64?) {
        guard self.accountId != accountId else { return }
        cancel()
        self.accountId = accountId
        dismissed = nil
    }

    func cancel() {
        if let prompt {
            sessions.cancelRosetta(prompt)
            dismissed = prompt
        }
        generation += 1
        work?.cancel()
        work = nil
        working = nil
        failed = nil
    }

    func install() {
        guard let request = prompt, !isInstalling else { return }
        generation += 1
        work?.cancel()
        let current = generation
        working = request
        failed = nil
        work = Task {
            let deadline = now().advanced(by: readinessTimeout)
            // LaunchServices can return before Rosetta is usable, or fail to
            // reopen its helper after installation. Its callback only proves
            // cancellation; readiness always comes from the Rust supervisor.
            let opening = Task {
                do {
                    try Task.checkCancellation()
                    try await installRosetta()
                } catch {
                    guard current == generation, prompt == request, !Task.isCancelled else { return }
                    if RosettaInstaller.wasCancelled(error) { cancel() }
                }
            }
            defer {
                opening.cancel()
                if current == generation, working == request {
                    working = nil
                    work = nil
                }
            }
            do {
                for _ in 0..<readinessAttempts {
                    guard now() < deadline else { break }
                    try await readinessDelay()
                    guard current == generation, prompt == request, !Task.isCancelled else { return }
                    guard now() < deadline else { break }
                    do {
                        try await sessions.resumeAfterRosetta(request)
                        guard current == generation, prompt == request, !Task.isCancelled else { return }
                        dismissed = request
                        RosettaInstaller.logger.notice("Rust confirmed Rosetta readiness")
                        return
                    } catch {
                        guard current == generation, prompt == request, !Task.isCancelled else { return }
                        // A lost session or another launch failure is not
                        // evidence that Rosetta merely needs more time.
                        guard Self.rosettaUnavailable(error) else { throw error }
                    }
                }
                failed = request
            } catch {
                guard current == generation, prompt == request, !Task.isCancelled else { return }
                failed = request
            }
        }
    }

    private static func rosettaUnavailable(_ error: Error) -> Bool {
        if case let SteamError.Failure(reason) = error {
            return reason == "backend.steam.runtime.rosettaUnavailable"
        }
        return false
    }
}

@MainActor
private enum RosettaInstaller {
    static let logger = Logger(subsystem: AppIdentity.storageIdentifier, category: "Rosetta")

    static func install() async throws {
        let helper = Bundle.main.bundleURL.appending(path: "Contents/Helpers/Forge Rosetta.app")
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.promptsUserIfNeeded = true
        configuration.createsNewApplicationInstance = true
        configuration.activates = false
        do {
            _ = try await NSWorkspace.shared.openApplication(at: helper, configuration: configuration)
            logger.notice("LaunchServices Rosetta helper completed")
        } catch {
            let cocoaError = error as NSError
            logger.error("LaunchServices Rosetta helper failed: domain=\(cocoaError.domain, privacy: .public) code=\(cocoaError.code)")
            throw error
        }
    }

    static func wasCancelled(_ error: Error) -> Bool {
        if error is CancellationError { return true }
        let error = error as NSError
        return (error.domain == NSCocoaErrorDomain && error.code == NSUserCancelledError)
            || (error.domain == NSOSStatusErrorDomain && error.code == -128)
    }
}
