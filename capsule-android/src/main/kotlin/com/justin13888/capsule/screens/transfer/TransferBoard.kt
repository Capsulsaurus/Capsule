package com.justin13888.capsule.screens.transfer

import com.justin13888.capsule.screens.domain.BlobHash
import com.justin13888.capsule.screens.domain.StampedTransferEvent
import com.justin13888.capsule.screens.domain.TransferEvent
import com.justin13888.capsule.screens.domain.TransferEventPort
import com.justin13888.capsule.screens.domain.TransferFeedItem

/**
 * The real-time transfer view's model: what every upload and fetch is doing, folded
 * from the transfer event stream ([TransferEventPort]) and nothing else.
 *
 * A pure fold, the same one the TUI's transfer view and the Apple client's
 * `TransferBoard` run. Each [StampedTransferEvent] carries the milliseconds since the
 * view started watching, and [tick] moves the clock forward between events; the board
 * never reads a clock, so a recorded stream replays into exactly the frames it drew
 * live. Immutable: every step returns the next board, which is what a Compose state
 * holder publishes.
 *
 * What it derives: progress (the acknowledged or landed prefix, and what the server
 * [Transfer.held] before this run), the chunk in flight and the [Scar] it leaves after a
 * retry or checksum refusal, adaptive chunk sizes, throughput over the last
 * [THROUGHPUT_WINDOW_MILLISECONDS] of stream time, and custody.
 *
 * It stays bounded over a long sync whether or not transfers finish: finished transfers
 * past [FINISHED_KEPT] go oldest first, an unfinished one silent for
 * [STALE_AFTER_MILLISECONDS] goes, at most [TRANSFERS_KEPT] are held, and scars and
 * samples are capped. Age is the stream's own time, never a wall clock.
 */
