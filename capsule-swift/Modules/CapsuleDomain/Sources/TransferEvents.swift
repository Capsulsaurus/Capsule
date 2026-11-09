import CapsuleFoundation
import Foundation

// MARK: - TransferRetryReason

/// Why an upload chunk is sent again, or from a different offset than the last
/// acknowledgement.
///
/// Mirrors `capsule_sdk::events::RetryReason` case for case, so an adapter over
/// the SDK's event stream maps each one without interpretation.
public enum TransferRetryReason: Sendable, Equatable, Hashable {
    /// The connection reset or the request was lost; the same chunk is re-sent
    /// after the backoff.
    case transportReset(waitMilliseconds: UInt64)
    /// The server named a different offset (`offset_mismatch`); the transfer
    /// continues from it.
    case offsetRealign(toOffset: UInt64)
    /// The session was gone; a new one is created and the transfer continues
    /// from the offset it reports.
    case sessionLost
    /// The server asked the client to wait (`rate_limited`, `unavailable`,
    /// `storage_full`); afterwards the transfer resumes from the offset `HEAD`
    /// reports.
    case serverWait(waitMilliseconds: UInt64)
}

// MARK: - TransferEvent

/// One step of an upload, a custody check, or a ranged fetch, as the SDK took
/// it (*Transfer Protocol*).
///
/// Mirrors `capsule_sdk::events::TransferEvent`: the SDK publishes one per
/// step, in the order its state machine took them, and a progress surface folds
/// them into what it draws. Offsets and lengths are bytes of ciphertext, `blob`
/// is the content address, and nothing here carries plaintext or key material.
///
/// Progress is advisory. A transfer never waits on its observer, so a slow
/// subscriber misses events (``TransferFeedItem/lagged(_:)``) and the transfer's
/// result is still whatever its call returned.
public enum TransferEvent: Sendable, Equatable, Hashable {
    // MARK: Upload

    /// `POST /upload` opened a session. `resumeOffset` is non-zero when the
    /// server returned an existing session for the same blob.
    case sessionCreated(blob: BlobHash, session: UploadID, size: UInt64, resumeOffset: UInt64)
    /// An existing session resumed from the offset `HEAD` reported.
    case sessionResumed(blob: BlobHash, session: UploadID, size: UInt64, offset: UInt64)
    /// A chunk left the client.
    case chunkSent(blob: BlobHash, session: UploadID, offset: UInt64, length: UInt64)
    /// The server acknowledged a chunk; `ackedOffset` is its new received-byte
    /// count.
    case chunkAcked(blob: BlobHash, session: UploadID, offset: UInt64, length: UInt64, ackedOffset: UInt64)
    /// A chunk will be sent again, or the transfer moves to another offset or
    /// session.
    case chunkRetried(blob: BlobHash, session: UploadID, offset: UInt64, length: UInt64, reason: TransferRetryReason)
    /// The server refused a chunk whose body did not match its checksum; the
    /// same chunk is re-sent.
    case checksumRejected(blob: BlobHash, session: UploadID, offset: UInt64, length: UInt64)
    /// Every declared byte reached the server; it is finalizing the blob.
    case finalize(blob: BlobHash, session: UploadID, size: UInt64)
    /// The server already held the blob: resolved as a merge, nothing sent.
    case merged(blob: BlobHash, assetRef: String)

    // MARK: Custody

    /// `POST /storage/verify` answered for an asset.
    case verify(asset: AssetID, durable: Bool, blobs: UInt64, notDurable: UInt64)
    /// A custody receipt for one blob verified under the pinned attestation
    /// keys.
    case receipt(asset: AssetID, blob: BlobHash, receiptSequence: UInt64)

    // MARK: Fetch

    /// A ranged request left the client. `maxLength` is the bounded window, or
    /// `nil` for the open-ended remainder.
    case rangeRequested(blob: BlobHash, offset: UInt64, maxLength: UInt64?)
    /// Bytes of a ranged request landed.
    case rangeReceived(blob: BlobHash, offset: UInt64, length: UInt64)
    /// A ranged request brought nothing; the fetch backs off and resumes from
    /// the same offset. `stalled` is set when the no-bytes bound tripped.
    case fetchRetried(blob: BlobHash, offset: UInt64, stalled: Bool, waitMilliseconds: UInt64)
    /// The fetched bytes hashed to the requested content address.
    case fetchVerified(blob: BlobHash, size: UInt64)
    /// The fetched bytes did not hash to the requested address and were
    /// discarded.
    case integrityRejected(blob: BlobHash, size: UInt64)
}

// MARK: - StampedTransferEvent

/// A transfer event and when it arrived: milliseconds since the observer
/// started watching, on a monotonic clock.
///
/// The stamp is taken by the adapter that receives the event, so whatever
/// folds the stream never reads a clock, and a recorded stream replays into
/// exactly the frames it drew live.
public struct StampedTransferEvent: Sendable, Equatable, Hashable {
    /// Milliseconds since the observer started watching.
    public var atMilliseconds: UInt64
    /// The event.
    public var event: TransferEvent

    public init(atMilliseconds: UInt64, event: TransferEvent) {
        self.atMilliseconds = atMilliseconds
        self.event = event
    }
}

// MARK: - TransferFeedItem

/// What a subscription to the transfer event stream yields.
public enum TransferFeedItem: Sendable, Equatable, Hashable {
    /// The next event.
    case event(StampedTransferEvent)
    /// The observer fell behind and this many events never arrived. The
    /// transfers carried on; only their progress went unseen.
    case lagged(UInt64)
}
