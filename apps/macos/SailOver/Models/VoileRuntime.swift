import Foundation
import Observation

/// Voile, the Wine runtime Windows games run in. SailOver installs the release
/// it pins on the first Windows launch, and the launch button shows its download.
@MainActor @Observable
final class VoileRuntime {
    static let live = VoileRuntime(locations: .live)

    /// How much of the release has downloaded while it installs, nil otherwise.
    private(set) var installFraction: Double?
    @ObservationIgnored private let locations: SailOverLocations

    init(locations: SailOverLocations) {
        self.locations = locations
    }

    /// Installs the runtime unless it is in place. Cancelling the calling task
    /// cancels the install.
    func prepare() async throws {
        guard voileRuntime(locations: locations) == nil else { return }
        let install = VoileInstall()
        installFraction = 0
        let progress = Task { [weak self] in
            while !Task.isCancelled {
                let snapshot = install.progress()
                if snapshot.totalBytes > 0 {
                    self?.installFraction = min(1, Double(snapshot.receivedBytes) / Double(snapshot.totalBytes))
                }
                try? await Task.sleep(for: .milliseconds(250))
            }
        }
        defer {
            progress.cancel()
            installFraction = nil
        }
        do {
            let (locations, archive) = (locations, Self.developmentArchive)
            _ = try await withTaskCancellationHandler {
                try await install.run(locations: locations, archive: archive)
            } onCancel: { install.cancel() }
        } catch VoileError.Cancelled {
            throw CancellationError()
        } catch let VoileError.Failure(reason) {
            throw GameLaunchFailure.wineUnavailable(reason: reason)
        }
    }

    /// A local copy of the pinned release, which replaces its download in
    /// development builds while the release is private.
    private static var developmentArchive: String? {
        #if DEBUG
        ProcessInfo.processInfo.environment["SAILOVER_VOILE_ARCHIVE"].flatMap { $0.isEmpty ? nil : $0 }
        #else
        nil
        #endif
    }
}
