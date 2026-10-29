import CapsuleDomain
import CapsuleFoundation
import Foundation
import Testing

@testable import CapsuleCatalogFFI

/// A scripted subscription: answers its items in order, then `nil` forever,
/// counting every pull.
private final class ScriptedSource: FFITransferEventSource, @unchecked Sendable {
    private let lock = NSLock()
    private var items: [FfiTransferFeedItem]
    private var pullCount = 0

    init(_ items: [FfiTransferFeedItem]) {
        self.items = items
    }

    var pulls: Int {
        lock.withLock { pullCount }
    }

    func next() async -> FfiTransferFeedItem? {
        lock.withLock {
            pullCount += 1
            return items.isEmpty ? nil : items.removeFirst()
        }
    }
}

/// A subscription whose pull waits until its task is cancelled, as a quiet
/// session's does.
private struct QuietSource: FFITransferEventSource {
    func next() async -> FfiTransferFeedItem? {
        while !Task.isCancelled {
            await Task.yield()
        }
        return nil
    }
}

private func collect(_ stream: AsyncStream<TransferFeedItem>) async -> [TransferFeedItem] {
    var items: [TransferFeedItem] = []
    for await item in stream {
        items.append(item)
    }
    return items
}

@Suite("FFITransferEventFeed — the SDK's transfer events as the domain's")
struct FFITransferEventFeedTests {
    private static let blob = "ab12"
    private static let session = "0192f0c4-0000-7000-8000-000000000001"
    private static let asset = "0192f0c4-0000-7000-8000-000000000002"

    /// Every SDK event, one per case, paired with the domain event it must map to.
    private static let pairs: [(FfiTransferEvent, TransferEvent)] = {
        let blob = BlobHash(Self.blob)
        let session = UploadID(Self.session)
        let asset = AssetID.managed(uuid: Self.asset)
        return [
            (
                .sessionCreated(blob: Self.blob, sessionId: Self.session, size: 100, resumeOffset: 10),
                .sessionCreated(blob: blob, session: session, size: 100, resumeOffset: 10)
            ),
            (
                .sessionResumed(blob: Self.blob, sessionId: Self.session, size: 100, offset: 20),
                .sessionResumed(blob: blob, session: session, size: 100, offset: 20)
            ),
            (
                .chunkSent(blob: Self.blob, sessionId: Self.session, offset: 20, len: 30),
                .chunkSent(blob: blob, session: session, offset: 20, length: 30)
            ),
            (
                .chunkAcked(blob: Self.blob, sessionId: Self.session, offset: 20, len: 30, ackedOffset: 50),
                .chunkAcked(blob: blob, session: session, offset: 20, length: 30, ackedOffset: 50)
            ),
            (
                .chunkRetried(
                    blob: Self.blob, sessionId: Self.session, offset: 50, len: 30,
                    reason: .transportReset(waitMs: 250)
                ),
                .chunkRetried(
                    blob: blob, session: session, offset: 50, length: 30,
                    reason: .transportReset(waitMilliseconds: 250)
                )
            ),
            (
                .chunkRetried(blob: Self.blob, sessionId: Self.session, offset: 50, len: 30, reason: .offsetRealign(to: 40)),
                .chunkRetried(blob: blob, session: session, offset: 50, length: 30, reason: .offsetRealign(toOffset: 40))
            ),
            (
                .chunkRetried(blob: Self.blob, sessionId: Self.session, offset: 50, len: 30, reason: .sessionLost),
                .chunkRetried(blob: blob, session: session, offset: 50, length: 30, reason: .sessionLost)
            ),
            (
                .chunkRetried(blob: Self.blob, sessionId: Self.session, offset: 50, len: 30, reason: .serverWait(waitMs: 900)),
                .chunkRetried(
                    blob: blob, session: session, offset: 50, length: 30,
                    reason: .serverWait(waitMilliseconds: 900)
                )
            ),
            (
                .checksumRejected(blob: Self.blob, sessionId: Self.session, offset: 50, len: 30),
                .checksumRejected(blob: blob, session: session, offset: 50, length: 30)
            ),
            (
                .finalize(blob: Self.blob, sessionId: Self.session, size: 100),
                .finalize(blob: blob, session: session, size: 100)
            ),
            (.merged(blob: Self.blob, assetRef: "ref-1"), .merged(blob: blob, assetRef: "ref-1")),
            (
                .verify(assetId: Self.asset, durable: true, blobs: 2, notDurable: 1),
                .verify(asset: asset, durable: true, blobs: 2, notDurable: 1)
            ),
            (
                .receipt(assetId: Self.asset, blob: Self.blob, receiptSeq: 7),
                .receipt(asset: asset, blob: blob, receiptSequence: 7)
            ),
            (
                .rangeRequested(blob: Self.blob, offset: 0, maxLen: 64),
                .rangeRequested(blob: blob, offset: 0, maxLength: 64)
            ),
            (
                .rangeRequested(blob: Self.blob, offset: 64, maxLen: nil),
                .rangeRequested(blob: blob, offset: 64, maxLength: nil)
            ),
            (.rangeReceived(blob: Self.blob, offset: 0, len: 64), .rangeReceived(blob: blob, offset: 0, length: 64)),
            (
                .fetchRetried(blob: Self.blob, offset: 64, stalled: true, waitMs: 500),
                .fetchRetried(blob: blob, offset: 64, stalled: true, waitMilliseconds: 500)
            ),
            (.fetchVerified(blob: Self.blob, size: 128), .fetchVerified(blob: blob, size: 128)),
            (.integrityRejected(blob: Self.blob, size: 128), .integrityRejected(blob: blob, size: 128)),
        ]
    }()

