import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - StorageConsumer

/// One line of "what is using this device's disk".
public struct StorageConsumer: Sendable, Equatable, Identifiable {
    public enum Kind: String, Sendable, Equatable {
        case tier
        /// Soft-deleted assets inside their retention window. Counted here
        /// **and** charged against quota — the cross-link matters.
        case trash
        /// Device-owned originals not yet confirmed durable. Exempt from the
        /// automatic sweep; only a `durable` verdict may release them.
        case unreleasedOriginals
    }

    public var kind: Kind
    public var tier: RepresentationTier?
    public var bytes: UInt64
    /// Whether the automatic sweep may touch it.
    public var isExempt: Bool

    public var id: String { "\(kind.rawValue)-\(tier?.rawValue ?? -1)" }

    public init(kind: Kind, tier: RepresentationTier? = nil, bytes: UInt64, isExempt: Bool) {
        self.kind = kind
        self.tier = tier
        self.bytes = bytes
        self.isExempt = isExempt
    }
}

// MARK: - StorageReclamationModel

/// Drives ``StorageReclamationView``.
///
/// This screen is about **local disk**, which is a different question from
/// quota with different remedies: evicting thumbnails saves little and costs
/// re-fetches, while releasing originals already durable on the server saves a
/// lot and costs nothing. Conflating the two is how a cache-clearing feature
/// ends up deleting an only copy, which is why ``StoragePort`` and
/// ``QuotaPort`` are separate ports and these are separate screens.
///
/// Design doc: *Filesystem — Client: Space Recovery*.
@MainActor
@Observable
public final class StorageReclamationModel {
    public private(set) var phase: ScreenPhase = .loading
    public private(set) var breakdown = LocalStorageBreakdown()
    public private(set) var settings = LibrarySettings()
    public private(set) var connection: ConnectionClass = .unmetered
    /// The plan the user is being asked to consent to. `nil` until previewed.
    public private(set) var pendingPlan: EvictionPlan?
    public private(set) var isBusy = false
    /// Bytes freed by the last confirmed sweep, for the confirmation line.
    public private(set) var lastReclaimedBytes: UInt64?

    private let storage: any StoragePort
    private let settingsPort: any SettingsPort
    private let sync: any SyncPort

    public init(storage: any StoragePort, settings: any SettingsPort, sync: any SyncPort) {
        self.storage = storage
        settingsPort = settings
        self.sync = sync
    }

    // MARK: Derived

    /// The cache byte budget, or the reclaimable total when the user has not
    /// set one — a budget slider has to start somewhere honest.
    public var cacheBudgetBytes: UInt64 {
        settings.cacheBudgetBytes ?? breakdown.reclaimableBytes
    }

    /// Whether the user has set a budget at all.
    public var hasExplicitBudget: Bool { settings.cacheBudgetBytes != nil }

    /// How far over budget the reclaimable set is. Zero when inside it.
    public var overBudgetBytes: UInt64 {
        breakdown.reclaimableBytes.subtractingSaturating(cacheBudgetBytes)
    }

    /// Everything using disk, biggest first, with the exempt rows marked.
    ///
    /// Per-asset consumers are deliberately absent: no port exposes a per-asset
    /// byte size, so a "largest photos" list would have to be fabricated.
    public var consumers: [StorageConsumer] {
        var rows = RepresentationTier.allCases.compactMap { tier -> StorageConsumer? in
            let bytes = breakdown.bytesByTier[tier] ?? 0
            guard bytes > 0 else { return nil }
            return StorageConsumer(kind: .tier, tier: tier, bytes: bytes, isExempt: !tier.isReclaimable)
        }
        if breakdown.trashBytes > 0 {
            rows.append(StorageConsumer(kind: .trash, bytes: breakdown.trashBytes, isExempt: false))
        }
        if breakdown.unreleasedOriginalBytes > 0 {
            rows.append(StorageConsumer(
                kind: .unreleasedOriginals,
                bytes: breakdown.unreleasedOriginalBytes,
                isExempt: true
            ))
        }
        return rows.sorted { $0.bytes > $1.bytes }
    }

    /// The exempt bytes, as one number for the pinned-and-exempt section.
    public var exemptBytes: UInt64 { breakdown.unreleasedOriginalBytes }

    // MARK: Loading

    public func load() async {
        await reload()
    }

    public func reload() async {
        do {
            connection = try await sync.status().connectionClass
            breakdown = try await storage.localBreakdown()
            settings = try await settingsPort.settings()
            phase = breakdown.totalBytes == 0 ? .empty : .ready
        } catch {
            phase = ScreenPhase.resolve(error, connection: connection)
        }
    }

    // MARK: Budget

    /// Set the cache budget. Device-local: a budget is about *this* disk and
    /// deliberately does not sync to a Mac with a different one.
    public func setCacheBudget(_ bytes: UInt64) async {
        var updated = settings
        updated.cacheBudgetBytes = bytes
        do {
            try await settingsPort.update(updated)
            settings = updated
            refreshPlanIfPending()
        } catch {
            phase = ScreenPhase.resolve(error, connection: connection)
        }
    }

    // MARK: Reclamation

    /// Compute — and show — what would be evicted, without evicting anything.
    public func previewEviction(targetBytes: UInt64) {
        pendingPlan = EvictionPlan.preview(targetBytes: targetBytes, breakdown: breakdown)
    }

    /// The plan that would bring the reclaimable set back inside budget.
    public func previewEvictionToBudget() {
        previewEviction(targetBytes: overBudgetBytes)
    }

    public func discardPlan() {
        pendingPlan = nil
    }

    /// Carry out the previewed plan and nothing else.
    ///
    /// The port is asked for exactly the byte count the user saw. Evicting more
    /// than was shown — even usefully — would break the consent this screen
    /// exists to obtain.
    public func confirmEviction() async {
        guard let pendingPlan, !pendingPlan.isEmpty else { return }
        isBusy = true
        defer { isBusy = false }
        do {
            lastReclaimedBytes = try await storage.evictCache(targetBytes: pendingPlan.reclaimedBytes)
            self.pendingPlan = nil
            await reload()
        } catch {
            phase = ScreenPhase.resolve(error, connection: connection)
        }
    }

    private func refreshPlanIfPending() {
        guard pendingPlan != nil else { return }
        previewEvictionToBudget()
    }
}
