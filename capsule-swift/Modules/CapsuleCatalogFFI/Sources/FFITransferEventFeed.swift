import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import os

/// The transfer event stream's log. A pull that returns nothing is the end of
/// a session, which is the one moment of this stream worth an INFO line.
private let transferLog = Logger(subsystem: CapsuleLog.subsystem, category: "transfer-events")

// MARK: - Source seam

/// One open subscription to the SDK's transfer events: the single verb of
/// `FfiTransferSubscription` the feed pulls.
///
/// Narrowed to a protocol so the feed is tested over a scripted source rather
/// than a live session; the generated object conforms as declared.
protocol FFITransferEventSource: Sendable {
    /// The next item, or `nil` once the session publishing events is gone.
    func next() async -> FfiTransferFeedItem?
}

extension FfiTransferSubscription: FFITransferEventSource {}

// MARK: - FFITransferEventFeed

/// The Rust-backed ``TransferEventPort``: `FfiSession.transferEvents()`
/// projected onto the domain's ``TransferFeedItem``.
///
/// Every ``events()`` call opens its own subscription, so each stream is
/// stamped from the moment it was asked for, as the port promises. The stream
/// pulls `next()` only when its consumer asks for an element, so a consumer
/// that stops reading stops pulling and the SDK's bounded channel reports what
/// it missed as ``TransferFeedItem/lagged(_:)`` instead of this side buffering
/// it. The stream finishes when the session is gone.
///
/// Cancelling the consuming task finishes the stream at once, even while a
/// pull is waiting on a quiet session. uniffi's Swift async calls do not
/// observe task cancellation (the generated `uniffiRustCallAsync` never
/// cancels its Rust future), so each pull is raced against the consumer's
/// cancellation here: see ``FFITransferEventFeed/pull(_:)``. The abandoned
/// pull stays pending in Rust until the next event or the session's end, and
/// whatever it then returns is dropped with the subscription.
public struct FFITransferEventFeed: TransferEventPort {
    private let open: @Sendable () -> any FFITransferEventSource

    /// Stream the transfer events `session` publishes.
    public init(session: FfiSession) {
        self.init { session.transferEvents() }
    }

    /// Stream from whatever `open` subscribes to. The seam tests drive.
    init(open: @escaping @Sendable () -> any FFITransferEventSource) {
        self.open = open
    }

    public func events() -> AsyncStream<TransferFeedItem> {
        let source = open()
        transferLog.debug("transfer event subscription opened")
        return AsyncStream(unfolding: {
            guard let item = await Self.pull(source) else {
                if Task.isCancelled {
                    transferLog.debug("transfer event stream ended: consumer cancelled")
                } else {
                    transferLog.info("transfer event stream ended: session gone")
                }
                return nil
            }
            if case let .lagged(missed) = item {
                transferLog.debug("transfer event subscriber lagged, missed=\(missed)")
            }
            return TransferFeedItem(item)
        })
    }

    /// One pull from `source` that returns `nil` as soon as the calling task
    /// is cancelled, whether or not `source.next()` honours cancellation.
    ///
    /// The pull runs in its own task, and whichever settles first - its
    /// result or the caller's cancellation - resumes the caller; the other is
    /// ignored. An item that arrives after cancellation is dropped.
    static func pull(_ source: any FFITransferEventSource) async -> FfiTransferFeedItem? {
        let race = PullRace()
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                guard race.arm(continuation) else { return }
                Task {
                    let item = await source.next()
                    if !race.settle(item), item != nil {
                        transferLog.debug("transfer event dropped: arrived after its consumer cancelled")
                    }
                }
            }
        } onCancel: {
            race.settle(nil)
        }
    }
}

/// The settle-once meeting point of one pull and its caller's cancellation.
///
/// `onCancel` may run before the continuation is armed (a task cancelled
/// before it pulls), so a settlement with no continuation yet is held and
/// delivered by ``arm(_:)``.
private final class PullRace: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<FfiTransferFeedItem?, Never>?
    private var settled = false

    /// Hand over the caller's continuation. `false` when the race was already
    /// settled by cancellation, in which case `continuation` is resumed with
    /// `nil` here and no pull should start.
    func arm(_ continuation: CheckedContinuation<FfiTransferFeedItem?, Never>) -> Bool {
        lock.lock()
        if settled {
            lock.unlock()
            continuation.resume(returning: nil)
            return false
        }
        self.continuation = continuation
        lock.unlock()
        return true
    }

    /// Settle with `item`, resuming the caller if this is the first
    /// settlement. `false` when the race had already been settled.
    @discardableResult
    func settle(_ item: FfiTransferFeedItem?) -> Bool {
        lock.lock()
        guard !settled else {
            lock.unlock()
            return false
        }
        settled = true
        let waiting = continuation
        continuation = nil
        lock.unlock()
        waiting?.resume(returning: item)
        return true
    }
}

