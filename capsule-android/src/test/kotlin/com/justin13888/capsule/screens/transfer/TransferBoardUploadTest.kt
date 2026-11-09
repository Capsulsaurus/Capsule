package com.justin13888.capsule.screens.transfer

import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.BlobHash
import com.justin13888.capsule.screens.domain.TransferEvent
import com.justin13888.capsule.screens.domain.TransferFeedItem
import com.justin13888.capsule.screens.domain.TransferRetryReason
import com.justin13888.capsule.screens.domain.UploadId
import com.justin13888.capsule.screens.transfer.BoardFixture.KIB
import com.justin13888.capsule.screens.transfer.BoardFixture.acked
import com.justin13888.capsule.screens.transfer.BoardFixture.at
import com.justin13888.capsule.screens.transfer.BoardFixture.blob
import com.justin13888.capsule.screens.transfer.BoardFixture.board
import com.justin13888.capsule.screens.transfer.BoardFixture.created
import com.justin13888.capsule.screens.transfer.BoardFixture.only
import com.justin13888.capsule.screens.transfer.BoardFixture.retried
import com.justin13888.capsule.screens.transfer.BoardFixture.sent
import com.justin13888.capsule.screens.transfer.BoardFixture.session
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The board's fold over an upload: progress, the chunk in flight, scars, resume,
 * adaptive chunk size, throughput and custody. The same cases the TUI's transfer view
 * and the Apple client's board pin, so every surface draws one stream alike.
 */
class TransferBoardUploadTest {
    private val asset = AssetId("a1")

    @Test
    fun aNewBoardIsEmpty() {
        val board = TransferBoard()
        assertTrue(board.transfers.isEmpty())
        assertEquals(0, board.throughput)
        assertEquals(0, board.activeCount)
        assertEquals(0, board.missed)
        assertEquals(0, board.nowMilliseconds)
    }

    @Test
    fun aCleanUploadMovesItsPrefixAndFinalizes() {
        val board =
            board(
                at(0, created(512 * KIB)),
                at(10, sent(0, 256 * KIB)),
                at(20, acked(0, 256 * KIB)),
                at(30, sent(256 * KIB, 256 * KIB)),
                at(40, acked(256 * KIB, 256 * KIB)),
                at(50, TransferEvent.Finalize(blob, session, 512 * KIB)),
            )
        val transfer = only(board)
        assertEquals(512 * KIB, transfer.done)
        assertEquals(512 * KIB, transfer.moved)
        assertNull(transfer.active)
        assertEquals(Phase.FINALIZING, transfer.phase)
        assertEquals(0, transfer.held)
        // Finalizing is still live: a receipt has yet to verify it.
        assertEquals(1, board.activeCount)
    }

    @Test
    fun aShortWriteLeavesThePrefixAtWhatTheServerKept() {
        val board =
            board(
                at(0, created(512 * KIB)),
                at(1, sent(0, 256 * KIB)),
                at(2, TransferEvent.ChunkAcked(blob, session, 0, 256 * KIB, ackedOffset = 100 * KIB)),
            )
        assertEquals(100 * KIB, only(board).done)
        assertEquals(100 * KIB, only(board).moved)
    }

    @Test
    fun aReceiptVerifiesTheUploadAndCountsTowardCustody() {
        val board =
            board(
                at(0, created(KIB)),
                at(5, TransferEvent.Finalize(blob, session, KIB)),
                at(6, TransferEvent.Verify(asset, durable = true, blobs = 1, notDurable = 0)),
                at(7, TransferEvent.Receipt(asset, blob, receiptSequence = 4)),
            )
        val transfer = only(board)
        assertEquals(Phase.VERIFIED, transfer.phase)
        assertEquals(4, transfer.receiptSequence)
        assertEquals(0, board.activeCount)
        assertEquals(Custody(durable = 1, notDurable = 0, receipts = 1), board.custody)
    }

