package com.justin13888.capsule.screens.transfer

import com.justin13888.capsule.screens.domain.StampedTransferEvent
import com.justin13888.capsule.screens.domain.TransferFeedItem
import com.justin13888.capsule.screens.transfer.BoardFixture.sent
import kotlin.test.Test
import kotlin.test.assertEquals

/** A recorded stream replays at its stamps' pace; a live one, stamped on arrival, is never held. */
class TransferPacingTest {
    private fun event(at: Long) = TransferFeedItem.Event(StampedTransferEvent(at, sent(0, 1)))

    @Test
    fun anEventAheadOfTheClockIsHeldUntilItsStamp() {
        assertEquals(400, TransferBoardViewModel.pacingDelay(event(500), elapsed = 100))
    }

    @Test
    fun anEventAtOrBehindTheClockLandsAtOnce() {
        assertEquals(0, TransferBoardViewModel.pacingDelay(event(100), elapsed = 100))
        assertEquals(0, TransferBoardViewModel.pacingDelay(event(10), elapsed = 100))
    }

    @Test
    fun aLagNoticeIsNeverHeld() {
        assertEquals(0, TransferBoardViewModel.pacingDelay(TransferFeedItem.Lagged(3), elapsed = 0))
    }
}
