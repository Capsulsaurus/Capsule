package com.justin13888.capsule.screens.transfer

import com.justin13888.capsule.screens.domain.BlobHash
import com.justin13888.capsule.screens.domain.TransferRetryReason
import com.justin13888.capsule.screens.domain.UploadId

/** Which way the bytes move. */
enum class Direction {
    /** To the server: an upload session. */
    UPLOAD,

    /** From the server: a ranged fetch. */
    FETCH,
}

/** One cell of a transfer's chunk strip: what the bytes it covers are doing. */
enum class Cell {
    /** Not sent yet. */
    PENDING,

    /** Held by the server before this run sent anything. */
    HELD,

    /** Acknowledged (or landed, for a fetch). */
    ACKED,

    /** Acknowledged after a retry. */
    ACKED_AFTER_RETRY,

    /** Acknowledged after a checksum refusal. */
    ACKED_AFTER_REJECT,

    /** Verified: a custody receipt, or a matching hash. */
    VERIFIED,

    /** In flight now. */
    IN_FLIGHT,

    /** Being retried now. */
    RETRYING,

    /** Refused for a checksum mismatch, or the whole fetch failed its hash. */
    REJECTED,
}

/** What the chunk in flight is doing. */
enum class ChunkState(
    internal val cell: Cell,
) {
    /** Sent (or requested) and not yet answered. */
    IN_FLIGHT(Cell.IN_FLIGHT),

    /** Refused or lost; it is sent again after a backoff. */
    RETRYING(Cell.RETRYING),

    /** The server refused it for a checksum mismatch; it is sent again. */
    REJECTED(Cell.REJECTED),
}

/** The one chunk a transfer has in flight (the SDK sends sequentially); [length] `null` is open-ended. */
data class ActiveChunk(
    val offset: Long,
    val length: Long?,
    val state: ChunkState,
)

/** What went wrong with a chunk that has since moved on. */
enum class ScarKind {
    /** It needed a retry. */
    RETRIED,

    /** It was refused for a checksum mismatch. */
    REJECTED,
}

/** Where trouble was, after the chunk moved on: a byte range ([length] `null` to the end) and what happened. */
data class Scar(
    val offset: Long,
    val length: Long?,
    val kind: ScarKind,
)

/** Why the last retry happened, as the stream said. */
sealed interface RetryNote {
    /** An upload chunk was retried. */
    data class Upload(
        val reason: TransferRetryReason,
    ) : RetryNote

    /** A ranged fetch brought nothing and backs off. */
    data class Fetch(
        val stalled: Boolean,
        val waitMilliseconds: Long,
    ) : RetryNote
}

/** Where a transfer is in its life. */
enum class Phase(
    /** No further event changes this transfer; a new one for the same blob starts afresh. */
    val isFinished: Boolean,
) {
    /** Moving bytes, or about to. */
    ACTIVE(false),

    /** Every byte of an upload reached the server; it is finalizing. */
    FINALIZING(false),

    /** A custody receipt verified the upload, or the fetched bytes hashed to the address. */
    VERIFIED(true),

    /** The server already held the blob; nothing was sent. */
    MERGED(true),

    /** The fetched bytes did not hash to the requested address and were discarded. */
    INTEGRITY_REJECTED(true),
}

/** Custody answers seen, across every asset. */
data class Custody(
    /** Assets the server called durable. */
    val durable: Long = 0,
    /** Assets with at least one blob not durable. */
    val notDurable: Long = 0,
    /** Receipts verified. */
    val receipts: Long = 0,
)

/** Bytes moved at one instant, kept while inside the throughput window. */
data class Sample(
    val milliseconds: Long,
    val bytes: Long,
)

/**
 * Everything the transfer view shows about one transfer. Immutable: the board's fold
 * replaces it with an updated copy on every event.
 */