    @Test
    fun aReceiptForABlobNeverSeenUploadingIsCountedAndNothingMore() {
        val board =
            board(
                at(0, TransferEvent.Receipt(asset, BlobHash("elsewhere"), 1)),
                at(1, TransferEvent.Verify(asset, durable = false, blobs = 2, notDurable = 1)),
            )
        assertTrue(board.transfers.isEmpty())
        assertEquals(1, board.custody.receipts)
        assertEquals(1, board.custody.notDurable)
    }

    @Test
    fun aRetryMarksTheChunkAndLeavesAScarAfterItLands() {
        var board =
            board(
                at(0, created(512 * KIB)),
                at(1, sent(0, 256 * KIB)),
                at(2, retried(0, 256 * KIB, TransferRetryReason.TransportReset(250))),
            )
        assertEquals(1, only(board).retries)
        assertEquals(RetryNote.Upload(TransferRetryReason.TransportReset(250)), only(board).lastRetry)
        assertEquals(listOf(Cell.RETRYING, Cell.PENDING), only(board).cells(2))

        board = board.apply(at(3, sent(0, 256 * KIB))).apply(at(4, acked(0, 256 * KIB)))
        assertEquals(listOf(Cell.ACKED_AFTER_RETRY, Cell.PENDING), only(board).cells(2))
    }

    @Test
    fun aChecksumRefusalCountsMarksTheChunkAndOutranksARetryScar() {
        var board =
            board(
                at(0, created(512 * KIB)),
                at(1, sent(0, 256 * KIB)),
                at(2, TransferEvent.ChecksumRejected(blob, session, 0, 256 * KIB)),
            )
        assertEquals(1, only(board).rejects)
        assertEquals(listOf(Cell.REJECTED, Cell.PENDING), only(board).cells(2))
        board = board.apply(at(3, sent(0, 256 * KIB)))
        assertEquals(listOf(Cell.IN_FLIGHT, Cell.PENDING), only(board).cells(2))
        board = board.apply(at(4, acked(0, 256 * KIB)))
        assertEquals(listOf(Cell.ACKED_AFTER_REJECT, Cell.PENDING), only(board).cells(2))
    }

    @Test
    fun anOffsetRealignMovesThePrefixBack() {
        val board =
            board(
                at(0, created(1024 * KIB)),
                at(1, sent(0, 512 * KIB)),
                at(2, acked(0, 512 * KIB)),
                at(3, sent(512 * KIB, 512 * KIB)),
                at(4, retried(512 * KIB, 512 * KIB, TransferRetryReason.OffsetRealign(256 * KIB))),
            )
        assertEquals(256 * KIB, only(board).done)
        assertEquals(listOf(Cell.ACKED, Cell.PENDING, Cell.RETRYING, Cell.RETRYING), only(board).cells(4))
    }

    @Test
    fun aResumedUploadShowsWhatTheServerAlreadyHeld() {
        val board =
            board(
                at(0, created(1024 * KIB, resume = 512 * KIB)),
                at(1, sent(512 * KIB, 256 * KIB)),
                at(2, acked(512 * KIB, 256 * KIB)),
            )
        val transfer = only(board)
        assertEquals(512 * KIB, transfer.held)
        assertEquals(1, transfer.resumes)
        assertEquals(512 * KIB, transfer.resumedAt)
        assertEquals(256 * KIB, transfer.moved)
        assertEquals(listOf(Cell.HELD, Cell.HELD, Cell.ACKED, Cell.PENDING), transfer.cells(4))
    }

    @Test
    fun aResumeAfterACutContinuesTheSameTransferAndThisRunsBytesAreNotHeld() {
        val board =
            board(
                at(0, created(1024 * KIB)),
                at(1, sent(0, 256 * KIB)),
                at(2, acked(0, 256 * KIB)),
                at(3, sent(256 * KIB, 256 * KIB)),
                at(4, TransferEvent.SessionResumed(blob, session, 1024 * KIB, 256 * KIB)),
            )
        val transfer = only(board)
        assertEquals(1, transfer.resumes)
        assertEquals(1, transfer.sessions)
        assertNull(transfer.active)
        assertEquals(0, transfer.held)
        assertEquals(256 * KIB, transfer.done)
    }

