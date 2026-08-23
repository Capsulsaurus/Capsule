import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import FeatureSettings
import Foundation
import Synchronization

// MARK: - Instants

/// The instant every suite in this target measures against.
///
/// The same anchor `CapsuleMock` uses, so a stub clock and a mock world in one
/// test agree about what time it is. Nothing here calls `Date()`: grace windows
/// and staleness thresholds are differences between two instants, and a test
/// that cannot pin "now" can only assert that a countdown is *some* number.
enum SettingsInstant {
    /// 2026-08-22T12:00:00Z.
    static let reference = CapsuleTimestamp(epochSeconds: 1787400000)

    static func seconds(_ count: Int64) -> CapsuleTimestamp {
        CapsuleTimestamp(epochSeconds: reference.epochSeconds + count)
    }

    static func days(_ count: Int) -> CapsuleTimestamp {
        seconds(Int64(count) * 86400)
    }

    /// A clock stopped at ``reference``.
    static let clock = SettingsClock.fixed(reference)
}

/// The error a stub raises when a screen asks it for something it was not
/// configured to answer.
enum StubError {
    static func failure(_ code: ErrorCode = .syncCursorInvalid) -> CapsuleError {
        CapsuleError(code: code, detail: "stub: configured failure")
    }

    static let unimplemented = CapsuleError(
        code: .unknown("stub.unimplemented"),
        detail: "stub: this screen does not call this method"
    )
}

// MARK: - StubSyncPort

/// A ``SyncPort`` that answers one question: what class of connection is this?
///
/// That is the only thing ``SettingsConnectivity`` reads, and telling "offline"
/// apart from "failed" is the whole reason every settings model holds one.
actor StubSyncPort: SyncPort {
    private let connection: ConnectionClass?

    /// - Parameter connection: `nil` makes even the local read fail, which is
    ///   the case where a screen must **not** claim the user is offline.
    init(connection: ConnectionClass? = .unmetered) {
        self.connection = connection
    }

    func status() async throws -> SyncStatus {
        guard let connection else { throw StubError.failure() }
        return SyncStatus(connectionClass: connection)
    }

    func synchronize() async throws {}

    func forceSynchronize() async throws {}

    func snoozeStalenessNotification(until _: CapsuleTimestamp) async throws {}

    func syncScope() async throws -> SyncScope { .metadataAndThumbnails }

    func setSyncScope(_: SyncScope) async throws {}

    func fetchRepresentation(
        _: RepresentationTier,
        for _: AssetID
    ) async throws -> LocalRepresentations {
        throw StubError.unimplemented
    }

    nonisolated func changes() -> AsyncStream<SyncStatus> {
        AsyncStream { $0.finish() }
    }
}

extension SettingsConnectivity {
    /// A connectivity probe over a stub, so a view model can be built with two
    /// doubles and nothing else.
    static func stub(connection: ConnectionClass? = .unmetered) -> SettingsConnectivity {
        SettingsConnectivity(sync: StubSyncPort(connection: connection))
    }
}

// MARK: - StubSettingsPort

/// A ``SettingsPort`` over an in-memory document.
actor StubSettingsPort: SettingsPort {
    private var document: LibrarySettings
    private var defaultAlbum: AlbumID?
    private var overrides: [ImportScope: AlbumID]
    private let readFailure: CapsuleError?
    private let writeFailure: CapsuleError?

    init(
        document: LibrarySettings = LibrarySettings(),
        defaultAlbum: AlbumID? = nil,
        overrides: [ImportScope: AlbumID] = [:],
        readFailure: CapsuleError? = nil,
        writeFailure: CapsuleError? = nil
    ) {
        self.document = document
        self.defaultAlbum = defaultAlbum
        self.overrides = overrides
        self.readFailure = readFailure
        self.writeFailure = writeFailure
    }

    var storedDocument: LibrarySettings { document }
    var storedDefaultAlbum: AlbumID? { defaultAlbum }

    func settings() async throws -> LibrarySettings {
        if let readFailure { throw readFailure }
        return document
    }

    func update(_ settings: LibrarySettings) async throws {
        if let writeFailure { throw writeFailure }
        document = settings
    }

    func defaultAlbumID() async throws -> AlbumID? {
        if let readFailure { throw readFailure }
        return defaultAlbum
    }

    func setDefaultAlbumID(_ identifier: AlbumID) async throws {
        if let writeFailure { throw writeFailure }
        defaultAlbum = identifier
    }

    func scopeOverrides() async throws -> [ImportScope: AlbumID] {
        if let readFailure { throw readFailure }
        return overrides
    }

    func setScopeOverride(_ albumID: AlbumID?, for scope: ImportScope) async throws {
        if let writeFailure { throw writeFailure }
        overrides[scope] = albumID
    }

    nonisolated func changes() -> AsyncStream<LibrarySettings> {
        AsyncStream { $0.finish() }
    }
}

