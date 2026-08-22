import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - StorageSettingsModel

/// Drives the Storage screen: what the server is charging, what this device is
/// holding, and what may safely stop being held.
///
/// Quota and local storage are separate ports for a reason this screen has to
/// keep visible: quota is what the *server* is charging, local storage is what
/// this *device* holds and whether it is safe to stop holding it. Conflating
/// them is how a cache-clearing feature ends up deleting an only copy — so the
/// reclaim action is described in terms of the verify-before-destroy gate,
/// which "releases local bytes only after a `durable` verdict", not in terms of
/// freeing space.
@MainActor
@Observable
public final class StorageSettingsModel {
    public private(set) var phase: SettingsPhase = .loading
    public private(set) var breakdown: LocalStorageBreakdown?
    public private(set) var quota: QuotaStatus?
    public private(set) var cacheBudgetBytes: UInt64?
    /// Bytes the last reclaim actually released. `nil` until one has run.
    public private(set) var lastReclaimedBytes: UInt64?
    public private(set) var isWorking = false

    private let storage: any StoragePort
    private let quotaPort: any QuotaPort
    private let settings: any SettingsPort
    private let connectivity: SettingsConnectivity

    /// The budgets offered, in bytes. A short list rather than a free-text
    /// field: the number is a disk budget, and a text field invites a value
    /// that is either meaningless or unachievable.
    public static let budgetOptions: [UInt64] = [
        4 * gibibyte, 8 * gibibyte, 16 * gibibyte, 24 * gibibyte, 48 * gibibyte, 96 * gibibyte,
    ]

    private static let gibibyte: UInt64 = 1073741824

    public init(
        storage: any StoragePort,
        quota quotaPort: any QuotaPort,
        settings: any SettingsPort,
        connectivity: SettingsConnectivity
    ) {
        self.storage = storage
        self.quotaPort = quotaPort
        self.settings = settings
        self.connectivity = connectivity
    }

    public func load() async {
        phase = .loading
        do {
            breakdown = try await storage.localBreakdown()
            quota = try await quotaPort.status()
            cacheBudgetBytes = try await settings.settings().cacheBudgetBytes
            phase = .ready
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    /// The tiers this device is spending disk on, cheapest rung first.
    public var tiers: [RepresentationTier] {
        RepresentationTier.allCases.sorted()
    }

    public func bytes(for tier: RepresentationTier) -> UInt64 {
        breakdown?.bytesByTier[tier] ?? 0
    }

    /// Bytes that are re-fetchable and therefore safe to evict.
    public var reclaimableBytes: UInt64 { breakdown?.reclaimableBytes ?? 0 }

    /// Bytes this device owns that the server has not confirmed durable.
    ///
    /// Never reclaimable. Surfaced as its own figure so a user who wonders why
    /// clearing the cache did not free much can see the answer.
    public var unreleasedOriginalBytes: UInt64 { breakdown?.unreleasedOriginalBytes ?? 0 }

    public func setCacheBudget(_ bytes: UInt64?) async {
        await perform {
            var current = try await self.settings.settings()
            current.cacheBudgetBytes = bytes
            try await self.settings.update(current)
            self.cacheBudgetBytes = bytes
        }
    }

    /// Evict re-fetchable cached tiers down to the requested saving.
    ///
    /// Never touches a device-owned original that has not been confirmed
    /// durable — that is the port's guarantee, not this model's, and the screen
    /// states it rather than restating it as a promise of its own.
    public func evictCache(targetBytes: UInt64) async {
        await perform {
            self.lastReclaimedBytes = try await self.storage.evictCache(targetBytes: targetBytes)
            self.breakdown = try await self.storage.localBreakdown()
        }
    }

    private func perform(_ work: @escaping () async throws -> Void) async {
        isWorking = true
        defer { isWorking = false }
        do {
            try await work()
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }
}

// MARK: - Presentation

public extension RepresentationTier {
    var titleKey: String {
        switch self {
        case .dominantColour: "ios.settings.storage.tier.dominant_colour"
        case .lqip: "ios.settings.storage.tier.lqip"
        case .thumbnail: "ios.settings.storage.tier.thumbnail"
        case .preview: "ios.settings.storage.tier.preview"
        case .original: "ios.settings.storage.tier.original"
        }
    }
}

public extension QuotaState {
    var titleKey: String {
        switch self {
        case .withinQuota: "ios.settings.storage.quota.within"
        case .softWarning: "ios.settings.storage.quota.soft_warning"
        case .hardExceeded: "ios.settings.storage.quota.hard_exceeded"
        case .graceExpired: "ios.settings.storage.quota.grace_expired"
        case .suspended: "ios.settings.storage.quota.suspended"
        case .unknown: "ios.settings.storage.quota.unknown"
        }
    }

    var tone: SettingsTone {
        switch self {
        case .withinQuota: .positive
        case .softWarning, .hardExceeded: .caution
        case .graceExpired, .suspended: .critical
        case .unknown: .neutral
        }
    }
}