    @Test
    fun aLostSessionReopensOnTheSameTransfer() {
        val board =
            board(
                at(0, created(1024 * KIB)),
                at(1, sent(0, 256 * KIB)),
                at(2, acked(0, 256 * KIB)),
                at(3, sent(256 * KIB, 256 * KIB)),
                at(4, retried(256 * KIB, 256 * KIB, TransferRetryReason.SessionLost)),
                at(5, created(1024 * KIB, resume = 128 * KIB, session = UploadId("s2"))),
            )
        val transfer = only(board)
        assertEquals(2, transfer.sessions)
        assertEquals(UploadId("s2"), transfer.session)
        assertEquals(128 * KIB, transfer.done)
        assertEquals(128 * KIB, transfer.resumedAt)
    }

    @Test
    fun anUploadAfterFinalizingStartsANewTransfer() {
        val board =
            board(
                at(0, created(KIB)),
                at(1, TransferEvent.Finalize(blob, session, KIB)),
                at(2, created(KIB, session = UploadId("s2"))),
            )
        assertEquals(2, board.transfers.size)
    }

    @Test
    fun anEventWhoseCreateWasMissedStillGetsATransfer() {
        val transfer = only(board(at(0, sent(0, 4 * KIB))))
        assertNull(transfer.size)
        assertNull(transfer.cells(10))
        assertEquals(4 * KIB, transfer.chunkSize)
    }

    @Test
    fun aMergeSendsNothingAndFinishesTheTransfer() {
        val board = board(at(0, TransferEvent.Merged(blob, "a1")))
        assertEquals(Phase.MERGED, only(board).phase)
        assertEquals(0, board.activeCount)
    }

    @Test
    fun chunkResizesAreCountedAndTheTailIsNotOne() {
        val board =
            board(
                at(0, created(1000 * KIB)),
                at(1, sent(0, 128 * KIB)),
                at(2, acked(0, 128 * KIB)),
                at(3, sent(128 * KIB, 128 * KIB)),
                at(4, acked(128 * KIB, 128 * KIB)),
                at(5, sent(256 * KIB, 256 * KIB)),
                at(6, acked(256 * KIB, 256 * KIB)),
                at(7, sent(512 * KIB, 128 * KIB)),
                at(8, acked(512 * KIB, 128 * KIB)),
                // The tail: what remains, not a resize.
                at(9, sent(640 * KIB, 360 * KIB)),
            )
        val transfer = only(board)
        assertEquals(2, transfer.chunkSizeChanges)
        assertEquals(listOf(256 * KIB, 128 * KIB), transfer.chunkSizes)
        assertEquals(128 * KIB, transfer.chunkSize)
    }

    @Test
    fun throughputIsTheWindowsAverageAndDecaysOnTicksAndTheClockNeverGoesBack() {
        var board =
            board(
                at(0, created(8192 * KIB)),
                at(500, acked(0, 1024 * KIB)),
                at(1500, acked(1024 * KIB, 1024 * KIB)),
            )
        // 2 MiB over a 2 s window: 1 MiB/s.
        assertEquals(1024 * KIB, board.throughput)
        board = board.tick(2500)
        // The first sample (500 ms) has left the window (500, 2500].
        assertEquals(512 * KIB, board.throughput)
        board = board.tick(10_000)
        assertEquals(0, board.throughput)
        board = board.tick(5)
        assertEquals(10_000, board.nowMilliseconds)
    }

    @Test
    fun missedEventsAreCounted() {
        val board = TransferBoard().apply(TransferFeedItem.Lagged(3)).apply(TransferFeedItem.Lagged(4))
        assertEquals(7, board.missed)
    }

    @Test
    fun byteArithmeticSaturatesInsteadOfWrapping() {
        assertEquals(Long.MAX_VALUE, saturatingAdd(Long.MAX_VALUE - 1, 5))
        assertEquals(Long.MAX_VALUE, saturatingMultiply(Long.MAX_VALUE / 2, 1000))
        val board = TransferBoard().apply(TransferFeedItem.Lagged(Long.MAX_VALUE)).apply(TransferFeedItem.Lagged(1))
        assertEquals(Long.MAX_VALUE, board.missed)
    }
}
