package com.justin13888.capsule.screens.transfer

import com.justin13888.capsule.screens.domain.TransferFeedItem
import com.justin13888.capsule.screens.mock.MockTransferEventFeed
import kotlinx.coroutines.flow.fold
import kotlinx.coroutines.runBlocking
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

/**
 * The mock lane's recorded stream folds into the board the transfer view previews:
 * every state the view draws is reached, and the stream replays into the same board
 * every time.
 */
class TransferBoardDemoFeedTest {
    private fun fold(feed: MockTransferEventFeed): TransferBoard =
        runBlocking { feed.events().fold(TransferBoard()) { board, item -> board.apply(item) } }

    @Test
    fun theDemoUploadResumesResizesRetriesIsRefusedOnceAndIsVerifiedByAReceipt() {
        val board = fold(MockTransferEventFeed.demo(seed = 1))
        val upload = board.transfers.first { it.direction == Direction.UPLOAD }
        assertEquals(Phase.VERIFIED, upload.phase)
        assertTrue(upload.held > 0)
        assertEquals(1, upload.retries)
        assertEquals(1, upload.rejects)
        assertTrue(upload.chunkSizeChanges >= 1)
        assertEquals(1, upload.receiptSequence)
        assertEquals(Custody(durable = 1, notDurable = 0, receipts = 1), board.custody)
    }

    @Test
    fun theDemoFetchStallsOnceAndVerifies() {
        val board = fold(MockTransferEventFeed.demo(seed = 1))
        val fetch = board.transfers.first { it.direction == Direction.FETCH }
        assertEquals(Phase.VERIFIED, fetch.phase)
        assertEquals(RetryNote.Fetch(stalled = true, waitMilliseconds = 500), fetch.lastRetry)
        assertEquals(0, board.activeCount)
    }

    @Test
    fun theStreamIsInStampOrderAndReplaysIntoTheSameBoard() {
        val feed = MockTransferEventFeed.demo(seed = 1)
        val stamps = feed.script.filterIsInstance<TransferFeedItem.Event>().map { it.stamped.atMilliseconds }
        assertEquals(stamps.sorted(), stamps)
        assertEquals(fold(feed), fold(feed))
    }
}