data class Transfer(
    /** The blob's content address. */
    val blob: BlobHash,
    /** Which way the bytes move. */
    val direction: Direction,
    /** When the first event for it arrived. */
    val startedMilliseconds: Long,
    /** When the last event for it arrived. */
    val lastMilliseconds: Long = startedMilliseconds,
    /** The current upload session, once one is open. */
    val session: UploadId? = null,
    /** The blob's size, once the stream has said it. */
    val size: Long? = null,
    /** The authoritative prefix: bytes acknowledged (upload) or landed (fetch). */
    val done: Long = 0,
    /** The prefix the server held before this run sent anything. */
    val held: Long = 0,
    /** The chunk in flight. */
    val active: ActiveChunk? = null,
    /** Where chunks needed a retry or were refused, newest last. */
    val scars: List<Scar> = emptyList(),
    /** Distinct upload sessions it ran in (more than one after a lost session). */
    val sessions: Int = 0,
    /** Times the transfer resumed from bytes the server already held. */
    val resumes: Int = 0,
    /** The offset of the last resume. */
    val resumedAt: Long? = null,
    /** Retries taken. */
    val retries: Int = 0,
    /** Checksum refusals (upload) or failed hashes (fetch). */
    val rejects: Int = 0,
    /** Why the last retry happened. */
    val lastRetry: RetryNote? = null,
    /** Distinct chunk sizes, the current one last. */
    val chunkSizes: List<Long> = emptyList(),
    /** How many times the adaptive strategy resized the chunk. */
    val chunkSizeChanges: Int = 0,
    /** Bytes moved by this run. */
    val moved: Long = 0,
    /** The receipt that verified an upload. */
    val receiptSequence: Long? = null,
    /** Where it is in its life. */
    val phase: Phase = Phase.ACTIVE,
    /** Bytes moved, within the throughput window. */
    val samples: List<Sample> = emptyList(),
) {
    /** The current chunk size, once a non-final chunk has been sent. */
    val chunkSize: Long? get() = chunkSizes.lastOrNull()

    /**
     * Bytes per second over the [TransferBoard.THROUGHPUT_WINDOW_MILLISECONDS] ending at
     * [now]: the window is `(now - window, now]`, or everything before the clock has run
     * a whole window.
     */
    fun throughput(now: Long): Long {
        val bytes =
            samples
                .filter { TransferBoard.inWindow(it.milliseconds, now) }
                .fold(0L) { sum, sample -> saturatingAdd(sum, sample.bytes) }
        return saturatingMultiply(bytes, MILLIS_PER_SECOND) / TransferBoard.THROUGHPUT_WINDOW_MILLISECONDS
    }

    /**
     * The chunk strip in [wanted] cells (fewer when the blob has fewer bytes), or `null`
     * while the size is unknown or zero.
     */
    fun cells(wanted: Int): List<Cell>? {
        val size = size ?: return null
        if (size <= 0 || wanted <= 0) return null
        val count = minOf(wanted.toLong(), size)
        // floor(size * index / count) without overflow: count fits an Int, so the
        // remainder term's product does too.
        val bound = { index: Long -> (size / count) * index + (size % count) * index / count }
        return (0 until count).map { cell(bound(it), bound(it + 1), size) }
    }

    /** The state of the bytes `low until high` of a blob of [size] bytes. */
    private fun cell(
        low: Long,
        high: Long,
        size: Long,
    ): Cell {
        when (phase) {
            Phase.VERIFIED -> return Cell.VERIFIED
            Phase.INTEGRITY_REJECTED -> return Cell.REJECTED
            Phase.ACTIVE, Phase.FINALIZING, Phase.MERGED -> Unit
        }
        val overlaps = { offset: Long, length: Long? ->
            val end = length?.let { saturatingAdd(offset, it) } ?: size
            offset < high && low < end
        }
        val chunk = active
        return when {
            chunk != null && overlaps(chunk.offset, chunk.length) -> chunk.state.cell
            high <= held -> Cell.HELD
            high > done -> Cell.PENDING
            else -> acked(overlaps)
        }
    }

    /** An acknowledged cell, marked by the worst scar that [overlaps] it. */
    private fun acked(overlaps: (Long, Long?) -> Boolean): Cell {
        val hits = scars.filter { overlaps(it.offset, it.length) }
        return when {
            hits.any { it.kind == ScarKind.REJECTED } -> Cell.ACKED_AFTER_REJECT
            hits.isNotEmpty() -> Cell.ACKED_AFTER_RETRY
            else -> Cell.ACKED
        }
    }

    // Folding: each returns the transfer after one step.

    /** The server's offset is [offset]: everything below it is held, nothing above it is. */
    internal fun realigned(offset: Long): Transfer = copy(done = offset, held = minOf(held, offset))

    /** The transfer now runs in [newSession] over a blob of [newSize] bytes. */
    internal fun opened(
        newSession: UploadId,
        newSize: Long,
    ): Transfer =
        copy(
            sessions = if (session != newSession) sessions + 1 else sessions,
            session = newSession,
            size = newSize,
            active = null,
        )

    /** A resume reported [offset] as already held; before this run moved a byte, that prefix is the server's. */
    internal fun resumed(offset: Long): Transfer {
        val counted = if (offset > 0) copy(resumes = resumes + 1, resumedAt = offset) else this
        val kept = if (moved == 0L) counted.copy(held = offset) else counted
        return kept.realigned(offset)
    }

    internal fun scarred(
        offset: Long,
        length: Long?,
        kind: ScarKind,
    ): Transfer = copy(scars = (scars + Scar(offset, length, kind)).takeLast(TransferBoard.SCARS_KEPT))

    internal fun sent(
        offset: Long,
        length: Long,
    ): Transfer {
        val inFlight = copy(active = ActiveChunk(offset, length, ChunkState.IN_FLIGHT))
        // The last chunk is whatever remains, not the adaptive size.
        val tail = size?.let { saturatingAdd(offset, length) >= it } ?: false
        if (tail || chunkSize == length) return inFlight
        return inFlight.copy(
            chunkSizeChanges = if (chunkSizes.isEmpty()) chunkSizeChanges else chunkSizeChanges + 1,
            chunkSizes = (chunkSizes.filter { it != length } + length).takeLast(TransferBoard.CHUNK_SIZES_KEPT),
        )
    }

    /** The server acknowledged the chunk at [offset] and now holds [ackedOffset] bytes. */
    internal fun acked(
        offset: Long,
        ackedOffset: Long,
        now: Long,
    ): Transfer = movedBy(if (ackedOffset > offset) ackedOffset - offset else 0, now).copy(done = ackedOffset, active = null)

    /** An upload chunk will be sent again, or the transfer moves to the offset the server named. */
    internal fun retried(
        offset: Long,
        length: Long,
        reason: TransferRetryReason,
    ): Transfer {
        val marked =
            scarred(offset, length, ScarKind.RETRIED).copy(
                retries = retries + 1,
                lastRetry = RetryNote.Upload(reason),
                active = ActiveChunk(offset, length, ChunkState.RETRYING),
            )
        return if (reason is TransferRetryReason.OffsetRealign) marked.realigned(reason.toOffset) else marked
    }

    /** The server refused a chunk for a checksum mismatch. */
    internal fun rejected(
        offset: Long,
        length: Long,
    ): Transfer =
        scarred(offset, length, ScarKind.REJECTED).copy(
            rejects = rejects + 1,
            active = ActiveChunk(offset, length, ChunkState.REJECTED),
        )

    /** Every byte reached the server, which is finalizing the blob. */
    internal fun finalizing(
        finalSession: UploadId,
        finalSize: Long,
    ): Transfer = copy(session = finalSession, size = finalSize, done = finalSize, active = null, phase = Phase.FINALIZING)

    internal fun movedBy(
        bytes: Long,
        now: Long,
    ): Transfer = copy(moved = saturatingAdd(moved, bytes), samples = samples + Sample(now, bytes))

    internal fun pruned(now: Long): Transfer {
        if (samples.all { TransferBoard.inWindow(it.milliseconds, now) }) return this
        return copy(samples = samples.filter { TransferBoard.inWindow(it.milliseconds, now) })
    }

    private companion object {
        const val MILLIS_PER_SECOND = 1000L
    }
}

/** [a] + [b] for non-negative byte counts, pinned at [Long.MAX_VALUE] instead of wrapping. */
internal fun saturatingAdd(
    a: Long,
    b: Long,
): Long {
    val sum = a + b
    return if (((a xor sum) and (b xor sum)) < 0) Long.MAX_VALUE else sum
}

internal fun saturatingMultiply(
    a: Long,
    b: Long,
): Long =
    try {
        Math.multiplyExact(a, b)
    } catch (_: ArithmeticException) {
        Long.MAX_VALUE
    }
