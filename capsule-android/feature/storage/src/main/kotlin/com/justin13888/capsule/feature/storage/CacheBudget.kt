package com.justin13888.capsule.feature.storage

/**
 * How much rebuildable data this device is allowed to keep.
 *
 * A short closed set rather than a free byte entry. The client filesystem design makes the
 * budget the user's one lever over automatic reclamation, and a lever with four notches is
 * one a user can actually reason about; a numeric field invites a value like `1` that the
 * user meant as gigabytes and the sweep reads as bytes.
 *
 * Every option is safe to choose: everything inside the budget is a thumbnail, a preview or
 * a transcode, all of which are rebuilt or fetched again on demand. Originals this device
 * owns are never inside it — they leave only through the verify-before-destroy gate.
 */
enum class CacheBudget(
    /** The ceiling handed to the eviction sweep. */
    val bytes: Long,
) {
    /** For a phone that is short of room and mostly online. */
    Small(1L * BYTES_PER_GIB),

    /** The default: a browsable offline grid without a large standing cost. */
    Moderate(4L * BYTES_PER_GIB),

    /** For a device that spends time away from a network. */
    Large(16L * BYTES_PER_GIB),

    /** For a tablet used as the main library. */
    Generous(64L * BYTES_PER_GIB),
    ;

    companion object {
        /** What a fresh install starts on. */
        val DEFAULT = Moderate

        /** The smallest budget that would hold [bytes], or the largest option if none would. */
        fun smallestHolding(bytes: Long): CacheBudget = entries.firstOrNull { it.bytes >= bytes } ?: Generous
    }
}

private const val BYTES_PER_GIB = 1024L * 1024 * 1024
