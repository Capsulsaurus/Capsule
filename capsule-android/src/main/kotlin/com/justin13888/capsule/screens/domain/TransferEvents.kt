package com.justin13888.capsule.screens.domain

/**
 * Why an upload chunk is sent again, or from a different offset than the last
 * acknowledgement.
 *
 * Mirrors `capsule_sdk::events::RetryReason` case for case, so an adapter over the
 * SDK's event stream maps each one without interpretation.
 */
sealed interface TransferRetryReason {
    /** The connection reset or the request was lost; the same chunk is re-sent after the backoff. */
    data class TransportReset(
        val waitMilliseconds: Long,
    ) : TransferRetryReason

    /** The server named a different offset (`offset_mismatch`); the transfer continues from it. */
    data class OffsetRealign(
        val toOffset: Long,
    ) : TransferRetryReason

    /** The session was gone; a new one is created and the transfer continues from its offset. */
    data object SessionLost : TransferRetryReason

    /**
     * The server asked the client to wait (`rate_limited`, `unavailable`, `storage_full`);
     * afterwards the transfer resumes from the offset `HEAD` reports.
     */
    data class ServerWait(
        val waitMilliseconds: Long,
    ) : TransferRetryReason
}

/**
 * One step of an upload, a custody check, or a ranged fetch, as the SDK took it
 * (*Transfer Protocol*).
 *
 * Mirrors `capsule_sdk::events::TransferEvent`: the SDK publishes one per step, in the
 * order its state machine took them, and a progress surface folds them into what it
 * draws. Offsets and lengths are bytes of ciphertext (non-negative; the SDK's `u64`
 * saturates at [Long.MAX_VALUE] here), `blob` is the content address, and nothing here
 * carries plaintext or key material.
 *
 * Progress is advisory. A transfer never waits on its observer, so a slow subscriber
 * misses events ([TransferFeedItem.Lagged]) and the transfer's result is still whatever
 * its call returned.
 */
sealed interface TransferEvent {
    /** `POST /upload` opened a session; [resumeOffset] is non-zero for an existing one. */
    data class SessionCreated(
        val blob: BlobHash,
        val session: UploadId,
        val size: Long,
        val resumeOffset: Long,
    ) : TransferEvent

    /** An existing session resumed from the offset `HEAD` reported. */
    data class SessionResumed(
        val blob: BlobHash,
        val session: UploadId,
        val size: Long,
        val offset: Long,
    ) : TransferEvent

    /** A chunk left the client. */
    data class ChunkSent(
        val blob: BlobHash,
        val session: UploadId,
        val offset: Long,
        val length: Long,
    ) : TransferEvent

    /** The server acknowledged a chunk; [ackedOffset] is its new received-byte count. */
    data class ChunkAcked(
        val blob: BlobHash,
        val session: UploadId,
        val offset: Long,
        val length: Long,
        val ackedOffset: Long,
    ) : TransferEvent

    /** A chunk will be sent again, or the transfer moves to another offset or session. */
    data class ChunkRetried(
        val blob: BlobHash,
        val session: UploadId,
        val offset: Long,
        val length: Long,
        val reason: TransferRetryReason,
    ) : TransferEvent

    /** The server refused a chunk whose body did not match its checksum; it is re-sent. */
    data class ChecksumRejected(
        val blob: BlobHash,
        val session: UploadId,
        val offset: Long,
        val length: Long,
    ) : TransferEvent

    /** Every declared byte reached the server; it is finalizing the blob. */
    data class Finalize(
        val blob: BlobHash,
        val session: UploadId,
        val size: Long,
    ) : TransferEvent

    /** The server already held the blob: resolved as a merge, nothing sent. */
    data class Merged(
        val blob: BlobHash,
        val assetRef: String,
    ) : TransferEvent

    /** `POST /storage/verify` answered for an asset. */
    data class Verify(
        val asset: AssetId,
        val durable: Boolean,
        val blobs: Long,
        val notDurable: Long,
    ) : TransferEvent

    /** A custody receipt for one blob verified under the pinned attestation keys. */
    data class Receipt(
        val asset: AssetId,
        val blob: BlobHash,
        val receiptSequence: Long,
    ) : TransferEvent

    /** A ranged request left the client; [maxLength] is the window, or `null` for the rest. */
    data class RangeRequested(
        val blob: BlobHash,
        val offset: Long,
        val maxLength: Long?,
    ) : TransferEvent

    /** Bytes of a ranged request landed. */
    data class RangeReceived(
        val blob: BlobHash,
        val offset: Long,
        val length: Long,
    ) : TransferEvent

    /**
     * A ranged request brought nothing; the fetch backs off and resumes from the same
     * offset. [stalled] is set when the no-bytes bound tripped.
     */
    data class FetchRetried(
        val blob: BlobHash,
        val offset: Long,
        val stalled: Boolean,
        val waitMilliseconds: Long,
    ) : TransferEvent

    /** The fetched bytes hashed to the requested content address. */
    data class FetchVerified(
        val blob: BlobHash,
        val size: Long,
    ) : TransferEvent

    /** The fetched bytes did not hash to the requested address and were discarded. */
    data class IntegrityRejected(
        val blob: BlobHash,
        val size: Long,
    ) : TransferEvent
}

/**
 * A transfer event and when it arrived: milliseconds since the observer started
 * watching, on a monotonic clock.
 *
 * The stamp is taken by the adapter that receives the event, so whatever folds the
 * stream never reads a clock, and a recorded stream replays into exactly the frames it
 * drew live.
 */
data class StampedTransferEvent(
    val atMilliseconds: Long,
    val event: TransferEvent,
)

/** What a subscription to the transfer event stream yields. */
sealed interface TransferFeedItem {
    /** The next event. */
    data class Event(
        val stamped: StampedTransferEvent,
    ) : TransferFeedItem

    /** The observer fell behind and [count] events never arrived; only their progress went unseen. */
    data class Lagged(
        val count: Long,
    ) : TransferFeedItem
}
