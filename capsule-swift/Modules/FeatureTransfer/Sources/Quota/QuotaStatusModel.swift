import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - QuotaRemediation

/// What a user over quota can actually *do*.
///
/// The grace-expired state is explicitly required to be "a discoverable,
/// remediable state (what is full, what to delete) rather than an opaque
/// mid-import error" (*Quota — Scope Decisions: Grace-window UX*). That is why
/// remediations are a modelled list rather than prose in a banner: the screen
/// cannot render the state without also rendering the way out of it.
public enum QuotaRemediation: String, Sendable, Equatable, CaseIterable, Identifiable {
    /// The highest-leverage action: trash is charged at full size until purge.
    case emptyTrash
    /// Cross-link to what is biggest, so "what to delete" is answerable.
    case reviewLargest
    /// The only route out of a suspension, which is an administrative fact no
    /// amount of deleting will lift.
    case contactAdministrator

    public var id: String { rawValue }

    public var titleKey: String {
        switch self {
        case .emptyTrash: "ios.quota.remediation.empty_trash"
        case .reviewLargest: "ios.quota.remediation.review_largest"
        case .contactAdministrator: "ios.quota.remediation.contact_admin"
        }
    }

    public var systemImage: String {
        switch self {
        case .emptyTrash: "trash"
        case .reviewLargest: "chart.bar.doc.horizontal"
        case .contactAdministrator: "person.crop.circle.badge.questionmark"
        }
    }
}

// MARK: - QuotaPermissions

/// What still works in a given quota state.
///
/// Derived from ``QuotaState`` rather than re-decided, because the product
/// promise is that **a user can always delete their way back under quota** —
/// reads, deletes, and restore-from-trash keep working in every state including
/// `graceExpired`. A screen that got this wrong would hide the only exit.
public struct QuotaPermissions: Sendable, Equatable {
    public var newUploads: Bool
    public var metadataGrowth: Bool
    public var reclaimingWrites: Bool

    public init(newUploads: Bool, metadataGrowth: Bool, reclaimingWrites: Bool) {
        self.newUploads = newUploads
        self.metadataGrowth = metadataGrowth
        self.reclaimingWrites = reclaimingWrites
    }

    public static func forState(_ state: QuotaState) -> QuotaPermissions {
        QuotaPermissions(
            newUploads: state.permitsNewUploads,
            metadataGrowth: state.permitsMetadataGrowth,
            reclaimingWrites: state.permitsReclaimingWrites
        )
    }

    /// The remediations a state admits.
    ///
    /// A suspended account is offered the administrator and nothing else:
    /// emptying the trash is a reclaiming write, and reclaiming writes are the
    /// one thing suspension takes away.
    public static func remediations(for state: QuotaState) -> [QuotaRemediation] {
        guard state.warrantsWarning else { return [] }
        guard state.permitsReclaimingWrites else { return [.contactAdministrator] }
        return [.emptyTrash, .reviewLargest]
    }
}

// MARK: - QuotaStatusModel

/// Drives ``QuotaStatusView``.
///
/// Design doc: *Quota — Thresholds and States, Enforcement Points*.
@MainActor
@Observable
public final class QuotaStatusModel {
    public private(set) var phase: ScreenPhase = .loading
    public private(set) var quota = QuotaStatus(used: 0, softLimit: 0, hardLimit: .max, state: .withinQuota)
    public private(set) var breakdown = QuotaCategoryBreakdown(
        segments: [], usedBytes: 0, freeBytes: 0, isEstimated: false
    )
    public private(set) var connection: ConnectionClass = .unmetered

    private let quotaPort: any QuotaPort
    private let storage: any StoragePort
    private let sync: any SyncPort
    private let clock: TransferClock
    private nonisolated(unsafe) var observation: Task<Void, Never>?

    public init(
        quota: any QuotaPort,
        storage: any StoragePort,
        sync: any SyncPort,
        clock: TransferClock = .system
    ) {
        quotaPort = quota
        self.storage = storage
        self.sync = sync
        self.clock = clock
    }

    deinit {
        observation?.cancel()
    }

    // MARK: Derived

    public var permissions: QuotaPermissions { QuotaPermissions.forState(quota.state) }

    public var remediations: [QuotaRemediation] { QuotaPermissions.remediations(for: quota.state) }

    /// Whether a warning banner belongs on screen at all.
    public var warrants: Bool { quota.state.warrantsWarning }

    /// When the grace window closes, if the account is inside one.
    ///
    /// Shown as a deadline rather than a "days remaining" integer so a user who
    /// opens the app on the last day sees a date, not a "1".
    public var graceDeadline: CapsuleTimestamp? {
        guard let since = quota.hardExceededSince, quota.state == .hardExceeded else { return nil }
        let window = Int64(QuotaStatus.defaultGraceWindowDays) * 86400
        return CapsuleTimestamp(epochSeconds: since.epochSeconds + window)
    }

    /// How long the account has been over the hard limit, for the expired state.
    public var overLimitSince: CapsuleTimestamp? { quota.hardExceededSince }

    public var now: CapsuleTimestamp { clock.now }

    // MARK: Loading

    public func load() async {
        await reload()
        observeChanges()
    }

    public func reload() async {
        do {
            connection = try await sync.status().connectionClass
            let latest = try await quotaPort.status()
            let local = try await storage.localBreakdown()
            apply(quota: latest, local: local)
        } catch {
            phase = ScreenPhase.resolve(error, connection: connection)
        }
    }

    private func apply(quota latest: QuotaStatus, local: LocalStorageBreakdown) {
        quota = latest
        breakdown = QuotaCategoryBreakdown.make(quota: latest, local: local)
        phase = resolvedPhase()
    }

    /// A brand-new account with nothing stored is genuinely empty, and saying
    /// "0 of 512 GB" with a stacked bar of nothing reads as broken.
    private func resolvedPhase() -> ScreenPhase {
        guard connection.isUsable else { return .offline }
        return quota.used == 0 && breakdown.segments.isEmpty ? .empty : .ready
    }

    private func observeChanges() {
        observation?.cancel()
        let stream = quotaPort.changes()
        let storage = storage
        observation = Task { [weak self] in
            for await latest in stream {
                guard !Task.isCancelled else { return }
                let local = await (try? storage.localBreakdown()) ?? LocalStorageBreakdown()
                await self?.apply(quota: latest, local: local)
            }
        }
    }
}