    @Test("every SDK event maps case for case and field for field, stamp included")
    func mapsEveryEvent() async {
        let script = Self.pairs.enumerated().map { index, pair in
            FfiTransferFeedItem.event(stamped: FfiStampedTransferEvent(atMs: UInt64(index) * 10, event: pair.0))
        }
        let feed = FFITransferEventFeed { ScriptedSource(script) }

        let items = await collect(feed.events())

        let expected = Self.pairs.enumerated().map { index, pair in
            TransferFeedItem.event(StampedTransferEvent(atMilliseconds: UInt64(index) * 10, event: pair.1))
        }
        #expect(items == expected)
    }

    @Test("a lag passes through with its count, and the stream carries on after it")
    func passesLagThrough() async {
        let event = FfiTransferEvent.fetchVerified(blob: Self.blob, size: 1)
        let feed = FFITransferEventFeed {
            ScriptedSource([.lagged(missed: 42), .event(stamped: FfiStampedTransferEvent(atMs: 5, event: event))])
        }

        let items = await collect(feed.events())

        #expect(items == [
            .lagged(42),
            .event(StampedTransferEvent(atMilliseconds: 5, event: .fetchVerified(blob: BlobHash(Self.blob), size: 1))),
        ])
    }

    @Test("the stream finishes once the session is gone and never pulls again")
    func finishesWhenSessionGone() async {
        let source = ScriptedSource([.lagged(missed: 1)])
        let feed = FFITransferEventFeed { source }
        var iterator = feed.events().makeAsyncIterator()

        let first = await iterator.next()
        let end = await iterator.next()
        let afterEnd = await iterator.next()

        #expect(first == .lagged(1))
        #expect(end == nil)
        #expect(afterEnd == nil)
        #expect(source.pulls == 2)
    }

    @Test("each stream opens its own subscription, and only when asked for")
    func opensPerStream() async {
        let opened = ScriptedSource([])
        let counter = OpenCounter()
        let feed = FFITransferEventFeed {
            counter.increment()
            return opened
        }
        #expect(counter.value == 0)

        _ = await collect(feed.events())
        _ = await collect(feed.events())

        #expect(counter.value == 2)
    }

    @Test("the feed pulls only as fast as its consumer reads")
    func pullsAtConsumerPace() async {
        let source = ScriptedSource([.lagged(missed: 1), .lagged(missed: 2), .lagged(missed: 3)])
        let feed = FFITransferEventFeed { source }
        var iterator = feed.events().makeAsyncIterator()

        let first = await iterator.next()

        #expect(first == .lagged(1))
        #expect(source.pulls == 1)
    }

    @Test("cancelling the consumer ends a pull that is waiting on a quiet session")
    func cancellationEndsTheWait() async {
        let feed = FFITransferEventFeed { QuietSource() }
        let consumer = Task { await collect(feed.events()) }

        consumer.cancel()
        let items = await consumer.value

        #expect(items.isEmpty)
    }
}

/// Counts how many subscriptions a feed opened.
private final class OpenCounter: @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0

    var value: Int {
        lock.withLock { count }
    }

    func increment() {
        lock.withLock { count += 1 }
    }
}
