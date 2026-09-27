import Foundation
@testable import SailOver

extension SteamModel {
    /// Rust's installations of the fake session a test signs in, whose
    /// launches `start` hands off and whose Cloud `maintenance` syncs.
    static func installations(
        library: URL,
        maintenance: CloudMaintenanceFixture = CloudMaintenanceFixture(),
        start: @escaping @MainActor (SteamLaunchPlan) async throws -> any GameSupervisor = { _ in
            throw SteamError.Failure(reason: "backend.steam.runtime.startFailed")
        }
    ) -> MakeInstallations {
        { session, credentials, sessions, receipts in
            SteamInstallationsBackend(try! SteamAccountInstallations.withContent(
                content: SessionSteamContent(session, start: start, maintain: maintenance.steam),
                accountId: credentials.steamId,
                sessions: sessions, receipts: receipts.path, locations: .of(library: library)
            ))
        }
    }
}

extension EpicModel {
    /// Rust's installations of the fake session a test signs in, whose
    /// launches `start` hands off and whose Cloud `maintenance` syncs.
    static func installations(
        library: URL,
        maintenance: CloudMaintenanceFixture = CloudMaintenanceFixture(),
        start: @escaping @MainActor (EpicCloudConfiguration) async throws -> any GameSupervisor = { _ in
            throw EpicError.Service(message: "backend.epic.cloud.failed")
        }
    ) -> MakeInstallations {
        { session, account, language, credentials, sessions, receipts in
            EpicInstallationsBackend(try! EpicAccountInstallations.withContent(
                content: SessionEpicContent(session, start: start, maintain: maintenance.epic),
                accountId: account, language: language,
                credentials: credentials, sessions: sessions, receipts: receipts.path, locations: .of(library: library)
            ))
        }
    }
}