data class TransferBoard(
    /** The board's clock: the latest stamp or tick seen. */
    val nowMilliseconds: Long = 0,
    /** The transfers, oldest first. */
    val transfers: List<Transfer> = emptyList(),
    /** Events the view fell too far behind to receive. */
    val missed: Long = 0,
    /** Custody answers seen. */
    val custody: Custody = Custody(),
) {
    /** Bytes per second across every transfer over the window ending now. */
    val throughput: Long
        get() = transfers.fold(0L) { sum, transfer -> saturatingAdd(sum, transfer.throughput(nowMilliseconds)) }

    /** Transfers not yet finished. */
    val activeCount: Int get() = transfers.count { !it.phase.isFinished }

    /** Fold one item of the feed in. */
    fun apply(item: TransferFeedItem): TransferBoard =
        when (item) {
            is TransferFeedItem.Event -> apply(item.stamped)
            is TransferFeedItem.Lagged -> copy(missed = saturatingAdd(missed, item.count))
        }

    /** Move the clock to [now] (never backwards), so throughput decays while nothing moves. */
    fun tick(now: Long): TransferBoard {
        val clock = maxOf(nowMilliseconds, now)
        val kept =
            transfers
                .map { it.pruned(clock) }
                .filterNot { !it.phase.isFinished && clock - minOf(clock, it.lastMilliseconds) >= STALE_AFTER_MILLISECONDS }
        return copy(nowMilliseconds = clock, transfers = kept)
    }

    /** Fold one event in. */
    fun apply(stamped: StampedTransferEvent): TransferBoard {
        val fold = Fold(tick(stamped.atMilliseconds))
        fold.apply(stamped.event)
        return fold.finish()
    }

    /** One event's worth of mutation over a working copy of the board. */
    private class Fold(
        board: TransferBoard,
    ) {
        private val now = board.nowMilliseconds
        private val missed = board.missed
        private var custody = board.custody
        private val transfers = board.transfers.toMutableList()

        fun apply(event: TransferEvent) {
            when (event) {
                is TransferEvent.Verify ->
                    custody =
                        if (event.durable) {
                            custody.copy(durable = saturatingAdd(custody.durable, 1))
                        } else {
                            custody.copy(notDurable = saturatingAdd(custody.notDurable, 1))
                        }
                is TransferEvent.Receipt -> receipt(event.blob, event.receiptSequence)
                is TransferEvent.RangeRequested,
                is TransferEvent.RangeReceived,
                is TransferEvent.FetchRetried,
                is TransferEvent.FetchVerified,
                is TransferEvent.IntegrityRejected,
                -> applyFetch(event)
                else -> applyUpload(event)
            }
        }

        fun finish(): TransferBoard {
            dropOldFinished()
            capTotal()
            return TransferBoard(now, transfers.toList(), missed, custody)
        }

        private fun applyUpload(event: TransferEvent) {
            when (event) {
                is TransferEvent.SessionCreated -> {
                    // A create after a lost session continues the same transfer; after
                    // finalizing, it is a new upload of the same blob.
                    update(entry(Direction.UPLOAD, event.blob) { it == Phase.ACTIVE }) {
                        it.opened(event.session, event.size).resumed(event.resumeOffset)
                    }
                }
                is TransferEvent.SessionResumed ->
                    update(live(Direction.UPLOAD, event.blob)) { it.opened(event.session, event.size).resumed(event.offset) }
                is TransferEvent.ChunkSent -> update(live(Direction.UPLOAD, event.blob)) { it.sent(event.offset, event.length) }
                is TransferEvent.ChunkAcked ->
                    update(live(Direction.UPLOAD, event.blob)) { it.acked(event.offset, event.ackedOffset, now) }
                is TransferEvent.ChunkRetried ->
                    update(live(Direction.UPLOAD, event.blob)) { it.retried(event.offset, event.length, event.reason) }
                is TransferEvent.ChecksumRejected ->
                    update(live(Direction.UPLOAD, event.blob)) { it.rejected(event.offset, event.length) }
                is TransferEvent.Finalize ->
                    update(live(Direction.UPLOAD, event.blob)) { it.finalizing(event.session, event.size) }
                is TransferEvent.Merged -> update(live(Direction.UPLOAD, event.blob)) { it.copy(active = null, phase = Phase.MERGED) }
                else -> Unit
            }
        }

        private fun applyFetch(event: TransferEvent) {
            when (event) {
                is TransferEvent.RangeRequested ->
                    update(live(Direction.FETCH, event.blob)) {
                        val requested = it.copy(active = ActiveChunk(event.offset, event.maxLength, ChunkState.IN_FLIGHT))
                        // A fetch keeps one window for its whole run (only the tail asks for
                        // less), so its first bounded window is its chunk size.
                        val window = event.maxLength
                        if (window != null && it.chunkSizes.isEmpty()) requested.copy(chunkSizes = listOf(window)) else requested
                    }
                is TransferEvent.RangeReceived ->
                    update(live(Direction.FETCH, event.blob)) {
                        it.movedBy(event.length, now).copy(done = saturatingAdd(event.offset, event.length), active = null)
                    }
                is TransferEvent.FetchRetried ->
                    update(live(Direction.FETCH, event.blob)) {
                        val length = it.active?.length
                        it
                            .scarred(event.offset, length, ScarKind.RETRIED)
                            .copy(
                                retries = it.retries + 1,
                                lastRetry = RetryNote.Fetch(event.stalled, event.waitMilliseconds),
                                active = ActiveChunk(event.offset, length, ChunkState.RETRYING),
                            ).realigned(event.offset)
                    }
                is TransferEvent.FetchVerified ->
                    update(live(Direction.FETCH, event.blob)) {
                        it.copy(size = event.size, done = event.size, active = null, phase = Phase.VERIFIED)
                    }
                is TransferEvent.IntegrityRejected ->
                    update(live(Direction.FETCH, event.blob)) {
                        it.copy(size = event.size, rejects = it.rejects + 1, active = null, phase = Phase.INTEGRITY_REJECTED)
                    }
                else -> Unit
            }
        }

        /** A receipt verifies the latest upload of the blob; one for a blob never seen uploaded is only counted. */
        private fun receipt(
            blob: BlobHash,
            sequence: Long,
        ) {
            custody = custody.copy(receipts = saturatingAdd(custody.receipts, 1))
            val index = transfers.indexOfLast { it.direction == Direction.UPLOAD && it.blob == blob }
            if (index < 0) return
            update(index) {
                val stamped = it.copy(lastMilliseconds = now, receiptSequence = sequence)
                if (it.phase == Phase.MERGED) {
                    stamped
                } else {
                    stamped.copy(done = it.size ?: it.done, active = null, phase = Phase.VERIFIED)
                }
            }
        }

        private inline fun update(
            index: Int,
            step: (Transfer) -> Transfer,
        ) {
            transfers[index] = step(transfers[index])
        }

        /** The live transfer of [blob] that way, or a new one (the stream may have lagged past its start). */
        private fun live(
            direction: Direction,
            blob: BlobHash,
        ): Int = entry(direction, blob) { !it.isFinished }

        /** The latest transfer of [blob] that way whose phase [continues], or a new one, stamped now. */
        private fun entry(
            direction: Direction,
            blob: BlobHash,
            continues: (Phase) -> Boolean,
        ): Int {
            val found = transfers.indexOfLast { it.direction == direction && it.blob == blob && continues(it.phase) }
            if (found < 0) {
                transfers.add(Transfer(blob = blob, direction = direction, startedMilliseconds = now))
                return transfers.lastIndex
            }
            transfers[found] = transfers[found].copy(lastMilliseconds = now)
            return found
        }

        /** Hold at most [TRANSFERS_KEPT]: drop the one silent longest until it fits. */
        private fun capTotal() {
            while (transfers.size > TRANSFERS_KEPT) {
                val oldest = transfers.indices.minBy { transfers[it].lastMilliseconds }
                transfers.removeAt(oldest)
            }
        }

        /** Keep the newest [FINISHED_KEPT] finished transfers. */
        private fun dropOldFinished() {
            var excess = transfers.count { it.phase.isFinished } - FINISHED_KEPT
            if (excess <= 0) return
            val iterator = transfers.iterator()
            while (excess > 0 && iterator.hasNext()) {
                if (iterator.next().phase.isFinished) {
                    iterator.remove()
                    excess -= 1
                }
            }
        }
    }

    companion object {
        /** The span of stream time throughput is averaged over. */
        const val THROUGHPUT_WINDOW_MILLISECONDS = 2000L

        /** Finished transfers (verified, merged, rejected) kept for display. */
        const val FINISHED_KEPT = 32

        /** Stream time an unfinished transfer may go without an event before it is dropped. */
        const val STALE_AFTER_MILLISECONDS = 300_000L

        /** Transfers held in all; the one silent longest goes past this. */
        const val TRANSFERS_KEPT = 256

        /** Scars kept per transfer. */
        const val SCARS_KEPT = 64

        /** Distinct chunk sizes remembered per transfer, the current one last. */
        const val CHUNK_SIZES_KEPT = 4

        /** Whether a sample at [milliseconds] is inside the throughput window ending at [now]. */
        internal fun inWindow(
            milliseconds: Long,
            now: Long,
        ): Boolean = now < THROUGHPUT_WINDOW_MILLISECONDS || milliseconds > now - THROUGHPUT_WINDOW_MILLISECONDS
    }
}
