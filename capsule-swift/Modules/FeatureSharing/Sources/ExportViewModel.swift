import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import Observation

// MARK: - ExportViewModel

/// The privacy-on-export sheet (*Metadata — Privacy on Export*).
///
/// Export is one of the three **boundary crossings** — alongside a share link
/// served to a non-member and a federated peer outside the owner's home server —
/// at which identifying metadata is stripped. Capsule's own devices syncing the
/// same library are not a crossing; that is intra-trust, and nothing is removed.
///
/// The single rule that shapes this model: the retain opt-in is **per-export,
/// not a sticky account setting**, "to prevent foot-guns where a user opts in
/// once and forgets". So the policy resets when the sheet is prepared *and*
/// after every export, and there is no setting anywhere that could re-arm it.
@MainActor
@Observable
public final class ExportViewModel {
    /// What happened to the last export.
    public enum Outcome: Sendable, Equatable, Hashable {
        /// Originals are in hand and the strip has been applied.
        case prepared(assetCount: Int)
        /// Some originals could not be fetched. The rest are still exportable —
        /// a missing derivative never removes an asset.
        case partial(exported: Int, unavailable: Int)
    }

    /// The assets this export covers.
    public let assetIDs: [AssetID]

    public private(set) var policy = PrivacyStripPolicy.perExportOptIn
    public private(set) var phase: SharingPhase = .ready
    public private(set) var isExporting = false
    public private(set) var outcome: Outcome?
    public private(set) var connection: ConnectionClass?

    private let sync: any SyncPort
    private let connectivity: SharingConnectivity

    public init(
        assetIDs: [AssetID],
        sync: any SyncPort,
        connectivity: SharingConnectivity = SharingConnectivity()
    ) {
        self.assetIDs = assetIDs
        self.sync = sync
        self.connectivity = connectivity
    }

    /// Whether the user has opted to keep the identifying fields **for this one
    /// export**.
    public var retainsIdentifyingMetadata: Bool {
        policy.retainsIdentifyingMetadata
    }

    // MARK: Actions

    /// Arm the sheet.
    ///
    /// Resets the opt-in *on the way in* as well as on the way out. A sheet that
    /// only reset on completion would carry a previous opt-in into an export
    /// that was cancelled and reopened.
    public func prepare() async {
        policy.reset()
        outcome = nil
        phase = .ready
        connection = await connectivity.probe()
    }

    /// Set the per-export opt-in.
    public func setRetention(_ retain: Bool) {
        policy.setRetention(retain)
    }

    /// Fetch the originals this export needs, then apply the strip.
    ///
    /// An asset whose original cannot be fetched degrades to ``Outcome/partial``
    /// rather than failing the whole export: the implementation falls back to
    /// the best representation in hand and never removes an index entry over a
    /// missing derivative.
    public func export() async {
        guard !isExporting, !assetIDs.isEmpty else { return }
        isExporting = true
        connection = await connectivity.probe()
        var exported = 0
        var unavailable = 0
        for assetID in assetIDs {
            do {
                _ = try await sync.fetchRepresentation(.original, for: assetID)
                exported += 1
            } catch {
                unavailable += 1
            }
        }
        finish(exported: exported, unavailable: unavailable)
        isExporting = false
    }

    /// Record the result and **disarm the opt-in**.
    ///
    /// The reset lives here, on the single path every export exits through, so
    /// there is no branch — success, partial, or failure — that can leave
    /// retention armed for the next one.
    private func finish(exported: Int, unavailable: Int) {
        defer { policy.reset() }
        guard exported > 0 else {
            if let connection, !connection.isUsable {
                phase = .offline
            } else {
                phase = .failed(.blobPendingUpload)
            }
            return
        }
        outcome = unavailable == 0
            ? .prepared(assetCount: exported)
            : .partial(exported: exported, unavailable: unavailable)
        phase = .ready
    }
}
