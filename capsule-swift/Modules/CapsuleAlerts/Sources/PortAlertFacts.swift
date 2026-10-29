import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - PortAlertFacts

/// The predicate's input, read from the ports that already hold each fact.
///
/// Nothing new is persisted for alerts: the last completed sync and the
/// un-synced count are ``SyncPort/status()``, the last quota state is
/// ``QuotaPort/status()``, the quarantine and drop counts are their own ports,
/// the recovery cadence is ``RecoveryPort/summary()``, and the one switchable
/// class's disable is ``SettingsPort/settings()``. Each read is independent:
/// one that fails leaves its fact absent — "not known", which raises nothing —
/// rather than failing the whole pass, so an offline quota read never silences
/// the sync alert.
public struct PortAlertFacts: AlertFactsPort {
    private let sync: any SyncPort
    private let quota: any QuotaPort
    private let quarantine: any QuarantinePort
    private let drops: any DropPort
    private let recovery: any RecoveryPort
    private let settings: any SettingsPort

    public init(
        sync: any SyncPort,
        quota: any QuotaPort,
        quarantine: any QuarantinePort,
        drops: any DropPort,
        recovery: any RecoveryPort,
        settings: any SettingsPort
    ) {
        self.sync = sync
        self.quota = quota
        self.quarantine = quarantine
        self.drops = drops
        self.recovery = recovery
        self.settings = settings
    }

    public func facts() async -> NotifyInput {
        var input = NotifyInput()

        if let status = await read("sync", { try await sync.status() }) {
            input.lastCompletedSync = status.lastCompletedSyncAt
            input.unsyncedChanges = status.pendingUploadCount
            if let until = status.staleNotificationSnoozedUntil {
                input.suppressedUntil[.syncStale] = until
            }
        }

        if let status = await read("quota", { try await quota.status() }), status.state.isKnown {
            input.quotaState = status.state
        }

        // Pending drops sit on a quarantine surface too, but they have their own
        // class: counting them in both raises both.
        let held = await read("quarantine") { try await quarantine.itemCount() } ?? 0
        let heldDrops = await read("quarantine drops") {
            try await quarantine.items(
                on: .pendingDropAwaitingAdoption,
                offset: 0,
                limit: PageRequest.defaultLimit
            )
        }.map { Self.count($0) } ?? 0
        input.quarantinePending = max(0, held - heldDrops)

        input.dropsPending = await read("drops") {
            try await drops.pendingDrops(offset: 0, limit: PageRequest.defaultLimit)
        }.map { Self.count($0) } ?? 0

        if let summary = await read("recovery", { try await recovery.summary() }),
           summary.isConfigured,
           let due = summary.verification.nextDueAt {
            input.recoveryNextDue = due
            input.recoverySnoozeBudgetSpent = !summary.verification.canSnooze
            input.recoveryRewrapDue = summary.verification.shouldOfferGuidedRewrap
        }

        if let current = await read("settings", { try await settings.settings() }),
           !current.stalenessNotificationEnabled {
            input.disabled.insert(.syncStale)
        }
        return input
    }

    private static func count(_ page: Page<some Sendable & Equatable>) -> Int {
        page.totalCount ?? page.items.count
    }

    /// One independent read: its value, or `nil` (logged) when it failed.
    private func read<Value: Sendable>(
        _ fact: String,
        _ body: @Sendable () async throws -> Value
    ) async -> Value? {
        do {
            return try await body()
        } catch {
            CapsuleLog.alerts.notice(
                "alert fact \(fact, privacy: .public) unavailable: \(String(describing: error), privacy: .public)"
            )
            return nil
        }
    }
}
