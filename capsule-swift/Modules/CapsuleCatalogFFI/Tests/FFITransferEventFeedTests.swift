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

/// A quiet session's subscription as uniffi 0.31 really presents it: a pull
/// that ignores task cancellation and returns only when the test releases it,
/// the way the generated `uniffiRustCallAsync` returns only when its Rust
/// future does.
private final class CancellationDeafSource: FFITransferEventSource, @unchecked Sendable {
    private let lock = NSLock()
    private var waiting: CheckedContinuation<FfiTransferFeedItem?, Never>?
    private var pending: FfiTransferFeedItem??
    private let pulledContinuation: AsyncStream<Void>.Continuation

    /// Yields once each time a pull starts waiting.
    let pulled: AsyncStream<Void>

    init() {
        (pulled, pulledContinuation) = AsyncStream.makeStream()
    }

    func next() async -> FfiTransferFeedItem? {
        await withCheckedContinuation { continuation in
            lock.lock()
            if let item = pending {
                pending = nil
                lock.unlock()
                continuation.resume(returning: item)
                return
            }
            waiting = continuation
            lock.unlock()
            pulledContinuation.yield()
        }
    }

    /// Answer the waiting pull with `item`, as an event or the session's end would.
    func release(_ item: FfiTransferFeedItem?) {
        lock.lock()
        let continuation = waiting
        waiting = nil
        if continuation == nil { pending = .some(item) }
        lock.unlock()
        continuation?.resume(returning: item)
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

    @Test("cancelling the consumer ends a pull waiting on a quiet session that ignores cancellation")
    func cancellationEndsTheWait() async {
        let source = CancellationDeafSource()
        let feed = FFITransferEventFeed { source }
        let consumer = Task { await collect(feed.events()) }
        var pulls = source.pulled.makeAsyncIterator()
        _ = await pulls.next()

        consumer.cancel()
        let items = await consumer.value

        #expect(items.isEmpty)
        // The abandoned pull answers late; its item reaches nobody.
        source.release(.lagged(missed: 1))
    }

    @Test("a consumer cancelled before it pulls ends without starting a pull")
    func cancelledBeforePull() async {
        let source = ScriptedSource([.lagged(missed: 1)])
        let item = await Task {
            withUnsafeCurrentTask { $0?.cancel() }
            return await FFITransferEventFeed.pull(source)
        }.value

        #expect(item == nil)
        #expect(source.pulls == 0)
    }

    @Test("a stream opened after a cancelled one is served by its own subscription")
    func resubscribesAfterCancellation() async {
        let quiet = CancellationDeafSource()
        let counter = OpenCounter()
        let feed = FFITransferEventFeed {
            counter.increment()
            let source: any FFITransferEventSource = counter.value == 1 ? quiet : ScriptedSource([.lagged(missed: 7)])
            return source
        }
        let first = Task { await collect(feed.events()) }
        var pulls = quiet.pulled.makeAsyncIterator()
        _ = await pulls.next()
        first.cancel()
        _ = await first.value

        let second = await collect(feed.events())

        #expect(second == [.lagged(7)])
        #expect(counter.value == 2)
        quiet.release(nil)
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
