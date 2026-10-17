import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import Observation

// MARK: - UploadDetailModel

/// Drives ``UploadDetailView`` — one asset's bundle of upload sessions.
///
/// Design docs: *Upload Protocol* (the session state machine, the resumption
/// rule, adaptive chunk sizing, the error taxonomy) and *Download and
/// Synchronization — Upload Tiering* (which tier is gated on which connection).
@MainActor
@Observable
public final class UploadDetailModel {
    // MARK: Observable state

    public private(set) var phase: ScreenPhase = .loading
    /// This asset's sessions, in ladder order.
    public private(set) var sessions: [UploadSession] = []
    /// Failures with their documented recoveries already resolved.
    public private(set) var failures: [UploadFailure] = []
    public private(set) var connection: ConnectionClass = .unmetered
    /// Set when a recovery hits the `426` hard stop, so the view can route to
    /// ``ProtocolUpgradeRequiredView`` instead of offering a button.
    public private(set) var requiresProtocolUpgrade = false

    // MARK: Dependencies

    public let assetID: AssetID
    private let uploads: any UploadPort
    private let sync: any SyncPort
    private let clock: TransferClock
    private var throughput = ThroughputBook()
    private nonisolated(unsafe) var observation: Task<Void, Never>?

    public init(
        assetID: AssetID,
        uploads: any UploadPort,
        sync: any SyncPort,
        clock: TransferClock = .system
    ) {
        self.assetID = assetID
        self.uploads = uploads
        self.sync = sync
        self.clock = clock
    }

    deinit {
        observation?.cancel()
    }

    // MARK: Derived

    /// The rungs of the ladder for this asset alone.
    public var tierProgress: [TierProgress] { TierProgress.derive(from: sessions) }

    /// Observed rate for one session, `nil` until measured.
    public func rate(for id: UploadID) -> Double? { throughput.rate(for: id) }

    /// The authoritative resumption point: the next byte the server expects.
    ///
    /// Resumption is `HEAD`-driven — the server's offset is the truth, and the
    /// local work queue is a rebuildable cache (*Upload Protocol — Idempotency
    /// and Resumption*). The screen therefore shows the **server's** offset,
    /// never a locally-remembered one.
    public func resumptionPoint(for session: UploadSession) -> UInt64 { session.offset }

    /// The adaptive chunk plan for a session, derived from what has actually
    /// moved.
    public func chunkPlan(for session: UploadSession) -> AdaptiveChunkPlan {
        let rate = throughput.rate(for: session.id)
        let sent = min(session.offset, session.declaredSize)
        let starting = AdaptiveChunkPlan.startingSize(declaredSize: session.declaredSize)
        return AdaptiveChunkPlan.make(
            declaredSize: session.declaredSize,
            observedBytesPerSecond: rate,
            bytesSentAtCurrentSize: sent,
            chunksSentAtCurrentSize: starting == 0 ? 0 : Int(sent / starting),
            connection: connection
        )
    }

    /// Whether the tier's session would be allowed to open on this connection.
    public func isGated(_ tier: UploadTier) -> Bool { !tier.canOpen(on: connection) }

    // MARK: Loading

    public func load() async {
        await reload()
        observeChanges()
    }

    public func reload() async {
        do {
            connection = try await sync.status().connectionClass
            await apply(sessions: try uploads.activeSessions())
        } catch {
            phase = ScreenPhase.resolve(error, connection: connection)
        }
    }

    // MARK: Recovery

    /// Carry out a recovery.
    ///
    /// Every branch here is the documented one for the code, and there is
    /// deliberately no fallback that "just retries": a `duplicate_blob` retried
    /// as a transfer would re-upload bytes the server already holds, and a
    /// `426` retried at all would loop forever against a server that will never
    /// accept this build.
    public func recover(_ failure: UploadFailure) async {
        guard failure.option.isAutomatable else {
            requiresProtocolUpgrade = failure.option.requiresProtocolUpgrade
            return
        }
        switch failure.option.action {
        case .recreateSession, .realignViaHead, .resendChunk, .retryWithBackoff, .refreshAndRetry:
            // All four re-derive the queue from server truth, which is exactly
            // what a reload does: sessions are re-read, offsets come back
            // authoritative, and nothing local is trusted.
            await reload()
        case .mergeExistingBlob:
            // A merge is strictly additive — it links the stored blob to the new
            // asset reference and never deletes a blob or rewrites a manifest —
            // so dropping the redundant session is the whole client action.
            await cancel(failure.uploadID)
        case .abortWithUpgrade, .surfaceToUser, .reportAsDefect:
            requiresProtocolUpgrade = failure.option.requiresProtocolUpgrade
        }
    }

    /// Cancel one session. Refused once finalization has begun; the refusal is
    /// shown, not swallowed.
    public func cancel(_ id: UploadID) async {
        do {
            try await uploads.cancelSession(id)
            await reload()
        } catch let error as CapsuleError {
            record(error, for: id)
        } catch {
            phase = ScreenPhase.resolve(error, connection: connection)
        }
    }

    // MARK: Projection

    private func apply(sessions all: [UploadSession]) async {
        guard case let .managed(uuid) = assetID else {
            phase = .empty
            return
        }
        let mine = all.filter { $0.assetID == uuid }.sorted { $0.tier < $1.tier }
        throughput.record(mine, at: clock.now)
        sessions = mine
        failures = mine.compactMap(UploadFailure.fromTerminal) + failures.filter { failure in
            !mine.contains { $0.id == failure.uploadID && $0.state == .failedProcessing }
        }
        requiresProtocolUpgrade = failures.contains { $0.option.requiresProtocolUpgrade }
        phase = resolvedPhase()
    }

    private func record(_ error: CapsuleError, for id: UploadID) {
        let tier = sessions.first { $0.id == id }?.tier ?? .index
        let failure = UploadFailure(uploadID: id, tier: tier, code: error.code)
        failures.removeAll { $0.uploadID == id }
        failures.append(failure)
        requiresProtocolUpgrade = failure.option.requiresProtocolUpgrade
        phase = resolvedPhase()
    }

    private func resolvedPhase() -> ScreenPhase {
        guard connection.isUsable else { return .offline }
        return sessions.isEmpty && failures.isEmpty ? .empty : .ready
    }

    private func observeChanges() {
        observation?.cancel()
        let stream = uploads.changes()
        observation = Task { [weak self] in
            for await sessions in stream {
                guard !Task.isCancelled else { return }
                await self?.apply(sessions: sessions)
            }
        }
    }
}
