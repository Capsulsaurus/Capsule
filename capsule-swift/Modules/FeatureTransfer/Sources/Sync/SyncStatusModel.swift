import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - SyncStatusModel

/// Drives ``SyncStatusView``.
///
/// The two-week staleness rule is a **product surface, not a bug** (*Download
/// and Synchronization — Notifications*): a mobile OS may grant no background
/// window for days, and a library silently falling out of date defeats the
/// point of keeping content safe elsewhere. So the prompt is surfaced, it is
/// snoozeable, and — critically — it **never blocks anything**. It is a banner
/// on a screen that works without it, never a modal, never a gate.
@MainActor
@Observable
public final class SyncStatusModel {
    /// How long a snooze lasts. The doc's own example: another two weeks.
    public static let snoozeDays = SyncStatus.stalenessThresholdDays

    public private(set) var phase: ScreenPhase = .loading
    public private(set) var status = SyncStatus()
    public private(set) var isSyncing = false
    /// Set when a sync was refused; shown inline, never as a blocking alert.
    public private(set) var lastRefusal: CapsuleError?

    private let sync: any SyncPort
    private let clock: TransferClock
    private nonisolated(unsafe) var observation: Task<Void, Never>?

    public init(sync: any SyncPort, clock: TransferClock = .system) {
        self.sync = sync
        self.clock = clock
    }

    deinit {
        observation?.cancel()
    }

    // MARK: Derived

    public var now: CapsuleTimestamp { clock.now }

    public var connection: ConnectionClass { status.connectionClass }

    /// Whether the staleness prompt belongs on screen.
    ///
    /// Both halves are required — behind by the threshold **and** holding
    /// un-synced changes. A library with nothing to sync is not stale however
    /// long it has been, and warning about it would be noise.
    public var isStale: Bool { status.isStale(at: clock.now) }

    /// Whether the prompt is currently snoozed.
    public var isSnoozed: Bool {
        guard let until = status.staleNotificationSnoozedUntil else { return false }
        return clock.now < until
    }

    /// The point the client has reconciled up to.
    ///
    /// The sync cursor itself is an **opaque token** the client never
    /// interprets (*Download and Synchronization — Discovering What Changed*),
    /// so what a person can be told is the instant it stands at. Rendering the
    /// token would be showing a value with no meaning to anyone.
    public var cursorPosition: CapsuleTimestamp? { status.lastCompletedSyncAt }

    /// Whether a large reconciliation would run right now without a force.
    public var canRunLargeReconciliation: Bool { status.canRunLargeReconciliation }

    /// Outstanding work in both directions.
    public var hasPendingWork: Bool { status.hasPendingWork }

    // MARK: Loading

    public func load() async {
        await reload()
        observeChanges()
    }

    public func reload() async {
        do {
            status = try await sync.status()
            phase = resolvedPhase()
        } catch {
            phase = ScreenPhase.resolve(error, connection: status.connectionClass)
        }
    }

    // MARK: Actions

    /// Reconcile now, **subject to** the metered and Wi-Fi criteria.
    public func synchronizeNow() async {
        await run { try await self.sync.synchronize() }
    }

    /// Reconcile **regardless** of the criteria.
    ///
    /// The one-tap escape hatch offered with the staleness prompt, and never
    /// automatic: it spends the user's data, so it may only ever be something
    /// they chose.
    public func forceSynchronizeNow() async {
        await run { try await self.sync.forceSynchronize() }
    }

    /// Snooze the prompt.
    ///
    /// Suppresses the **warning** only. Auto sync is unaffected — a user who
    /// dismisses a notice has not asked to stop syncing, and conflating the two
    /// would quietly strand their library.
    public func snoozeStalenessPrompt() async {
        do {
            try await sync.snoozeStalenessNotification(until: clock.offset(days: Self.snoozeDays))
            await reload()
        } catch let error as CapsuleError {
            lastRefusal = error
        } catch {
            phase = ScreenPhase.resolve(error, connection: status.connectionClass)
        }
    }

    // MARK: Internals

    private func run(_ operation: @escaping () async throws -> Void) async {
        isSyncing = true
        lastRefusal = nil
        defer { isSyncing = false }
        do {
            try await operation()
            await reload()
        } catch let error as CapsuleError {
            // A refusal is reported inline. The screen keeps working: every
            // local read still answers, which is the offline-first contract.
            lastRefusal = error
            await reload()
        } catch {
            phase = ScreenPhase.resolve(error, connection: status.connectionClass)
        }
    }

    private func resolvedPhase() -> ScreenPhase {
        guard status.connectionClass.isUsable else { return .offline }
        return .ready
    }

    private func observeChanges() {
        observation?.cancel()
        let stream = sync.changes()
        observation = Task { [weak self] in
            for await latest in stream {
                guard !Task.isCancelled else { return }
                await self?.apply(latest)
            }
        }
    }

    private func apply(_ latest: SyncStatus) {
        status = latest
        phase = resolvedPhase()
    }
}