// MARK: - StubQuotaPort

actor StubQuotaPort: QuotaPort {
    private let quota: QuotaStatus?

    init(quota: QuotaStatus? = QuotaStatus(
        used: 118 * 1073741824,
        softLimit: 409 * 1073741824,
        hardLimit: 512 * 1073741824,
        state: .withinQuota
    )) {
        self.quota = quota
    }

    func status() async throws -> QuotaStatus {
        guard let quota else { throw StubError.failure(.quotaExceeded) }
        return quota
    }

    func wouldAdmit(additionalBytes _: UInt64) async throws -> Bool { true }

    nonisolated func changes() -> AsyncStream<QuotaStatus> {
        AsyncStream { $0.finish() }
    }
}

// MARK: - StubLocalAuthenticator

/// The seam over the platform ceremony.
///
/// The ceremony itself is the one part of the Security screen that cannot be
/// exercised in a unit test — `LocalAuthentication` would put a real system
/// sheet on screen — so it is the one part that must be behind a double.
struct StubLocalAuthenticator: LocalAuthenticator {
    enum Outcome: Sendable {
        /// The user authenticated.
        case granted
        /// The user dismissed the sheet. Not an error, and not a success.
        case cancelled
        /// The ceremony itself failed.
        case failed(CapsuleError)
    }

    var method: LocalAuthMethod = .biometric
    var outcome: Outcome = .granted

    func availableMethod() async -> LocalAuthMethod { method }

    func authenticate(reasonKey _: String) async throws -> Bool {
        switch outcome {
        case .granted: true
        case .cancelled: false
        case let .failed(error): throw error
        }
    }
}

// MARK: - StubAuthPort

/// An ``AuthPort`` that reports one state. The settings screens read the state
/// and never drive a sign-in, so that is all it needs to answer.
actor StubAuthPort: AuthPort {
    private var authState: AuthState

    init(state: AuthState = .signedIn(StubAuthPort.account)) {
        authState = state
    }

    static let account = AccountSummary(
        handle: "avery@capsule.example",
        userID: "user-1",
        displayName: "avery",
        homeServer: "capsule.example",
        accountType: .registered
    )

    func state() async -> AuthState { authState }

    func signInLocally(handle _: String) async throws -> AccountSummary {
        throw StubError.unimplemented
    }

    func signInWithIdentityProvider(issuer _: URL) async throws -> AccountSummary {
        throw StubError.unimplemented
    }

    func confirmLocalAuthentication() async throws {
        authState = .signedIn(Self.account)
    }

    func signOut() async throws {
        authState = .signedOut
    }

    nonisolated func changes() -> AsyncStream<AuthState> {
        AsyncStream { $0.finish() }
    }
}

// MARK: - MovableClock

/// A clock a test can wind forward.
///
/// The grace window is the one thing on the Security screen that is genuinely
/// about the passage of time, and the only honest way to test an expiry is to
/// move the clock rather than to wait for it.
final class MovableClock: Sendable {
    private let instant: Mutex<CapsuleTimestamp>

    init(_ start: CapsuleTimestamp = SettingsInstant.reference) {
        instant = Mutex(start)
    }

    /// The clock, in the shape the view models take.
    var settingsClock: SettingsClock {
        SettingsClock { self.instant.withLock { $0 } }
    }

    /// Wind forward by a number of seconds.
    func advance(seconds: Int64) {
        instant.withLock { current in
            current = CapsuleTimestamp(epochSeconds: current.epochSeconds + seconds)
        }
    }
}
