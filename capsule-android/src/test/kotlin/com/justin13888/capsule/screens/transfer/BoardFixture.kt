package com.justin13888.capsule.screens.transfer

import com.justin13888.capsule.screens.domain.BlobHash
import com.justin13888.capsule.screens.domain.StampedTransferEvent
import com.justin13888.capsule.screens.domain.TransferEvent
import com.justin13888.capsule.screens.domain.TransferRetryReason
import com.justin13888.capsule.screens.domain.UploadId
import kotlin.test.assertEquals

/** Builders for the transfer board's tests: one blob, one session, unless a test names others. */
object BoardFixture {
    const val KIB = 1024L
    val blob = BlobHash("b10b")
    val session = UploadId("s1")

    fun at(
        milliseconds: Long,
        event: TransferEvent,
    ) = StampedTransferEvent(milliseconds, event)

    fun created(
        size: Long,
        resume: Long = 0,
        session: UploadId = BoardFixture.session,
        blob: BlobHash = BoardFixture.blob,
    ) = TransferEvent.SessionCreated(blob, session, size, resume)

    fun sent(
        offset: Long,
        length: Long,
    ) = TransferEvent.ChunkSent(blob, session, offset, length)

    fun acked(
        offset: Long,
        length: Long,
    ) = TransferEvent.ChunkAcked(blob, session, offset, length, offset + length)

    fun retried(
        offset: Long,
        length: Long,
        reason: TransferRetryReason,
    ) = TransferEvent.ChunkRetried(blob, session, offset, length, reason)

    fun board(vararg events: StampedTransferEvent): TransferBoard = events.fold(TransferBoard()) { board, event -> board.apply(event) }

    fun only(board: TransferBoard): Transfer {
        assertEquals(1, board.transfers.size, "expected exactly one transfer")
        return board.transfers.single()
    }
}
