import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - AutoSyncCriterion

/// One of the conditions auto sync waits for.
///
/// The criteria "are strict and scale with the reconciliation amount"
/// (*Download and Synchronization — Synchronization Criteria*): a small
/// reconciliation runs on any non-metered class, a large one waits for
/// unmetered Wi-Fi, and on iOS a very large batch additionally requests
/// `requiresExternalPower`.
///
/// Two of the four are derivable from the connection class this client already
/// has. The other two are **facts about the OS scheduler**, and this layer says
/// "unknown" rather than guessing: auto sync is implemented only where it can
/// be guaranteed to behave, and a settings screen that claimed to know the
/// battery state without asking for it would be inventing the same guarantee.
public enum AutoSyncCriterion: String, Sendable, Equatable, CaseIterable, Identifiable {
    /// Any non-metered class. Gates metadata deltas and a handful of assets.
    case nonMeteredForSmallWork
    /// Unmetered Wi-Fi. Gates bulk uploads and original-tier downloads.
    case unmeteredForBulkWork
    /// External power, which iOS requires for very large batches.
    case externalPowerForLargeBatches
    /// A background window the OS may simply not grant for days — which is
    /// exactly what the two-week staleness prompt exists for.
    case backgroundWindow

    public var id: String { rawValue }

    /// Whether the criterion holds right now.
    public enum Standing: Sendable, Equatable {
        case satisfied
        case notSatisfied
        /// Not observable from this layer.
        case unknown
    }

    public func standing(connection: ConnectionClass) -> Standing {
        switch self {
        case .nonMeteredForSmallWork:
            connection.permitsSmallReconciliation ? .satisfied : .notSatisfied
        case .unmeteredForBulkWork:
            connection.permitsBulkTransfer ? .satisfied : .notSatisfied
        case .externalPowerForLargeBatches, .backgroundWindow:
            .unknown
        }
    }

    public var titleKey: String {
        switch self {
        case .nonMeteredForSmallWork: "app.sync.criterion.non_metered"
        case .unmeteredForBulkWork: "app.sync.criterion.unmetered"
        case .externalPowerForLargeBatches: "app.sync.criterion.external_power"
        case .backgroundWindow: "app.sync.criterion.background_window"
        }
    }

    public var explanationKey: String {
        switch self {
        case .nonMeteredForSmallWork: "app.sync.criterion.non_metered.description"
        case .unmeteredForBulkWork: "app.sync.criterion.unmetered.description"
        case .externalPowerForLargeBatches: "app.sync.criterion.external_power.description"
        case .backgroundWindow: "app.sync.criterion.background_window.description"
        }
    }
}

// MARK: - SyncScopeSettingsModel

/// Drives ``SyncScopeSettingsView``.
///
/// Scope and upload policy are the two settings this screen actually writes.
/// Everything above the configured scope is fetched **lazily, on demand**, and
/// the original is never fetched speculatively unless this device was its
/// uploader (*Download and Synchronization — Synchronization Scope*).
@MainActor
@Observable
public final class SyncScopeSettingsModel {
    public private(set) var phase: ScreenPhase = .loading
    public private(set) var scope: SyncScope = .metadataAndThumbnails
    public private(set) var policy: UploadPolicy = .full
    public private(set) var settings = LibrarySettings()
    public private(set) var connection: ConnectionClass = .unmetered
    /// Set when a write was refused — a scope written by a newer client cannot
    /// be echoed back, and the refusal is shown rather than swallowed.
    public private(set) var lastRefusal: CapsuleError?

    private let sync: any SyncPort
    private let uploads: any UploadPort
    private let settingsPort: any SettingsPort

    public init(sync: any SyncPort, uploads: any UploadPort, settings: any SettingsPort) {
        self.sync = sync
        self.uploads = uploads
        settingsPort = settings
    }

    // MARK: Derived

    /// The criteria, with their current standing.
    public var criteria: [AutoSyncCriterion] { AutoSyncCriterion.allCases }

    public func standing(of criterion: AutoSyncCriterion) -> AutoSyncCriterion.Standing {
        criterion.standing(connection: connection)
    }

    /// The highest tier this scope fetches eagerly. `nil` for a scope written by
    /// a newer client, which this build must not guess at — it fetches nothing
    /// eagerly rather than over-fetching on a metered plan.
    public var eagerTier: RepresentationTier? { scope.eagerTier }

    /// The scopes this build can write. An `unknown` value round-trips
    /// untouched and is never offered as a choice.
    public var selectableScopes: [SyncScope] { SyncScope.knownCases }

    public var selectablePolicies: [UploadPolicy] { UploadPolicy.knownCases }

    // MARK: Loading

    public func load() async {
        await reload()
    }

    public func reload() async {
        do {
            let status = try await sync.status()
            connection = status.connectionClass
            scope = try await sync.syncScope()
            policy = try await uploads.uploadPolicy()
            settings = try await settingsPort.settings()
            phase = connection.isUsable ? .ready : .offline
        } catch {
            phase = ScreenPhase.resolve(error, connection: connection)
        }
    }

    // MARK: Writes

    public func setScope(_ value: SyncScope) async {
        lastRefusal = nil
        do {
            try await sync.setSyncScope(value)
            scope = value
        } catch let error as CapsuleError {
            lastRefusal = error
        } catch {
            phase = ScreenPhase.resolve(error, connection: connection)
        }
    }

    /// Set the upload policy. Client-side session **ordering** only — the
    /// server has no mode branch to switch, so this changes when sessions open
    /// and nothing else.
    public func setPolicy(_ value: UploadPolicy) async {
        lastRefusal = nil
        do {
            try await uploads.setUploadPolicy(value)
            policy = value
        } catch let error as CapsuleError {
            lastRefusal = error
        } catch {
            phase = ScreenPhase.resolve(error, connection: connection)
        }
    }

    public func setAutoSyncEnabled(_ enabled: Bool) async {
        var updated = settings
        updated.autoSyncEnabled = enabled
        await write(updated)
    }

    /// Disabling opts out of the **warning** entirely and does not affect auto
    /// sync itself.
    public func setStalenessNotificationEnabled(_ enabled: Bool) async {
        var updated = settings
        updated.stalenessNotificationEnabled = enabled
        await write(updated)
    }

    private func write(_ updated: LibrarySettings) async {
        do {
            try await settingsPort.update(updated)
            settings = updated
        } catch let error as CapsuleError {
            lastRefusal = error
        } catch {
            phase = ScreenPhase.resolve(error, connection: connection)
        }
    }
}
