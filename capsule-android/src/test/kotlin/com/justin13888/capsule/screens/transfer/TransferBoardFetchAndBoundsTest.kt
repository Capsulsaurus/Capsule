package com.justin13888.capsule.screens.transfer

import com.justin13888.capsule.screens.domain.BlobHash
import com.justin13888.capsule.screens.domain.TransferEvent
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
 * The board's fold over a ranged fetch, the chunk strip's geometry, and the bounds that
 * keep the board small over a long sync whether or not its transfers ever finish.
 */
class TransferBoardFetchAndBoundsTest {
    private val fetched = BlobHash("f")

    private fun createdBlob(name: String) = created(KIB, session = UploadId("s"), blob = BlobHash(name))

    @Test
    fun aFetchTracksItsLandedPrefixItsRetriesAndItsVerdict() {
        var board =
            board(
                at(0, TransferEvent.RangeRequested(fetched, 0, 512)),
                at(0, TransferEvent.RangeReceived(fetched, 0, 512)),
                at(0, TransferEvent.RangeRequested(fetched, 512, 512)),
                at(0, TransferEvent.FetchRetried(fetched, 512, stalled = true, waitMilliseconds = 40)),
            )
        val transfer = only(board)
        assertEquals(Direction.FETCH, transfer.direction)
        assertEquals(512, transfer.done)
        assertEquals(1, transfer.retries)
        assertEquals(512, transfer.chunkSize)
        assertEquals(RetryNote.Fetch(stalled = true, waitMilliseconds = 40), transfer.lastRetry)
        // A fetch's size is unknown until its verdict.
        assertNull(transfer.cells(4))

        board = board.apply(at(0, TransferEvent.FetchVerified(fetched, 1024)))
        assertEquals(Phase.VERIFIED, only(board).phase)
        assertEquals(listOf(Cell.VERIFIED, Cell.VERIFIED), only(board).cells(2))
    }

    @Test
    fun aFetchThatFailsItsHashIsRejectedWhole() {
        val transfer = only(board(at(0, TransferEvent.IntegrityRejected(fetched, 10))))
        assertEquals(Phase.INTEGRITY_REJECTED, transfer.phase)
        assertEquals(1, transfer.rejects)
        assertEquals(listOf(Cell.REJECTED, Cell.REJECTED), transfer.cells(2))
    }

    @Test
    fun anOpenEndedFetchCoversTheRestOfTheStrip() {
        val transfer =
            Transfer(
                blob = fetched,
                direction = Direction.FETCH,
                startedMilliseconds = 0,
                size = 400,
                done = 100,
                active = ActiveChunk(100, null, ChunkState.IN_FLIGHT),
            )
        assertEquals(listOf(Cell.ACKED, Cell.IN_FLIGHT, Cell.IN_FLIGHT, Cell.IN_FLIGHT), transfer.cells(4))
    }

    @Test
    fun theStripHasNoMoreCellsThanBytesAndNoneForAnEmptyBlob() {
        val transfer = Transfer(blob = BlobHash("b"), direction = Direction.UPLOAD, startedMilliseconds = 0, size = 3)
        assertEquals(3, transfer.cells(10)?.size)
        assertNull(transfer.cells(0))
        assertNull(transfer.copy(size = 0).cells(10))
    }

    @Test
    fun stripBoundsStayExactForAHugeBlob() {
        val huge = Long.MAX_VALUE - 7
        val transfer =
            Transfer(blob = BlobHash("h"), direction = Direction.UPLOAD, startedMilliseconds = 0, size = huge, done = huge / 2)
        val cells = transfer.cells(4)
        assertEquals(listOf(Cell.ACKED, Cell.ACKED, Cell.PENDING, Cell.PENDING), cells)
    }

    @Test
    fun aPartlyAcknowledgedCellIsStillPending() {
        val board = board(at(0, created(400)), at(1, sent(0, 150)), at(2, acked(0, 150)))
        assertEquals(listOf(Cell.ACKED, Cell.PENDING, Cell.PENDING, Cell.PENDING), only(board).cells(4))
    }

