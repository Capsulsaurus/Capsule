import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - TransferSegment

/// The three things a transfer centre is asked about.
public enum TransferSegment: String, Sendable, Equatable, CaseIterable, Identifiable {
    /// Bytes leaving this device.
    case uploads
    /// Representations this device is waiting on.
    case downloads
    /// Terminal sessions — the receipts, per *Upload Protocol — Session
    /// Lifetime and Discard*, which survive precisely so a finished transfer is
    /// observable rather than vanished.
    case activity

    public var id: String { rawValue }

    var titleKey: String {
        switch self {
        case .uploads: "ios.transfer.segment.uploads"
        case .downloads: "ios.transfer.segment.downloads"
        case .activity: "ios.transfer.segment.activity"
        }
    }
}

// MARK: - TierProgress

/// One rung of the staged ladder, aggregated across every session on it.
///
/// The ladder is the visual anchor of this screen because it is the product
/// promise of staged uploads (*Download and Synchronization — Upload Tiering*):
/// T0 escaping the device is what turns a drowned phone into a *known* loss.
/// Rendering the three rungs as one bar would hide exactly that.
public struct TierProgress: Sendable, Equatable, Identifiable {
    /// Whether the rung has anything to do, is doing it, or is done.
    public enum Standing: Sendable, Equatable {
        /// No session on this rung. Not the same as "finished".
        case idle
        /// At least one session has bytes outstanding.
        case inFlight
        /// Every session on the rung reached a terminal state.
        case settled
    }

    public var tier: UploadTier
    public var transferredBytes: UInt64
    public var totalBytes: UInt64
    public var sessionCount: Int
    public var standing: Standing

    public var id: UploadTier { tier }

    public init(
        tier: UploadTier,
        transferredBytes: UInt64,
        totalBytes: UInt64,
        sessionCount: Int,
        standing: Standing
    ) {
        self.tier = tier
        self.transferredBytes = transferredBytes
        self.totalBytes = totalBytes
        self.sessionCount = sessionCount
        self.standing = standing
    }

    /// Transferred fraction, 0…1. An idle rung reads zero, never one: nothing
    /// queued is not the same as everything done, and drawing a full arc for it
    /// would claim originals are safe when none have been offered.
    public var fractionComplete: Double {
        totalBytes == 0 ? 0 : min(1, Double(transferredBytes) / Double(totalBytes))
    }

    /// Aggregate every session onto the three rungs, **always returning all
    /// three in ladder order**.
    ///
    /// A rung with no sessions still appears, as ``Standing/idle``. The ladder
    /// is a fixed shape the user learns to read; a ring that grew a third arc
    /// only once originals started would make the missing rung invisible at the
    /// exact moment it matters.
    public static func derive(from sessions: [UploadSession]) -> [TierProgress] {
        UploadTier.ladder.map { tier in
            let onTier = sessions.filter { $0.tier == tier }
            let transferred = onTier.reduce(UInt64.zero) { $0 + min($1.offset, $1.declaredSize) }
            let total = onTier.reduce(UInt64.zero) { $0 + $1.declaredSize }
            let standing: Standing = if onTier.isEmpty {
                .idle
            } else if onTier.allSatisfy(\.state.isTerminal) {
                .settled
            } else {
                .inFlight
            }
            return TierProgress(
                tier: tier,
                transferredBytes: transferred,
                totalBytes: total,
                sessionCount: onTier.count,
                standing: standing
            )
        }
    }
}

// MARK: - TransferRow

/// One asset in flight, as the hub lists it.
///
/// Identified by **capture date and nothing else**. No filename crosses the
/// wire — the manifest carries no path (*Upload Protocol — What Gets
/// Uploaded*) — so a row that showed one would be displaying a value the
/// protocol deliberately never learns. There is no filename field on this type
/// to accidentally bind to.
public struct TransferRow: Sendable, Equatable, Identifiable {
    /// The asset's catalog identifier, as the session reports it.
    public var assetID: AssetID
    /// Capture instant, resolved from the library. Absent while the metadata
    /// for a just-imported asset has not been projected yet.
    public var captureDate: CapsuleTimestamp?
    /// The LQIP dominant colour — the bottom rung of the degrade ladder, and
    /// why a row is never blank (*Download and Synchronization — Tiered,
    /// On-Demand Fetch*).
    public var dominantColour: CapsuleDomain.RGBColor?
    public var mediaType: MediaType?
    /// Every session of this asset's bundle, in ladder order. The tier chips.
    public var sessions: [UploadSession]
    /// Observed rate across the asset's sessions, `nil` until measured.
    public var bytesPerSecond: Double?

    public var id: AssetID { assetID }

    public init(
        assetID: AssetID,
        captureDate: CapsuleTimestamp? = nil,
        dominantColour: CapsuleDomain.RGBColor? = nil,
        mediaType: MediaType? = nil,
        sessions: [UploadSession],
        bytesPerSecond: Double? = nil
    ) {
        self.assetID = assetID
        self.captureDate = captureDate
        self.dominantColour = dominantColour
        self.mediaType = mediaType
        self.sessions = sessions
        self.bytesPerSecond = bytesPerSecond
    }

    /// The tiers this asset has sessions on, in ladder order.
    public var tiers: [UploadTier] {
        UploadTier.ladder.filter { tier in sessions.contains { $0.tier == tier } }
    }

    /// Bytes moved across the whole bundle over bytes declared.
    public var fractionComplete: Double {
        let total = sessions.reduce(UInt64.zero) { $0 + $1.declaredSize }
        guard total > 0 else { return 0 }
        let moved = sessions.reduce(UInt64.zero) { $0 + min($1.offset, $1.declaredSize) }
        return min(1, Double(moved) / Double(total))
    }

    /// The furthest-along state of the bundle, for the row's status badge.
    public var headlineState: UploadSessionState {
        sessions.first { $0.state == .failedProcessing }?.state
            ?? sessions.first { $0.state == .uploading }?.state
            ?? sessions.first?.state
            ?? .pending
    }

    /// Group sessions into per-asset rows, newest capture first.
    ///
    /// Rows are keyed on the asset rather than the session because a bundle is
    /// what a person recognises: three sessions for one photograph are one
    /// thing happening, not three.
    public static func group(
        _ sessions: [UploadSession],
        assets: [AssetID: LibraryAsset],
        throughput: ThroughputBook
    ) -> [TransferRow] {
        Dictionary(grouping: sessions) { AssetID.managed(uuid: $0.assetID) }
            .map { assetID, group in
                let asset = assets[assetID]
                let rates = group.compactMap { throughput.rate(for: $0.id) }
                return TransferRow(
                    assetID: assetID,
                    captureDate: asset?.captureTime.effectiveCaptureTimestamp,
                    dominantColour: asset?.lqip?.dominantColor,
                    mediaType: asset?.mediaType,
                    sessions: group.sorted { $0.tier < $1.tier },
                    bytesPerSecond: rates.isEmpty ? nil : rates.reduce(0, +)
                )
            }
            .sorted { lhs, rhs in
                let left = lhs.captureDate?.epochSeconds ?? .min
                let right = rhs.captureDate?.epochSeconds ?? .min
                if left != right { return left > right }
                return lhs.assetID.hashValue > rhs.assetID.hashValue
            }
    }
}
