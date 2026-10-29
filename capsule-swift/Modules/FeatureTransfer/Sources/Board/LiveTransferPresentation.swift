import CapsuleDomain
import SwiftUI

// MARK: - Direction

public extension TransferBoard.Direction {
    /// Which way the bytes move, named.
    var badge: TransferBadge {
        switch self {
        case .upload:
            TransferBadge(titleKey: "app.transfer.live.direction.upload", systemImage: "arrow.up.circle", tint: .blue)
        case .fetch:
            TransferBadge(titleKey: "app.transfer.live.direction.fetch", systemImage: "arrow.down.circle", tint: .teal)
        }
    }
}

// MARK: - Phase

public extension TransferBoard.Phase {
    /// Where a transfer is in its life, named.
    var badge: TransferBadge {
        switch self {
        case .active:
            TransferBadge(titleKey: "app.transfer.live.phase.active", systemImage: "arrow.left.arrow.right", tint: .blue)
        case .finalizing:
            TransferBadge(titleKey: "app.transfer.live.phase.finalizing", systemImage: "hourglass", tint: .indigo)
        case .verified:
            TransferBadge(titleKey: "app.transfer.live.phase.verified", systemImage: "checkmark.seal.fill", tint: .green)
        case .merged:
            TransferBadge(titleKey: "app.transfer.live.phase.merged", systemImage: "square.on.square", tint: .secondary)
        case .integrityRejected:
            TransferBadge(
                titleKey: "app.transfer.live.phase.integrity_rejected",
                systemImage: "xmark.octagon.fill",
                tint: .red
            )
        }
    }
}

// MARK: - Cell

public extension TransferBoard.Cell {
    /// How one cell of the chunk strip is filled.
    ///
    /// Colour is never the only signal: the strip carries an accessibility
    /// value, and the row beside it names the phase, the retries and the
    /// refusals in words.
    var tint: Color {
        switch self {
        case .pending: .secondary.opacity(0.25)
        case .held: .gray
        case .acked: .blue
        case .ackedAfterRetry: .orange
        case .ackedAfterReject: .purple
        case .verified: .green
        case .inFlight: .cyan
        case .retrying: .yellow
        case .rejected: .red
        }
    }
}

// MARK: - RetryNote

public extension TransferBoard.RetryNote {
    /// Why the last retry happened, in the user's terms.
    var titleKey: String {
        switch self {
        case let .upload(reason):
            switch reason {
            case .transportReset: "app.transfer.live.retry.transport_reset"
            case .offsetRealign: "app.transfer.live.retry.offset_realign"
            case .sessionLost: "app.transfer.live.retry.session_lost"
            case .serverWait: "app.transfer.live.retry.server_wait"
            }
        case let .fetch(stalled, _):
            stalled ? "app.transfer.live.retry.fetch_stalled" : "app.transfer.live.retry.fetch_empty"
        }
    }
}

// MARK: - Transfer

public extension TransferBoard.Transfer {
    /// What keeps a row the same row while its progress changes: the blob, the
    /// direction, and when the board first saw it (a second upload of the same
    /// blob is a new row).
    struct Identity: Hashable, Sendable {
        public var blob: BlobHash
        public var direction: TransferBoard.Direction
        public var startedMilliseconds: UInt64
    }

    /// This transfer's row identity.
    var identity: Identity {
        Identity(blob: blob, direction: direction, startedMilliseconds: startedMilliseconds)
    }

    /// How much of the blob is done, `0…1`, or `nil` while the size is unknown.
    var fractionDone: Double? {
        guard let size, size > 0 else { return nil }
        return min(Double(done) / Double(size), 1)
    }

    /// Chunk sizes the adaptive strategy used before the current one, newest
    /// last.
    var earlierChunkSizes: [UInt64] {
        Array(chunkSizes.dropLast())
    }
}