    @Test
    fun finishedTransfersPastTheBoundAreDroppedOldestFirstAndALiveOneNeverIs() {
        var board = TransferBoard()
        for (index in 0 until TransferBoard.FINISHED_KEPT + 3) {
            board = board.apply(at(index.toLong(), TransferEvent.FetchVerified(BlobHash("f$index"), 1)))
        }
        board = board.apply(at(100, sent(0, 1)))
        assertEquals(TransferBoard.FINISHED_KEPT + 1, board.transfers.size)
        assertEquals(BlobHash("f3"), board.transfers.first().blob)
    }

    @Test
    fun anUnfinishedTransferSilentForTheStaleIntervalAgesOut() {
        val stale = TransferBoard.STALE_AFTER_MILLISECONDS
        var board = TransferBoard().apply(at(0, createdBlob("quiet"))).apply(at(1, createdBlob("busy")))
        board = board.tick(stale - 1)
        assertEquals(2, board.transfers.size)
        // An event keeps one alive; the other crosses the interval on a tick.
        board = board.apply(at(stale - 1, createdBlob("busy"))).tick(stale)
        assertEquals(1, board.transfers.size)
        assertTrue(board.tick(2 * stale).transfers.isEmpty())
    }

    @Test
    fun aFinalizingUploadWhoseReceiptNeverCameAgesOut() {
        val board = board(at(0, created(KIB)), at(2, TransferEvent.Finalize(blob, session, KIB)))
        assertEquals(Phase.FINALIZING, only(board).phase)
        assertTrue(board.tick(2 + TransferBoard.STALE_AFTER_MILLISECONDS).transfers.isEmpty())
    }

    @Test
    fun anEventAfterAgeingOutStartsAFreshTransfer() {
        val stale = TransferBoard.STALE_AFTER_MILLISECONDS
        val board = board(at(0, created(KIB))).tick(stale).apply(at(stale + 1, sent(0, 1)))
        assertEquals(stale + 1, only(board).startedMilliseconds)
    }

    @Test
    fun neverFinishedTransfersKeepTheBoardBoundedByTheCapAndByAge() {
        val kept = TransferBoard.TRANSFERS_KEPT
        var board = TransferBoard()
        for (index in 0 until kept * 4) {
            board = board.apply(at(index.toLong(), createdBlob("c$index")))
            assertTrue(board.transfers.size <= kept)
        }
        assertEquals(kept, board.transfers.size)
        // The ones silent longest were the ones dropped.
        assertEquals(BlobHash("c${kept * 3}"), board.transfers.first().blob)

        var aged = TransferBoard()
        val spacing = TransferBoard.STALE_AFTER_MILLISECONDS / 8
        var peak = 0
        for (index in 0L until 2000L) {
            aged = aged.apply(at(index * spacing, createdBlob("a$index")))
            peak = maxOf(peak, aged.transfers.size)
        }
        assertTrue(peak <= 8)
        assertTrue(aged.activeCount <= 8)
    }

    @Test
    fun scarsAreBoundedAndRetriesAreStillAllCounted() {
        val events =
            listOf(at(0, created(KIB))) +
                (0 until TransferBoard.SCARS_KEPT + 5).map { at(it.toLong(), retried(0, 1, TransferRetryReason.ServerWait(0))) }
        val transfer = only(board(*events.toTypedArray()))
        assertEquals(TransferBoard.SCARS_KEPT, transfer.scars.size)
        assertEquals(TransferBoard.SCARS_KEPT + 5, transfer.retries)
    }

    @Test
    fun chunkSizeHistoryIsBoundedToTheLatestSizes() {
        var offset = 0L
        val sends =
            listOf(4L, 8, 16, 32, 64, 128).mapIndexed { index, length ->
                at(index + 1L, sent(offset, length * KIB)).also { offset += length * KIB }
            }
        val transfer = only(board(at(0, created(100_000 * KIB)), *sends.toTypedArray()))
        assertEquals(5, transfer.chunkSizeChanges)
        assertEquals(TransferBoard.CHUNK_SIZES_KEPT, transfer.chunkSizes.size)
        assertEquals(128 * KIB, transfer.chunkSize)
    }
}
