import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - MockTransferEventFeed

/// A recorded transfer event stream, replayed to every subscriber.
///
/// Each item carries its own stamp, so a subscriber folds the script into the
/// same frames on every run: the feed yields the items in order, at once, and
/// finishes. Pacing them to the stamps is the consumer's business, since the
/// board it folds into is driven by the stamps and never by arrival time.
public struct MockTransferEventFeed: TransferEventPort {
    /// The items every subscription yields, in order.
    public let script: [TransferFeedItem]

    public init(script: [TransferFeedItem]) {
        self.script = script
    }

    public func events() -> AsyncStream<TransferFeedItem> {
        let script = script
        return AsyncStream { continuation in
            for item in script {
                continuation.yield(item)
            }
            continuation.finish()
        }
    }
}

// MARK: - Demo script

public extension MockTransferEventFeed {
    /// One upload and one fetch, showing everything the transfer view draws:
    /// a resume over bytes the server already held, an adaptive resize, a
    /// transport retry, a checksum refusal, finalization and a custody
    /// receipt; and a ranged fetch that stalls once before its hash verifies.
    static func demo(seed: UInt64) -> MockTransferEventFeed {
        let mib: UInt64 = 1024 * 1024
        let upload = MockIdentifiers.blobHash(seed: seed, ordinal: 0)
        let fetch = MockIdentifiers.blobHash(seed: seed, ordinal: 1)
        let session = MockIdentifiers.uploadID(seed: seed, ordinal: 0)
        let asset = AssetID.managed(uuid: MockIdentifiers.uploadID(seed: seed, ordinal: 1).rawValue)
        let size = 12 * mib
        let steps: [(UInt64, TransferEvent)] = [
            (0, .sessionCreated(blob: upload, session: session, size: size, resumeOffset: 2 * mib)),
            (40, .chunkSent(blob: upload, session: session, offset: 2 * mib, length: mib)),
            (420, .chunkAcked(blob: upload, session: session, offset: 2 * mib, length: mib, ackedOffset: 3 * mib)),
            (440, .chunkSent(blob: upload, session: session, offset: 3 * mib, length: 2 * mib)),
            (700, .chunkRetried(
                blob: upload, session: session, offset: 3 * mib, length: 2 * mib,
                reason: .transportReset(waitMilliseconds: 250)
            )),
            (960, .chunkSent(blob: upload, session: session, offset: 3 * mib, length: 2 * mib)),
            (1500, .chunkAcked(blob: upload, session: session, offset: 3 * mib, length: 2 * mib, ackedOffset: 5 * mib)),
            (1520, .chunkSent(blob: upload, session: session, offset: 5 * mib, length: 2 * mib)),
            (1900, .checksumRejected(blob: upload, session: session, offset: 5 * mib, length: 2 * mib)),
            (1920, .chunkSent(blob: upload, session: session, offset: 5 * mib, length: 2 * mib)),
            (2400, .chunkAcked(blob: upload, session: session, offset: 5 * mib, length: 2 * mib, ackedOffset: 7 * mib)),
            (2420, .chunkSent(blob: upload, session: session, offset: 7 * mib, length: 5 * mib)),
            (3600, .chunkAcked(blob: upload, session: session, offset: 7 * mib, length: 5 * mib, ackedOffset: size)),
            (3620, .finalize(blob: upload, session: session, size: size)),
            (3900, .verify(asset: asset, durable: true, blobs: 1, notDurable: 0)),
            (3920, .receipt(asset: asset, blob: upload, receiptSequence: 1)),
            (100, .rangeRequested(blob: fetch, offset: 0, maxLength: mib)),
            (600, .rangeReceived(blob: fetch, offset: 0, length: mib)),
            (620, .rangeRequested(blob: fetch, offset: mib, maxLength: mib)),
            (2620, .fetchRetried(blob: fetch, offset: mib, stalled: true, waitMilliseconds: 500)),
            (3120, .rangeRequested(blob: fetch, offset: mib, maxLength: mib)),
            (3500, .rangeReceived(blob: fetch, offset: mib, length: mib / 2)),
            (3520, .fetchVerified(blob: fetch, size: mib + mib / 2)),
        ]
        let ordered = steps.enumerated()
            .sorted { ($0.element.0, $0.offset) < ($1.element.0, $1.offset) }
            .map { TransferFeedItem.event(StampedTransferEvent(atMilliseconds: $0.element.0, event: $0.element.1)) }
        return MockTransferEventFeed(script: ordered)
    }
}
