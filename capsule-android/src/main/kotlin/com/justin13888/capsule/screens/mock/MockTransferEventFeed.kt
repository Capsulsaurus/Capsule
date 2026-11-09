package com.justin13888.capsule.screens.mock

import com.justin13888.capsule.screens.domain.StampedTransferEvent
import com.justin13888.capsule.screens.domain.TransferEvent
import com.justin13888.capsule.screens.domain.TransferEventPort
import com.justin13888.capsule.screens.domain.TransferFeedItem
import com.justin13888.capsule.screens.domain.TransferRetryReason
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.asFlow

/**
 * A recorded transfer event stream, replayed to every subscriber.
 *
 * Each item carries its own stamp, so a subscriber folds the script into the same
 * frames on every run: the feed emits the items in order, at once, and completes.
 * Pacing them to the stamps is the consumer's business, since the board it folds into
 * is driven by the stamps and never by arrival time.
 */
class MockTransferEventFeed(
    /** The items every subscription yields, in order. */
    val script: List<TransferFeedItem>,
) : TransferEventPort {
    override fun events(): Flow<TransferFeedItem> = script.asFlow()

    companion object {
        private const val MIB = 1024L * 1024L

        /**
         * One upload and one fetch, showing everything the transfer view draws: a resume
         * over bytes the server already held, an adaptive resize, a transport retry, a
         * checksum refusal, finalization and a custody receipt; and a ranged fetch that
         * stalls once before its hash verifies. The same script as the Apple client's
         * demo feed.
         */
        fun demo(seed: Long): MockTransferEventFeed {
            val upload = MockIdentifiers.blobHash(seed, 0)
            val fetch = MockIdentifiers.blobHash(seed, 1)
            val session = MockIdentifiers.uploadId(seed, 0)
            val asset = MockIdentifiers.assetId(seed, 0)
            val size = 12 * MIB
            val steps =
                listOf(
                    0L to TransferEvent.SessionCreated(upload, session, size, resumeOffset = 2 * MIB),
                    40L to TransferEvent.ChunkSent(upload, session, 2 * MIB, MIB),
                    420L to TransferEvent.ChunkAcked(upload, session, 2 * MIB, MIB, ackedOffset = 3 * MIB),
                    440L to TransferEvent.ChunkSent(upload, session, 3 * MIB, 2 * MIB),
                    700L to
                        TransferEvent.ChunkRetried(upload, session, 3 * MIB, 2 * MIB, TransferRetryReason.TransportReset(250)),
                    960L to TransferEvent.ChunkSent(upload, session, 3 * MIB, 2 * MIB),
                    1500L to TransferEvent.ChunkAcked(upload, session, 3 * MIB, 2 * MIB, ackedOffset = 5 * MIB),
                    1520L to TransferEvent.ChunkSent(upload, session, 5 * MIB, 2 * MIB),
                    1900L to TransferEvent.ChecksumRejected(upload, session, 5 * MIB, 2 * MIB),
                    1920L to TransferEvent.ChunkSent(upload, session, 5 * MIB, 2 * MIB),
                    2400L to TransferEvent.ChunkAcked(upload, session, 5 * MIB, 2 * MIB, ackedOffset = 7 * MIB),
                    2420L to TransferEvent.ChunkSent(upload, session, 7 * MIB, 5 * MIB),
                    3600L to TransferEvent.ChunkAcked(upload, session, 7 * MIB, 5 * MIB, ackedOffset = size),
                    3620L to TransferEvent.Finalize(upload, session, size),
                    3900L to TransferEvent.Verify(asset, durable = true, blobs = 1, notDurable = 0),
                    3920L to TransferEvent.Receipt(asset, upload, receiptSequence = 1),
                    100L to TransferEvent.RangeRequested(fetch, 0, MIB),
                    600L to TransferEvent.RangeReceived(fetch, 0, MIB),
                    620L to TransferEvent.RangeRequested(fetch, MIB, MIB),
                    2620L to TransferEvent.FetchRetried(fetch, MIB, stalled = true, waitMilliseconds = 500),
                    3120L to TransferEvent.RangeRequested(fetch, MIB, MIB),
                    3500L to TransferEvent.RangeReceived(fetch, MIB, MIB / 2),
                    3520L to TransferEvent.FetchVerified(fetch, MIB + MIB / 2),
                )
            // sortedBy is stable, so two steps at one stamp keep their written order.
            val ordered =
                steps
                    .sortedBy { it.first }
                    .map { (at, event) -> TransferFeedItem.Event(StampedTransferEvent(at, event)) }
            return MockTransferEventFeed(ordered)
        }
    }
}
