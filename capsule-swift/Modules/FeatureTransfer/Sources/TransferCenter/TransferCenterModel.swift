import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import Observation

// MARK: - TransferCenterModel

/// Drives ``TransferCenterView``.
///
/// Reads three ports and owns no view types, so every derivation on this screen
/// — the tier ladder, the row grouping, the throughput measurement — is
/// assertable without rendering anything.
///
/// Design docs: *Download and Synchronization — Upload Tiering* (the ladder and
/// its connection gates), *Upload Protocol — Session State Machine* (what the
/// row badges mean).
@MainActor
@Observable
public final class TransferCenterModel {
    // MARK: Observable state

    public private(set) var phase: ScreenPhase = .loading
    /// The three rungs, always all three, in ladder order.
    public private(set) var tierProgress: [TierProgress] = TierProgress.derive(from: [])
    /// One row per asset in flight.
    public private(set) var rows: [TransferRow] = []
    /// Terminal sessions — the activity segment.
    public private(set) var settledRows: [TransferRow] = []
    public private(set) var status = SyncStatus()
    public private(set) var policy: UploadPolicy = .full
    /// Aggregate observed rate, `nil` until two samples exist.
    public private(set) var aggregateBytesPerSecond: Double?
    /// Which segment the user is looking at.
    public var segment: TransferSegment = .uploads

    // MARK: Dependencies

    private let uploads: any UploadPort
    private let sync: any SyncPort
    private let library: any LibraryPort
    private let clock: TransferClock
    private var throughput = ThroughputBook()
    // `nonisolated(unsafe)` so `deinit` can cancel without hopping actors; the
    // task itself only ever touches main-actor state.
    private nonisolated(unsafe) var observation: Task<Void, Never>?

    public init(
        uploads: any UploadPort,
        sync: any SyncPort,
        library: any LibraryPort,
        clock: TransferClock = .system
    ) {
        self.uploads = uploads
        self.sync = sync
        self.library = library
        self.clock = clock
    }

    deinit {
        observation?.cancel()
    }

    // MARK: Derived

    /// The connection class the footer chip reports.
    public var connection: ConnectionClass { status.connectionClass }

    /// Server changes this device has not applied yet — the downloads segment's
    /// only quantity, because the sync feed reports a pending count and not a
    /// per-asset queue.
    public var pendingDownloadCount: Int { status.pendingDownloadCount }

    /// Whether a tier would be allowed to open right now, for the ladder's
    /// "waiting for Wi-Fi" annotations.
    public func isGated(_ tier: UploadTier) -> Bool {
        !tier.canOpen(on: status.connectionClass)
    }

    // MARK: Loading

    /// Load and begin observing. Call once, on appear.
    public func load() async {
        await reload()
        observeChanges()
    }

    /// Re-read every port. Also the retry action on the failure and offline
    /// placeholders.
    public func reload() async {
        do {
            let latest = try await sync.status()
            status = latest
            policy = try await uploads.uploadPolicy()
            await apply(sessions: try uploads.activeSessions())
        } catch {
            phase = ScreenPhase.resolve(error, connection: status.connectionClass)
        }
    }

    /// Force every in-flight transfer regardless of the metered and Wi-Fi
    /// criteria. Only ever on the user's explicit say-so — it spends their data.
    public func forceUploads() async {
        let assetIDs = rows.map(\.assetID)
        guard !assetIDs.isEmpty else { return }
        do {
            try await uploads.forceUpload(assetIDs: assetIDs)
            await reload()
        } catch {
            phase = ScreenPhase.resolve(error, connection: status.connectionClass)
        }
    }

    /// Cancel one session. Refused once finalization has begun, and the refusal
    /// is surfaced rather than swallowed.
    public func cancel(_ id: UploadID) async {
        do {
            try await uploads.cancelSession(id)
            await reload()
        } catch {
            phase = ScreenPhase.resolve(error, connection: status.connectionClass)
        }
    }

    // MARK: Projection

    private func apply(sessions: [UploadSession]) async {
        throughput.record(sessions, at: clock.now)
        aggregateBytesPerSecond = throughput.aggregateBytesPerSecond
        tierProgress = TierProgress.derive(from: sessions)
        let assets = await resolveAssets(for: sessions)
        let active = sessions.filter { !$0.state.isTerminal }
        rows = TransferRow.group(active, assets: assets, throughput: throughput)
        settledRows = TransferRow.group(
            sessions.filter(\.state.isTerminal),
            assets: assets,
            throughput: throughput
        )
        phase = resolvedPhase()
    }

    /// Resolve capture dates and LQIP colours in one batched read.
    ///
    /// A miss is not an error: an asset whose metadata has not been projected
    /// yet still gets a row, drawn at the bottom of the degrade ladder.
    private func resolveAssets(for sessions: [UploadSession]) async -> [AssetID: LibraryAsset] {
        let ids = Set(sessions.map { AssetID.managed(uuid: $0.assetID) })
        guard !ids.isEmpty else { return [:] }
        guard let assets = try? await library.assets(for: Array(ids)) else { return [:] }
        return Dictionary(assets.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
    }

    /// Offline is a phase of its own even when rows are present: the list is
    /// perfectly readable, the actions are not available.
    private func resolvedPhase() -> ScreenPhase {
        guard status.connectionClass.isUsable else { return .offline }
        let hasAnything = !rows.isEmpty || !settledRows.isEmpty || status.pendingDownloadCount > 0
        return hasAnything ? .ready : .empty
    }

    private func observeChanges() {
        observation?.cancel()
        let uploadStream = uploads.changes()
        let syncStream = sync.changes()
        observation = Task { [weak self] in
            await withTaskGroup(of: Void.self) { group in
                group.addTask { [weak self] in
                    for await sessions in uploadStream {
                        guard !Task.isCancelled else { return }
                        await self?.apply(sessions: sessions)
                    }
                }
                group.addTask { [weak self] in
                    for await status in syncStream {
                        guard !Task.isCancelled else { return }
                        await self?.applyStatus(status)
                    }
                }
            }
        }
    }

    private func applyStatus(_ latest: SyncStatus) {
        status = latest
        phase = resolvedPhase()
    }
}
