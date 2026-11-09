package com.justin13888.capsule.screens.transfer

import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.BlobHash
import com.justin13888.capsule.screens.domain.StampedTransferEvent
import com.justin13888.capsule.screens.domain.TransferEvent
import com.justin13888.capsule.screens.domain.TransferEventPort
import com.justin13888.capsule.screens.domain.TransferFeedItem
import com.justin13888.capsule.screens.domain.TransferRetryReason
import com.justin13888.capsule.screens.domain.UploadId
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import uniffi.capsule_sdk.FfiRetryReason
import uniffi.capsule_sdk.FfiTransferEvent
import uniffi.capsule_sdk.FfiTransferFeedItem
import uniffi.capsule_sdk.FfiTransferSubscriptionInterface
import java.util.logging.Level
import java.util.logging.Logger

/**
 * [TransferEventPort] over the SDK's transfer event stream: each collection opens one
 * `FfiTransferSubscription` through [subscribe] (`FfiSession::transfer_events`) and
 * pulls it until the session publishing the events is gone.
 *
 * Nothing here reads a clock: the SDK stamps each event as it is received, with the
 * milliseconds since the subscription opened on a monotonic clock, and the stamp is
 * carried over as it is. A subscriber that fell behind reads [TransferFeedItem.Lagged]
 * with the count the SDK says it missed. Unsigned counts and offsets saturate at
 * [Long.MAX_VALUE].
 *
 * Cancelling the collector cancels the pending pull, which drops the SDK's wait and
 * nothing else; the subscription is released when the flow completes either way.
 *
 * Progress is advisory, so a core that cannot be reached - no `libcapsule_sdk.so` or no
 * JNA dispatch library, a [LinkageError] - ends the stream with a logged warning rather
 * than taking down the screen that watches it.
 */
class FfiTransferEventPort(
    private val subscribe: () -> FfiTransferSubscriptionInterface,
) : TransferEventPort {
    override fun events(): Flow<TransferFeedItem> =
        flow {
            val subscription =
                try {
                    subscribe()
                } catch (error: LinkageError) {
                    log.log(Level.WARNING, "transfer events: the core's native library is unavailable", error)
                    return@flow
                }
            log.fine("transfer events: subscription opened")
            try {
                while (true) {
                    val item = subscription.next() ?: break
                    emit(item.toFeedItem())
                }
                log.fine("transfer events: the session publishing them is gone")
            } catch (error: LinkageError) {
                log.log(Level.WARNING, "transfer events: the core's native library is unavailable", error)
            } finally {
                (subscription as? AutoCloseable)?.close()
            }
        }

    companion object {
        private val log: Logger = Logger.getLogger("capsule.transfer")

        /** The domain's item for one pull of the SDK's subscription. */
        fun FfiTransferFeedItem.toFeedItem(): TransferFeedItem =
            when (this) {
                is FfiTransferFeedItem.Event ->
                    TransferFeedItem.Event(StampedTransferEvent(stamped.atMs.saturated(), stamped.event.toEvent()))
                is FfiTransferFeedItem.Lagged -> TransferFeedItem.Lagged(missed.saturated())
            }

        /** The domain's event for the SDK's, field for field. */
        @Suppress("CyclomaticComplexMethod", "LongMethod")
        fun FfiTransferEvent.toEvent(): TransferEvent =
            when (this) {
                is FfiTransferEvent.SessionCreated ->
                    TransferEvent.SessionCreated(
                        BlobHash(blob),
                        UploadId(sessionId),
                        size.saturated(),
                        resumeOffset.saturated(),
                    )
                is FfiTransferEvent.SessionResumed ->
                    TransferEvent.SessionResumed(BlobHash(blob), UploadId(sessionId), size.saturated(), offset.saturated())
                is FfiTransferEvent.ChunkSent ->
                    TransferEvent.ChunkSent(BlobHash(blob), UploadId(sessionId), offset.saturated(), len.saturated())
                is FfiTransferEvent.ChunkAcked ->
                    TransferEvent.ChunkAcked(
                        BlobHash(blob),
                        UploadId(sessionId),
                        offset.saturated(),
                        len.saturated(),
                        ackedOffset.saturated(),
                    )
                is FfiTransferEvent.ChunkRetried ->
                    TransferEvent.ChunkRetried(
                        BlobHash(blob),
                        UploadId(sessionId),
                        offset.saturated(),
                        len.saturated(),
                        reason.toReason(),
                    )
                is FfiTransferEvent.ChecksumRejected ->
                    TransferEvent.ChecksumRejected(BlobHash(blob), UploadId(sessionId), offset.saturated(), len.saturated())
                is FfiTransferEvent.Finalize -> TransferEvent.Finalize(BlobHash(blob), UploadId(sessionId), size.saturated())
                is FfiTransferEvent.Merged -> TransferEvent.Merged(BlobHash(blob), assetRef)
                is FfiTransferEvent.Verify ->
                    TransferEvent.Verify(AssetId(assetId), durable, blobs.saturated(), notDurable.saturated())
                is FfiTransferEvent.Receipt -> TransferEvent.Receipt(AssetId(assetId), BlobHash(blob), receiptSeq.saturated())
                is FfiTransferEvent.RangeRequested ->
                    TransferEvent.RangeRequested(BlobHash(blob), offset.saturated(), maxLen?.saturated())
                is FfiTransferEvent.RangeReceived -> TransferEvent.RangeReceived(BlobHash(blob), offset.saturated(), len.saturated())
                is FfiTransferEvent.FetchRetried ->
                    TransferEvent.FetchRetried(BlobHash(blob), offset.saturated(), stalled, waitMs.saturated())
                is FfiTransferEvent.FetchVerified -> TransferEvent.FetchVerified(BlobHash(blob), size.saturated())
                is FfiTransferEvent.IntegrityRejected -> TransferEvent.IntegrityRejected(BlobHash(blob), size.saturated())
            }

        private fun FfiRetryReason.toReason(): TransferRetryReason =
            when (this) {
                is FfiRetryReason.TransportReset -> TransferRetryReason.TransportReset(waitMs.saturated())
                is FfiRetryReason.OffsetRealign -> TransferRetryReason.OffsetRealign(to.saturated())
                is FfiRetryReason.SessionLost -> TransferRetryReason.SessionLost
                is FfiRetryReason.ServerWait -> TransferRetryReason.ServerWait(waitMs.saturated())
            }

        /** An SDK `u64` as a non-negative [Long], saturating at [Long.MAX_VALUE]. */
        internal fun ULong.saturated(): Long = if (this > Long.MAX_VALUE.toULong()) Long.MAX_VALUE else toLong()
    }
}