// MARK: - Mapping

extension TransferFeedItem {
    /// The domain mirror of one pull.
    init(_ item: FfiTransferFeedItem) {
        switch item {
        case let .event(stamped):
            self = .event(StampedTransferEvent(atMilliseconds: stamped.atMs, event: TransferEvent(stamped.event)))
        case let .lagged(missed):
            self = .lagged(missed)
        }
    }
}

extension TransferEvent {
    /// The domain mirror of one SDK event: case for case and field for field,
    /// with the content address, session id and asset id typed.
    ///
    /// One exhaustive switch over the SDK's fifteen cases: splitting it would
    /// only hide which case maps where, so the complexity rule is waived here.
    init(_ event: FfiTransferEvent) { // swiftlint:disable:this cyclomatic_complexity
        switch event {
        case let .sessionCreated(blob, sessionID, size, resumeOffset):
            self = .sessionCreated(
                blob: BlobHash(blob), session: UploadID(sessionID), size: size, resumeOffset: resumeOffset
            )
        case let .sessionResumed(blob, sessionID, size, offset):
            self = .sessionResumed(blob: BlobHash(blob), session: UploadID(sessionID), size: size, offset: offset)
        case let .chunkSent(blob, sessionID, offset, length):
            self = .chunkSent(blob: BlobHash(blob), session: UploadID(sessionID), offset: offset, length: length)
        case let .chunkAcked(blob, sessionID, offset, length, ackedOffset):
            self = .chunkAcked(
                blob: BlobHash(blob), session: UploadID(sessionID), offset: offset, length: length,
                ackedOffset: ackedOffset
            )
        case let .chunkRetried(blob, sessionID, offset, length, reason):
            self = .chunkRetried(
                blob: BlobHash(blob), session: UploadID(sessionID), offset: offset, length: length,
                reason: TransferRetryReason(reason)
            )
        case let .checksumRejected(blob, sessionID, offset, length):
            self = .checksumRejected(blob: BlobHash(blob), session: UploadID(sessionID), offset: offset, length: length)
        case let .finalize(blob, sessionID, size):
            self = .finalize(blob: BlobHash(blob), session: UploadID(sessionID), size: size)
        case let .merged(blob, assetRef):
            self = .merged(blob: BlobHash(blob), assetRef: assetRef)
        case let .verify(assetID, durable, blobs, notDurable):
            self = .verify(asset: .managed(uuid: assetID), durable: durable, blobs: blobs, notDurable: notDurable)
        case let .receipt(assetID, blob, receiptSequence):
            self = .receipt(asset: .managed(uuid: assetID), blob: BlobHash(blob), receiptSequence: receiptSequence)
        case let .rangeRequested(blob, offset, maxLength):
            self = .rangeRequested(blob: BlobHash(blob), offset: offset, maxLength: maxLength)
        case let .rangeReceived(blob, offset, length):
            self = .rangeReceived(blob: BlobHash(blob), offset: offset, length: length)
        case let .fetchRetried(blob, offset, stalled, waitMilliseconds):
            self = .fetchRetried(
                blob: BlobHash(blob), offset: offset, stalled: stalled, waitMilliseconds: waitMilliseconds
            )
        case let .fetchVerified(blob, size):
            self = .fetchVerified(blob: BlobHash(blob), size: size)
        case let .integrityRejected(blob, size):
            self = .integrityRejected(blob: BlobHash(blob), size: size)
        }
    }
}

extension TransferRetryReason {
    /// The domain mirror of the SDK's retry reason.
    init(_ reason: FfiRetryReason) {
        switch reason {
        case let .transportReset(waitMilliseconds):
            self = .transportReset(waitMilliseconds: waitMilliseconds)
        case let .offsetRealign(offset):
            self = .offsetRealign(toOffset: offset)
        case .sessionLost:
            self = .sessionLost
        case let .serverWait(waitMilliseconds):
            self = .serverWait(waitMilliseconds: waitMilliseconds)
        }
    }
}
