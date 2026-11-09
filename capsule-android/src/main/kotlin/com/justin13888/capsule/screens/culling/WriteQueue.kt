package com.justin13888.capsule.screens.culling

import kotlinx.coroutines.channels.Channel

/**
 * Runs store writes one at a time, in the order they were [submit]ted. Each verdict used to
 * launch its own coroutine, so two quick verdicts on one asset could reach the store out of
 * order and leave a different flag stored than the one shown.
 */
class WriteQueue {
    private val writes = Channel<suspend () -> Unit>(Channel.UNLIMITED)

    /** Queue [write] behind every write already queued. */
    fun submit(write: suspend () -> Unit) {
        writes.trySend(write)
    }

    /** Drain the queue until it is [close]d; call from one coroutine only. */
    suspend fun run() {
        for (write in writes) write()
    }

    /** Stop accepting writes; [run] returns once the queued ones are done. */
    fun close() {
        writes.close()
    }
}
