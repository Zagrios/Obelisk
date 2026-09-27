import Foundation

#if DEBUG
// A storefront session that is not Rust's own, such as a stand-in a test
// signs in, reaches Rust's installations through these adapters. A signed-in
// Rust session never does.

/// Runs `body` to its end for a caller on a Rust thread, and returns how it ended.
private func blockingResult<T: Sendable>(_ body: @escaping @Sendable () async throws -> T) -> Result<T, Error> {
    let box = BlockingOutcome<T>()
    let semaphore = DispatchSemaphore(value: 0)
    Task {
        let result: Result<T, Error>
        do { result = .success(try await body()) } catch { result = .failure(error) }
        box.set(result)
        semaphore.signal()
    }
    semaphore.wait()
    return box.get()
}

private final class BlockingOutcome<T>: @unchecked Sendable {
    private let lock = NSLock()
    private var value: Result<T, Error>?
    func set(_ value: Result<T, Error>) { lock.withLock { self.value = value } }
    func get() -> Result<T, Error> { lock.withLock { value! } }
}

private func steamError(_ error: Error) -> SteamError {
    switch error {
    case let error as SteamError: error
    case is CancellationError: .Cancelled
    default: .Failure(reason: "\(error)")
    }
}

private func sessionError(_ error: Error) -> GameSessionError {
    switch error {
    case let error as GameSessionError: error
    case is CancellationError: .Cancelled
    case let SteamError.Failure(reason): .Failure(reason: reason)
    case let EpicError.Service(message): .Failure(reason: message)
    case EpicError.Network: .Failure(reason: "backend.epic.network")
    case EpicError.Unauthorized: .Failure(reason: "backend.epic.unauthorized")
    case EpicError.InvalidResponse: .Failure(reason: "backend.epic.invalid_response")
    default: .Failure(reason: "\(error)")
    }
}

private func epicError(_ error: Error) -> EpicError {
    switch error {
    case let error as EpicError: error
    case is CancellationError: .Service(message: "backend.epic.cancelled")
    default: .Service(message: "\(error)")
    }
}

/// A Cloud maintenance sync a test runs in place of a storefront's: it shows
/// progress and asks through `CloudSync`, and ends when it returns.
typealias SteamCloudMaintenanceStandIn = @Sendable (_ appId: UInt32, _ config: SteamCloudConfiguration?,
                                                    _ sync: CloudSync) async throws -> Void
typealias EpicCloudMaintenanceStandIn = @Sendable (_ configuration: EpicCloudConfiguration,
                                                   _ sync: CloudSync) async throws -> Void

/// A Steam session a test drives, as Rust's installations reach it.
/// `start` hands a launch plan to the supervisor the test stands in, and
/// `maintain` runs its Cloud maintenance.
final class SessionSteamContent: SteamContent, @unchecked Sendable {
    private let session: any SteamSessionProtocol
    private let start: @MainActor (SteamLaunchPlan) async throws -> any GameSupervisor
    private let maintain: SteamCloudMaintenanceStandIn

    init(_ session: any SteamSessionProtocol,
         start: @escaping @MainActor (SteamLaunchPlan) async throws -> any GameSupervisor,
         maintain: @escaping SteamCloudMaintenanceStandIn = { _, _, _ in
             throw SteamError.Failure(reason: "backend.steam.cloud.network")
         }) {
        self.session = session
        self.start = start
        self.maintain = maintain
    }

    func download(appId: UInt32, platform: String, language: String, directory: String) throws -> SteamInstallation {
        let session = session
        return try blockingResult {
            try await session.download(appId: appId, platform: platform, language: language, directory: directory)
        }.mapError(steamError).get()
    }
    func downloadProgress() -> GameTransfer? { session.downloadProgress() }
    func cancelDownload() { session.cancelDownload() }
    func launchPlan(appId: UInt32, platform: String, directory: String) throws -> SteamLaunchPlan {
        let session = session
        return try blockingResult {
            try await session.launchPlan(appId: appId, platform: platform, directory: directory)
        }.mapError(steamError).get()
    }
    func handOff(plan: SteamLaunchPlan) throws -> any GameSupervisor {
        let start = start
        return try blockingResult { try await start(plan) }.mapError(sessionError).get()
    }
    func archiveCloud(config: SteamCloudConfiguration) throws {
        let session = session
        try blockingResult { try await session.archiveCloud(config: config) }.mapError(steamError).get()
    }
    func maintainCloud(appId: UInt32, config: SteamCloudConfiguration?, sync: CloudSync) throws {
        let maintain = maintain
        try blockingResult { try await maintain(appId, config, sync) }.mapError(steamError).get()
    }
}

/// An Epic session a test drives, as Rust's installations reach it.
/// `start` hands a build to the supervisor the test stands in, and
/// `maintain` runs its Cloud maintenance.
final class SessionEpicContent: EpicContent, @unchecked Sendable {
    private let session: any EpicSessionProtocol
    private let start: @MainActor (EpicCloudConfiguration) async throws -> any GameSupervisor
    private let maintain: EpicCloudMaintenanceStandIn

    init(_ session: any EpicSessionProtocol,
         start: @escaping @MainActor (EpicCloudConfiguration) async throws -> any GameSupervisor,
         maintain: @escaping EpicCloudMaintenanceStandIn = { _, _ in
             throw EpicError.Service(message: "backend.epic.cloud.failed")
         }) {
        self.session = session
        self.start = start
        self.maintain = maintain
    }

    func prepare() throws {
        let session = session
        _ = try blockingResult { try await session.prepare() }.mapError(epicError).get()
    }
    func download(namespace: String, catalogItemId: String, appName: String, platform: String,
                  directory: String) throws -> EpicInstallation {
        let session = session
        return try blockingResult {
            try await session.download(namespace: namespace, catalogItemId: catalogItemId, appName: appName,
                                       platform: platform, directory: directory)
        }.mapError(epicError).get()
    }
    func downloadProgress() -> GameTransfer? { session.downloadProgress() }
    func cancelDownload() { session.cancelDownload() }
    func handOff(configuration: EpicCloudConfiguration) throws -> any GameSupervisor {
        let start = start
        return try blockingResult { try await start(configuration) }.mapError(sessionError).get()
    }
    func archiveCloud(configuration: EpicCloudConfiguration) throws {
        let session = session
        try blockingResult { try await session.archiveCloud(configuration: configuration) }.mapError(epicError).get()
    }
    func maintainCloud(configuration: EpicCloudConfiguration, sync: CloudSync) throws {
        let maintain = maintain
        try blockingResult { try await maintain(configuration, sync) }.mapError(epicError).get()
    }
}
#endif
