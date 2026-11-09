import CapsuleDomain
import CapsuleFoundation
@testable import FeatureTransfer

/// Builders for the transfer board's tests: one blob, one session, unless a
/// test names others.
enum BoardFixture {
    static let kib: UInt64 = 1024
    static let blob = BlobHash("b10b")
    static let session = UploadID("s1")

    static func at(_ milliseconds: UInt64, _ event: TransferEvent) -> StampedTransferEvent {
        StampedTransferEvent(atMilliseconds: milliseconds, event: event)
    }

    static func created(
        _ size: UInt64,
        resume: UInt64 = 0,
        session: UploadID = session,
        blob: BlobHash = blob
    ) -> TransferEvent {
        .sessionCreated(blob: blob, session: session, size: size, resumeOffset: resume)
    }

    static func sent(_ offset: UInt64, _ length: UInt64) -> TransferEvent {
        .chunkSent(blob: blob, session: session, offset: offset, length: length)
    }

    static func acked(_ offset: UInt64, _ length: UInt64) -> TransferEvent {
        .chunkAcked(blob: blob, session: session, offset: offset, length: length, ackedOffset: offset + length)
    }

    static func retried(_ offset: UInt64, _ length: UInt64, _ reason: TransferRetryReason) -> TransferEvent {
        .chunkRetried(blob: blob, session: session, offset: offset, length: length, reason: reason)
    }

    static func board(_ events: [StampedTransferEvent]) -> TransferBoard {
        var board = TransferBoard()
        for event in events {
            board.apply(event)
        }
        return board
    }
}
